//! Regression tests for the Discord follow-up suggestion tap (#1852).
//!
//! A message typed by a human runs the tool-loop display path: the flow
//! group opens immediately with its 🕒 clock, tool steps and narration fold
//! into it, approvals route to the buttons, and chained suggestions render
//! again. The tap used to ride the bare `send_message_with_display`
//! single-completion send — zero tools, zero progress events, zero
//! approvals — so the channel sat silent from the `▶️` echo until a plain
//! reply landed, and any action the suggestion asked for simply never
//! happened. These guards hold the wiring structurally, the same way
//! `slack_followup_tap_tool_loop_test.rs` guards #1838.

/// The tap branch must hand the turn to the tool-loop display path and never
/// call the bare interaction route again.
#[test]
fn followup_tap_branch_delegates_to_the_display_path() {
    let agent = include_str!("../channels/discord/agent.rs");
    let start = agent
        .find("FOLLOWUP_PREFIX)")
        .expect("follow-up tap branch present");
    let rest = &agent[start..];
    let end = rest
        .find("// Select menu pick (#382)")
        .expect("select-menu branch terminates the tap branch");
    let branch = &rest[..end];
    assert!(
        branch.contains("route_followup_turn("),
        "#1852: a tapped suggestion must run through route_followup_turn"
    );
    assert!(
        !branch.contains("route_interaction_turn("),
        "the tap branch must not ride the bare interaction route"
    );
    assert!(
        !branch.contains(".send_message("),
        "the tap branch must not ride the bare single-completion send"
    );
}

/// #1990's rule reached every turn entry point: `3f0086081` added the claim
/// to `handle_message` and the tracked runner but skipped the follow-up tap,
/// so a tap on a live turn forked a second concurrent tool loop on the same
/// session. Each entry point must claim the slot before it touches the cancel
/// token, and the tap must queue its pick instead of dropping it.
#[test]
fn every_turn_entry_point_claims_the_slot_before_storing_a_token() {
    let entry_points = [
        ("handler.rs", include_str!("../channels/discord/handler.rs")),
        (
            "tracked_turn.rs",
            include_str!("../channels/discord/tracked_turn.rs"),
        ),
        (
            "interactions.rs",
            include_str!("../channels/discord/interactions.rs"),
        ),
    ];
    for (name, source) in entry_points {
        let claim = source
            .find("try_begin_turn(")
            .unwrap_or_else(|| panic!("{name} must claim the session turn slot (#1990)"));
        let store = source
            .find("store_cancel_token(")
            .unwrap_or_else(|| panic!("{name} stores a cancel token"));
        assert!(
            claim < store,
            "{name}: the slot claim must precede store_cancel_token, otherwise a \
             turn that lost the race takes over the live turn's cancel token"
        );
    }

    let interactions = include_str!("../channels/discord/interactions.rs");
    let start = interactions
        .find("pub(crate) async fn route_followup_turn(")
        .expect("tap-turn helper present");
    let body = &interactions[start..];
    assert!(
        body.contains("enqueue_followup("),
        "a tap that loses the claim must queue the pick for the live turn, \
         never drop the user's choice"
    );
    assert!(
        body.contains("crate::brain::agent::PushOrigin::Ingress"),
        "the queued tap is ingress-born, same origin a typed follow-up carries"
    );
}

/// The tap turn itself must ride the tool-loop display path with both
/// callbacks wired, and the live status must be born BEFORE the turn is
/// dispatched — that ordering is the whole of #1852.
#[test]
fn tap_turn_opens_live_status_before_dispatching() {
    let interactions = include_str!("../channels/discord/interactions.rs");
    let start = interactions
        .find("pub(crate) async fn route_followup_turn(")
        .expect("tap-turn helper present");
    let body = &interactions[start..];

    assert!(
        body.contains("send_message_with_tools_and_display("),
        "the tap turn must call the tool-loop display entry point"
    );
    assert!(
        !body.contains(".send_message_with_display("),
        "the tap turn must never regress to the bare single-call path"
    );
    assert!(
        !body.contains(".send_message("),
        "the tap turn must never regress to the bare send"
    );
    assert!(
        body.contains("Some(progress_cb)"),
        "progress events must reach the channel callback"
    );
    assert!(
        body.contains("Some(approval_cb)"),
        "tool approvals must route to the Discord buttons"
    );

    // #1852 guarantee: the status is visible from second zero, so the group
    // shell and its ticker spawn before the agent turn starts.
    let dispatch = body
        .find("send_message_with_tools_and_display(")
        .expect("turn dispatch present");
    let shell = body
        .find("let turn_shell")
        .expect("the step group must open at turn start");
    assert!(
        shell < dispatch,
        "the turn-start group shell must be created before the tap turn is dispatched"
    );
    let ticker = body
        .find("spawn_flow_ticker(")
        .expect("the flow ticker must run for tap turns");
    assert!(
        ticker < dispatch,
        "spawn_flow_ticker must run before the tap turn is dispatched"
    );
}

/// The tap path must handle the chained-suggestion event: buttons minted by
/// a tapped turn would otherwise die silently, recreating the bug this fix
/// removes one level up.
#[test]
fn tap_path_renders_chained_suggestions() {
    let interactions = include_str!("../channels/discord/interactions.rs");
    let start = interactions
        .find("pub(crate) async fn route_followup_turn(")
        .expect("tap-turn helper present");
    let body = &interactions[start..];
    assert!(
        body.contains("SuggestedOptions"),
        "the tap progress callback must observe suggestion events"
    );
    assert!(
        body.contains("render_suggestions("),
        "suggestion events must post clickable buttons again"
    );
}

/// The other interaction routes stay on their bare contract on purpose:
/// modal forms and select-menu picks are synthetic steering prompts, not
/// user-intent turns (#1852 scoped the routing change to the tap only).
/// #1850 added the second user-intent caller: a picked slash command rebuilds
/// its invocation and joins the tap on the display path, which is why the bare
/// count is still exactly two.
#[test]
fn bare_route_still_serves_the_synthetic_steering_callers() {
    let interactions = include_str!("../channels/discord/interactions.rs");
    assert!(
        interactions.contains("pub(crate) async fn route_interaction_turn("),
        "route_interaction_turn must survive for the synthetic callers"
    );
    assert!(
        interactions.contains(".send_message_with_display("),
        "route_interaction_turn keeps its documented single-call contract"
    );
    // #2011: the bare route answers on a channel whose governor exists, so its
    // signature carries the handle. The old shape had no way to reach it, which
    // is why #1910 left this one write ungated.
    assert!(
        interactions.contains("governor: &super::governor::Governor,"),
        "route_interaction_turn must take the governor that gates its reply"
    );
    let agent = include_str!("../channels/discord/agent.rs");
    let synthetic_calls = agent.matches("route_interaction_turn(").count();
    assert_eq!(
        synthetic_calls, 2,
        "exactly the modal-form and select-menu branches dispatch through \
         the bare route; a third means the tap regressed"
    );
    assert_eq!(
        agent.matches("route_followup_turn(").count(),
        2,
        "the follow-up tap and the slash-command arm (#1850) are the two \
         user-intent callers of the tool-loop display path; anything routed \
         off these is single-completion code running a request to do work"
    );
}

/// #2011: this reply loop was the last raw write in the module, so a
/// `retry_after` here was logged as a generic delivery failure and the channel
/// stayed unparked while the next tick kept fighting the window. Delivery must
/// ride `governor::say` on the content surface, and both synthetic callers must
/// hand over the governor they hold.
#[test]
fn bare_route_delivers_its_reply_through_the_governor() {
    let interactions = include_str!("../channels/discord/interactions.rs");
    let start = interactions
        .find("pub(crate) async fn route_interaction_turn(")
        .expect("bare route present");
    let body = &interactions[start..];
    let end = body
        .find("/// Run a tapped follow-up suggestion")
        .expect("the tap helper terminates the bare route");
    let body = &body[..end];
    assert!(
        !body.contains("channel.say(&ctx.http"),
        "a raw say bypasses the budget and learns nothing from a 429 (#2011)"
    );
    assert!(
        body.contains("super::governor::say("),
        "the bare route's reply must be gated (#2011)"
    );
    assert!(
        body.contains("super::governor::Surface::Send,"),
        "an answer is content: chrome's drop-on-refusal rule must not apply"
    );

    let agent = include_str!("../channels/discord/agent.rs");
    let dispatch = "route_interaction_turn(\n                        &ctx2,\n                        &dstate.governor,";
    assert_eq!(
        agent.matches(dispatch).count(),
        2,
        "both synthetic callers pass the governor into the bare route; one short \
         means a dispatch went back to the ungated shape"
    );
}
