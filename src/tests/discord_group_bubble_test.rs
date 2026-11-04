//! The tool-group bubble slot (#2192): a birth may sleep in the governor, so
//! it must never hold the lock the settle path needs.
//!
//! The bug this pins is an ordering one, not an end state: before the slot,
//! the birth held the message-id mutex across `governor::say`, and `say`
//! sleeps for the channel park window on `Decision::Hold`. A turn that had
//! already produced its answer then waited on that mutex to stamp its settled
//! bubble, so answer latency became a function of the governor policy. The
//! tests below assert the ORDERING directly: a read and a store both return
//! while a birth is parked, and a second birth still waits (one bubble per
//! turn).

use std::time::Duration;

use serenity::model::id::MessageId;
use tokio::sync::oneshot;

use crate::channels::discord::group_bubble::BubbleSlot;

/// A birth in flight: the claim is held and the "post" is parked in the
/// governor. Returns the slot plus a handle that resolves once the claim is
/// taken, so the test never races on a sleep.
async fn parked_birth(
    slot: &std::sync::Arc<BubbleSlot>,
) -> (oneshot::Receiver<()>, tokio::task::JoinHandle<()>) {
    let (claimed_tx, claimed_rx) = oneshot::channel();
    let holder = {
        let slot = slot.clone();
        tokio::spawn(async move {
            let _claim = slot.birth_claim().await;
            let _ = claimed_tx.send(());
            // Stand-in for `governor::say` on `Decision::Hold`: minutes of
            // park in production, longer than this test will wait.
            tokio::time::sleep(Duration::from_secs(30)).await;
            slot.store(MessageId::new(9)).await;
        })
    };
    (claimed_rx, holder)
}

#[tokio::test]
async fn governor_sleep_does_not_hold_turn_group() {
    let slot = std::sync::Arc::new(BubbleSlot::new());
    let (claimed, holder) = parked_birth(&slot).await;
    claimed.await.expect("birth task took the claim");

    // The settle path reads the id to stamp the finished bubble. It must not
    // wait for the parked birth: with the id lock held across the post this
    // call blocks until the governor wakes.
    let read = tokio::time::timeout(Duration::from_millis(250), slot.read()).await;
    assert!(
        read.is_ok(),
        "settle path waited on an in-flight bubble birth: the id lock is still held across the governor sleep"
    );
    assert_eq!(read.unwrap(), None, "no bubble has been born yet");

    holder.abort();
}

#[tokio::test]
async fn a_parked_birth_does_not_block_the_settle_store() {
    let slot = std::sync::Arc::new(BubbleSlot::new());
    let (claimed, holder) = parked_birth(&slot).await;
    claimed.await.expect("birth task took the claim");

    let stored =
        tokio::time::timeout(Duration::from_millis(250), slot.store(MessageId::new(4242))).await;
    assert!(
        stored.is_ok(),
        "store waited on an in-flight bubble birth: the id lock is still held across the governor sleep"
    );
    assert_eq!(slot.read().await, Some(MessageId::new(4242)));

    holder.abort();
}

#[tokio::test]
async fn a_second_birth_waits_for_the_first() {
    let slot = std::sync::Arc::new(BubbleSlot::new());
    let (claimed, holder) = parked_birth(&slot).await;
    claimed.await.expect("birth task took the claim");

    // One bubble per turn: a second tool event must not post its own. It
    // waits for the claim, then finds the id through `read`.
    let second = tokio::time::timeout(Duration::from_millis(120), slot.birth_claim()).await;
    assert!(
        second.is_err(),
        "a second birth ran while the first was in flight: two bubbles for one turn"
    );

    holder.abort();
    // Releasing the parked claim lets the next birth through.
    let third = tokio::time::timeout(Duration::from_millis(500), slot.birth_claim()).await;
    assert!(
        third.is_ok(),
        "birth claim stayed stuck after the holder left"
    );
}

#[tokio::test]
async fn a_later_tool_event_appends_to_the_born_bubble() {
    let slot = BubbleSlot::new();
    assert_eq!(slot.read().await, None);

    slot.store(MessageId::new(77)).await;
    assert_eq!(slot.read().await, Some(MessageId::new(77)));

    // The id handle the ticker and the settle paths hold sees the same value.
    let handle = slot.mid_handle();
    assert_eq!(*handle.lock().await, Some(MessageId::new(77)));
}
