//! The one-line message preview both session surfaces draw (#2701, #2737).
//!
//! The GUI rail card shows `You: ...` or `Crabs: ...` under a session's name so
//! the list says what the conversation is about, not just when it happened.
//! The TUI draws the same line from the same query, so the wording lives here
//! once: two copies would drift on the pronouns long before anyone noticed.

/// How many characters of a message a preview shows, ellipsis included.
pub const PREVIEW_CHARS: usize = 60;

/// What the reader calls the speaker.
///
/// The prefix names the speaker the way a person thinks of them, not the way
/// the table stores the role. A role that is neither `user` nor `assistant`
/// keeps its own name rather than pretending to be one of them.
pub fn speaker(role: &str) -> &str {
    match role {
        "user" => "You",
        "assistant" => "Crabs",
        other => other,
    }
}

/// Char-safe truncation: walk chars, never slice bytes, so a multi-byte
/// character at the cut can never panic.
pub fn fit(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The preview line: who spoke, then the text cut to fit.
pub fn preview_line(role: &str, text: &str) -> String {
    format!("{}: {}", speaker(role), fit(text, PREVIEW_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_names_the_speaker_the_way_the_reader_thinks_of_them() {
        assert_eq!(preview_line("user", "hello"), "You: hello");
        assert_eq!(preview_line("assistant", "hi"), "Crabs: hi");
        // A tool row is not disguised as one of the two voices.
        assert_eq!(preview_line("tool", "x"), "tool: x");
    }

    #[test]
    fn preview_cuts_on_a_char_boundary_and_marks_the_cut() {
        let long = "🦀".repeat(PREVIEW_CHARS + 5);
        let line = preview_line("user", &long);
        assert!(line.ends_with('…'), "the cut is marked");
        assert_eq!(
            line.chars().count(),
            "You: ".chars().count() + PREVIEW_CHARS
        );
    }

    #[test]
    fn a_short_message_is_not_touched() {
        assert_eq!(preview_line("user", "short"), "You: short");
    }
}
