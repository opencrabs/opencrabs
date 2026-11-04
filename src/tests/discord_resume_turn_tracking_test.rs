//! Source pins for the #1990 Discord turn tracking: the orderings and the
//! wiring that the behavioral tests cannot reach because they need a live
//! bot (HTTP, serenity context). These are the load-bearing sequencing
//! promises of the fix, broken deliberately = red here.

const TRACKED_TURN_SRC: &str = include_str!("../channels/discord/tracked_turn.rs");
const HANDLER_SRC: &str = include_str!("../channels/discord/handler.rs");
const RESUME_SRC: &str = include_str!("../channels/discord/resume.rs");
const MANAGER_SRC: &str = include_str!("../channels/manager.rs");
const UI_SRC: &str = include_str!("../cli/ui.rs");

#[test]
fn runner_claims_the_slot_before_any_token_can_cancel() {
    // The store no longer cancels (it only overwrites, #652's Discord twin),
    // but the ordering is still load-bearing: a resume turn that stored
    // before losing the claim would take over the live turn's cancel slot, so
    // /stop would reach the newcomer and the running turn would keep going.
    // The needle is the code form, `.store_cancel_token(`: the module docs
    // name the call in prose at the top of the file, and a bare-word find
    // would rank the sentence above the claim instead of the statement.
    let claim = TRACKED_TURN_SRC
        .find("dstate.try_begin_turn(session_id)")
        .expect("runner claims the turn slot");
    let store = TRACKED_TURN_SRC
        .find(".store_cancel_token(session_id")
        .expect("runner stores a cancel token");
    assert!(
        claim < store,
        "the slot claim must precede store_cancel_token (#1990 landmine)"
    );
}

#[test]
fn runner_flush_releases_the_slot_before_draining() {
    // The flushed follow-up must be able to re-claim the slot: dropping
    // the guard first is what makes the flush-vs-inbound race safe.
    let drop_guard = TRACKED_TURN_SRC
        .find("drop(_turn_guard);")
        .expect("runner releases the slot explicitly before the flush");
    let drain = TRACKED_TURN_SRC
        .find("dstate.drain_followups(session_id)")
        .expect("runner drains leftover follow-ups at turn end");
    assert!(
        drop_guard < drain,
        "the guard must drop before the end-of-turn flush drains (#1990)"
    );
    // ...and the flushed turn rides the same runner, not a bare send.
    assert!(
        TRACKED_TURN_SRC.contains("ResumeDispatch::Display"),
        "the flush spawns a tracked Display turn"
    );
}

#[test]
fn ingress_handler_claims_queues_and_acks_instead_of_forking() {
    // Inbound messages during a live turn: queue + 👀 ack + return. Never
    // store_cancel_token on that path.
    assert!(
        HANDLER_SRC.contains("discord_state.try_begin_turn(session_id)"),
        "ingress claims the session slot (#1990)"
    );
    assert!(
        HANDLER_SRC.contains("mid-turn follow-up queued for session"),
        "the queued path is logged for the issue's repro"
    );
    assert!(
        HANDLER_SRC.contains("ReactionType::Unicode(\"👀\".to_string())"),
        "a queued follow-up gets the 👀 ack like Telegram's"
    );
    // End of the turn: flush what the loop never injected.
    assert!(
        HANDLER_SRC.contains("drop(_turn_guard);")
            && HANDLER_SRC.contains("discord_state.drain_followups(session_id)"),
        "the ingress turn flushes its queue at end (#1990)"
    );
}

#[test]
fn background_completion_rides_the_runner_and_queues_on_loss() {
    // The bg-resume producer no longer runs an invisible all-None turn: it
    // calls the tracked runner, and a lost claim hands the WHOLE message
    // (origin + bg_meta) to the queue, not just its text.
    assert!(
        RESUME_SRC.contains("run_tracked_resume_turn"),
        "background completions run visibly through the runner (#1990)"
    );
    let dispatch = RESUME_SRC
        .find("ResumeDispatch::Push")
        .expect("the completion push is the runner's dispatch");
    assert!(
        RESUME_SRC[..dispatch].contains("await"),
        "the runner call awaits the turn"
    );
    assert!(
        RESUME_SRC.contains("state.enqueue_followup(session_id, msg)"),
        "a Queued outcome re-queues the full message"
    );
}

#[test]
fn tool_loop_injection_callback_is_wired_for_discord() {
    // Mid-round injection only works if the service holds the drain: the
    // queue callback must reach create_agent_service_full's first slot.
    let wiring = MANAGER_SRC
        .find("followup_queue_callback")
        .expect("manager wires the follow-up queue callback");
    assert!(
        MANAGER_SRC[wiring..].contains("create_agent_service_full(Some(followup_cb)"),
        "the callback is passed as the queue slot, not the enqueue slot"
    );
}

#[test]
fn boot_recovery_has_a_discord_branch_promising_the_hold() {
    // ui.rs: discord sessions ride the runner with Recovery dispatch, and
    // the replay prompt tells the model follow-ups HOLD (#1990 item 4).
    assert!(
        UI_SRC.contains("crate::channels::discord::tracked_turn::ResumeDispatch::Recovery"),
        "boot recovery dispatches a tracked Recovery turn for discord"
    );
    assert!(
        UI_SRC.contains("it is held for"),
        "the recovery prompt promises the mid-turn hold (#1990)"
    );
    // Parked semantics keep the ledger honest when the slot is already
    // taken at boot.
    assert!(
        UI_SRC.contains("ResumeTurnOutcome::Queued") && UI_SRC.contains("record_parked()"),
        "a boot replay that lost the claim parks in the ledger (#1242)"
    );
}
