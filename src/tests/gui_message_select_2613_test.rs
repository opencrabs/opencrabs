//! The message menu's contract and what the transcript refuses to select
//! (#2613).
//!
//! The window has no headless harness, so the wiring is read from source. The
//! parts that are pure (the action list) are called for real: the type makes a
//! dead entry unreachable, and this test pins the labels it paints.

use crate::tests::gui_dashboard_source::DASHBOARD;
use crate::ui_core::message_select::MessageAction;

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const MAIN: &str = include_str!("../gui/main.rs");
const PANE_VIEW: &str = include_str!("../gui/pane_view.rs");

/// The menu is the set of actions that can actually be carried out, in the
/// order Telegram shows them. A label with no path behind it never ships.
#[test]
fn the_menu_offers_only_the_actions_this_version_wires() {
    assert_eq!(
        MessageAction::ALL.map(|action| action.label()),
        ["Copy as Markdown", "React", "Reply", "Forward", "Select"],
        "the menu is exactly the wired actions, in menu order"
    );

    // Out of scope on purpose (#2613): `messages` has no `reply_to` and no pin
    // column, translation needs a provider, delete needs its own confirmation
    // path, and a message link does not apply to a local transcript.
    for absent in ["Pin", "Translate", "Delete", "Copy Message Link", "Report"] {
        assert!(
            !DASHBOARD.contains(&format!("\"{absent}\".to_string()")),
            "{absent} has no path yet, so it is not offered as a dead entry"
        );
    }
}

/// Reply and Forward read the row the press carried, on every row (#2827).
///
/// The earlier version of this test asserted a role gate, `let is_answer`,
/// which #2827 removed when it offered the actions on the reader's own rows
/// too. The gate went and the test stayed, so the suite read red against a
/// feature that works: the contract is now that the menu reads the pressed row
/// and never a role.
#[test]
fn reply_and_forward_read_the_pressed_row_on_every_row() {
    let menu = DASHBOARD
        .split("pub(crate) fn msg_menu_items(")
        .nth(1)
        .expect("one builder resolves the row menu")
        // The next item in the impl: the menu ends where the pick toggle
        // begins. The old anchor named a function that no longer exists, so
        // the region ran to the end of the module and read code that is not
        // the menu.
        .split("/// Toggle one row")
        .next()
        .unwrap_or_default();
    let reply = menu
        .find("MessageAction::Reply.label()")
        .expect("the menu offers Reply");
    let forward = menu
        .find("MessageAction::Forward.label()")
        .expect("and Forward");
    assert!(
        reply < forward,
        "Reply comes before Forward, the order Telegram shows"
    );
    assert!(
        menu.contains("this.msg_reply_row(cx)") && menu.contains("this.msg_forward_row(cx)"),
        "each entry is wired to its own single-row path"
    );
    assert!(
        menu.contains("self.msg_menu_row.clone()"),
        "the actions read the row that was pressed"
    );
    assert!(
        !menu.contains("row.role") && !menu.contains("let is_answer"),
        "there is no role gate: an answer and the reader's own message share the menu"
    );
}

/// Right-click belongs to the whole row, including structured card padding
/// (#2923). The inner body keeps left-click selection only.
#[test]
fn right_press_is_on_the_outer_message_row() {
    let row = TRANSCRIPT
        .split("fn message_row(")
        .nth(1)
        .and_then(|rest| rest.split("fn inline_edit_box(").next())
        .expect("one stored-message row builder");
    let body = row
        .split("let mut body = div()")
        .nth(1)
        .and_then(|rest| rest.split("row = row.child(body);").next())
        .expect("message body");
    assert!(!body.contains("MouseButton::Right"));
    assert!(row.contains("row = row.on_mouse_down(MouseButton::Right"));
    assert!(
        row.find("row = row.child(body);").unwrap()
            < row.find("row = row.on_mouse_down(MouseButton::Right").unwrap(),
        "the outer row owns the press after its body and controls are attached"
    );
}

/// The main chat, not only a split leaf, swaps its composer for the forward
/// destination picker (#2924).
#[test]
fn the_unsplit_chat_renders_the_forward_picker() {
    let chat = MAIN
        .split("if !self.window_is_split() {\n                    let open = self.chat_session;")
        .nth(1)
        .and_then(|rest| rest.split("if let Some(strip) = topic_bottom").next())
        .expect("the unsplit chat composer region");
    assert!(chat.contains("self.forward_pick.is_some()"));
    assert!(chat.contains("self.forward_leaf.is_none()"));
    assert!(chat.contains("Some(self.forward_picker(cx))"));
    assert!(chat.contains("Some(self.select_bar(cx))"));
}

/// Choosing a destination opens it and stages a reply for review. It must not
/// resurrect the old direct history write or send anything behind the user's
/// back (#2924).
#[test]
fn forward_opens_the_destination_and_stages_a_composer_reply() {
    let forward = DASHBOARD
        .split("pub(crate) fn forward_into(")
        .nth(1)
        .and_then(|rest| rest.split("/// Refetch the turn's session transcript").next())
        .expect("forward destination commit");
    assert!(forward.contains("open_picked_in_focused_pane(target, true, cx)"));
    assert!(forward.contains("self.reply_rows(rows, leaf, cx)"));
    assert!(forward.contains("self.pane_focus = Some(target)"));
    assert!(forward.contains("self.pane_focus = None"));
    assert!(forward.contains("!self.pane_states.contains_key(&target)"));
    assert!(
        forward.find("open_picked_in_focused_pane(target, true, cx)").unwrap()
            < forward.find("self.reply_rows(rows, leaf, cx)").unwrap(),
        "the destination is open before its composer receives the quote"
    );
    for forbidden in [
        "transcript::forward_rows",
        "cx.spawn",
        "reload_transcript",
        "install_loaded_in_pane",
    ] {
        assert!(
            !forward.contains(forbidden),
            "forward must stage a draft, not run `{forbidden}`"
        );
    }
}

/// Every visible live entry is selectable and has its own action target in
/// both chat surfaces (#2925). Collapsed previews and the activity lead count
/// too; they are visible text, not a special dead zone.
#[test]
fn live_entries_wire_exact_actions_in_main_and_split() {
    let body = TRANSCRIPT
        .split("fn live_card_body(")
        .nth(1)
        .and_then(|rest| rest.split("/// One line of the live card's body").next())
        .expect("the live body");
    assert!(body.contains("selectable_row(d, ordinal, body)"));
    assert!(body.contains("press.get(&entry)"));
    assert!(body.contains("format!(\"live-entry-{entry}\")"));

    let card = TRANSCRIPT
        .split("fn live_card(")
        .nth(1)
        .and_then(|rest| rest.split("fn live_card_footer(").next())
        .expect("the live card");
    assert!(card.contains("format!(\"live-preview-{entry}\")"));
    assert!(card.contains("\"live-activity\".to_string()"));
    assert!(card.contains("Some(entry)"));

    assert!(MAIN.contains("transcript::live_action_rows(&self.turn.live, session)"));
    assert!(MAIN.contains("this.live_menu_open(row.clone(), None, ev.position, cx)"));
    assert!(PANE_VIEW.contains("transcript::live_action_rows(&self.live, session)"));
    assert!(
        PANE_VIEW.contains("this.live_menu_open(row.clone(), Some(session), ev.position, cx)")
    );
}

/// Live rows are ephemeral, so only actions that do not require a persisted
/// transcript identity appear. Copy uses the pressed live row even if stored
/// rows were picked earlier (#2925).
#[test]
fn live_menu_gates_stored_only_actions_and_copies_the_pressed_row() {
    let open = DASHBOARD
        .split("pub(crate) fn live_menu_open(")
        .nth(1)
        .and_then(|rest| rest.split("pub(crate) fn msg_react_open(").next())
        .expect("the live menu entry point");
    assert!(open.contains("self.msg_menu_open(row, leaf, at, cx)"));
    assert!(open.contains("self.msg_menu_live = true"));

    let menu = DASHBOARD
        .split("pub(crate) fn msg_menu_items(")
        .nth(1)
        .and_then(|rest| rest.split("/// Toggle one row").next())
        .expect("the row menu");
    assert!(menu.contains("if !self.msg_menu_live {"));
    assert!(menu.contains("if !self.msg_menu_live && self.msg_menu_leaf.is_none()"));
    for action in ["CopyMarkdown", "Reply", "Forward"] {
        assert!(menu.contains(&format!("MessageAction::{action}.label()")));
    }

    let copy = DASHBOARD
        .split("pub(crate) fn msg_copy_row(")
        .nth(1)
        .and_then(|rest| rest.split("pub(crate) fn msg_copy_markdown(").next())
        .expect("the menu copy action");
    assert!(copy.contains("if live || self.msg_select.is_empty()"));
    assert!(copy.contains("if !live && let Some(id) = id"));
}

/// Only a stored `Message` row can enter Select mode. Live entries have their
/// own ephemeral action rows, but no persisted identity and no pick box.
#[test]
fn only_stored_messages_can_be_picked() {
    assert_eq!(
        TRANSCRIPT.matches("fn message_row(").count(),
        1,
        "one builder draws the pickable row"
    );
    assert_eq!(
        TRANSCRIPT.matches("= message_row(").count(),
        1,
        "and one call site uses it"
    );

    let arm = TRANSCRIPT
        .split("Block::Msg {\n                    message_id: Some(id),")
        .nth(1)
        .expect("the pickable arm matches a stored message by its id");
    let arm = arm.split("_ => match").next().unwrap_or_default();
    assert!(
        arm.contains("= message_row("),
        "the pick box and the stored-row menu hang off the id-bearing arm"
    );
    assert!(
        arm.contains("message_click.get(id)") && arm.contains("message_press.get(id)"),
        "the stored gesture handlers are looked up by the row's own id"
    );
}
