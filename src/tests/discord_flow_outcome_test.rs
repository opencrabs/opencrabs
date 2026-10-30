//! #1911: a Discord turn that ends badly says *how* it ended, using the same
//! vocabulary as the Telegram settled header.
//!
//! Before this the settled line had one blunt word for every failure, so a
//! turn that ran out of time and a turn that crashed looked identical, and
//! both differed from a turn the user stopped. The outcome is a shared
//! [`FlowOutcome`] now, so this pins the rendering, the priority order, and
//! the error classification in one place.

use crate::brain::agent::AgentError;
use crate::channels::background_work::{FlowOutcome, outcome_for_error};
use crate::channels::discord::tool_group::{GroupEntry, GroupState, SettledStatus, render_content};
use std::time::{Duration, Instant};

fn settled(outcome: Option<FlowOutcome>, waiting: Option<&str>) -> GroupState {
    GroupState {
        page: 0,
        entries: vec![
            GroupEntry {
                call_id: String::new(),
                task_id: None,
                name: "bash".into(),
                context: String::new(),
                status: Some(true),
            },
            GroupEntry {
                call_id: String::new(),
                task_id: None,
                name: "read_file".into(),
                context: String::new(),
                status: Some(true),
            },
        ],
        notes: vec![],
        expanded: false,
        started_at: Instant::now(),
        settled: Some(SettledStatus {
            elapsed: Duration::from_secs(42),
            ctx: None,
            waiting: waiting.map(|s| s.to_string()),
            outcome,
        }),
    }
}

#[test]
fn a_timed_out_turn_reads_timed_out_and_not_a_cross() {
    let line = render_content(&settled(Some(FlowOutcome::TimedOut), None));
    assert!(line.contains("⏱ Timed out"), "got: {line}");
    assert!(!line.contains('❌'), "a timeout is not a crash: {line}");
    assert!(!line.contains('🕒'), "the live clock must be gone: {line}");
}

#[test]
fn a_failed_turn_reads_failed() {
    let line = render_content(&settled(Some(FlowOutcome::Failed), None));
    assert!(line.contains("❌ Failed"), "got: {line}");
    assert!(
        !line.contains('✅'),
        "a failed turn cannot claim success: {line}"
    );
}

#[test]
fn a_cancelled_turn_reads_cancelled() {
    let line = render_content(&settled(Some(FlowOutcome::Cancelled), None));
    assert!(line.contains("❌ Cancelled"), "got: {line}");
}

#[test]
fn a_normal_settle_keeps_the_plain_counts_chrome() {
    let line = render_content(&settled(None, None));
    assert!(line.contains("✅ **2 tool calls**"), "got: {line}");
}

#[test]
fn waiting_outranks_a_terminal_outcome() {
    // #1987: a turn that ended with background work still alive is not over,
    // whatever the delivery arm stamped. The waiting verb wins.
    let line = render_content(&settled(
        Some(FlowOutcome::TimedOut),
        Some("waiting for 1 background task"),
    ));
    assert!(
        line.contains("⏳ waiting for 1 background task"),
        "got: {line}"
    );
    assert!(!line.contains("Timed out"), "got: {line}");
}

#[test]
fn the_verb_vocabulary_is_pinned_for_both_channels() {
    // Telegram's settled header and Discord's settled line render from these
    // same strings: changing one changes both, on purpose.
    assert_eq!(FlowOutcome::Finished.icon_verb(), ("✅", "Finished"));
    assert_eq!(FlowOutcome::Failed.icon_verb(), ("❌", "Failed"));
    assert_eq!(FlowOutcome::TimedOut.icon_verb(), ("⏱", "Timed out"));
    assert_eq!(FlowOutcome::Cancelled.icon_verb(), ("❌", "Cancelled"));
}

#[test]
fn timeout_wording_classifies_as_timed_out() {
    for text in [
        "request timed out after 300s",
        "thinking-loop timeout after 600s",
        "deadline exceeded",
    ] {
        let err = AgentError::ToolError(text.into());
        assert_eq!(
            outcome_for_error(&err),
            FlowOutcome::TimedOut,
            "misclassified: {text}"
        );
    }
}

#[test]
fn any_other_error_classifies_as_failed() {
    let err = AgentError::Internal("provider returned 500".into());
    assert_eq!(outcome_for_error(&err), FlowOutcome::Failed);
}
