//! Structural pins for the Ctrl+C documentation drift (#1770, #1771).
//!
//! The real behavior lives in `src/tui/app/state.rs`: when the transcript is
//! scrolled up (`!auto_scroll`), the first Ctrl+C press snaps back to bottom
//! and clears the input — no quit, no quit hint. Both documented surfaces
//! (README shortcut table, `/help` dialog) described only clear-input/quit,
//! lying by omission. Source scans keep the docs welded to the behavior.

const README: &str = include_str!("../../README.md");

#[test]
fn readme_ctrl_c_row_documents_snap_to_bottom() {
    let row = README
        .lines()
        .find(|l| l.trim_start().starts_with("| `Ctrl+C` |"))
        .expect("README must keep a `Ctrl+C` row in the Keyboard Shortcuts table");
    assert!(
        row.contains("bottom") && row.contains("scrolled"),
        "#1770: the Ctrl+C row must document that a scrolled-up transcript \
         snaps to bottom on the first press, not only clear-input/quit. \
         Offending row: {row}"
    );
    assert!(
        row.contains("quit"),
        "#1770: the Ctrl+C row must keep the double-press quit behavior too. \
         Offending row: {row}"
    );
}

#[test]
fn help_dialog_ctrl_c_row_documents_snap_and_fits_the_column() {
    // The chord rows moved out of `help.rs` into the shared table in
    // `ui_core::help_keys` (#2320), so the pin follows the data instead of
    // scanning the renderer's source text.
    let row = crate::ui_core::help_keys::find("ctrl+c")
        .expect("the help table must keep a ctrl-c row in the GLOBAL section");
    assert_eq!(row.tui, "Ctrl+C");
    assert!(
        row.description.contains("Snap bottom"),
        "#1771: the /help dialog must document that the first Ctrl+C press \
         snaps the scrolled-up transcript to bottom. Offending row: {row:?}"
    );
    // The dialog renders two 50% columns with no wrap, so a long description
    // silently clips at the column edge (#1771's proposed 50-char string
    // would have). Every row shares that budget now that they live in one
    // table, so check them all rather than only Ctrl+C.
    for row in crate::ui_core::help_keys::rows() {
        assert!(
            row.description.len() <= 34,
            "#1771: {} help description is {} chars; keep it within the \
             no-wrap column budget (<= 34). Offending description: {}",
            row.tui,
            row.description.len(),
            row.description
        );
    }
}
