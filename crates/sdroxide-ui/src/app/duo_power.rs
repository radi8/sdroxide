//! The ELAD FDM-DUO's power buttons beside Drive: 1W, 3W, 5W and MAX (AJ3G,
//! 18 Oct 2026). Each puts the DUO's menu 34 `TX POWER` on that setting, the
//! radio's own ceiling; Drive then sets the power within it. MAX is the
//! default and what the HR500 needs: at 5 W the DUO cannot drive the amplifier
//! to full output on most bands.

/// The buttons: label, menu 34 setting as `TP`'s parameter, and what the
/// radio's step is called.
pub(in crate::app) const PRESETS: [(&str, u8); 4] = [("1W", 2), ("3W", 6), ("5W", 8), ("MAX", 9)];

/// The hover for the button that sets `code`.
pub(in crate::app) fn hover(code: u8) -> String {
    let base = "Sets the FDM-DUO's own power setting (menu 34 TX POWER). Drive and Tune then \
                set the power within it, and SDRoxide sends the setting again each time it \
                connects.";
    match code {
        9 => format!(
            "MAX: the FDM-DUO's full output, about 8 W. The setting to use with the HR500 in \
             line.\n\n{base}"
        ),
        8 => format!(
            "5 W. With the HR500 in line it will not reach full output on most bands at this \
             setting: use MAX, and Drive to bring it down.\n\n{base}"
        ),
        _ => format!("{:.0} W, for running barefoot or QRP.\n\n{base}", step_w(code)),
    }
}

/// Menu 34's numbered steps, in watts, by `TP` parameter — the radio's own
/// labels (`sdroxide_cat::elad::TX_POWER_STEPS_W`, repeated here because the
/// UI does not depend on the CAT crate).
fn step_w(code: u8) -> f32 {
    const STEPS: [f32; 9] = [0.3, 0.5, 1.0, 1.2, 1.5, 2.0, 3.0, 4.0, 5.0];
    STEPS.get(code as usize).copied().unwrap_or(5.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buttons_are_menu_34s_own_steps() {
        let watts: Vec<f32> = PRESETS.iter().filter(|p| p.1 < 9).map(|p| step_w(p.1)).collect();
        assert_eq!(watts, vec![1.0, 3.0, 5.0]);
        assert_eq!(PRESETS[3], ("MAX", 9));
        assert!(hover(8).contains("HR500"));
    }
}
