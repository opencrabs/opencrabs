//! The window's overlays paint above the panes (#2331). gpui paints children in
//! the order they are added, so the panes must be attached before the delete
//! dialog and the key reference. A pane added after an overlay covers it.
//!
//! The window has no headless harness, so the order is checked in the render
//! source.

const GUI_MAIN: &str = include_str!("../gui/main.rs");

fn position(needle: &str) -> usize {
    GUI_MAIN
        .find(needle)
        .unwrap_or_else(|| panic!("render source contains {needle}"))
}

#[test]
fn the_panes_are_attached_before_the_key_reference() {
    assert!(
        position("root = root.child(right);") < position("root = root.child(help_overlay(close));"),
        "the key reference must paint above the panes"
    );
}

#[test]
fn the_panes_are_attached_before_the_delete_dialog() {
    assert!(
        position("root = root.child(right);")
            < position("root = root.child(confirm_delete_overlay("),
        "the delete dialog must paint above the panes"
    );
}

#[test]
fn the_key_reference_fills_the_window_up_to_a_limit() {
    let panel = position("        .w_full()\n        .h_full()\n        .max_w(rems(40.0))");
    assert!(panel > 0);
}
