//! The Hardrock amplifier: which one, on which port, and what it should do on
//! its own. The live panel is the AMP window; this tab only sets the link up.

use eframe::egui::{self, ComboBox, RichText};
use sdroxide_types::{AMP_BAUD_RATES, AmpConfig, AmpModel, AmpStatus};

use crate::chrome::StyledCombo;
use crate::{chrome, theme};

pub(in crate::app) fn settings_amp_tab(
    ui: &mut egui::Ui,
    cfg: &mut AmpConfig,
    seeded: bool,
    status: &AmpStatus,
    serial_ports: &[String],
    apply: &mut bool,
) {
    ui.label(RichText::new("Amplifier").size(14.0).strong().color(theme::CYAN()));
    ui.add_space(4.0);
    if !seeded {
        ui.label(RichText::new("Waiting for the station's amplifier configuration…").weak());
        return;
    }
    ui.label(
        RichText::new(
            "A HobbyPCB Hardrock-500 or Hardrock-50 on its USB serial port. SDRoxide reads its \
             meters, switches its band, mode and antenna, and runs its tuner from the AMP window. \
             The amplifier is still keyed by the radio's PTT line as before.",
        )
        .weak(),
    );
    ui.add_space(8.0);

    chrome::checkbox(ui, &mut cfg.enabled, "Enable");
    ui.add_space(4.0);
    egui::Grid::new("amp-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Model");
        super::enum_combo(ui, "amp-model", &mut cfg.model, &AmpModel::ALL, AmpModel::label);
        ui.end_row();
        if !cfg.model.tested() {
            ui.label("");
            ui.label(
                RichText::new(
                    "Untested: this panel has only been tested with a Hardrock-500. \
                     Meter limits for this model are scaled from the Hardrock-500's \
                     and may be wrong. Watch the amplifier's own display.",
                )
                .color(theme::YELLOW()),
            );
            ui.end_row();
        }

        ui.label("Port");
        let shown = if cfg.serial.path.is_empty() {
            "— choose —".to_string()
        } else {
            cfg.serial.path.clone()
        };
        // The list is of the engine's machine, like the T/R switch's.
        ComboBox::from_id_salt("amp-port").width(200.0).selected_text(shown).show_styled(
            ui,
            |ui| {
                for p in serial_ports {
                    if ui.selectable_label(&cfg.serial.path == p, p).clicked() {
                        cfg.serial.path = p.clone();
                    }
                }
            },
        );
        ui.end_row();

        ui.label("Baud");
        ComboBox::from_id_salt("amp-baud")
            .width(200.0)
            .selected_text(cfg.serial.baud.to_string())
            .show_styled(ui, |ui| {
                for b in AMP_BAUD_RATES {
                    if ui.selectable_label(cfg.serial.baud == b, b.to_string()).clicked() {
                        cfg.serial.baud = b;
                    }
                }
            })
            .response
            .on_hover_text(
                "Must match the amplifier's menu. HobbyPCB's own control panel needs 115200.",
            );
        ui.end_row();
    });
    ui.add_space(6.0);
    chrome::checkbox(ui, &mut cfg.follow_band, "Follow the radio's band").on_hover_text(
        "Sends the transmit frequency to the amplifier on every band change, so its filters \
         follow the radio without a CAT cable of their own. Never sent during a transmission.",
    );
    chrome::checkbox(ui, &mut cfg.feed_tx_meter, "Show its power and SWR on the TX meter")
        .on_hover_text(
            "Only where the radio does not measure them itself, and only while the amplifier is \
             in line.",
        );
    ui.add_space(8.0);

    let (text, col) = if !status.configured {
        ("Off".to_string(), theme::TEXT())
    } else if status.connected {
        (format!("Connected to the {}", status.model.label()), theme::GREEN())
    } else if let Some(e) = &status.error {
        (e.clone(), theme::ALERT())
    } else {
        ("Connecting…".to_string(), theme::YELLOW())
    };
    ui.horizontal(|ui| {
        ui.label("Status");
        ui.label(RichText::new(text).color(col));
    });
    ui.add_space(6.0);
    if chrome::chip_accent(
        ui,
        false,
        RichText::new(" APPLY ").strong(),
        theme::GREEN(),
        theme::INK_ON_CYAN(),
    )
    .on_hover_text("Save and (re)open the amplifier's port")
    .clicked()
    {
        *apply = true;
    }
}
