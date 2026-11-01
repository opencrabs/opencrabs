//! The desktop themes screen (#2342): what it lists, how it is reached, and how
//! it leaves. The window has no headless harness, so the screen wiring is
//! checked against the handler source, as the Esc tests are.

use crate::tui::render::theme_names;
use crate::ui_core::help_keys;

const GUI_MAIN: &str = include_str!("../gui/main.rs");

#[test]
fn the_screen_lists_the_built_in_presets_and_no_name_twice() {
    let names = theme_names();
    assert!(names.contains(&"alucard"), "the light default is listed");
    assert!(names.contains(&"dracula"), "a dark preset is listed");
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "no theme is listed twice");
}

#[test]
fn ctrl_shift_t_is_listed_in_the_help_table_for_the_window() {
    let row = help_keys::gui_rows()
        .find(|r| r.gui == Some("ctrl-shift-t"))
        .expect("the help screen lists ctrl+shift+t for the window");
    assert!(row.description.to_lowercase().contains("theme"));
}

#[test]
fn esc_on_the_themes_screen_restores_the_palette() {
    let start = GUI_MAIN
        .find("Screen::Themes => {\n                    self.close_themes(true, cx);")
        .expect("the Esc handler answers the themes screen with a restore");
    assert!(start > 0);
}

#[test]
fn keep_writes_the_key_for_the_current_appearance() {
    assert!(GUI_MAIN.contains("\"dark_theme\""));
    assert!(GUI_MAIN.contains("\"light_theme\""));
    assert!(GUI_MAIN.contains("Config::write_key_string(\"gui\", key, &name)"));
}

#[test]
fn the_themes_screen_is_reachable_by_its_chord() {
    assert!(GUI_MAIN.contains("ev.keystroke.modifiers.shift && ev.keystroke.key == \"t\""));
    assert!(GUI_MAIN.contains("self.toggle_themes(cx);"));
}
