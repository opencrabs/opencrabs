//! A message queued mid-turn must come back to the user, never vanish (#2236).
//!
//! The mid-turn queue has one consumer: the agent's tool-loop callback, which
//! drains it at a tool-call boundary. When the model answers straight after a
//! queue there is no boundary left, and `complete_response` used to drop the
//! entry on the floor. The user had watched it listed as queued, so the loss
//! was silent: no send, no error, no echo.

use crate::brain::agent::QueuedUserMessage;
use crate::tui::app::messaging::unconsumed_user_text;

#[test]
fn a_typed_queued_message_is_handed_back_verbatim() {
    let queue = vec![QueuedUserMessage::plain("ping bre".to_string())];
    assert_eq!(
        unconsumed_user_text(&queue).as_deref(),
        Some("ping bre"),
        "a user-typed queued message must survive the turn ending"
    );
}

#[test]
fn several_typed_messages_are_joined_with_newlines() {
    let queue = vec![
        QueuedUserMessage::plain("hello bre".to_string()),
        QueuedUserMessage::plain("ping bre".to_string()),
    ];
    assert_eq!(
        unconsumed_user_text(&queue).as_deref(),
        Some("hello bre\nping bre"),
        "every typed entry comes back, in the order it was queued"
    );
}

#[test]
fn a_synthetic_push_is_never_pasted_into_the_input_box() {
    // Background-task completions carry `[System: ...]` framing in
    // `context_text` and a compact tag as `display_text`. Restoring the display
    // half would put scaffolding in front of the user as if they typed it.
    let queue = vec![QueuedUserMessage::system(
        "[System: background task finished] full command and output here".to_string(),
        "background task finished".to_string(),
    )];
    assert_eq!(
        unconsumed_user_text(&queue),
        None,
        "a synthetic push must stay out of the input box"
    );
}

#[test]
fn a_mixed_queue_restores_only_the_typed_entry() {
    let queue = vec![
        QueuedUserMessage::system(
            "[System: detached bash finished] cargo test --all-features".to_string(),
            "detached bash finished".to_string(),
        ),
        QueuedUserMessage::plain("ping bre".to_string()),
    ];
    assert_eq!(
        unconsumed_user_text(&queue).as_deref(),
        Some("ping bre"),
        "the typed half is restored, the synthetic half is dropped"
    );
}

#[test]
fn an_empty_queue_restores_nothing() {
    assert_eq!(unconsumed_user_text(&[]), None);
}
