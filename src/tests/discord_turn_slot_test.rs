//! Behavioral tests for the Discord turn slot and mid-turn follow-up
//! queue (#1990). Pure state: no bot, no HTTP, no DB pool (the durable
//! twin in `notify_queue` no-ops without a runtime, exactly the path
//! these tests ride).

use std::sync::Arc;

use crate::brain::agent::{PushOrigin, QueuedUserMessage};
use crate::channels::discord::DiscordState;

fn queued(text: &str) -> QueuedUserMessage {
    QueuedUserMessage {
        context_text: text.to_string(),
        display_text: text.to_string(),
        origin: PushOrigin::Ingress,
        bg_meta: None,
    }
}

#[test]
fn slot_claim_is_exclusive_and_released_by_drop() {
    let state = Arc::new(DiscordState::new());
    let sid = uuid::Uuid::new_v4();

    // The first claim wins.
    let guard = state
        .try_begin_turn(sid)
        .expect("first claim takes the slot");

    // A second claim while it is held returns None: that caller queues its
    // follow-up instead of forking a second concurrent tool loop. The whole
    // of #1990's fork bug in one assertion. Contending a held slot is a
    // pure read: the insert fails, the set keeps the one entry.
    assert!(
        state.try_begin_turn(sid).is_none(),
        "the slot is held, no double claim"
    );

    // Drop releases the slot; a re-claim then succeeds.
    drop(guard);
    let second = state
        .try_begin_turn(sid)
        .expect("the slot is free after the guard dropped");

    // A different session is never blocked by this one's live turn. The
    // other-session guard must be HELD across the check: an `is_some()`
    // alone drops it inside the assertion, freeing the slot before the
    // next line ever ran.
    let other = uuid::Uuid::new_v4();
    let other_guard = state
        .try_begin_turn(other)
        .expect("a second session claims its own slot while sid is held");
    assert!(
        state.try_begin_turn(sid).is_none() && state.try_begin_turn(other).is_none(),
        "both sessions hold their own slots at once"
    );
    drop(other_guard);
    drop(second);
}

#[test]
fn followups_join_fifo_and_leave_no_husk() {
    let state = DiscordState::new();
    let sid = uuid::Uuid::new_v4();

    // Empty queue: nothing, and no entry left behind.
    assert!(state.drain_followups(sid).is_none());

    state.enqueue_followup(sid, queued("first message"));
    state.enqueue_followup(sid, queued("second message"));

    // ONE joined message answers a whole burst (#1837 rule): both texts
    // present, arrival order kept.
    let joined = state
        .drain_followups(sid)
        .expect("two queued items join into one");
    let ctx = &joined.context_text;
    let (Some(i1), Some(i2)) = (ctx.find("first message"), ctx.find("second message")) else {
        panic!("joined context lost an item: {ctx:?}");
    };
    assert!(i1 < i2, "FIFO order kept: {ctx:?}");

    // The drain emptied the entry: no husk, a repeat drain is nothing.
    assert!(state.drain_followups(sid).is_none());
}

#[tokio::test]
async fn queue_callback_drains_the_session_queue_per_session() {
    let state = Arc::new(DiscordState::new());
    let sid_a = uuid::Uuid::new_v4();
    let sid_b = uuid::Uuid::new_v4();
    state.enqueue_followup(sid_a, queued("alpha follow-up"));
    state.enqueue_followup(sid_b, queued("beta follow-up"));

    // The callback wired into AgentService (manager.rs) is what the tool
    // loop calls between rounds. It drains session A's queue and nothing
    // else.
    let cb = state.followup_queue_callback();
    let drained_a = cb(sid_a).await.expect("session A had a follow-up");
    assert!(drained_a.context_text.contains("alpha follow-up"));

    // A is empty now, and the end-of-turn flush shares this same queue:
    // a second drain of A is nothing, B still has its one item, untouched
    // by A's drain (the per-session keying is the whole callback contract).
    assert!(cb(sid_a).await.is_none());
    let drained_b = cb(sid_b).await.expect("session B survives A's drain");
    assert!(drained_b.context_text.contains("beta follow-up"));
}

#[tokio::test]
async fn storing_a_token_never_cancels_the_previous_one() {
    let state = DiscordState::new();
    let sid = uuid::Uuid::new_v4();

    // A completed turn leaves its token behind: it never cancels its own
    // token and `remove_cancel_token` keeps non-cancelled ones on purpose.
    let finished = tokio_util::sync::CancellationToken::new();
    state.store_cancel_token(sid, finished.clone()).await;

    // The next turn stores its own. It must REPLACE the leftover silently:
    // cancelling it here killed nothing and logged "cancelling previous
    // in-flight agent call" for a turn that had ended minutes earlier.
    let live = tokio_util::sync::CancellationToken::new();
    state.store_cancel_token(sid, live.clone()).await;
    assert!(
        !finished.is_cancelled(),
        "a finished turn's leftover token must never be cancelled by the next turn"
    );

    // /stop reaches the token the RUNNING turn stored.
    assert!(
        state.cancel_session(sid).await,
        "the running turn's token must be the one cancel_session reaches"
    );
    assert!(live.is_cancelled(), "/stop cancels the live token");
    assert!(
        !state.cancel_session(sid).await,
        "cancel_session removes what it cancelled, a repeat finds nothing"
    );
}

#[tokio::test]
async fn remove_cancel_token_keeps_a_live_token_and_reaps_a_cancelled_one() {
    let state = DiscordState::new();
    let sid = uuid::Uuid::new_v4();

    // Cleanup from a finishing turn must not strip the live token: /stop
    // would then find nothing while the call is still running.
    let live = tokio_util::sync::CancellationToken::new();
    state.store_cancel_token(sid, live.clone()).await;
    state.remove_cancel_token(sid).await;
    assert!(
        state.cancel_session(sid).await,
        "the live token survived the cleanup pass"
    );

    // A cancelled token IS reaped, so /stop leaves no husk behind.
    let cancelled = tokio_util::sync::CancellationToken::new();
    state.store_cancel_token(sid, cancelled.clone()).await;
    cancelled.cancel();
    state.remove_cancel_token(sid).await;
    assert!(
        !state.cancel_session(sid).await,
        "a cancelled token is reaped by the cleanup pass"
    );
}

#[tokio::test]
async fn concurrent_claims_contend_for_one_slot() {
    let state = Arc::new(DiscordState::new());
    let sid = uuid::Uuid::new_v4();

    // Hold the slot HERE while racers try to claim: a guard only released
    // at task end would let every racer win sequentially, so the parent
    // keeps its guard across the contention (and the racers await it).
    let held = state
        .try_begin_turn(sid)
        .expect("the parent claims the slot first");
    let mut handles = Vec::new();
    for _ in 0..4 {
        let s = state.clone();
        handles.push(tokio::spawn(async move { s.try_begin_turn(sid).is_some() }));
    }
    for h in handles {
        assert!(
            !h.await.unwrap(),
            "a held slot excludes every concurrent claimer"
        );
    }
    drop(held);

    // After the release a fresh claim wins: guards truly free the slot,
    // on every path including the early-return ones.
    assert!(state.try_begin_turn(sid).is_some());
}
