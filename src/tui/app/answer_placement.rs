//! Where a turn's final answer goes when it is written back to the chat.
//!
//! `complete_response` overwrites the last assistant row with the final
//! answer whenever an intermediate text arrived earlier in the turn. That is
//! only correct when no tool group ran after that row. If tools ran after it,
//! the overwrite moves the answer above those tools, and the fold then hides
//! it inside the working-out group. The answer must instead be a new row
//! after the tool groups.

use crate::tui::app::DisplayMessage;

/// True when the newest assistant row sits before a tool group, meaning tools
/// ran after the last visible text and a final answer must not overwrite it.
pub(crate) fn last_text_precedes_tool_group(messages: &[DisplayMessage]) -> bool {
    let last_assistant = messages.iter().rposition(|m| m.role == "assistant");
    let last_group = messages.iter().rposition(|m| m.role == "tool_group");
    match (last_assistant, last_group) {
        (Some(a), Some(g)) => a < g,
        _ => false,
    }
}

/// True when `new` may be written over a row that already holds `existing`
/// text.
///
/// Only an extension counts: the same text, or the same text with more behind
/// it. A row holding a delivered report must never be replaced by a later,
/// shorter closing line, because the report the user already read then reads as
/// vanished while the database still holds it (#2165). When this returns false
/// the answer becomes its own row instead, which loses nothing.
pub(crate) fn answer_extends_row(existing: &str, new: &str) -> bool {
    let existing = existing.trim();
    let new = new.trim();
    !existing.is_empty() && !new.is_empty() && new.starts_with(existing)
}

/// True when the response text is already on screen as this turn's assistant
/// row, so appending it again would render the same answer twice.
///
/// A turn that halts on an option-surface tool (`suggest_options`) streams its
/// answer first and calls the tool after, so the answer row sits BEFORE the
/// tool group and `last_text_precedes_tool_group` says to append. The response
/// the tool loop synthesizes on that halt carries the same text again, which
/// would show the answer a second time under the group. The DB keeps one row
/// per turn, so a reload never shows the pair; only the live path does.
///
/// Only a row from the current turn counts. An assistant row that sits before
/// the newest user row belongs to an earlier turn, and an answer that repeats
/// it is a real answer, not a duplicate.
///
/// The row compared against is the last one that actually CARRIES TEXT, not the
/// last assistant row outright. Most agentic turns push a reasoning-only row
/// (`content` empty, the thinking in `details`) after the text and before the
/// tool group, so comparing against that row compared against `""` and the
/// dedupe never fired: the answer rendered twice, once capped as intermediate
/// narration and once in full (#2165).
pub(crate) fn answer_already_on_screen(messages: &[DisplayMessage], content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let Some(last_assistant) = messages
        .iter()
        .rposition(|m| m.role == "assistant" && !m.content.trim().is_empty())
    else {
        return false;
    };
    if messages
        .iter()
        .rposition(|m| m.role == "user")
        .is_some_and(|newest_user| newest_user > last_assistant)
    {
        return false;
    }
    messages[last_assistant].content.trim() == trimmed
}
