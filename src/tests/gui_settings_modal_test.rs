//! Desktop settings modal wiring and secret-handling contract (#2878).
//!
//! GPUI has no headless window harness here. Pure setting behaviour lives in
//! `gui/settings_view_test.rs`; these tests pin its window integration, while
//! `config_remove_key_test.rs` exercises the real file mutation.

use crate::ui_core::help_keys;

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const GUI_SETTINGS: &str = include_str!("../gui/dashboard_settings.rs");
const GUI_KEYS: &str = include_str!("../gui/dashboard_keys.rs");
const GUI_NAV: &str = include_str!("../gui/dashboard_nav.rs");
const SETTINGS_VIEW: &str = include_str!("../gui/settings_view.rs");
const SETTINGS_MODAL: &str = include_str!("../gui/settings_modal.rs");

#[test]
fn ctrl_comma_is_listed_as_the_window_settings_chord() {
    let row = help_keys::gui_rows()
        .find(|row| row.gui == Some("ctrl-comma"))
        .expect("the help screen lists ctrl+, for settings");
    assert!(row.description.to_lowercase().contains("settings"));
}

#[test]
fn the_modal_owns_the_keyboard_and_paints_above_the_window() {
    for needle in [
        "fn toggle_settings",
        "fn close_settings",
        "fn settings_modal_key",
        ".take(self.settings_values.len())",
        "settings_view::body_index(self.settings_cursor",
    ] {
        assert!(GUI_SETTINGS.contains(needle), "missing settings behaviour: {needle}");
    }
    assert!(GUI_NAV.contains("return vec![OverlayId::Settings];"));
    for needle in ["if self.settings_open {", "self.settings_modal_key(ks, cx);"] {
        assert!(GUI_KEYS.contains(needle), "missing key ownership: {needle}");
    }
    assert!(GUI_KEYS.contains("ev.keystroke.modifiers.control && ev.keystroke.key == \",\""));
    assert!(SETTINGS_MODAL.contains(".track_scroll(scroll)"));
    let modal = GUI_MAIN
        .find("root = root.child(self.settings_modal(cx));")
        .expect("settings modal is attached");
    let panes = GUI_MAIN.find(".child(right)").expect("panes are attached");
    assert!(panes < modal, "settings paints above the panes");
    assert!(SETTINGS_MODAL.contains(".id(\"settings-modal-scrim\")"));
    assert!(SETTINGS_MODAL.contains(".occlude()"));
}

#[test]
fn a_failed_config_load_paints_no_invented_settings() {
    for needle in [
        "self.settings_values = Vec::new();",
        "self.settings_values.len() + self.settings_creds.len()",
        ".take(self.settings_values.len())",
        "if self.settings_row_count() == 0 {",
    ] {
        assert!(GUI_SETTINGS.contains(needle), "missing empty-config guard: {needle}");
    }
    assert!(
        !GUI_SETTINGS.contains("unwrap_or(settings_view::RowValue::Bool(false))"),
        "a missing value must not become a fallback setting row"
    );
}

#[test]
fn writes_use_the_config_apis_and_credentials_stay_masked() {
    for needle in [
        "Config::write_key(spec.section, spec.key",
        "Config::write_key_string(spec.section, spec.key",
        "Config::remove_key(spec.section, spec.key",
        "Config::write_keys_key(&cred.section, \"api_key\"",
        "format!(\"saved {}.api_key\", cred.section)",
        "cx.read_from_clipboard()",
        "buffer.chars().count()",
        "\"\\u{2022}\".repeat(len.max(1))",
    ] {
        assert!(GUI_SETTINGS.contains(needle), "missing write or mask: {needle}");
    }
    assert!(
        SETTINGS_VIEW.contains("pub struct CredSpec"),
        "credential rows carry only section, label and configured state"
    );
    assert!(
        !SETTINGS_VIEW.contains("api_key: cfg"),
        "credential rows must not retain the secret"
    );
}

#[test]
fn startup_only_settings_are_labelled() {
    for key in ["lazy_tools", "vector_enabled"] {
        let at = SETTINGS_VIEW
            .find(&format!("key: \"{key}\""))
            .unwrap_or_else(|| panic!("{key} has a settings row"));
        let row = SETTINGS_VIEW
            .get(at..)
            .and_then(|tail| tail.split("SettingSpec").next())
            .expect("setting row body");
        assert!(row.contains("restart: true"), "{key} needs a restart label");
    }
}
