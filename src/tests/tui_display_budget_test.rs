//! Tests for `App::trim_messages_to_display_window` (#2155, superseding the
//! token budget from #1944).
//!
//! The old trim measured a token budget over raw DB rows. Those rows include
//! the persisted tool-ledger JSON, and a single such row can carry hundreds of
//! thousands of tokens on its own, so a couple of oversized rows at the end of
//! a session collapsed the visible history to two or three messages. Switching
//! away and back then looked like the transcript had been wiped (#2155).
//!
//! A count window cannot be eaten by one oversized row, and the newest row is
//! still never the one that gets hidden.

use crate::db::models::Message;
use crate::tui::app::App;

fn row(seq: i32, role: &str, content: &str) -> Message {
    Message {
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::nil(),
        role: role.to_string(),
        content: content.to_string(),
        sequence: seq,
        created_at: chrono::Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    }
}

/// The #2155 shape: the newest rows are enormous tool-ledger payloads. They
/// must not shrink the window, and the newest row must still be displayed.
#[test]
fn oversized_newest_rows_do_not_shrink_the_window() {
    let huge = "{\"tool\":\"bash\",\"output\":\"".to_string() + &"x".repeat(400_000) + "\"}";
    let mut msgs: Vec<Message> = (1..=500).map(|s| row(s, "user", "short row")).collect();
    let len = msgs.len() as i32;
    msgs[len as usize - 1] = row(len, "assistant", &huge);
    msgs[len as usize - 2] = row(len - 1, "tool_group", &huge);

    let (kept, hidden) = App::trim_messages_to_display_window(&msgs, 200);
    assert_eq!(kept.len(), 200, "the window is a count, not a token budget");
    assert_eq!(hidden, 300, "everything older sits behind the marker");
    assert_eq!(
        kept.last().map(|m| m.sequence),
        Some(len),
        "the newest row is never the one hidden"
    );
}

/// A history shorter than the window is shown whole, with no marker.
#[test]
fn a_short_history_is_shown_whole() {
    let msgs: Vec<Message> = (1..=12).map(|s| row(s, "user", "hi")).collect();
    let (kept, hidden) = App::trim_messages_to_display_window(&msgs, 200);
    assert_eq!(kept.len(), 12);
    assert_eq!(hidden, 0);
}

/// A zero window still cannot blank a pane that has messages.
#[test]
fn a_zero_window_still_keeps_the_newest_row() {
    let msgs = vec![row(1, "assistant", "answer")];
    let (kept, hidden) = App::trim_messages_to_display_window(&msgs, 0);
    assert_eq!(kept.len(), 1);
    assert_eq!(hidden, 0);
}

/// Empty input stays empty: the floor must not conjure a row.
#[test]
fn empty_history_trims_to_empty() {
    let (kept, hidden) = App::trim_messages_to_display_window(&[], 200);
    assert!(kept.is_empty());
    assert_eq!(hidden, 0);
}

/// The kept slice is always the tail, in order, so sequences stay monotonic.
#[test]
fn the_kept_rows_are_the_newest_tail_in_order() {
    let msgs: Vec<Message> = (1..=50).map(|s| row(s, "user", "row")).collect();
    let (kept, hidden) = App::trim_messages_to_display_window(&msgs, 10);
    assert_eq!(hidden, 40);
    assert_eq!(kept.first().map(|m| m.sequence), Some(41));
    assert_eq!(kept.last().map(|m| m.sequence), Some(50));
    assert!(
        kept.windows(2).all(|w| w[0].sequence < w[1].sequence),
        "the tail must keep database order"
    );
}
