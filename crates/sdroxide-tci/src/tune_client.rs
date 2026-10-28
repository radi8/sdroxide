//! Outbound TCI client that follows this radio's dial.
//!
//! sdroxide's other TCI client ([`crate::TciDevice`]) takes wideband IQ from
//! somebody else's rig and uses that rig as the front end. This one does not.
//! It connects to a TCI server — 590Ctrl, in the setup it was written for —
//! and sends the VFO whenever the operator tunes here, so that radio moves
//! with the dial. One text line, `vfo:0,0,<hz>;`. No IQ, no audio, and never
//! `trx:` or `tune:`: those key the transmitter, and a tune of the dial is
//! not a request to go on the air.
//!
//! Incoming text is read and discarded. 590Ctrl answers a frequency command
//! by broadcasting the new VFO, and acting on that would chase our own echo.

use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use tungstenite::{Message, WebSocket};

use crate::{HANDSHAKE_TIMEOUT, connect_socket, split_addr, ws_handshake};

/// How long a connect attempt may sit before the worker tries again.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// Read timeout once the socket is up. Also the longest [`TciTuneClient`]'s
/// drop waits to join the worker, and the fastest a new frequency goes out.
const READ_TIMEOUT: Duration = Duration::from_millis(50);

/// Don't hand the far radio a new frequency more often than this. A waterfall
/// drag is a frequency per frame; the server only needs the latest one, and
/// 590Ctrl still has to turn each one into a CAT command.
const MIN_SEND_GAP: Duration = Duration::from_millis(50);

/// What the settings dialog shows for the outbound client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TciTuneStatus {
    pub connected: bool,
    pub error: Option<String>,
}

/// The engine's handle on the outbound client. Dropping it closes the socket.
pub struct TciTuneClient {
    hz: Arc<AtomicU64>,
    address: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    status_rx: Receiver<TciTuneStatus>,
    thread: Option<JoinHandle<()>>,
}

impl TciTuneClient {
    /// Start connecting to `address` (`host[:port]`, port 50001 if omitted).
    /// The socket comes up on the worker thread; [`Self::poll_status`] reports
    /// when it has.
    pub fn start(address: String) -> Self {
        let address = Arc::new(Mutex::new(address));
        let hz = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (status_tx, status_rx) = crossbeam_channel::unbounded();
        let thread = thread::spawn({
            let address = Arc::clone(&address);
            let hz = Arc::clone(&hz);
            let stop = Arc::clone(&stop);
            move || worker(address, hz, stop, status_tx)
        });
        Self { hz, address, stop, status_rx, thread: Some(thread) }
    }

    /// The address the worker is aiming at.
    pub fn address(&self) -> String {
        self.address.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Point at a different server. The worker drops the old socket and
    /// connects to this one; the frequency it has is sent again once up.
    pub fn set_address(&self, address: String) {
        if let Ok(mut g) = self.address.lock() {
            *g = address;
        }
    }

    /// Remember `hz` as the dial to send. Only the newest value is kept, so a
    /// drag does not queue a line per step.
    pub fn tune(&self, hz: u64) {
        self.hz.store(hz, Ordering::Relaxed);
    }

    /// The newest status the worker has reported since the last call.
    pub fn poll_status(&self) -> Option<TciTuneStatus> {
        let mut last = None;
        while let Ok(status) = self.status_rx.try_recv() {
            last = Some(status);
        }
        last
    }
}

impl Drop for TciTuneClient {
    fn drop(&mut self) {
        // Don't join: a worker inside `connect` can take seconds to notice,
        // and this runs on the engine thread. It exits on its own.
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take();
    }
}

/// The one command this client ever writes.
pub(crate) fn vfo_line(hz: u64) -> String {
    format!("vfo:0,0,{hz};")
}

fn worker(
    address: Arc<Mutex<String>>,
    hz: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    status_tx: Sender<TciTuneStatus>,
) {
    let mut backoff = Duration::from_millis(200);
    let mut socket: Option<WebSocket<TcpStream>> = None;
    let mut connected_to = String::new();
    let mut sent: u64 = 0;
    let mut last_tx = Instant::now() - MIN_SEND_GAP;
    let mut reported = TciTuneStatus { connected: false, error: None };

    while !stop.load(Ordering::Relaxed) {
        let want_addr = address.lock().map(|g| g.clone()).unwrap_or_default();
        if socket.is_some() && want_addr != connected_to {
            close(&mut socket);
            sent = 0;
            report(&status_tx, &mut reported, TciTuneStatus { connected: false, error: None });
        }

        if socket.is_none() {
            if want_addr.trim().is_empty() {
                report(
                    &status_tx,
                    &mut reported,
                    TciTuneStatus {
                        connected: false,
                        error: Some("TCI Client needs a host:port".into()),
                    },
                );
                sleep_until_stop(&stop, Duration::from_millis(200));
                continue;
            }
            match connect(&want_addr) {
                Ok(ws) => {
                    tracing::info!(addr = %want_addr, "TCI client connected");
                    socket = Some(ws);
                    connected_to = want_addr;
                    backoff = Duration::from_millis(200);
                    sent = 0;
                    report(
                        &status_tx,
                        &mut reported,
                        TciTuneStatus { connected: true, error: None },
                    );
                }
                Err(error) => {
                    tracing::warn!("TCI client: {error}");
                    report(
                        &status_tx,
                        &mut reported,
                        TciTuneStatus { connected: false, error: Some(error) },
                    );
                    sleep_until_stop(&stop, backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    continue;
                }
            }
        }

        if !drain(&mut socket, &status_tx, &mut reported) {
            sent = 0;
            continue;
        }

        let want = hz.load(Ordering::Relaxed);
        if want > 0 && want != sent && last_tx.elapsed() >= MIN_SEND_GAP {
            let line = vfo_line(want);
            match send_line(socket.as_mut(), line) {
                Ok(()) => {
                    tracing::trace!(hz = want, "TCI client tune");
                    sent = want;
                    last_tx = Instant::now();
                }
                Err(error) => {
                    tracing::warn!("TCI client send: {error}");
                    close(&mut socket);
                    sent = 0;
                    report(
                        &status_tx,
                        &mut reported,
                        TciTuneStatus { connected: false, error: Some(format!("send: {error}")) },
                    );
                }
            }
        }
    }
    close(&mut socket);
}

fn send_line(socket: Option<&mut WebSocket<TcpStream>>, line: String) -> Result<(), String> {
    let Some(ws) = socket else {
        return Err("not connected".into());
    };
    ws.send(Message::Text(line.into())).map_err(|error| error.to_string())?;
    ws.flush().map_err(|error| error.to_string())
}

/// Read whatever the server has queued, up to a bound so a chatty server
/// cannot starve the send. `false` means the socket is gone.
fn drain(
    socket: &mut Option<WebSocket<TcpStream>>,
    status_tx: &Sender<TciTuneStatus>,
    reported: &mut TciTuneStatus,
) -> bool {
    for _ in 0..64 {
        // The read borrows the socket. Finish it before `close` takes the
        // `Option` back, or the two borrows overlap.
        let read = match socket.as_mut() {
            Some(ws) => ws.read(),
            None => return false,
        };
        match read {
            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => {
                close(socket);
                report(
                    status_tx,
                    reported,
                    TciTuneStatus {
                        connected: false,
                        error: Some("the TCI server closed the connection".into()),
                    },
                );
                return false;
            }
            Ok(_) => continue,
            Err(tungstenite::Error::Io(error))
                if error.kind() == ErrorKind::WouldBlock || error.kind() == ErrorKind::TimedOut =>
            {
                return true;
            }
            Err(error) => {
                tracing::warn!("TCI client read: {error}");
                close(socket);
                report(
                    status_tx,
                    reported,
                    TciTuneStatus { connected: false, error: Some(format!("read: {error}")) },
                );
                return false;
            }
        }
    }
    true
}

fn connect(address: &str) -> Result<WebSocket<TcpStream>, String> {
    let (host, port) = split_addr(address)?;
    let sockaddr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| format!("resolve {host}:{port}: {error}"))?
        .next()
        .ok_or_else(|| format!("no address for {host}:{port}"))?;
    let stream = connect_socket(&sockaddr, &host, port, CONNECT_TIMEOUT).map_err(|error| {
        // `connect_socket` names ExpertSDR3, which is the IQ client. This
        // socket is the control client, and the program on the other end is
        // whatever is serving TCI there — 590Ctrl, for the usual one.
        if error.contains("nothing is serving TCI") {
            format!(
                "connect {host}:{port}: nothing is listening. Start 590Ctrl and turn its TCI \
                 server on, on this port"
            )
        } else {
            error
        }
    })?;
    let url = format!("ws://{host}:{port}/");
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok();
    let ws = ws_handshake(stream, &url, deadline)?;
    ws.get_ref().set_read_timeout(Some(READ_TIMEOUT)).ok();
    // A frequency line is a few dozen bytes. Holding it for a full segment
    // would put the radio a segment behind the dial.
    ws.get_ref().set_nodelay(true).ok();
    Ok(ws)
}

fn close(socket: &mut Option<WebSocket<TcpStream>>) {
    if let Some(mut ws) = socket.take() {
        let _ = ws.close(None);
        let _ = ws.flush();
    }
}

fn report(tx: &Sender<TciTuneStatus>, last: &mut TciTuneStatus, next: TciTuneStatus) {
    if *last == next {
        return;
    }
    *last = next.clone();
    let _ = tx.send(next);
}

fn sleep_until_stop(stop: &AtomicBool, total: Duration) {
    let until = Instant::now() + total;
    while !stop.load(Ordering::Relaxed) && Instant::now() < until {
        let slice = Duration::from_millis(50).min(until.saturating_duration_since(Instant::now()));
        thread::sleep(slice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn vfo_line_is_a_frequency_and_not_a_key() {
        let line = vfo_line(7_074_000);
        assert_eq!(line, "vfo:0,0,7074000;");
        assert!(!line.contains("trx"));
        assert!(!line.contains("tune:"));
        assert!(!line.contains("audio"));
        assert!(!line.contains("iq"));
    }

    #[test]
    fn a_tune_is_one_vfo_line_and_nothing_that_keys_or_streams() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            stream.set_read_timeout(Some(Duration::from_millis(200))).ok();
            let mut ws = tungstenite::accept(stream).expect("handshake");
            let _ = ws.send(Message::Text("protocol:590Ctrl,1.9;ready;".into()));
            let mut lines = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                match ws.read() {
                    Ok(Message::Text(text)) => {
                        let text = text.to_string();
                        let done = text.contains("7075500");
                        lines.push(text);
                        if done {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(error))
                        if error.kind() == ErrorKind::WouldBlock
                            || error.kind() == ErrorKind::TimedOut => {}
                    Err(_) => break,
                }
            }
            lines
        });

        let client = TciTuneClient::start(format!("127.0.0.1:{port}"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if client.poll_status().is_some_and(|status| status.connected) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // Two tunes inside the send gap: the radio must end on the later one,
        // and neither line may be anything but a VFO.
        client.tune(7_074_000);
        client.tune(7_075_500);

        let lines = server.join().expect("server");
        let joined = lines.join("");
        assert!(joined.contains("vfo:0,0,7075500;"), "expected the final dial, got {lines:?}");
        let lower = joined.to_ascii_lowercase();
        assert!(!lower.contains("trx:"), "{lines:?}");
        assert!(!lower.contains("tune:"), "{lines:?}");
        assert!(!lower.contains("audio"), "{lines:?}");
        assert!(!lower.contains("iq"), "{lines:?}");
        drop(client);
    }
}
