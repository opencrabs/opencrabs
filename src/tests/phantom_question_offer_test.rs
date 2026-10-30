//! #2083: a reply that asks the user something after a tool call is not a
//! promised action. A real promised action, with or without a question, is
//! still detected.

use crate::brain::agent::service::phantom::has_forward_intent_post_success;

#[test]
fn a_greeting_that_asks_the_user_is_not_a_promised_action() {
    let reply = "Brain's online: running on deepseek, home is ~/.opencrabs.\n\n\
        Two things, one at a time:\n\n\
        1. Who am I working with? Your name, and where you are.\n\n\
        Then I'll ask what you do, and we're done with the interview.\n\n\
        Want me to set one up? I'd walk you through it one field at a time.";
    assert!(!has_forward_intent_post_success(reply));
}

#[test]
fn an_offer_with_a_question_is_not_a_promised_action() {
    let reply = "Your fallback block is disabled and has no keys. Want me to set one up? \
        I'll walk you through it field by field.";
    assert!(!has_forward_intent_post_success(reply));
}

#[test]
fn a_real_promised_action_without_a_question_is_still_detected() {
    let reply = "The build is clean. Let me run the test suite and check the diff for regressions.";
    assert!(has_forward_intent_post_success(reply));
}

#[test]
fn a_work_announcement_still_counts_even_with_a_question_after_it() {
    let reply = "Running the test suite now. Want me to commit after?";
    assert!(has_forward_intent_post_success(reply));
}
