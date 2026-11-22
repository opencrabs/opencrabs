//! #3142: a plan that completes in the desktop app shows its checklist in the
//! conversation that finished it.
//!
//! Reported: the completed plan card never appeared when a plan finished. It
//! showed for older turns, because a reload turns the `plan_completed` row into
//! a card, and it never showed for the turn that just finished, because #3032
//! made that turn join the transcript from the window's live state. Live state
//! holds tool calls, narration and the answer. It holds no database rows, and
//! the checklist is one.
//!
//! The plan file archives at settle, so the live strip goes away at the same
//! moment. Without the row the plan left the screen entirely.
//!
//! GPUI has no headless render harness here, so these are source pins, the same
//! shape as `gui_turn_append_3032_test.rs`: they hold the route the record
//! takes, not the paint.

const BRIDGE: &str = include_str!("../gui/agent_bridge.rs");
const TURNS: &str = include_str!("../gui/dashboard_turns.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

/// The settle event carries the row. Without the field the window has no way
/// to learn that this turn finished a plan.
#[test]
fn the_settle_event_carries_the_completed_plan_row() {
    let done = BRIDGE
        .split("pub enum TurnEvent {")
        .nth(1)
        .expect("the event enum must exist")
        .split("/// The turn failed; carries the user-facing message")
        .next()
        .expect("the Done arm must end before the Failed arm");
    assert!(
        done.contains("completed_plan: Option<Box<opencrabs::db::models::Message>>,"),
        "Done must carry the checklist row the tool loop handed back"
    );
}

/// Both places that report a finished turn fill the field from the response.
/// A resumed turn re-runs a parked prompt, so it settles through the second
/// site, and a field filled at one site only would leave that turn empty.
#[test]
fn both_settle_sites_fill_the_field_from_the_response() {
    let filled = BRIDGE
        .matches("completed_plan: response.completed_plan.map(Box::new),")
        .count();
    assert_eq!(
        filled, 2,
        "the sent turn and the resumed turn both report their own checklist"
    );
}

/// The append puts the block on the turn. The card is built by one function,
/// shared with the reload path, so the two paths cannot drift apart.
#[test]
fn the_append_pushes_the_card_onto_the_finished_turn() {
    assert!(
        TURNS.contains(".and_then(transcript::completed_plan_block)"),
        "the Done arm must decode the row it was handed"
    );
    assert!(
        TURNS.contains("turn.blocks.push(block);"),
        "the decoded card must join the turn before it goes on the list"
    );
    let append = TURNS
        .split("match transcript::turn_from_live(&self.turn.live, None) {")
        .nth(1)
        .expect("the append must build from the card that painted the turn");
    let push = append
        .find("turn.blocks.push(block);")
        .expect("the card must be pushed");
    let listed = append
        .find("self.chat_state.transcript.turns.push(turn);")
        .expect("the turn must go on the list");
    assert!(
        push < listed,
        "the card has to be on the turn before the turn is listed"
    );
}

/// One decoder, two callers. The reload path reads the same row, so a second
/// copy of the decode is how the two paths would come to disagree.
#[test]
fn the_reload_path_and_the_append_path_share_one_decoder() {
    let decoder = TRANSCRIPT
        .split("pub fn completed_plan_block(message: &Message) -> Option<Block> {")
        .nth(1)
        .expect("the decoder must exist");
    let decoder = decoder
        .split("\n}\n")
        .next()
        .expect("the decoder body must end");
    assert!(
        decoder.contains("if message.role != PLAN_COMPLETED_ROLE {"),
        "the decoder must refuse a row that is not a checklist"
    );
    assert!(
        decoder.contains("CompletedPlanPayload::from_message(message)?"),
        "a malformed row must decode to nothing rather than paint raw JSON"
    );

    let rows = TRANSCRIPT
        .split("fn row_blocks(m: &Message, forward: Option<ForwardMark>) -> Vec<Block> {")
        .nth(1)
        .expect("the row expander must exist");
    let role_arm = rows
        .split("if m.role == PLAN_COMPLETED_ROLE {")
        .nth(1)
        .expect("the expander must have a checklist arm")
        .split("return blocks;")
        .next()
        .expect("the arm must return");
    assert!(
        role_arm.contains("completed_plan_block(m)"),
        "the reload must decode through the shared function"
    );
    assert!(
        !role_arm.contains("CompletedPlanView::new"),
        "a second decode in the expander is how the two paths drift"
    );
}
