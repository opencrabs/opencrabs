//! A context that shrank must always leave a marker behind.
//!
//! Dropping the oldest messages advances the live context but not the DB.
//! `messages_from_last_compaction` keeps looking at the previous anchor, so a
//! restart reloads exactly the history that just overflowed and overflows
//! again on the first turn. Two sessions died that way on 2026-05-05 (397%
//! and 372% context with 793k tokens still on disk, a fresh loop on every
//! user message). A marker carrying no summary is lossy; no marker at all is
//! unrecoverable, so the truncation path now returns an outcome of its own
//! rather than `None`.

use crate::brain::agent::service::AgentService;
use crate::brain::agent::service::compaction::CompactionOutcome;

/// Whatever the outcome, the persisted row has to be findable by the loader.
/// That prefix is the only thing standing between a restart and a replay.
const MARKER_PREFIX: &str = "[CONTEXT COMPACTION";

/// The outcome carries the APPLIED marker text — banner included — so the
/// persisted row and the live context cannot drift (#1928). Build one the way
/// the apply step does.
fn summarised(payload: &str) -> CompactionOutcome {
    CompactionOutcome::Summarised(format!(
        "[CONTEXT COMPACTION — The conversation was automatically compacted. \
         Below is a structured summary of everything before this point.]\n\n{payload}"
    ))
}

#[test]
fn summarised_marker_carries_the_summary() {
    let out = summarised("## What happened\nWe fixed the parser.");
    let marker = out.marker("");
    assert!(marker.starts_with(MARKER_PREFIX));
    assert!(marker.contains("We fixed the parser."));
}

#[test]
fn truncated_marker_is_still_a_marker() {
    let marker = CompactionOutcome::Truncated.marker("");
    assert!(
        marker.starts_with(MARKER_PREFIX),
        "truncation row is invisible to the loader: {marker}"
    );
}

#[test]
fn truncated_marker_does_not_promise_a_summary() {
    let marker = CompactionOutcome::Truncated.marker("");
    assert!(
        !marker.contains("Below is a structured summary"),
        "marker claims a summary that was never produced: {marker}"
    );
    assert!(
        marker.contains("No summary is available"),
        "marker leaves the agent guessing why history vanished: {marker}"
    );
}

#[test]
fn trigger_wording_rides_both_variants() {
    let trigger = " after token calibration revealed high context usage";
    for marker in [
        summarised("body").marker(trigger),
        CompactionOutcome::Truncated.marker(trigger),
    ] {
        assert!(marker.starts_with(MARKER_PREFIX));
        assert!(marker.contains(trigger.trim()), "trigger dropped: {marker}");
    }
}

/// The loader is the reason the prefix matters, so assert against the loader
/// itself rather than against a copy of the string it looks for.
#[test]
fn loader_anchors_on_a_truncation_marker() {
    let row = |content: &str| crate::db::models::Message {
        forward: None,
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::nil(),
        role: "user".to_string(),
        content: content.to_string(),
        sequence: 0,
        created_at: chrono::Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    };

    let all = vec![
        row("ancient history"),
        row("more ancient history"),
        row(&CompactionOutcome::Truncated.marker("")),
        row("after the truncation"),
    ];

    let kept = AgentService::messages_from_last_compaction(all);
    assert_eq!(kept.len(), 2, "loader ignored the truncation marker");
    assert!(kept[0].content.starts_with(MARKER_PREFIX));
    assert_eq!(kept[1].content, "after the truncation");
}

/// #175: a message that merely QUOTES the prefix must not re-anchor the window.
///
/// A marker is `role == "user" && starts_with(prefix)`. An `assistant` row
/// echoing the banner — which is what happened when a lane read one back out
/// of a tool result or a compaction summary — is not a marker. Before the fix
/// a bare substring match found the LATER quoting row, so the window silently
/// discarded everything between the real marker and the quote: the session
/// threw away its own history and re-anchored on a sentence about compaction
/// rather than on the compaction itself.
#[test]
fn loader_ignores_an_assistant_row_quoting_the_marker() {
    let row = |role: &str, content: &str| crate::db::models::Message {
        forward: None,
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::nil(),
        role: role.to_string(),
        content: content.to_string(),
        sequence: 0,
        created_at: chrono::Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    };

    let all = vec![
        row("user", "ancient history"),
        row("user", &summarised("real anchor").marker("")),
        row("user", "work done after the real compaction"),
        // An assistant row echoing the banner — the 2026-09-12 self-re-anchor.
        row(
            "assistant",
            "Found compaction marker at message 613/614 - loading 1 messages\n\
             [CONTEXT COMPACTION - quoted back out of a tool result]",
        ),
        row("user", "work done after the quote"),
    ];

    let kept = AgentService::messages_from_last_compaction(all);

    assert_eq!(
        kept.len(),
        4,
        "a quoting assistant row re-anchored the window (#175)"
    );
    assert!(
        kept[0].content.starts_with(MARKER_PREFIX),
        "loader anchored on the quote instead of the real marker"
    );
    assert_eq!(kept[1].content, "work done after the real compaction");
    assert_eq!(
        kept[2].role, "assistant",
        "the quoting row is history, not a marker — it must survive the window"
    );
    assert!(
        kept[2].content.contains(MARKER_PREFIX),
        "the quoting row must still be in the window (it is what the old match anchored on)"
    );
    assert_eq!(kept[3].content, "work done after the quote");
}

/// A `user` row is only a marker when the prefix BEGINS its content — quoting
/// it mid-text (as a lane does when it pastes one out of a log) is not an
/// anchor either.
#[test]
fn loader_ignores_a_user_row_quoting_the_marker_mid_text() {
    let row = |content: &str| crate::db::models::Message {
        forward: None,
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::nil(),
        role: "user".to_string(),
        content: content.to_string(),
        sequence: 0,
        created_at: chrono::Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    };

    let all = vec![
        row(&summarised("real anchor").marker("")),
        row("kept history"),
        row("I read this out of the log: [CONTEXT COMPACTION - a lane's tool result]"),
    ];

    let kept = AgentService::messages_from_last_compaction(all);

    assert_eq!(
        kept.len(),
        3,
        "a user row quoting the prefix mid-text re-anchored the window (#175)"
    );
    assert!(
        kept[0].content.starts_with(MARKER_PREFIX),
        "loader anchored on the mid-text quote instead of the real marker"
    );
}
