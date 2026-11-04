//! #2108: `/stop <session-id>` targets a turn running in ANOTHER session.
//!
//! Plain `/stop` can only reach the session the message landed in, so a wedged
//! turn in a different chat had no kill switch from Telegram. The command is
//! owner-only, resolves a full id or a unique prefix, and reports which of the
//! three states it found instead of a flat "no operation in progress".

use crate::brain::agent::QueuedUserMessage;
use crate::channels::commands::{
    ChannelCommand, MIN_TARGET_PREFIX, TargetMatch, handle_command, resolve_session_prefix,
    shortest_unique_prefix, targeted_stop_hint,
};
use crate::channels::telegram::TelegramState;
use crate::channels::telegram::state::TargetStop;
use crate::tests::agent_service_mocks::create_test_service_full;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ── parsing ──────────────────────────────────────────────────────────

#[tokio::test]
async fn stop_with_a_target_parses_into_stop_target() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/stop a1b2c3d4", sid, &agent, &svc, true, None).await;
    match cmd {
        ChannelCommand::StopTarget(target) => assert_eq!(target, "a1b2c3d4"),
        _ => panic!("`/stop <id>` must parse as StopTarget"),
    }
}

#[tokio::test]
async fn bare_stop_still_means_the_current_session() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/stop", sid, &agent, &svc, true, None).await;
    assert!(
        matches!(cmd, ChannelCommand::Stop),
        "bare /stop must keep cancelling the session the message landed in"
    );
}

#[tokio::test]
async fn stop_with_an_empty_target_is_refused_with_the_shape() {
    let (agent, svc, sid) = create_test_service_full().await;
    // `/stop ` with nothing after it normalises to bare /stop, so the empty
    // case that actually reaches the parser is the colon form.
    let cmd = handle_command("/stop:   ", sid, &agent, &svc, true, None).await;
    match cmd {
        ChannelCommand::UnknownCommand(msg) => {
            assert!(
                msg.contains("/stop <session-id>"),
                "the refusal must show the accepted shape, got: {msg}"
            );
        }
        _ => panic!("an empty target must be refused, not dispatched"),
    }
}

#[tokio::test]
async fn targeted_stop_is_owner_only() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/stop a1b2c3d4", sid, &agent, &svc, false, None).await;
    match cmd {
        ChannelCommand::UnknownCommand(msg) => {
            assert!(
                msg.to_lowercase().contains("owner"),
                "denial should mention owner-only, got: {msg}"
            );
        }
        _ => panic!("a non-owner must not be able to stop another session"),
    }
}

// ── prefix resolution ────────────────────────────────────────────────

#[test]
fn a_full_id_resolves_to_itself() {
    let id = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    assert_eq!(
        resolve_session_prefix(&id.to_string(), [&id].into_iter()),
        TargetMatch::Unique(id)
    );
}

#[test]
fn a_unique_prefix_resolves_to_its_session() {
    let a = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    let b = Uuid::parse_str("ffffffff-0000-4000-8000-000000000002").unwrap();
    assert_eq!(
        resolve_session_prefix("a1b2", [&a, &b].into_iter()),
        TargetMatch::Unique(a)
    );
}

#[test]
fn a_shared_prefix_is_reported_as_ambiguous_not_guessed() {
    let a = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    let b = Uuid::parse_str("a1b2ffff-0000-4000-8000-000000000002").unwrap();
    match resolve_session_prefix("a1b2", [&a, &b].into_iter()) {
        TargetMatch::Ambiguous(ids) => {
            assert_eq!(ids.len(), 2, "both candidates must be reported");
            assert!(ids.contains(&a) && ids.contains(&b));
        }
        other => panic!("an ambiguous prefix must never pick one session, got {other:?}"),
    }
}

#[test]
fn a_short_prefix_is_refused_outright() {
    let a = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    assert_eq!(
        resolve_session_prefix("a", [&a].into_iter()),
        TargetMatch::None,
        "a {MIN_TARGET_PREFIX}-char minimum stops a stray token selecting a session"
    );
}

#[test]
fn an_unknown_target_resolves_to_nothing() {
    let a = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    assert_eq!(
        resolve_session_prefix("deadbeef", [&a].into_iter()),
        TargetMatch::None
    );
}

#[test]
fn the_shortest_unique_prefix_is_long_enough_to_stay_unique() {
    let a = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    let b = Uuid::parse_str("a1b2ffff-0000-4000-8000-000000000002").unwrap();
    let prefix = shortest_unique_prefix(a, [&a, &b].into_iter());
    assert!(
        prefix.len() > MIN_TARGET_PREFIX,
        "a {MIN_TARGET_PREFIX}-char prefix is shared here, so it must be longer: {prefix}"
    );
    assert_eq!(
        resolve_session_prefix(&prefix, [&a, &b].into_iter()),
        TargetMatch::Unique(a)
    );
}

// ── the stop itself ──────────────────────────────────────────────────

#[tokio::test]
async fn stopping_session_a_from_here_cancels_a_and_leaves_b_alone() {
    let state = Arc::new(TelegramState::new());
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();

    let token_a = CancellationToken::new();
    let token_b = CancellationToken::new();
    state.store_cancel_token(a, token_a.clone()).await;
    state.store_cancel_token(b, token_b.clone()).await;
    let _turn_a = state.mark_turn_active(a);
    let _turn_b = state.mark_turn_active(b);

    let outcome = state.stop_session_target(a).await;
    assert_eq!(outcome, TargetStop::Delivered { queued_dropped: 0 });
    assert!(
        token_a.is_cancelled(),
        "the targeted session must be cancelled"
    );
    assert!(
        !token_b.is_cancelled(),
        "the session that sent the command must keep running"
    );
    assert!(state.is_turn_active(b), "the sender's turn is untouched");
}

#[tokio::test]
async fn the_receipt_reports_queued_messages_dropped_with_the_stop() {
    let state = Arc::new(TelegramState::new());
    let a = Uuid::new_v4();
    let token = CancellationToken::new();
    state.store_cancel_token(a, token.clone()).await;
    let _turn = state.mark_turn_active(a);
    state.enqueue_reaction(a, QueuedUserMessage::plain("follow-up one".into()));
    state.enqueue_reaction(a, QueuedUserMessage::plain("follow-up two".into()));

    let outcome = state.stop_session_target(a).await;
    assert_eq!(
        outcome,
        TargetStop::Delivered { queued_dropped: 2 },
        "a stop must say how much queued work it dropped"
    );
    assert!(
        state.drain_queued_items(a).is_empty(),
        "the queue is drained"
    );
}

#[tokio::test]
async fn an_idle_session_reports_queued_work_instead_of_nothing() {
    let state = Arc::new(TelegramState::new());
    let a = Uuid::new_v4();
    state.enqueue_reaction(a, QueuedUserMessage::plain("still waiting".into()));

    assert_eq!(
        state.stop_session_target(a).await,
        TargetStop::Queued { dropped: 1 },
        "nothing running but a queue is not the same as idle"
    );
}

#[tokio::test]
async fn a_fully_idle_session_says_so() {
    let state = Arc::new(TelegramState::new());
    assert_eq!(
        state.stop_session_target(Uuid::new_v4()).await,
        TargetStop::Idle
    );
}

#[tokio::test]
async fn only_live_turns_are_listed_as_running() {
    let state = Arc::new(TelegramState::new());
    let live = Uuid::new_v4();
    let finished = Uuid::new_v4();

    let _turn = state.mark_turn_active(live);
    // A completed turn leaves its non-cancelled token behind until the next
    // one; that must not be reported as running.
    state
        .store_cancel_token(finished, CancellationToken::new())
        .await;

    assert_eq!(state.running_session_ids(), vec![live]);
}

// ── the narrow prose guard ───────────────────────────────────────────

#[test]
fn a_targeted_stop_typed_as_prose_gets_the_exact_command() {
    let a = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    let hint = targeted_stop_hint("stop a1b2c3d4", &[a])
        .expect("an owner naming a running session after a stop word is a targeted stop");
    assert!(
        hint.contains("/stop a1b2"),
        "the hint must carry a runnable command, got: {hint}"
    );
}

#[test]
fn ordinary_instructions_are_never_caught_by_the_guard() {
    let a = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    for text in [
        "stop the docker container",
        "wait for the build",
        "cancel the subscription",
        "kill the process on port 8080",
        "the stop button in the UI is broken",
    ] {
        assert!(
            targeted_stop_hint(text, &[a]).is_none(),
            "must not intercept an ordinary instruction: {text:?}"
        );
    }
}

#[test]
fn a_recognised_stop_intent_is_left_to_the_kill_switch() {
    let a = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    for text in ["stop", "STOP!!!", "hold on"] {
        assert!(
            targeted_stop_hint(text, &[a]).is_none(),
            "a real stop intent belongs to the normal kill switch: {text:?}"
        );
    }
}

#[test]
fn the_guard_is_silent_when_nothing_is_running() {
    assert!(
        targeted_stop_hint("stop a1b2c3d4", &[]).is_none(),
        "there is nothing to target, so the message is ordinary prose"
    );
}

#[test]
fn a_session_id_the_guard_cannot_place_is_not_intercepted() {
    let a = Uuid::parse_str("a1b2c3d4-0000-4000-8000-000000000001").unwrap();
    assert!(
        targeted_stop_hint("stop deadbeef", &[a]).is_none(),
        "a hex token that names no running session is not a targeted stop"
    );
}
