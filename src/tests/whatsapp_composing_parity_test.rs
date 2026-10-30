//! #1989: WhatsApp's composing indicator had three gaps: the post-turn tail
//! duplicated the shared tick loop (so `/stop` could not end it and a new
//! turn stacked a second loop under the first), counted only background
//! shell tasks (so sub-agent work left the chat silent), and the resume turn
//! ran with no indicator at all. The loops live inside spawned handlers
//! holding live network clients, so these pins are source-level (the same
//! approach as the Slack thread persistence test); the behavioral halves
//! run against the shared helper directly in `channels_typing_tick_test.rs`.

const HANDLER: &str = include_str!("../channels/whatsapp/handler.rs");
const RESUME: &str = include_str!("../channels/whatsapp/resume.rs");

#[test]
fn tail_runs_through_the_shared_tick_not_a_copy() {
    assert!(
        HANDLER.contains("crate::channels::typing_tick::tick_while_detached("),
        "the handover tail must call the shared tick, not re-roll its own loop"
    );
    assert_eq!(
        HANDLER.matches("manager.running_for(").count(),
        0,
        "the duplicated `while manager.running_for` tail is the bug (#1989); \
         the shared helper owns that predicate now"
    );
}

#[test]
fn tail_counts_subagents_and_watches_the_stop_token() {
    assert!(
        HANDLER.contains("let agents = agent.subagent_manager();"),
        "the typing task must grab the sub-agent registry: shell-task-only \
         counting went silent the moment a turn ended with working agents"
    );
    assert!(
        HANDLER.contains("Some(stop.clone()),"),
        "the tail passes the session stop token into the shared tick, so \
         /stop and the next turn's registration can end the indicator"
    );
    assert!(
        HANDLER.contains("if !stop.is_cancelled()"),
        "paused is only sent after the work ends on its own; a /stop or a \
         new turn owns what the user sees from there"
    );
}

#[test]
fn stop_token_is_born_before_the_task_that_watches_it() {
    let created = HANDLER
        .find("let cancel_token = CancellationToken::new();")
        .expect("the session cancel token must be created in the handler");
    let watched = HANDLER
        .find("let stop = cancel_token.clone();")
        .expect("the typing task must clone the session token as its stop");
    let stored = HANDLER
        .find(".store_cancel_token(session_id, cancel_token.clone())")
        .expect("the token must still be registered for /stop");
    assert!(
        created < watched && watched < stored,
        "order must be create -> typing task takes its clone -> register: \
         created {created}, watched {watched}, stored {stored}"
    );
    assert_eq!(
        HANDLER
            .matches("let cancel_token = CancellationToken::new();")
            .count(),
        1,
        "exactly one token per turn; two would split /stop from the tail"
    );
}

#[test]
fn resume_turn_keeps_composing_until_the_delivery_path_ends() {
    assert!(
        RESUME.contains("let typing = tokio::spawn("),
        "the resume turn must run a composing ticker across the slowest \
         message of the cycle (the tail loop had already stopped)"
    );
    assert!(
        RESUME.contains("typing.abort();"),
        "the ticker must be cut when the flow ends, on every exit path"
    );
    // The abort sits after the wrapped flow, not inside an early-return arm:
    // a `return` from `resume_flow` still lands on the abort line.
    let flow = RESUME
        .find("let resume_flow = async {")
        .expect("the resume body must be wrapped so one cancel covers it all");
    let abort = RESUME.find("typing.abort();").expect("abort present");
    assert!(
        flow < abort,
        "the flow must open before the abort that ends its ticker"
    );
    assert!(
        RESUME.contains("async fn send_composing_now"),
        "the ticker's ping needs the transport-aware helper: state.client() \
         is async and may be None while the socket reconnects"
    );
}

// ── #2181: composing retries back off and give up ────────────────────

/// The composing loop used to retry every 5 seconds forever, connected or
/// not. During an outage that wrote one WARN per 5 seconds per session for a
/// call that could never succeed: 48 lines in 4 minutes, on a log that
/// already runs past 130k lines a day.
///
/// The policy is pure so it can be pinned without a socket. The shape that
/// matters is monotonic growth to a ceiling and a terminal `None`: a policy
/// that never returns `None` is the old bug with extra steps.
#[test]
fn composing_retries_back_off_then_give_up() {
    use crate::channels::whatsapp::handler::{
        COMPOSING_BASE_DELAY, COMPOSING_MAX_DELAY, COMPOSING_MAX_FAILURES, composing_retry_delay,
    };

    // A healthy send schedules the normal cadence.
    assert_eq!(composing_retry_delay(0), Some(COMPOSING_BASE_DELAY));

    let first = composing_retry_delay(1).expect("the first retry must still happen");
    let second = composing_retry_delay(2).expect("the second retry must still happen");
    // The issue specifies the ladder as 5s, 10s, 20s, 30s cap: the first
    // retry keeps the normal cadence, and growth starts from the second.
    assert_eq!(
        first, COMPOSING_BASE_DELAY,
        "the first retry after a failure keeps the normal cadence"
    );
    assert!(
        second > first,
        "the delay must grow, got {second:?} after {first:?}"
    );

    // Every delay stays under the ceiling, including the last one before the
    // cap, which is where an unclamped doubling would blow past 30s.
    for failures in 1..COMPOSING_MAX_FAILURES {
        let d = composing_retry_delay(failures).expect("below the cap a retry is scheduled");
        assert!(d <= COMPOSING_MAX_DELAY, "delay {d:?} exceeds the ceiling");
    }

    // The cap terminates the loop. Without this the issue is not fixed.
    assert_eq!(
        composing_retry_delay(COMPOSING_MAX_FAILURES),
        None,
        "the loop must stop after {COMPOSING_MAX_FAILURES} consecutive failures"
    );
    assert_eq!(composing_retry_delay(COMPOSING_MAX_FAILURES + 10), None);
}

/// A dead connection must be skipped rather than attempted. The check is what
/// keeps an outage from producing a WARN per tick, so its presence in the loop
/// is the other half of the fix.
#[test]
fn the_composing_loop_checks_the_connection() {
    const SRC: &str = include_str!("../channels/whatsapp/handler.rs");
    let loop_at = SRC
        .find("check the connection before every attempt")
        .expect("the composing loop lost its connection check");
    let after = &SRC[loop_at..];
    assert!(
        after.contains("state.is_connected().await"),
        "the composing loop must skip a dead connection instead of pinging it"
    );
    assert!(
        after.contains("composing_retry_delay(failures)"),
        "the composing loop must use the backoff policy, not a fixed 5s sleep"
    );
}
