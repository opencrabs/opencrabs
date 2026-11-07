//! A stop ends the turn instead of asking the service to notice (#2657).
//!
//! The GUI used to hand its cancel token to `send_message_with_tools_and_mode`
//! and nothing else. The service reads that token at the next iteration
//! boundary, so an Esc during a long stream logged `turn cancelled by Esc` and
//! then reported `turn done` a minute or two later. The window has no headless
//! harness, so these checks read the handler source: both runners must race
//! their future against the token, and Esc must reach the focused pane before
//! the window's own turn gate, because a split leaf runs its turn with only
//! `pane.busy` set. The behaviour of `race_turn` itself is pinned by the bin
//! suite (`gui_cancel_race_*`); the wiring is the contract here.

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const AGENT_BRIDGE: &str = include_str!("../gui/agent_bridge.rs");

/// The Esc branch, cut at the line where a stopped turn reports itself.
fn esc_stop_block() -> &'static str {
    let start = GUI_MAIN
        .find("if ev.keystroke.key == \"escape\" {")
        .expect("on_key has an escape branch");
    let tail = GUI_MAIN
        .get(start..)
        .expect("the escape branch starts on a char boundary");
    tail.split_once("logging::info(\"turn cancelled by Esc\")")
        .expect("the Esc stop logs its own line")
        .0
}

/// `stop_turn`, cut where its body ends and the next handler's doc begins.
fn stop_turn_body() -> &'static str {
    let start = GUI_MAIN
        .find("fn stop_turn(")
        .expect("the window handles /stop");
    GUI_MAIN
        .get(start..)
        .expect("the handler starts on a char boundary")
        .split("/// `/models` with no argument")
        .next()
        .expect("stop_turn ends before the picker handler")
}

/// The arm that settles a turn whose future was dropped.
fn cancelled_arm() -> &'static str {
    let start = GUI_MAIN
        .find("agent_bridge::TurnEvent::Cancelled { session } => {")
        .expect("the window settles a dropped turn");
    GUI_MAIN
        .get(start..)
        .expect("the settle arm starts on a char boundary")
        .split("agent_bridge::TurnEvent::TurnReverted")
        .next()
        .expect("the settle arm ends before the revert arm")
}

#[test]
fn gui_cancel_esc_reaches_the_pane_before_the_window_gate() {
    let branch = esc_stop_block();
    let pane_stop = branch
        .find("cancel_focused_pane_turn(\"Esc\"")
        .expect("Esc stops the focused pane's turn");
    let gate = branch
        .find("if !self.busy {")
        .expect("the window still gates its own turn");
    assert!(
        pane_stop < gate,
        "the pane stop sits before the gate: a split leaf sets pane.busy with \
         self.busy clear, so a gate-first order hands the keyboard back and \
         leaves that turn streaming"
    );
}

#[test]
fn gui_cancel_esc_still_releases_the_window_turn() {
    let branch = esc_stop_block();
    assert!(
        branch.contains("cancel.cancel()"),
        "the window's own turn still drops its future"
    );
    assert!(
        branch.contains("self.busy = false"),
        "and the window still reports itself idle"
    );
}

#[test]
fn gui_cancel_stop_and_esc_share_one_pane_stop() {
    assert!(
        stop_turn_body().contains("cancel_focused_pane_turn(\"/stop\""),
        "/stop reaches the focused pane through the same helper Esc uses"
    );
}

#[test]
fn gui_cancel_the_turn_races_its_stop_token() {
    let shipped = AGENT_BRIDGE
        .split("#[cfg(test)]")
        .next()
        .expect("the bridge ships its source");
    assert!(
        shipped.contains("pub async fn race_turn"),
        "one helper decides what a stopped turn does"
    );
    assert!(
        shipped.contains("_ = cancel.cancelled() => None"),
        "the token wins the race and the turn is dropped where it stands"
    );
    assert_eq!(
        shipped.matches("race_turn(turn, &cancel)").count(),
        2,
        "both runners race: the window's turn and the resumed one"
    );
}

#[test]
fn gui_cancel_the_settle_keeps_the_card_on_screen() {
    let arm = cancelled_arm();
    assert!(
        arm.contains("self.rail.clear(session)"),
        "a dropped turn stops the rail's spinner"
    );
    assert!(
        arm.contains("self.cancel = None"),
        "and the window lets go of the token"
    );
    assert!(
        !arm.contains("reload = "),
        "the settle must not re-read the ledger: a dropped future cannot promise \
         the service stored what it streamed, and a reload now would wipe the \
         text the user can still see"
    );
}
