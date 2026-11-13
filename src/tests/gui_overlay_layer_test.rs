//! The window's overlays paint above the panes (#2331). gpui paints children in
//! the order they are added, so the panes must be attached before the delete
//! dialog and the key reference. A pane added after an overlay covers it.
//!
//! The window has no headless harness, so the order is checked in the render
//! source. The needles stop at the call, never at the punctuation after it:
//! pinning a comma made these guards fail on a refactor that left the order
//! intact (#2395).

const GUI_MAIN: &str = include_str!("../gui/main.rs");

/// The dialogs themselves, which live in their own module (#2817 follow-up).
/// The order guards above read the render in the crate root, where each one is
/// attached; the guards below read the dialog they pin.
const OVERLAYS: &str = include_str!("../gui/overlays.rs");

/// The Help Center moved to its own module when it became four section cards
/// (#2893), so the layer guards read the screen where it is now painted.
const HELP_VIEW: &str = include_str!("../gui/help_view.rs");

fn position(needle: &str) -> usize {
    GUI_MAIN
        .find(needle)
        .unwrap_or_else(|| panic!("render source contains {needle}"))
}

#[test]
fn the_panes_are_attached_before_the_key_reference() {
    assert!(
        position(".child(right)") < position(".child(help_screen("),
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

/// The theme modal is a scrim over the window (#2619). It is attached last,
/// after the panes and after every other overlay, so a click on a card never
/// reaches the surface behind it, and the scrim itself carries `.occlude()`.
#[test]
fn the_theme_modal_paints_above_the_panes_and_takes_the_pointer() {
    let modal = position("root = root.child(self.themes_modal(cx))");
    assert!(
        position(".child(right)") < modal,
        "the theme modal must paint above the panes"
    );
    assert!(
        position(".child(command_palette::overlay(") < modal,
        "the theme modal is attached last, so it paints over the other overlays too"
    );
    // The scrim moved into the shared modal shell (#2879), so the guard reads
    // the shell and pins that the Theme modal still goes through it.
    let shell = include_str!("../gui/modal_shell.rs");
    let scrim = shell
        .find(".id(SharedString::from(format!(\"{prefix}-scrim\")))")
        .expect("the modal owns a scrim");
    let after = shell
        .get(scrim..)
        .expect("the scrim marker is a char boundary");
    let card = after
        .find(".child(card)")
        .expect("the scrim wraps the card");
    assert!(
        after
            .get(..card)
            .expect("the card marker is a char boundary")
            .contains(".occlude()"),
        "the scrim must take the pointer from the window"
    );
    let view = include_str!("../gui/themes_view.rs");
    assert!(
        view.contains("id: \"theme-modal\","),
        "the Theme modal must keep the id its scrim is named after"
    );
}

#[test]
fn the_key_reference_fills_the_window() {
    // The help screen is a full-window Help Center (#2403 replaced the capped
    // panel this guard used to watch). What has to hold now is that the screen
    // covers the window and takes the pointer, so a click lands in the
    // reference and not on a pane behind it.
    let screen = HELP_VIEW
        .find(".id(\"help-screen\")")
        .expect("the help screen names itself");
    let after = HELP_VIEW
        .get(screen..)
        .expect("the help screen marker is a char boundary");
    let header = after
        .find(".child(header)")
        .expect("the help screen stacks a header, a scroller and a footer");
    let frame = after
        .get(..header)
        .expect("the header marker is a char boundary");
    assert!(
        frame.contains(".size_full()"),
        "the help screen must fill the window"
    );
    assert!(
        frame.contains(".occlude()"),
        "the help screen must take the pointer from the panes"
    );
}
