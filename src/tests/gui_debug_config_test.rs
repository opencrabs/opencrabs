//! `[debug] gui_debug_logs` in config.toml switches on the desktop
//! window's keystroke trail (#2336). The window reads it at startup, so the
//! config key is what these pin: it defaults off and parses from its section.

use crate::config::types::DebugConfig;

#[test]
fn gui_debug_defaults_off() {
    assert!(!DebugConfig::default().gui_debug_logs);
}

#[test]
fn gui_debug_parses_from_the_debug_section() {
    let config: DebugConfig = toml::from_str("gui_debug_logs = true").unwrap();
    assert!(config.gui_debug_logs);
}

#[test]
fn an_absent_key_keeps_the_trail_off() {
    let config: DebugConfig = toml::from_str("profiling = false").unwrap();
    assert!(!config.gui_debug_logs);
}
