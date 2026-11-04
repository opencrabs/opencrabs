//! The desktop window's theme keys (#2341). The window reads `[gui]` from
//! config.toml and resolves each name through the TUI's theme lookup. These
//! tests pin the config shape and the lookup the window relies on.

use crate::config::types::GuiConfig;
use crate::tui::render::find_theme;

#[test]
fn gui_theme_keys_default_to_unset() {
    let config = GuiConfig::default();
    assert!(config.dark_theme.is_none());
    assert!(config.light_theme.is_none());
}

#[test]
fn gui_theme_keys_parse_from_the_gui_section() {
    let config: GuiConfig =
        toml::from_str("dark_theme = \"dracula\"\nlight_theme = \"solarized-light\"").unwrap();
    assert_eq!(config.dark_theme.as_deref(), Some("dracula"));
    assert_eq!(config.light_theme.as_deref(), Some("solarized-light"));
}

#[test]
fn the_light_default_resolves_to_alucard() {
    let theme = find_theme("alucard").expect("alucard is a built-in preset");
    assert_eq!(theme.name.to_ascii_lowercase(), "alucard");
}

#[test]
fn an_unknown_theme_name_does_not_resolve() {
    assert!(find_theme("no-such-theme").is_none());
}

#[test]
fn a_light_preset_carries_its_own_background() {
    // The window maps the TUI background onto its own; a preset without one
    // would fall back to the dark background, so the light default must set it.
    let theme = find_theme("alucard").expect("alucard is a built-in preset");
    assert!(theme.colors.background.is_some());
}
