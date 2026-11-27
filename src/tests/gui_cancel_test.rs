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

const AGENT_BRIDGE: &str = include_str!("../gui/agent_bridge.rs");
/// The handler, the key reader and the settle arm moved into modules of their
/// own (#2822), so each region below reads the file it lives in.
const DASHBOARD_KEYS: &str = include_str!("../gui/dashboard_keys.rs");
const DASHBOARD_COMMANDS: &str = include_str!("../gui/dashboard_commands.rs");
const DASHBOARD_TURNS: &str = include_str!("../gui/dashboard_turns.rs");
const DASHBOARD_NAV: &str = include_str!("../gui/dashboard_nav.rs");
const GUI_MAIN: &str = concat!(
    include_str!("../gui/main.rs"),
    include_str!("../gui/app.rs"),
    include_str!("../gui/dashboard_render.rs"),
    include_str!("../gui/dashboard_state.rs"),
);

/// The Esc branch, cut at the line where a stopped turn reports itself.
fn esc_stop_block() -> &'static str {
    let start = DASHBOARD_KEYS
        .find("if ev.keystroke.key == \"escape\" {")
        .expect("on_key has an escape branch");
    let tail = DASHBOARD_KEYS
        .get(start..)
        .expect("the escape branch starts on a char boundary");
    tail.split_once("logging::info(\"turn cancelled by Esc\")")
        .expect("the Esc stop logs its own line")
        .0
}

/// `stop_turn`, from its signature to the end of the file. It is the last
/// handler in its module (#2822), so the slice ends there rather than at the
/// next handler's doc comment, which now lives in another module.
fn stop_turn_body() -> &'static str {
    let start = DASHBOARD_COMMANDS
        .find("fn stop_turn(")
        .expect("the window handles /stop");
    DASHBOARD_COMMANDS
        .get(start..)
        .expect("the handler starts on a char boundary")
}

/// The arm that settles a turn whose future was dropped.
fn cancelled_arm() -> &'static str {
    let start = DASHBOARD_TURNS
        .find("agent_bridge::TurnEvent::Cancelled { session } => {")
        .expect("the window settles a dropped turn");
    DASHBOARD_TURNS
        .get(start..)
        .expect("the settle arm starts on a char boundary")
        .split("agent_bridge::TurnEvent::TurnReverted")
        .next()
        .expect("the settle arm ends before the revert arm")
}

/// `ctrl_c_press`, from its signature to the repaint that closes it.
fn ctrl_c_press_body() -> &'static str {
    let start = DASHBOARD_NAV
        .find("fn ctrl_c_press(")
        .expect("the window has one Ctrl+C handler");
    DASHBOARD_NAV
        .get(start..)
        .expect("the handler starts on a char boundary")
        // The handler ends on the repaint it asks for (#3102): the hint row is
        // decided from live state now, so the notify is all the field needs.
        .split("cx.notify();")
        .next()
        .expect("the handler ends where the repaint starts")
}

/// The Ctrl+C interceptor's judgement of the press, up to the point it
/// answers it.
fn ctrl_c_interceptor_block() -> &'static str {
    let start = GUI_MAIN
        .find("let falls_to_copy")
        .expect("the interceptor judges the press before answering it");
    GUI_MAIN
        .get(start..)
        .expect("the block starts on a char boundary")
        .split("cx.stop_propagation()")
        .next()
        .expect("the block ends where the press is answered")
}

#[test]
fn gui_cancel_esc_reaches_the_pane_before_the_window_gate() {
    let branch = esc_stop_block();
    let pane_stop = branch
        .find("cancel_focused_pane_turn(\"Esc\"")
        .expect("Esc stops the focused pane's turn");
    let gate = branch
        .find("if !self.turn.busy || self.split_focus().is_some() {")
        .expect("the window still gates its own turn, now on the caret too");
    assert!(
        pane_stop < gate,
        "the pane stop sits before the gate: a split leaf sets pane.busy with \
         self.turn.busy clear, so a gate-first order hands the keyboard back and \
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
        branch.contains("self.turn.busy = false"),
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
        arm.contains("self.turn.cancel = None"),
        "and the window lets go of the token"
    );
    assert!(
        !arm.contains("reload = "),
        "the settle must not re-read the ledger: a dropped future cannot promise \
         the service stored what it streamed, and a reload now would wipe the \
         text the user can still see"
    );
}

#[test]
fn gui_cancel_esc_gate_asks_where_the_caret_is() {
    let branch = esc_stop_block();
    assert!(
        branch.contains("!self.turn.busy || self.split_focus().is_some()"),
        "the window's own turn is the chat leaf's, so a caret on a pane takes \
         the idle branch instead of cancelling work it cannot see (#2885)"
    );
}

#[test]
fn gui_cancel_esc_returns_the_question_to_the_chat_leaf() {
    let branch = esc_stop_block();
    let owner = branch
        .find("if let Some(sid) = self.chat_session")
        .expect("the handed-back question is gated on the chat leaf's session");
    let field = branch
        .find("self.composer.editor.clone()")
        .expect("the question goes back to the field that owns chat_session");
    assert!(
        owner < field,
        "the field is the chat leaf's own, chosen inside the branch that names \
         its session (#2885)"
    );
    assert!(
        !branch.contains("let composer = self.composer_entity(cx);"),
        "composer_entity resolves the caret, so restoring through it handed one \
         conversation's question to another pane's field, one Enter from being \
         sent to the wrong session (#2885)"
    );
}

#[test]
fn gui_cancel_ctrl_c_reads_the_field_the_caret_is_on() {
    let body = ctrl_c_press_body();
    assert!(
        body.contains("let composer = self.composer_entity(cx);"),
        "the field is the focused composer, not the window's (#2885)"
    );
    assert!(
        !body.contains("self.composer.editor.read(cx).is_empty()"),
        "reading the window's field judged a draft the user was not typing in \
         and then cleared the one they were (#2885)"
    );
    assert!(
        !body.contains("self.pane_focus"),
        "the pane is resolved through the tree, not off an id that a closed \
         pane leaves stale (#2620)"
    );
}

#[test]
fn gui_cancel_ctrl_c_still_quits_the_window() {
    let body = ctrl_c_press_body();
    assert!(
        body.contains("cx.quit();"),
        "two presses still close the window, whatever the split looks like"
    );
    let quit = body
        .find("CtrlCAction::Quit =>")
        .expect("the ladder keeps its last step");
    let arm = body.get(quit..).expect("the arm starts on a char boundary");
    let gate = arm
        .find("if self.split_focus().is_none() {")
        .expect("the window's own turn is gated on the caret (#2885)");
    let take = arm
        .find("self.turn.cancel.take()")
        .expect("the window still drops its own token");
    assert!(
        gate < take,
        "the gate decides the take, so a caret on a pane leaves the chat leaf's \
         turn running"
    );
}

#[test]
fn gui_cancel_interceptor_asks_the_focused_field_for_a_selection() {
    let block = ctrl_c_interceptor_block();
    assert!(
        block.contains("let composer = this.composer_entity(cx);"),
        "a split draws one editor per leaf, so the selection being copied lives \
         in the focused composer (#2885)"
    );
    assert!(
        !block.contains("this.composer.editor.read(cx)"),
        "the window's editor answers for the chat leaf alone, so a selection in \
         a pane never copied and the quit ladder ran instead (#2885)"
    );
}

#[test]
fn gui_cancel_stop_leaves_the_chat_leaves_turn_alone_from_a_pane() {
    let body = stop_turn_body();
    let gate = body
        .find("let cancel = if self.split_focus().is_some() {")
        .expect("the window's own turn is gated on the caret (#2885)");
    let take = body
        .find("self.turn.cancel.take()")
        .expect("the window still takes its own token");
    assert!(
        gate < take,
        "the gate decides the take, so a caret on a pane never stops the chat \
         leaf's turn"
    );
    assert!(
        !body.contains("[self.focused_pane(), self.chat_session]"),
        "the stop clears the offer of the session it can reach, not every \
         session in the split (#2885)"
    );
}

/// #3216: a stopped exchange must survive the send that follows it.
///
/// The stop used to reload nothing, so the cancelled turn stayed marked live
/// and `persisted_turn_is_hidden` kept skipping its persisted copy. The next
/// send's `live.begin` then repointed the only copy of the exchange on screen
/// at the new question, and the cancelled question, its answer and its stop
/// marker vanished until a session switch rebuilt the rows from the ledger.
/// Nothing was deleted; the render just had no owner for them.
#[test]
fn gui_cancel_arms_the_settle_handshake_so_the_exchange_reloads() {
    let arm = cancelled_arm();
    assert!(
        arm.contains("self.turn.settle.begin_after_stop(self.chat_state.row_total());"),
        "the cancelled arm must arm the settle handshake, or the exchange stays \
         hidden until a session switch (#3216)"
    );
    assert!(
        arm.contains("self.reload_transcript(session, cx);"),
        "the cancelled arm must read the ledger on a budget, not leave the \
         exchange undrawn (#3216)"
    );
    assert!(
        !arm.contains("No reload on purpose"),
        "the old no-reload comment described the bug, not the behaviour (#3216)"
    );
}

/// The count the handshake arms for: the stop writes three rows past what the
/// window last read (question, the turn's own answer row, the stop marker),
/// so a read short of that must keep waiting instead of drawing the exchange
/// without its marker. The GUI is its own bin (`required-features` in
/// Cargo.toml), so the lib tests cannot link `Settle` and the contract is
/// pinned against the source, the way every other cancel pin here is.
#[test]
fn gui_cancel_settle_waits_for_all_three_rows() {
    const CHAT_LEAF: &str = include_str!("../gui/chat_leaf.rs");
    const TURN_SWAP: &str = include_str!("../gui/turn_swap.rs");

    let arm = CHAT_LEAF
        .split_once("pub(crate) fn begin_after_stop(")
        .expect("the stop arms its own handshake")
        .1
        .split_once("/// What each settled turn cost")
        .expect("the method ends where the metrics struct begins");
    assert!(
        arm.0.contains("self.expect = Some(row_total + 3);"),
        "the stop expects question + answer row + stop marker, not just one \
         new row, or the exchange draws before its marker is written (#3216)"
    );

    let landed = TURN_SWAP
        .split_once("pub(crate) fn settle_landed(")
        .expect("the shared landed predicate")
        .1;
    assert!(
        landed.contains(">="),
        "landed is a lower bound on the ledger, the same rule the Done path \
         uses, so the two paths cannot disagree (#3216)"
    );
}
