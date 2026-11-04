//! A loop that halts on an option-surface tool answers with the halting
//! iteration's text. The turn's accumulated narration is not the answer, so
//! the partial response must never repeat it after the tool group (#2239).

use crate::brain::agent::service::halt_answer::partial_response_text;

const NARRATION: &str = "Checking the state first.\n\nFound the cause in the config.";
const FINAL_ANSWER: &str = "The fix is in place and the tests pass.";

#[test]
fn halt_uses_only_the_halting_iteration_text() {
    let accumulated = format!("{NARRATION}\n\n{FINAL_ANSWER}");
    let text = partial_response_text(Some(FINAL_ANSWER), &accumulated);
    assert_eq!(text, FINAL_ANSWER);
    assert!(!text.contains("Checking the state first."));
}

#[test]
fn loop_without_halt_keeps_accumulated_text() {
    let accumulated = format!("{NARRATION}\n\n{FINAL_ANSWER}");
    let text = partial_response_text(None, &accumulated);
    assert_eq!(text, accumulated);
}

#[test]
fn halt_with_no_text_after_the_tools_yields_an_empty_answer() {
    // The halting iteration produced no text, so there is no answer to show.
    // The accumulated narration must not become the answer in its place.
    let accumulated = NARRATION.to_string();
    let text = partial_response_text(Some(""), &accumulated);
    assert!(text.is_empty());
}
