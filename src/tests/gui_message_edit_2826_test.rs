//! Editing a stored user message from the transcript (#2826).
//!
//! The window has no headless harness, so the wiring is read from source, the
//! way the other gui tests do it. The rule that cannot be bent is the order of
//! the edit send: a truncation that runs while a turn is streaming leaves the
//! assistant row that turn is still appending on a branch that no longer
//! exists, so the stop has to come first. That is asserted by position, not by
//! the presence of the calls.

use crate::tests::gui_dashboard_source::DASHBOARD;

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const MESSAGE_REPO: &str = include_str!("../db/repository/message.rs");
const EDITOR: &str = include_str!("../gui/editor.rs");
const ICON: &str = include_str!("../gui/icon.rs");

/// The row builder draws the button only where a rewrite makes sense: a user
/// row. An answer is the model's, and rewriting one would leave a transcript
/// claiming the model wrote something it never did.
#[test]
fn only_a_user_row_draws_the_edit_button() {
    let arm = TRANSCRIPT
        .split("Block::Msg {\n                    message_id: Some(id),")
        .nth(1)
        .expect("the stored-row arm matches a message by its id");
    let arm = arm.split("_ => match").next().unwrap_or_default();
    assert!(
        arm.contains("if role.as_str() == \"user\""),
        "the edit handler is handed to a user row and to nothing else"
    );

    let builder = TRANSCRIPT
        .split("fn message_row(")
        .nth(1)
        .expect("one builder draws the row")
        .split("fn reaction_chips(")
        .next()
        .unwrap_or_default();
    assert!(
        builder.contains("if let Some(handler) = edit"),
        "the button is drawn from the handler it was given, so a row with no \
         handler draws no button"
    );
    assert!(
        builder.contains(".group_hover(group,"),
        "the button appears on the row's own hover, not on every row's"
    );
    assert!(
        builder.contains("opacity(0.0)"),
        "it is invisible until the row is hovered"
    );
}

/// The truncation keeps the row it is replacing: the edited message survives
/// and only what came after it goes, in one session and no other.
#[test]
fn the_truncation_is_one_statement_bounded_by_the_session() {
    let after = MESSAGE_REPO
        .split("pub async fn delete_after(")
        .nth(1)
        .expect("the repository owns the truncation")
        .split("/// List all messages for a session")
        .next()
        .unwrap_or_default();
    assert!(
        after.contains("DELETE FROM messages WHERE session_id = ?1 AND sequence > ?2"),
        "the row at the edited sequence survives, and only that session is cut"
    );
}

/// A stop lands before the truncation. Truncating first lets the assistant row
/// a running turn is still appending land on history that no longer exists.
#[test]
fn the_send_stops_the_turn_before_it_truncates() {
    let send = DASHBOARD
        .split("fn send_edited(")
        .nth(1)
        .expect("the edit send is one function")
        .split("fn install_loaded(")
        .next()
        .unwrap_or_default();
    let stop = send
        .find("cancel.cancel()")
        .expect("the edit send stops the turn it interrupts");
    let truncate = send
        .find("truncate_after(")
        .expect("the edit send drops the rows after the edited one");
    assert!(
        stop < truncate,
        "the turn is stopped before the history under it is cut"
    );
    let send_at = send
        .rfind("send_message_showing(text, None, cx)")
        .expect("the new text goes out as the next turn");
    assert!(
        send_at > truncate,
        "and the new text is sent only once the rows are gone"
    );
    assert!(
        send.contains("if self.turn.busy") && send.contains("pane_states.get(&target.session)"),
        "both the window's turn and a split leaf running the same session stop"
    );
}

/// The edit is armed on one row and cleared by leaving the conversation, so a
/// send in another session can never truncate history nobody is looking at.
#[test]
fn the_edit_names_its_row_and_ends_with_its_conversation() {
    assert!(
        DASHBOARD.contains("self.composer.edit = Some(chat_leaf::EditTarget {"),
        "the button arms the composer with the row it came from"
    );
    assert!(
        DASHBOARD.contains("if self.chat_state.session() == Some(target.session)"),
        "a send only truncates when it is still in the edited row's conversation"
    );
    let nav = DASHBOARD
        .split("pub(crate) fn begin_conversation(")
        .nth(1)
        .expect("switching conversation is one function")
        .split("if switched {")
        .next()
        .unwrap_or_default();
    assert!(
        nav.contains("self.composer.edit = None;"),
        "leaving the conversation ends the edit"
    );
    assert!(
        EDITOR.contains("pub fn show_editing("),
        "the field says it is rewriting history rather than appending to it"
    );
}

/// The button is a pencil, not the word "Edit" (#2826).
///
/// It shipped as a text label in a box, and the owner read it as exactly what
/// it was: a placeholder. Every other affordance on the row is a picture, so
/// this one is an embedded vector drawn in the row's own colour.
#[test]
fn the_edit_button_draws_a_pencil() {
    assert!(
        ICON.contains("pub fn edit_icon("),
        "the pencil is drawn by the icon module, like every other mark"
    );
    assert!(
        ICON.contains("/src/assets/edit.svg"),
        "from an embedded vector, so the binary carries it"
    );
    let builder = TRANSCRIPT
        .split("fn message_row(")
        .nth(1)
        .expect("one builder draws the row")
        .split("fn reaction_chips(")
        .next()
        .unwrap_or_default();
    assert!(
        builder.contains("crate::icon::edit_icon("),
        "the button draws the pencil"
    );
    assert!(
        !builder.contains("SharedString::from(\"Edit\")"),
        "and no longer prints the word"
    );
}
