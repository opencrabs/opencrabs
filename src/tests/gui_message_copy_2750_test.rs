//! Copying one response from the transcript (#2750).
//!
//! The window has no headless harness, so the wiring is read as text. The part
//! that can be called for real is the payload: a copy from a row must put the
//! stored markdown on the clipboard, headings, tables and fences intact,
//! because that is what makes the menu entry honest.

use crate::tests::gui_dashboard_source::DASHBOARD;
use crate::ui_core::copy_select::copied_text;

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const ICON: &str = include_str!("../gui/icon.rs");

/// One answer, written the way the model writes them: a heading, a table and
/// a fence, all of which the renderer-to-plain path used to flatten.
const ANSWER: &str = "## Findings\n\n| file | line |\n|---|---|\n| a.rs | 12 |\n\n```rust\nlet x = 1;\n```";

/// The payload is the stored source, unchanged. If this ever stops holding,
/// the menu entry has to go rather than quietly copy a flattened body.
#[test]
fn the_copied_payload_is_the_stored_markdown() {
    let payload = copied_text(&[ANSWER.to_string()], 0, 0).expect("one row copies");
    assert_eq!(
        payload, ANSWER,
        "headings, tables and fences survive the copy"
    );
}

/// The button hangs off the answer rows only (#2750): a reader's own row
/// carries the pencil, and two buttons in one corner would collide.
#[test]
fn the_copy_icon_hangs_off_the_answer_rows_only() {
    let gate = TRANSCRIPT
        .split("let copy_click = if role.as_str() == \"assistant\"")
        .nth(1)
        .expect("the row reads its own role before it offers a copy");
    assert!(
        gate.starts_with(" {"),
        "the gate decides whether the handler is passed at all"
    );
    assert!(
        TRANSCRIPT.contains("msg-copy-{id}"),
        "the button has its own id, so its click cannot reach the row"
    );
    assert!(
        TRANSCRIPT.contains("crate::icon::copy_icon("),
        "the affordance is a picture, not a label"
    );
    assert!(
        ICON.contains("/src/assets/copy.svg"),
        "the copy mark is an embedded asset, like the pencil"
    );
}

/// The row menu names the format it copies (#2750), and the rich-text half is
/// absent on purpose: gpui's clipboard cannot carry `text/html` yet, and a
/// dead entry is worse than an absent one.
#[test]
fn the_row_menu_offers_copy_as_markdown() {
    assert!(
        DASHBOARD.contains("MessageAction::CopyMarkdown.label()"),
        "the row menu offers the format it actually writes"
    );
    assert!(
        !DASHBOARD.contains("Copy as Rich Text"),
        "rich text has no path on this clipboard, so it is not offered"
    );
}

/// The icon's confirmation is a glyph swap inside a fixed box, never a second
/// row, so the copy leaves the transcript where it was (#2748, #2750).
#[test]
fn the_copy_confirmation_costs_the_row_no_height() {
    let button = TRANSCRIPT
        .split("msg-copy-{id}")
        .nth(1)
        .expect("the copy button is drawn");
    let button = button.split(".on_click(").next().unwrap_or(button);
    assert!(
        button.contains(".absolute()"),
        "the button is placed, not laid out in the row"
    );
    assert!(
        button.contains(".opacity(0.0)") && button.contains(".group_hover(group,"),
        "and it stays invisible until the row is hovered"
    );
    assert!(
        button.contains("\\u{2713}"),
        "the check replaces the mark in the same box"
    );
    assert!(
        button.contains("if copied {"),
        "and the check follows the copied row"
    );
}
