//! Cancelling a turn (Esc-Esc) must not lose the completion for HTTP/streaming
//! providers (#1468), and must not re-persist history that is already durable
//! (#2127).
//!
//! The real-time live persister (#269) that writes streamed text to the DB as
//! it arrives is gated `if is_cli_provider`. For HTTP providers the assistant
//! row stays an empty placeholder until turn-end, so a cancel before then left
//! the row 0 chars, and `load_session` (called right after the abort to re-inline
//! tool calls) rebuilt the view from that empty row and wiped the in-memory copy
//! too: the whole visible completion vanished from screen AND disk. Confirmed on
//! `modelscope-qwen38max` with 0-length assistant rows in the live DB and abort
//! log lines carrying no "Persisted N chars" (the `content.is_empty()` early
//! return).
//!
//! `persist_streaming_state` reconstructs the turn's assistant text from
//! `self.messages` via `collect_turn_assistant_text` and writes it when the DB
//! row is still empty. These pin that reconstruction, the novel logic. It is
//! pure over the message slice, so it needs no live terminal or DB.
//!
//! #2127 added the second bound: reconstruction stops at `turn_start_len`, the
//! index where the in-flight turn begins. A turn resumed at boot has no user
//! row, so the last-user-message floor does not apply, and every visible row is
//! already durable. Without the anchor the whole previous turn was rebuilt and
//! written into the fresh placeholder row as one marker-less duplicate.

use crate::tui::app::DisplayMessage;
use crate::tui::app::messaging::{collect_turn_assistant_text, shift_turn_start};

fn dm(role: &str, content: &str) -> DisplayMessage {
    DisplayMessage {
        id: uuid::Uuid::new_v4(),
        role: role.to_string(),
        content: content.to_string(),
        timestamp: chrono::Utc::now(),
        token_count: None,
        cost: None,
        approval: None,
        approve_menu: None,
        details: None,
        expanded: false,
        expanded_full: false,
        tool_group: None,
        duration_secs: None,
    }
}

#[test]
fn collects_assistant_text_after_the_last_user_message() {
    // A turn that streamed text, ran a tool, then streamed more: the visible
    // completion is the two assistant entries after the last user message.
    let msgs = vec![
        dm("user", "earlier question"),
        dm("assistant", "earlier answer"),
        dm("user", "the question being answered now"),
        dm("assistant", "first half of the completion"),
        dm("tool_group", "3 tool calls"),
        dm("assistant", "second half of the completion"),
    ];
    let text = collect_turn_assistant_text(&msgs, 3);
    assert_eq!(
        text, "first half of the completion\n\nsecond half of the completion",
        "both assistant fragments of the in-flight turn must be reconstructed \
         in order, joined the way the DB row stores them"
    );
}

#[test]
fn never_pulls_text_from_a_previous_turn() {
    // The blast radius guard: if the reconstruction reached past the last user
    // message it would re-persist settled answers and duplicate history on
    // every cancel. Passed a zero anchor on purpose: the user-message floor
    // alone must hold when no anchor was set.
    let msgs = vec![
        dm("user", "q1"),
        dm("assistant", "answer to q1"),
        dm("user", "q2"),
        dm("assistant", "answer to q2"),
    ];
    let text = collect_turn_assistant_text(&msgs, 0);
    assert_eq!(text, "answer to q2");
    assert!(
        !text.contains("answer to q1"),
        "text before the last user message belongs to a settled turn that is \
         already durable; re-persisting it would duplicate history"
    );
}

#[test]
fn empty_when_the_turn_produced_no_visible_text() {
    // Cancelled before anything streamed: nothing to save, and persist must
    // stay a no-op rather than write an empty row.
    let msgs = vec![dm("user", "a question cancelled before any output")];
    assert_eq!(collect_turn_assistant_text(&msgs, 1), "");
}

#[test]
fn skips_blank_assistant_entries() {
    // Placeholders and whitespace-only fragments must not introduce separators
    // or empty content into the reconstructed text.
    let msgs = vec![
        dm("user", "q"),
        dm("assistant", "   "),
        dm("assistant", "real content"),
        dm("assistant", ""),
    ];
    assert_eq!(collect_turn_assistant_text(&msgs, 1), "real content");
}

#[test]
fn ignores_tool_group_entries() {
    // Tool groups persist through their own marker path; the reconstruction is
    // for assistant prose only, so tool_group rows are skipped, not rendered.
    let msgs = vec![
        dm("user", "q"),
        dm("tool_group", "2 tool calls"),
        dm("assistant", "the answer"),
    ];
    assert_eq!(collect_turn_assistant_text(&msgs, 1), "the answer");
}

// ── the #2127 bound ─────────────────────────────────────────────────────────

#[test]
fn a_turn_start_at_the_end_of_history_collects_nothing() {
    // #2127: a resumed turn has no user row and every visible row is already
    // durable. The anchor sits at the end of the loaded history, so the
    // reconstruction is empty and `persist_streaming_state` writes nothing.
    let msgs = vec![
        dm("user", "old question"),
        dm("assistant", "old answer"),
        dm("tool_group", "2 tool calls"),
        dm("assistant", "old answer tail"),
    ];
    assert_eq!(
        collect_turn_assistant_text(&msgs, msgs.len()),
        "",
        "durable rows must never be re-collected into the fresh placeholder row"
    );
}

#[test]
fn collects_only_rows_after_the_anchor_when_there_is_no_user_message() {
    // The other side of the same bound: a system-triggered turn pushes no user
    // row, but its own rows sit after the anchor and must still survive a
    // cancel (#1468 must keep working for these turns).
    let msgs = vec![
        dm("assistant", "durable answer from the previous turn"),
        dm("assistant", "this turn's first fragment"),
        dm("tool_group", "1 tool call"),
        dm("assistant", "this turn's second fragment"),
    ];
    assert_eq!(
        collect_turn_assistant_text(&msgs, 1),
        "this turn's first fragment\n\nthis turn's second fragment"
    );
}

#[test]
fn resumed_turn_with_all_durable_rows_persists_nothing() {
    // The incident shape from the issue: history loaded at boot, a resumed
    // request creates a fresh empty placeholder, nothing new is visible, the
    // user aborts two seconds later. Rows 7829-7831 (narration + tool groups +
    // answer) must not be rebuilt and written into seq 7832.
    let msgs = vec![
        dm("user", "Push it and close #2122"),
        dm(
            "assistant",
            "One contract item from the issue's test list ...",
        ),
        dm("tool_group", "3 tool calls"),
        dm(
            "assistant",
            "Done. #2122 is closed and both commits are on main.",
        ),
    ];
    let anchor = msgs.len();
    assert_eq!(
        collect_turn_assistant_text(&msgs, anchor),
        "",
        "a resumed turn that produced nothing must write nothing"
    );
}

#[test]
fn an_anchor_past_the_end_is_clamped() {
    // Defensive: a stale anchor (e.g. history trimmed under it) must not panic
    // on the slice index.
    let msgs = vec![dm("assistant", "only row")];
    assert_eq!(collect_turn_assistant_text(&msgs, 99), "");
}

// ── wiring sentinels (#1468, #2127) ─────────────────────────────────────────
//
// The pure tests above only prove the reconstruction helper is correct. The
// actual bug was that `persist_streaming_state` never USED it: it looked at
// `streaming_response` alone, found it empty (the text had been promoted into
// `self.messages`), and early-returned, leaving the non-CLI placeholder row at
// 0 chars. These sentinels fail if that call site is removed again, so the fix
// cannot silently regress while the helper tests stay green. Source-level
// because `persist_streaming_state` is an async `App` method over a live DB
// with no in-process seam, the same approach as `cli_silent_failure_test`.

const MESSAGING_SRC: &str = include_str!("../tui/app/messaging.rs");
const STATE_SRC: &str = include_str!("../tui/app/state.rs");

#[test]
fn persist_streaming_state_reconstructs_the_turn_from_messages() {
    assert!(
        MESSAGING_SRC.contains("collect_turn_assistant_text(&self.messages, self.turn_start_len)"),
        "persist_streaming_state must reconstruct the in-flight turn from \
         self.messages, bounded by the turn anchor; without the call the \
         HTTP-provider completion is lost on cancel (#1468) and without the \
         anchor the previous turn is duplicated (#2127)"
    );
}

#[test]
fn persist_streaming_state_writes_the_full_turn_when_the_db_row_is_empty() {
    assert!(
        MESSAGING_SRC.contains("let db_empty = msg.content.trim().is_empty();"),
        "the cancel-persist must detect the empty non-CLI placeholder row"
    );
    assert!(
        MESSAGING_SRC.contains("if db_empty && !memory_turn_text.trim().is_empty() {"),
        "when the DB row is still empty (HTTP provider; the live persister is \
         CLI-only) the full reconstructed turn must be written, not skipped, \
         or the completion vanishes on Esc-Esc (#1468)"
    );
}

#[test]
fn the_anchor_is_set_at_every_turn_start() {
    // load_session, send_message_inner and resume_background_turn live in
    // messaging.rs; PendingResumed (boot resume) lives in state.rs. A site
    // that disappears drops the count and fails here.
    assert!(
        MESSAGING_SRC
            .matches("self.turn_start_len = self.messages.len();")
            .count()
            >= 3,
        "load_session, send_message_inner and resume_background_turn must each \
         set the turn anchor (#2127)"
    );
    assert!(
        STATE_SRC
            .matches("self.turn_start_len = self.messages.len();")
            .count()
            >= 1,
        "the boot-resume handler (PendingResumed) must set the turn anchor, or \
         an aborted resume duplicates the previous turn (#2127)"
    );
    assert!(
        MESSAGING_SRC.contains("shift_turn_start("),
        "load_more_history prepends older rows, so the anchor must shift with \
         them or it points into the wrong turn (#2127). It must go through the \
         tested helper: measuring the prepended count after `append` drains the \
         source reports the whole history and clamps the anchor past the end"
    );
}

#[test]
fn prepending_rows_shifts_the_turn_anchor_by_the_front_block() {
    // 5 durable rows, turn starts at 5. Three older rows arrive at the front.
    assert_eq!(shift_turn_start(5, 3, 0, 8), 8);
}

#[test]
fn a_removed_history_marker_does_not_count_as_prepended() {
    // The marker at the front is dropped, two rows are prepended, so the old
    // index 4 lands at 4 + 2 - 1 = 5.
    assert_eq!(shift_turn_start(4, 2, 1, 6), 5);
}

#[test]
fn a_shift_can_never_point_past_the_end() {
    // The drained-append bug produced a prepended count equal to the whole
    // history; the clamp is what kept that from panicking.
    assert_eq!(shift_turn_start(5, 500, 0, 8), 8);
}

#[test]
fn a_shift_saturates_instead_of_overflowing() {
    // Both inputs at the type's limit must clamp, not panic in a debug build.
    assert_eq!(shift_turn_start(usize::MAX, usize::MAX, 0, 4), 4);
}

#[test]
fn an_anchor_already_at_the_end_stays_there_when_nothing_is_prepended() {
    assert_eq!(shift_turn_start(7, 0, 0, 7), 7);
}
