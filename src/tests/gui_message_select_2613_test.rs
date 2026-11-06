//! The message menu's contract and what the transcript refuses to select
//! (#2613).
//!
//! The window has no headless harness, so the wiring is read from source. The
//! parts that are pure (the action list) are called for real: the type makes a
//! dead entry unreachable, and this test pins the labels it paints.

use crate::ui_core::message_select::MessageAction;

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

/// The menu is the set of actions that can actually be carried out, in the
/// order Telegram shows them. A label with no path behind it never ships.
#[test]
fn the_menu_offers_only_the_actions_this_version_wires() {
    assert_eq!(
        MessageAction::ALL.map(|action| action.label()),
        ["Copy Text", "Forward", "Select"],
        "the menu is exactly the wired actions, in menu order"
    );

    // Out of scope on purpose (#2613): `messages` has no `reply_to` and no pin
    // column, translation needs a provider, delete needs its own confirmation
    // path, and a message link does not apply to a local transcript.
    for absent in [
        "Reply",
        "Pin",
        "Translate",
        "Delete",
        "Copy Message Link",
        "Report",
    ] {
        assert!(
            !GUI_MAIN.contains(&format!("\"{absent}\".to_string()")),
            "{absent} has no path yet, so it is not offered as a dead entry"
        );
    }
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
