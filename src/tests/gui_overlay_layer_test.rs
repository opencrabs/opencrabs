//! The window's overlays paint above the panes (#2331). gpui paints children in
//! the order they are added, so the panes must be attached before the delete
//! dialog and the key reference. A pane added after an overlay covers it.
//!
//! The window has no headless harness, so the order is checked in the render
//! source. The needles stop at the call, never at the punctuation after it:
//! pinning a comma made these guards fail on a refactor that left the order
//! intact (#2395).

const GUI_MAIN: &str = include_str!("../gui/main.rs");

fn position(needle: &str) -> usize {
    GUI_MAIN
        .find(needle)
        .unwrap_or_else(|| panic!("render source contains {needle}"))
}

#[test]
fn the_panes_are_attached_before_the_key_reference() {
    assert!(
        position(".child(right)") < position(".child(help_overlay("),
        "the key reference must paint above the panes"
    );
}

#[test]
fn the_panes_are_attached_before_the_delete_dialog() {
    assert!(
        position(".child(right)") < position(".child(confirm_delete_overlay("),
        "the delete dialog must paint above the panes"
    );
}

#[test]
fn the_key_reference_fills_the_window() {
    // The help screen is a full-window Help Center (#2403 replaced the capped
    // panel this guard used to watch). What has to hold now is that the screen
    // covers the window and takes the pointer, so a click lands in the
    // reference and not on a pane behind it.
    let screen = position(".id(\"help-screen\")");
    let after = &GUI_MAIN[screen..];
    let header = after
        .find(".child(header)")
        .expect("the help screen stacks a header, a scroller and a footer");
    let frame = &after[..header];
    assert!(
        frame.contains(".size_full()"),
        "the help screen must fill the window"
    );
    assert!(
        frame.contains(".occlude()"),
        "the help screen must take the pointer from the panes"
    );
}
