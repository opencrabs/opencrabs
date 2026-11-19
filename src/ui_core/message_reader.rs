//! When a message the reader typed opens in the document reader (#3053).
//!
//! The transcript draws a message in full: no line is ever hidden. Past a
//! point, though, a long prompt stops reading well inside the conversation
//! column, and the reader is a better place to read it at full width. These
//! are the two pure decisions that click needs: whether a body is long enough
//! to bother, and what the reader's header calls it.

/// Lines past which a message stops reading well in the conversation column.
///
/// A short question keeps its click free: a stray click on one line must not
/// throw a modal over the chat. Past this, the row answers a click and opens
/// the same text in the reader.
pub const READER_LINES: usize = 40;

/// Characters of the first line a reader's header prints.
pub const TITLE_CHARS: usize = 60;

/// Whether a body is long enough that a click should open it in the reader.
pub fn opens_in_reader(content: &str) -> bool {
    content.lines().count() > READER_LINES
}

/// What the reader's header calls a message: the first line that carries
/// text, shortened, so two long prompts are told apart by their own opening
/// words instead of both reading "message".
pub fn title(content: &str) -> String {
    let first = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let fitted = crate::ui_core::session_preview::fit(first, TITLE_CHARS);
    if fitted.trim().is_empty() {
        "message".to_string()
    } else {
        fitted
    }
}
