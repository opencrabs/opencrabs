//! #2194: a pending approval registration is removed however the wait ends.
//!
//! The registration was removed only by the button click, through
//! `resolve_pending_approval`. The 5-minute timeout branch and the
//! closed-channel branch both returned without touching the map, so every
//! approval nobody answered left one sender behind permanently.

use crate::channels::discord::DiscordState;

/// The timeout path never calls `resolve_pending_approval`, so the map has to
/// be cleaned by whoever owns the wait. This is the behaviour the handler now
/// relies on: drop without answering, and the entry is gone.
#[tokio::test]
async fn dropping_an_unanswered_approval_empties_the_registration() {
    let state = DiscordState::new();
    let (tx, rx) = tokio::sync::oneshot::channel::<(bool, bool)>();
    state
        .register_pending_approval("approval-timeout".to_string(), tx)
        .await;

    // Before the drop the registration resolves, which is what a click does.
    // Dropping first must make that impossible, or the entry outlived the wait.
    assert!(
        state.drop_pending_approval("approval-timeout").await,
        "the unanswered registration must still be there to drop"
    );
    assert!(
        !state
            .resolve_pending_approval("approval-timeout", true, false)
            .await,
        "a dropped approval must not be resolvable afterwards"
    );
    assert!(
        rx.await.is_err(),
        "dropping must close the channel: nobody is left waiting on it"
    );
}

/// A click already removed the entry, so the drop that runs after the wait has
/// nothing to do. It must report that rather than resurrecting anything.
#[tokio::test]
async fn dropping_a_resolved_approval_is_a_no_op() {
    let state = DiscordState::new();
    let (tx, mut rx) = tokio::sync::oneshot::channel::<(bool, bool)>();
    state
        .register_pending_approval("approval-click".to_string(), tx)
        .await;

    assert!(
        state
            .resolve_pending_approval("approval-click", true, true)
            .await
    );
    assert_eq!(rx.try_recv(), Ok((true, true)), "the click is delivered");
    assert!(
        !state.drop_pending_approval("approval-click").await,
        "the entry was already consumed by the click"
    );
}

/// The helper is only half of it: it has to run on the way out of the wait,
/// once, before any branch can return. A fix that only cleaned the timeout
/// branch would pass the tests above and still leak on a closed channel.
#[test]
fn the_wait_drops_the_registration_before_branching() {
    const SRC: &str = include_str!("../channels/discord/handler.rs");

    let wait = SRC
        .find("tokio::time::timeout(std::time::Duration::from_secs(300), rx).await")
        .expect("the approval wait is gone from the handler");
    // The send-failure path drops its registration before the wait, so the
    // search has to start at the wait or it finds that earlier call.
    let drop_at = SRC[wait..]
        .find("state.drop_pending_approval(&approval_id).await")
        .map(|i| wait + i)
        .expect("the registration is never dropped, so timed-out approvals leak");
    let branch = SRC[wait..]
        .find("match outcome {")
        .map(|i| wait + i)
        .expect("the outcome match is gone from the handler");

    assert!(
        wait < drop_at && drop_at < branch,
        "the drop must sit between the wait and the branch, so every exit is covered"
    );

    // The branches themselves must not have grown a second removal: one owner
    // for the cleanup is what keeps a future branch from forgetting it.
    let arms = &SRC[branch..];
    let arms = &arms[..arms.find("\n        })\n    })\n}").unwrap_or(arms.len())];
    assert!(
        !arms.contains("drop_pending_approval"),
        "cleanup belongs before the branch, not inside each arm"
    );
}
