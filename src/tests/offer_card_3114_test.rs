//! The follow-up offer card's shape and its keyboard (#3114).
//!
//! Three things were wrong on one surface, and all three were invisible to a
//! test that only asked what the hint said: the rows drew side by side, the
//! last row was a wall the arrows could not pass, and nothing on screen named
//! a way out of a card that holds the keyboard.
//!
//! There is no headless window to press a key in, so the behaviour is pinned at
//! the source. That is the same shape the rest of the desktop app's layout
//! tests use (`gui_voice_note_row_test`), and it is honest about what it
//! proves: the arm exists and does what it says, not that gpui dispatched it.

use crate::ui_core::hints::{HintState, task_line};

const COMPOSER_VIEW: &str = include_str!("../gui/composer_view.rs");
const DASHBOARD_SESSIONS: &str = include_str!("../gui/dashboard_sessions.rs");
const EDITOR: &str = include_str!("../gui/editor.rs");
const PANE_CHROME: &str = include_str!("../gui/pane_chrome.rs");

/// The slice of a file between two anchors, so a pin cannot match a string
/// somewhere else in the same file by accident.
fn region<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let from = source
        .find(start)
        .unwrap_or_else(|| panic!("missing start anchor {start:?}"));
    let rest = &source[from..];
    let to = rest
        .find(end)
        .unwrap_or_else(|| panic!("missing end anchor {end:?}"));
    &rest[..to]
}

/// The offer rows stack, one per line (#3114).
///
/// They used to wrap side by side, so two labels split the card's width and a
/// long one was cut while a row of its own had the room. The column is also
/// what the arrows describe: a list, not a paragraph.
#[test]
fn the_offer_rows_stack_one_above_the_other() {
    let strip = region(
        COMPOSER_VIEW,
        "pub(crate) fn options_strip(",
        "/// Whether a speech-to-text engine is configured",
    );
    assert!(
        strip.contains("flex_col().gap_1()"),
        "the buttons are a column, so one offer owns a line"
    );
    assert!(
        !strip.contains("flex_wrap"),
        "nothing wraps sideways any more"
    );
    assert!(
        strip.contains("flex_row()"),
        "the row keeps its icon-and-label layout inside the column"
    );
}

/// Down past the last row hands the keyboard back to the field (#3114).
///
/// Up from the composer reaches the card (#3101). Without the mirror, the last
/// row was a wall: only Esc, or a key that meant "back to typing", got the
/// caret back, and the screen never named either.
#[test]
fn down_past_the_last_row_returns_the_caret_to_the_field() {
    let keys = region(
        DASHBOARD_SESSIONS,
        "pub(crate) fn options_key(",
        "pub(crate) fn drain_pending(",
    );
    assert!(
        keys.contains("\"down\" if at + 1 >= offers.len() => {"),
        "the last row is a door, not a wall: {keys}"
    );
    let exit = region(
        keys,
        "\"down\" if at + 1 >= offers.len() => {",
        "\"down\" => self.options_focus = Some((session, at + 1)),",
    );
    assert!(
        exit.contains("self.options_focus = None;"),
        "the ring leaves the card"
    );
    assert!(
        exit.contains("self.request_composer_caret(cx);"),
        "and the caret comes back to the field"
    );
    // The clamp that made the last row a wall is gone: the plain arm advances
    // and nothing else.
    assert!(
        !keys.contains(".min(offers.len() - 1)"),
        "no clamp is left behind to swallow the key"
    );
}

/// Both offer lines name the way out (#3114).
///
/// The card holds the keyboard while it is focused, so a reader who followed
/// the line into it needs the exit in the same breath.
#[test]
fn the_offer_line_names_the_way_back_to_the_field() {
    let single = task_line(
        &HintState {
            offers: 1,
            ..HintState::default()
        },
        |_| true,
    )
    .expect("one offer earns the card");
    assert!(
        single.contains("back to the field"),
        "one offer is also the exit: {single}"
    );

    let many = task_line(
        &HintState {
            offers: 3,
            ..HintState::default()
        },
        |_| true,
    )
    .expect("three offers");
    assert!(
        many.contains("past the last") && many.contains("back to the field"),
        "the end of a list says what the end does: {many}"
    );
    assert!(
        many.contains('3'),
        "and how long the list is: {many}"
    );
}
