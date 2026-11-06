//! #1703 fix 3: the cron compaction boundary opens a run, it does not close it.
//!
//! Two vectors are covered here:
//!
//! 1. Seam parity - the boundary a cron run writes is recognised by the loader
//!    that reads it (`AgentService::messages_from_last_compaction`), and is a
//!    RESTART marker, not a delta segment.
//! 2. Death window - the rows a killed run left behind sit BEHIND the next
//!    fire's own boundary, so they are never reloaded as live context.
//!
//! Plus the structural pin: exactly one boundary per run, written before the
//! turn. A refactor that moves it back to run end, or that stamps both ends,
//! fails here instead of in production at 4am.

use crate::brain::agent::context::{COMPACTION_MARKER_PREFIX, SEGMENT_SENTINEL};
use crate::brain::agent::service::AgentService;
use crate::cron::scheduler::CRON_RUN_BOUNDARY;

const SCHED: &str = include_str!("../cron/scheduler.rs");

/// The body of `execute_job`, bounded at its next sibling so a helper defined
/// further down the file cannot satisfy (or break) these assertions by existing.
fn execute_job_body() -> &'static str {
    let start = SCHED
        .find("async fn execute_job(")
        .expect("execute_job exists in scheduler.rs");
    let rest = &SCHED[start..];
    let end = rest
        .find("async fn execute_direct_trigger_job")
        .expect("direct-trigger path follows execute_job");
    &rest[..end]
}

fn row(seq: i32, role: &str, content: &str) -> crate::db::models::Message {
    crate::db::models::Message {
        forward: None,
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

/// The boundary must be a marker the loader anchors on AND a restart marker:
/// a row carrying `SEGMENT_SENTINEL` extends the compaction window instead of
/// sealing it, which would reopen the exact hole this fix closes.
#[test]
fn boundary_is_a_restart_marker_the_loader_anchors_on() {
    assert!(
        CRON_RUN_BOUNDARY.starts_with(COMPACTION_MARKER_PREFIX),
        "cron boundary lost the loader's anchor prefix: {CRON_RUN_BOUNDARY}"
    );
    assert!(
        !CRON_RUN_BOUNDARY.contains(SEGMENT_SENTINEL),
        "cron boundary carries the segment sentinel, so it would extend the \
         window instead of restarting it: {CRON_RUN_BOUNDARY}"
    );
    // The loader keys on `role == "user" && starts_with(prefix)`, so the row
    // the scheduler writes must be a user row.
    let writer = SCHED
        .split_once("async fn open_run_boundary")
        .map(|(_, rest)| rest)
        .expect("open_run_boundary exists")
        .split_once("\n}\n")
        .map(|(body, _)| body)
        .expect("open_run_boundary body");
    assert!(
        writer.contains("\"user\".to_string()"),
        "the boundary row must be written as a user message or the loader \
         never sees it as a marker"
    );
    assert!(
        writer.contains("CRON_RUN_BOUNDARY"),
        "the boundary must come from the shared const, not an inline copy"
    );
}

/// Structural pin: ONE boundary per run, written BEFORE the agent turn.
///
/// Run end was the old site and it is the bug: a daemon killed mid-run never
/// reached it, so the next fire walked back to the PREVIOUS run's marker and
/// reloaded the dead run's prompt plus half-finished tool results as live
/// context.
#[test]
fn boundary_opens_the_run_and_does_not_double_stamp() {
    let body = execute_job_body();
    assert_eq!(
        body.matches("open_run_boundary(").count(),
        1,
        "execute_job must write exactly one boundary (no start+end double stamp)"
    );
    let marker = body
        .find("open_run_boundary(")
        .expect("boundary call in execute_job");
    let turn = body
        .find("agent.send_message_with_tools_and_callback(")
        .expect("the agent turn call in execute_job");
    assert!(
        marker < turn,
        "the boundary must be written before the turn reads history \
         (marker at {marker}, turn at {turn})"
    );
    assert!(
        !body.contains(COMPACTION_MARKER_PREFIX),
        "execute_job must not author its own copy of the boundary text"
    );
    // And nothing after the turn may stamp a boundary: the end-of-run write
    // was removed deliberately, so its return should still be the plain
    // success path.
    let after_turn = &body[turn..];
    assert!(
        !after_turn.contains("open_run_boundary("),
        "a boundary write after the turn is the #1703 bug reborn"
    );
}

/// Death window, end to end: run N opens, produces tool results, dies. Run
/// N+1 opens. The next fire must load from ITS OWN boundary, so every row the
/// dead run wrote stays behind it.
#[test]
fn a_run_killed_mid_flight_leaves_no_live_context_for_the_next_fire() {
    let dead_run_output = "TOOL RESULT: cat /etc/shadow equivalent, half written";
    let all = vec![
        // Run N-1 completed: its boundary, its prompt, its answer.
        row(1, "user", CRON_RUN_BOUNDARY),
        row(2, "user", "prompt of run N-1"),
        row(3, "assistant", "answer of run N-1"),
        // Run N opened, then the daemon died.
        row(4, "user", CRON_RUN_BOUNDARY),
        row(5, "user", "prompt of run N (never finished)"),
        row(6, "tool", dead_run_output),
        // Run N+1 opens: this is the write fix 3 moved to run START.
        row(7, "user", CRON_RUN_BOUNDARY),
        row(8, "user", "prompt of run N+1"),
    ];

    let kept = AgentService::messages_from_last_compaction(all);
    let texts: Vec<&str> = kept.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        texts,
        vec![CRON_RUN_BOUNDARY, "prompt of run N+1"],
        "the next fire reloaded rows that belong to earlier runs: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains(dead_run_output)),
        "a killed run's tool results were handed to the next fire (#1703)"
    );
}

/// Normal completion keeps behaving as before the move: a fire starts from an
/// empty context, it does not see its own predecessor's history.
#[test]
fn a_completed_run_still_starts_the_next_one_empty() {
    let all = vec![
        row(1, "user", CRON_RUN_BOUNDARY),
        row(2, "user", "standup recap"),
        row(3, "assistant", "here is your standup"),
        row(4, "user", CRON_RUN_BOUNDARY),
        row(5, "user", "standup recap"),
    ];
    let kept = AgentService::messages_from_last_compaction(all);
    assert_eq!(
        kept.len(),
        2,
        "a completed run must still bound its successor"
    );
    assert!(kept[0].content.starts_with(COMPACTION_MARKER_PREFIX));
    assert_eq!(kept[1].content, "standup recap");
}
