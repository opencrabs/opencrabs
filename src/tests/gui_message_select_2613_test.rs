//! The message menu's contract and what the transcript refuses to select
//! (#2613).
//!
//! The window has no headless harness, so the wiring is read from source. The
//! parts that are pure (the action list) are called for real: the type makes a
//! dead entry unreachable, and this test pins the labels it paints.

use crate::tests::gui_dashboard_source::DASHBOARD;
use crate::ui_core::message_select::MessageAction;

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

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

/// Only a stored `Message` row can be picked. Tool output, system notices,
/// plan cards and the live turn carry no `MessageId`, so they fall to the arm
/// that draws a plain row: no pick box, no menu, and a click in select mode is
/// a no-op rather than a crash.
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
        "the pick box and the menu hang off the id-bearing arm only"
    );
    // The same arm owns the click and the right-press, so a row that cannot be
    // picked also cannot open a menu that would act on it.
    assert!(
        arm.contains("message_click.get(id)") && arm.contains("message_press.get(id)"),
        "the gesture handlers are looked up by the row's own id"
    );
}
