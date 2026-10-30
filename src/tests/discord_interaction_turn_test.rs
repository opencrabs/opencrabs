//! The turn slot covers every Discord entry point that starts a turn (#2191).
//!
//! The select menu, the modal submit and the reaction handler each start an
//! agent turn. Before this, none of them touched the session turn slot, so a
//! component interaction arriving while a turn was live forked a SECOND
//! concurrent loop onto the same session row: history rows interleaved and
//! the new loop read a transcript the live one was still appending to.
//!
//! Pure state, like `discord_turn_slot_test`: no bot, no HTTP, no DB pool
//! (the durable twin in `notify_queue` no-ops without a runtime).

use std::sync::Arc;

use crate::channels::discord::DiscordState;

/// The defect in one assertion: a component interaction that arrives while a
/// turn is live must NOT start its own loop. It loses the claim and joins the
/// live turn's follow-up queue, so nothing the user did is dropped.
#[test]
fn interaction_waits_for_turn_slot() {
    let state = Arc::new(DiscordState::new());
    let sid = uuid::Uuid::new_v4();

    // A turn is already running on this session.
    let live = state
        .try_begin_turn(sid)
        .expect("the live turn owns the slot");

    // A select pick arrives mid-turn. It must lose the claim.
    let admission = state.claim_interaction_turn(
        sid,
        "picked \"Run the tests\" from your select menu".to_string(),
        "[System: adolfo picked \"Run the tests\"]".to_string(),
    );
    assert!(
        admission.is_none(),
        "a component interaction must not start a second loop while a turn is live"
    );

    // Losing the claim queues it for the live turn instead of dropping it,
    // and history keeps the compact tag, never the prompt scaffolding.
    let queued = state
        .drain_followups(sid)
        .expect("the interaction was queued for the live turn, not dropped");
    assert!(
        queued.context_text.contains("Run the tests"),
        "the live turn must receive what the user picked, got: {}",
        queued.context_text
    );
    assert!(
        !queued.display_text.contains("select menu"),
        "history keeps the compact system tag, not the prompt scaffolding, got: {}",
        queued.display_text
    );

    // Once the live turn ends, the next interaction claims normally.
    drop(live);
    let guard = state
        .claim_interaction_turn(
            sid,
            "picked \"Ship it\"".to_string(),
            "[System: adolfo picked \"Ship it\"]".to_string(),
        )
        .expect("the slot is free again after the live turn ended");
    assert!(
        state.drain_followups(sid).is_none(),
        "a claimed interaction runs now, it is not queued"
    );

    // And the claim it took is exclusive: a second interaction still waits.
    assert!(
        state
            .claim_interaction_turn(
                sid,
                "modal submit".to_string(),
                "[System: modal]".to_string()
            )
            .is_none(),
        "the running interaction holds the slot until it finishes"
    );
    drop(guard);
}

/// The other direction: an interaction on a session that is NOT mid-turn runs
/// immediately, and it holds the slot for as long as the turn lives.
#[test]
fn a_free_slot_lets_the_interaction_run() {
    let state = Arc::new(DiscordState::new());
    let sid = uuid::Uuid::new_v4();

    let guard = state
        .claim_interaction_turn(
            sid,
            "submitted the deploy form".to_string(),
            "[System: adolfo submitted the \"deploy\" form]".to_string(),
        )
        .expect("an idle session admits the interaction");

    assert!(
        state.drain_followups(sid).is_none(),
        "an admitted interaction has nothing to queue"
    );
    assert!(
        state.try_begin_turn(sid).is_none(),
        "the admitted interaction holds the slot, so a typed message waits too"
    );

    // A different session is never blocked by this one.
    let other = uuid::Uuid::new_v4();
    assert!(
        state
            .claim_interaction_turn(
                other,
                "picked x".to_string(),
                "[System: picked x]".to_string()
            )
            .is_some(),
        "sessions are independent"
    );

    drop(guard);
    assert!(
        state.try_begin_turn(sid).is_some(),
        "dropping the guard frees the slot for the next turn"
    );
}

/// Wiring pin: all three entry points go through the claim. A serenity
/// `Context` cannot be built in a unit test, so the routing itself is checked
/// at the source: the bare route (select menu, modal submit) and the reaction
/// handler must both claim, and both dispatch sites must hand over the state
/// the claim lives on. Without this, the seam above could pass while a caller
/// quietly went back to running unclaimed.
#[test]
fn every_interaction_entry_point_claims_the_slot() {
    let interactions = include_str!("../channels/discord/interactions.rs");
    let start = interactions
        .find("pub(crate) async fn route_interaction_turn(")
        .expect("the bare interaction route is present");
    let body = &interactions[start..];
    let end = body
        .find("/// Run a tapped follow-up suggestion")
        .expect("the tap helper terminates the bare route");
    let bare_route = &body[..end];
    assert!(
        bare_route.contains("claim_interaction_turn("),
        "the select menu and modal submit route must claim the session turn slot (#2191)"
    );
    assert!(
        bare_route.contains("discord_state: Arc<super::DiscordState>,"),
        "the bare route needs the state the slot lives on"
    );

    let reactions = include_str!("../channels/discord/reactions.rs");
    assert!(
        reactions.contains("claim_interaction_turn("),
        "a reaction must not fork a second loop onto a busy session (#2191)"
    );

    let agent = include_str!("../channels/discord/agent.rs");
    assert_eq!(
        agent
            .matches("dstate.clone(),\n                    )")
            .count(),
        2,
        "both synthetic callers (modal form, select menu) hand the state over; \
         one short means a dispatch went back to the unclaimed shape"
    );
}
