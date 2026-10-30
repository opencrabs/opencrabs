//! #1984 (typing-tick half): a child parked at `AwaitingInput` belongs to a
//! human, not the machine, so the handover typing indicator must not keep
//! ticking because of it. The #1985 extraction first counted
//! `working + awaiting`, which pins the Telegram/Discord tick loop open on a
//! follow-up that may never arrive, exactly the stuck clock #1984 reports.
//! These tests pin the predicate: awaiting-only returns with zero pings, a
//! mid-round child keeps the pings coming, and an unwired surface sends
//! nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::brain::agent::service::background_tasks::BackgroundTaskManager;
use crate::brain::tools::subagent::manager::{SubAgent, SubAgentManager, SubAgentState};
use crate::channels::typing_tick::tick_while_detached;

fn child(parent: Uuid, awaiting: bool) -> SubAgent {
    SubAgent {
        state: if awaiting {
            SubAgentState::AwaitingInput
        } else {
            SubAgentState::Running
        },
        ..SubAgent::new(
            Uuid::new_v4().to_string(),
            "test child",
            Uuid::new_v4(),
            parent,
        )
    }
}

/// Count pings for a 150 ms budget at a 10 ms tick. A still-looping case
/// gets cancelled by the timeout; every settled case returned on its own.
async fn run_tick(agents: Option<Arc<SubAgentManager>>, session: Uuid) -> usize {
    run_tick_with_stop(agents, session, None).await
}

/// Same budget with the optional stop token (#1989) wired through, so the
/// cancel semantics the WhatsApp tail depends on are exercised for real.
async fn run_tick_with_stop(
    agents: Option<Arc<SubAgentManager>>,
    session: Uuid,
    stop: Option<CancellationToken>,
) -> usize {
    let pings = Arc::new(AtomicUsize::new(0));
    let counted = pings.clone();
    let tick = tick_while_detached(
        None,
        agents,
        session,
        stop,
        Duration::from_millis(10),
        // Far outside every test's own 150 ms watchdog below, so the
        // existing cases keep exercising exactly what they exercised
        // before the #1984 ceiling landed.
        Duration::from_secs(5),
        move || {
            let pings = counted.clone();
            async move {
                pings.fetch_add(1, Ordering::SeqCst);
            }
        },
    );
    let _ = tokio::time::timeout(Duration::from_millis(150), tick).await;
    pings.load(Ordering::SeqCst)
}

#[tokio::test]
async fn awaiting_children_do_not_hold_the_typing_tick() {
    // #1984 acceptance: typing stops while every child is parked on a human.
    let mgr = Arc::new(SubAgentManager::new());
    let session = Uuid::new_v4();
    mgr.insert(child(session, true));
    mgr.insert(child(session, true));
    let pings = run_tick(Some(mgr), session).await;
    assert_eq!(pings, 0, "awaiting-only session must not ping at all");
}

#[tokio::test]
async fn working_children_hold_the_typing_tick() {
    let mgr = Arc::new(SubAgentManager::new());
    let session = Uuid::new_v4();
    mgr.insert(child(session, false));
    let pings = run_tick(Some(mgr), session).await;
    assert!(pings > 0, "a mid-round child must keep the tick alive");
}

#[tokio::test]
async fn unwired_surface_sends_nothing() {
    let pings = run_tick(None, Uuid::new_v4()).await;
    assert_eq!(pings, 0);
}

#[tokio::test]
async fn stop_token_ends_a_live_tick() {
    // #1989: the WhatsApp tail must answer `/stop` (and the next turn's
    // token takeover) while detached work is still running. A live child
    // keeps the count above zero forever, so only the stop token can break
    // this loop; without it the helper pings until the 150 ms timeout.
    let mgr = Arc::new(SubAgentManager::new());
    let session = Uuid::new_v4();
    mgr.insert(child(session, false));
    let stop = CancellationToken::new();
    let watcher = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(35)).await;
        watcher.cancel();
    });
    let pings = run_tick_with_stop(Some(mgr), session, Some(stop)).await;
    assert!(pings > 0, "the tick must ping while work runs");
    assert!(
        pings <= 10,
        "a cancelled stop must end the loop within a tick or two, got {pings}"
    );
}

#[tokio::test]
async fn cancelled_stop_before_entry_sends_nothing() {
    // The takeover case: the previous turn's stop token is already dead
    // before the old tail ever starts. A dead session must not get a single
    // post-handover ping.
    let mgr = Arc::new(SubAgentManager::new());
    let session = Uuid::new_v4();
    mgr.insert(child(session, false));
    let stop = CancellationToken::new();
    stop.cancel();
    let pings = run_tick_with_stop(Some(mgr), session, Some(stop)).await;
    assert_eq!(pings, 0, "an already-cancelled stop must ping nothing");
}

#[tokio::test]
async fn tick_loop_exits_at_its_ceiling() {
    // #1984 acceptance: no counted row, however stuck, can hold the
    // indicator past the ceiling. The row stays live (the ceiling stops
    // the PINGING, not the registry), so only the ceiling can break this
    // loop, and it must do so before the 150 ms watchdog.
    let bg = Arc::new(BackgroundTaskManager::new());
    let session = Uuid::new_v4();
    bg.mirror_started(session, "stuck row never finished");
    let pings = Arc::new(AtomicUsize::new(0));
    let counted = pings.clone();
    let tick = tick_while_detached(
        Some(bg.clone()),
        None,
        session,
        None,
        Duration::from_millis(10),
        Duration::from_millis(60),
        move || {
            let pings = counted.clone();
            async move {
                pings.fetch_add(1, Ordering::SeqCst);
            }
        },
    );
    let ended_on_its_own = tokio::time::timeout(Duration::from_millis(150), tick)
        .await
        .is_ok();
    let n = pings.load(Ordering::SeqCst);
    assert!(ended_on_its_own, "the ceiling must end the loop itself");
    // >=1: a zero-tick ceiling bug would exit before any ping; <=12: the
    // 60 ms ceiling at a 10 ms tick can only cover a handful of pings.
    assert!(
        (1..=12).contains(&n),
        "the ceiling should allow a handful of pings then stop, got {n}"
    );
    assert_eq!(
        bg.running_for(session),
        1,
        "the ceiling stops the pinging, not the registry"
    );
}

// ── #2189: the compaction pinger is bound to the turn ────────────────

/// The Discord compaction pinger used to run 12 fixed rounds of 8s with no
/// cancel token, so a compaction that finished in 2s left the client showing
/// "is typing" for another 90 seconds, and each compaction spawned a new
/// untracked 96s task.
///
/// A behavioural test would need a live gateway context, so this pins the two
/// things that make the defect impossible: the loop observes the turn's
/// cancellation token, and it is that token, not a fresh one.
#[test]
fn the_compaction_pinger_stops_with_the_turn() {
    const SRC: &str = include_str!("../channels/discord/handler.rs");

    let arm = SRC
        .find("ProgressEvent::Compacting { .. } =>")
        .expect("the compaction arm is gone");
    let rest = &SRC[arm..];
    let arm_body = &rest[..rest
        .find("ProgressEvent::ToolStarted")
        .unwrap_or(rest.len())];

    assert!(
        arm_body.contains("turn_typing.clone()"),
        "the pinger must use the turn's typing token, not one of its own"
    );
    assert!(
        arm_body.contains("cancel.is_cancelled()") && arm_body.contains("cancel.cancelled()"),
        "the pinger must observe cancellation before pinging and while waiting"
    );
    assert!(
        !arm_body.contains("for _ in 0..12"),
        "the fixed 12-round loop is the defect; it must be the named ceiling"
    );
}
