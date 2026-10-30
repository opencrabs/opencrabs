//! The WhatsApp session turn slot and mid-turn queue (#2179).
//!
//! Pure state: no client, no network, no DB pool. The durable twin in
//! `notify_queue` no-ops without a repo, exactly the path these tests ride.
//!
//! The bug these pin down: `store_cancel_token` cancelled the token it found,
//! so a second inbound message on a busy session killed the live turn. Three
//! messages in six seconds produced one answer, to the last one.

use std::sync::Arc;

use uuid::Uuid;

use crate::brain::agent::{PushOrigin, QueuedUserMessage};
use crate::channels::whatsapp::WhatsAppState;

fn queued(text: &str) -> QueuedUserMessage {
    QueuedUserMessage {
        context_text: text.to_string(),
        display_text: text.to_string(),
        origin: PushOrigin::Ingress,
        bg_meta: None,
    }
}

#[tokio::test]
async fn inbound_queues_behind_live_turn() {
    let state = Arc::new(WhatsAppState::new());
    let sid = Uuid::new_v4();

    // The first message claims the session's turn slot, exactly as
    // `handle_message` does before it sets up streaming and calls the agent.
    let live = state
        .try_begin_turn(sid)
        .expect("the first inbound message takes the slot");

    // The live turn stored its cancel token on the way in.
    let live_token = tokio_util::sync::CancellationToken::new();
    state.store_cancel_token(sid, live_token.clone()).await;

    // A second inbound message arrives while that turn is still running.
    // It must NOT claim the slot, and it must NOT cancel the live turn:
    // that cancel is the whole bug (#2179). It queues instead.
    assert!(
        state.try_begin_turn(sid).is_none(),
        "a second message cannot claim a slot that is already held"
    );
    state.enqueue_turn_message(sid, queued("second message"));

    // The pre-fix handler reached its `store_cancel_token` call site on the
    // second message too (there was no slot to stop it), and that store
    // hard-cancelled the live turn: this is the exact sequence that dropped
    // an in-flight answer. Storing must now be harmless.
    state
        .store_cancel_token(sid, tokio_util::sync::CancellationToken::new())
        .await;

    assert!(
        !live_token.is_cancelled(),
        "an inbound message must never cancel the turn already in flight"
    );

    // The second message is not lost: the tool loop drains it between
    // rounds through the callback wired in manager.rs.
    let cb = state.followup_queue_callback();
    let drained = cb(sid)
        .await
        .expect("the queued follow-up is delivered to the live turn");
    assert!(drained.context_text.contains("second message"));
    assert!(
        !live_token.is_cancelled(),
        "draining the queue does not disturb the live turn either"
    );

    // When the live turn finishes its guard drops, the slot frees up and the
    // next message can start a turn of its own instead of forking.
    drop(live);
    let next = state
        .try_begin_turn(sid)
        .expect("the slot is free once the live turn ended");
    drop(next);
}

#[tokio::test]
async fn storing_a_token_never_cancels_the_previous_one() {
    let state = Arc::new(WhatsAppState::new());
    let sid = Uuid::new_v4();

    let previous = tokio_util::sync::CancellationToken::new();
    state.store_cancel_token(sid, previous.clone()).await;

    let current = tokio_util::sync::CancellationToken::new();
    state.store_cancel_token(sid, current.clone()).await;

    assert!(
        !previous.is_cancelled(),
        "store replaces the token, it never cancels the turn that owns it"
    );
    assert!(!current.is_cancelled());

    // Genuine cancellation still works, and only through `cancel_session`:
    // that is the `/stop` path.
    assert!(state.cancel_session(sid).await);
    assert!(current.is_cancelled());
    assert!(!state.cancel_session(sid).await);
}

#[tokio::test]
async fn a_burst_joins_into_one_message_in_arrival_order() {
    let state = Arc::new(WhatsAppState::new());
    let sid = Uuid::new_v4();
    let other = Uuid::new_v4();

    let guard = state.try_begin_turn(sid).expect("turn claimed");
    state.enqueue_turn_message(sid, queued("first follow-up"));
    state.enqueue_turn_message(sid, queued("second follow-up"));
    state.enqueue_turn_message(other, queued("other session"));

    // One round-end answers the whole burst (#1837): both texts present, in
    // arrival order, in ONE message.
    let joined = state
        .drain_turn_messages(sid)
        .expect("the burst joins into one");
    let ctx = &joined.context_text;
    let (Some(first), Some(second)) = (ctx.find("first follow-up"), ctx.find("second follow-up"))
    else {
        panic!("joined context lost an item: {ctx:?}");
    };
    assert!(first < second, "FIFO order kept: {ctx:?}");

    // Per-session keying: the other session's message is untouched.
    let other_msg = state
        .drain_turn_messages(other)
        .expect("the other session keeps its own queue");
    assert!(other_msg.context_text.contains("other session"));

    // No husk left behind after a drain.
    assert!(state.drain_turn_messages(sid).is_none());
    assert!(!state.has_queued_turn_message(sid));
    drop(guard);
}
