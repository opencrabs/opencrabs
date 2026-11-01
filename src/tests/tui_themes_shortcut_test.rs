//! The terminal themes shortcut (#2343). Alt+T opens the same theme picker the
//! `/theme` command opens, and the help screen lists it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tui::events::keys;
use crate::ui_core::help_keys;

const APP_STATE: &str = include_str!("../tui/app/state.rs");

#[test]
fn alt_t_opens_the_theme_picker() {
    assert!(keys::is_themes(&KeyEvent::new(
        KeyCode::Char('t'),
        KeyModifiers::ALT
    )));
}

#[test]
fn plain_t_and_ctrl_t_do_not_open_it() {
    // Plain T types a letter, and Ctrl+T is reserved for topics.
    assert!(!keys::is_themes(&KeyEvent::new(
        KeyCode::Char('t'),
        KeyModifiers::NONE
    )));
    assert!(!keys::is_themes(&KeyEvent::new(
        KeyCode::Char('t'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn the_global_handler_routes_alt_t_to_the_picker() {
    assert!(APP_STATE.contains("keys::is_themes(&event) && self.theme_picker.is_none()"));
    assert!(APP_STATE.contains("ThemePickerState::open()"));
}

#[test]
fn the_help_table_lists_alt_t_for_the_terminal() {
    let row = help_keys::rows()
        .find(|r| r.tui == "Alt+T")
        .expect("the help screen lists Alt+T for the terminal");
    assert!(row.description.to_lowercase().contains("theme"));
}
