//! The text of the partial response a loop returns when it stops without a
//! final model response.
//!
//! An option-surface halt (`suggest_options`) ends the turn right after the
//! halting iteration, so the answer is that iteration's text. The turn's
//! accumulated text holds the narration of every iteration, so using it as the
//! answer repeats all of the working-out after the tool group (#2239).

/// The answer text for a loop that broke without a final response.
///
/// `option_halt_answer` is the text of the iteration that halted on an
/// option-surface tool, and `None` when the loop did not halt that way.
pub(crate) fn partial_response_text(
    option_halt_answer: Option<&str>,
    accumulated_text: &str,
) -> String {
    match option_halt_answer {
        Some(answer) => answer.to_string(),
        None => accumulated_text.to_string(),
    }
}
