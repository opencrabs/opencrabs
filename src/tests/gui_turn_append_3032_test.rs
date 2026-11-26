//! #3190: a finished turn is drawn once, from the rows.
//!
//! #3032 answered every `Done` by appending a turn built from the card that
//! was painting the exchange, to save a reload. The ledger already holds that
//! same exchange as rows, so the window ended up with two copies of one
//! answer: the appended turn and the persisted twin. The docked copy only left
//! when a session switch reloaded the window, which is exactly what the owner
//! reported.
//!
//! The completion now reads the rows. `reload_transcript` keeps the card up
//! until the settle test says the write landed, then takes it down with its
//! metrics attached, so the answer reaches the screen once.
//!
//! These are source pins, the same shape as the layout-boot pins (#2689): the
//! wiring lives in the gpui dashboard, which has no headless harness here, so
//! the pins hold the route rather than the paint.

const TURNS: &str = include_str!("../gui/dashboard_turns.rs");
const MESSAGES: &str = include_str!("../gui/dashboard_messages.rs");

/// The `Done` arm asks for the rows back. Nothing builds a second turn from
/// the live card, so the same answer cannot be listed beside its own row.
#[test]
fn a_finished_turn_reloads_the_rows() {
    let done = TURNS
        .split("agent_bridge::TurnEvent::Done {")
        .nth(1)
        .expect("the Done arm must exist")
        .split("agent_bridge::TurnEvent::Failed {")
        .next()
        .expect("the Done arm must end before the Failed arm");
    assert!(
        done.contains("reload = Some(sid);"),
        "the completion is rendered from the rows"
    );
    assert!(
        !TURNS.contains("turn_from_live"),
        "a turn built from the card is the copy that painted twice"
    );
    assert!(
        !TURNS.contains("turn appended:"),
        "the Done log line must say what it does now"
    );
}

/// The reload is what takes the card down, and only once the settle test says
/// the turn's rows are there. Dropping the card earlier would blank the answer
/// between the last chunk and the write.
#[test]
fn the_card_waits_for_the_rows_it_reloads() {
    let reload = MESSAGES
        .split("pub(crate) fn reload_transcript(")
        .nth(1)
        .expect("reload_transcript must exist");
    let landed = reload
        .find("let landed = settle_landed(expect, loaded.total);")
        .expect("the reload must test the settle expectation");
    let clear = reload
        .find("this.turn.live.clear();")
        .expect("the reload must take the card down");
    assert!(
        landed < clear,
        "the card is only cleared after the rows prove they landed"
    );
    assert!(
        reload.contains("this.attach_pending_metrics(sid);"),
        "the numbers recorded at Done belong to the turn the rows carry"
    );
}
