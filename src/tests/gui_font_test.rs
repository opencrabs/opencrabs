//! The window's typeface model (#2786).
//!
//! The resolution is pure, so these run without a window: they pin what
//! `[gui] font` and `[gui] mono_font` mean, and what the modal's list offers.

use crate::config::gui_font::{self, MONO_LAST_RESORT, SYSTEM_FAMILY};

fn installed(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn an_unset_font_is_the_system_ui_font() {
    assert_eq!(gui_font::resolve_ui(None), SYSTEM_FAMILY);
}

#[test]
fn the_word_system_is_the_system_ui_font() {
    assert_eq!(gui_font::resolve_ui(Some("system")), SYSTEM_FAMILY);
    assert_eq!(gui_font::resolve_ui(Some("SYSTEM")), SYSTEM_FAMILY);
    assert_eq!(gui_font::resolve_ui(Some("  ")), SYSTEM_FAMILY);
}

#[test]
fn a_saved_family_is_kept() {
    assert_eq!(gui_font::resolve_ui(Some("Avenir")), "Avenir");
    assert_eq!(gui_font::resolve_ui(Some("  Avenir  ")), "Avenir");
}

#[test]
fn the_mono_walk_prefers_jetbrains_mono() {
    let names = installed(&["Consolas", "JetBrains Mono", "Menlo"]);
    assert_eq!(gui_font::resolve_mono(None, &names), "JetBrains Mono");
}

#[test]
fn the_mono_walk_picks_consolas_when_it_is_all_there_is() {
    let names = installed(&["Segoe UI", "Consolas"]);
    assert_eq!(gui_font::resolve_mono(Some("auto"), &names), "Consolas");
}

#[test]
fn the_mono_walk_ends_at_the_generic_when_nothing_matches() {
    let names = installed(&["Segoe UI", "Arial"]);
    assert_eq!(gui_font::resolve_mono(None, &names), MONO_LAST_RESORT);
}

#[test]
fn a_saved_mono_that_is_installed_is_kept_in_the_platform_spelling() {
    let names = installed(&["JetBrains Mono", "Menlo"]);
    assert_eq!(
        gui_font::resolve_mono(Some("jetbrains mono"), &names),
        "JetBrains Mono"
    );
}

#[test]
fn a_saved_mono_that_is_not_installed_falls_back_to_the_walk() {
    let names = installed(&["Menlo"]);
    assert_eq!(gui_font::resolve_mono(Some("Fira Code"), &names), "Menlo");
}

#[test]
fn the_picker_lists_the_system_font_first() {
    let rows = gui_font::picker_rows(&installed(&["Menlo", "Avenir"]));
    assert_eq!(rows.first().map(String::as_str), Some(SYSTEM_FAMILY));
}

#[test]
fn the_picker_is_sorted_and_has_no_duplicates() {
    let rows = gui_font::picker_rows(&installed(&[
        "Menlo",
        "Avenir",
        "avenir",
        SYSTEM_FAMILY,
        "  ",
        "Zapfino",
    ]));
    // The system row first, then case-insensitive order, one row per family.
    assert_eq!(
        rows,
        vec![
            SYSTEM_FAMILY.to_string(),
            "Avenir".to_string(),
            "Menlo".to_string(),
            "Zapfino".to_string(),
        ]
    );
}

#[test]
fn the_picker_labels_the_system_row_in_words() {
    assert_eq!(gui_font::row_label(SYSTEM_FAMILY), "System (default)");
    assert_eq!(gui_font::row_label("Menlo"), "Menlo");
}
