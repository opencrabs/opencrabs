//! Regression for #3052: a phantom-blocked row must reach the reload split.
//!
//! The tool loop persists an iteration the phantom detector refused under
//! `<!-- phantom_blocked=1 -->` markers (#1172). A model that streams its
//! chain inline (Kimi K3 through `kimi.com/coding`) produces those rows with
//! no `<!-- reasoning -->` block and no tool ledger, so both reload gates
//! rejected the row and painted it whole: opener line, the narration the
//! detector withheld, closer line. Every other provider's phantom rows carry
//! a reasoning block, which is why only that family showed it.

use crate::db::models::Message;
use crate::tui::app::App;
use crate::tui::app::reasoning_split::BLOCKED_LABEL;

const OPEN: &str = "<!-- phantom_blocked=1 -->";
const CLOSE: &str = "<!-- /phantom_blocked=1 -->";

/// The persisted layout for an inline-reasoning phantom iteration: no
/// reasoning block, no tool ledger, just the marker pair around the text.
fn inline_phantom_row() -> Message {
    Message::new(
        uuid::Uuid::new_v4(),
        "assistant".to_string(),
        format!("{OPEN}\n\nCommitted `abc1234` on shared main.\n\n{CLOSE}\n"),
        1,
    )
}

#[test]
fn tui_inline_phantom_row_folds_instead_of_painting_markers() {
    let rows = App::expand_message(inline_phantom_row());
    assert!(!rows.is_empty(), "the row still renders as a folded step");

    for row in &rows {
        assert!(
            !row.content.contains("phantom_blocked"),
            "marker leaked into content: {}",
            row.content
        );
        assert!(
            !row.details
                .as_deref()
                .is_some_and(|d| d.contains("phantom_blocked")),
            "marker leaked into details"
        );
        assert!(
            !row.content.contains("Committed `abc1234`"),
            "blocked narration must never become answer text: {}",
            row.content
        );
    }

    let folded = rows.iter().any(|row| {
        row.details
            .as_deref()
            .is_some_and(|d| d.contains(BLOCKED_LABEL) && d.contains("Committed `abc1234`"))
    });
    assert!(folded, "the blocked section folds into one labelled row");
}

/// A normal reasoning row keeps its layout: the new gate arm must not turn
/// every assistant row into a split.
#[test]
fn tui_reasoning_row_keeps_its_chronology() {
    let content = "<!-- reasoning -->\nweighing options\n<!-- /reasoning -->\n\nhere is the answer";
    let row = Message::new(
        uuid::Uuid::new_v4(),
        "assistant".to_string(),
        content.to_string(),
        1,
    );
    let rows = App::expand_message(row);
    assert!(
        rows.iter()
            .any(|r| r.details.as_deref() == Some("weighing options")),
        "the reasoning keeps its own folded row"
    );
    let answer = rows
        .iter()
        .find(|r| r.content.contains("here is the answer"))
        .expect("the answer stays a visible row");
    assert_eq!(answer.details, None);
}
