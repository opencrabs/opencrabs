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
    format!("{}: {}", speaker(role), fit(&flatten(text), PREVIEW_CHARS))
}

/// Turn a stored message into one line of plain prose.
///
/// The rail is one line under a session name, and the stored text is a whole
/// assistant turn: `<!-- reasoning -->` markers, tool ledgers, fenced blocks,
/// bold and bullets. Painted raw, the card read `**A bateria` and
/// `<!-- tools ... -->`, which is what Daniel saw and reported as "caracteres
/// esquisitos" on 2026-11-21. The reader wants the gist, so every marker that
/// is a rendering instruction goes, and the words stay.
///
/// Reasoning and tool markup are removed by [`strip_llm_artifacts`], the same
/// boundary every channel already funnels through; what this adds is the
/// markdown flattening, because that function's contract is "safe to show",
/// not "plain".
pub fn flatten(text: &str) -> String {
    let cleaned = crate::utils::sanitize::strip_llm_artifacts(text);
    let mut out = String::with_capacity(cleaned.len());
    for line in cleaned.lines() {
        let line = line.trim();
        // A fence is punctuation, not content: the code inside it is the
        // content, and the preview is one line of words.
        if line.starts_with("```") {
            continue;
        }
        // A rule (---, ***, ___) carries no words at all.
        if !line.is_empty()
            && line
                .chars()
                .all(|c| matches!(c, '-' | '*' | '_' | '=' | ' '))
        {
            continue;
        }
        let body = strip_line_marker(line);
        if body.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&strip_inline_markers(body));
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop a heading, quote or bullet marker from the front of a line.
fn strip_line_marker(line: &str) -> &str {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        let after = if let Some(r) = trimmed.strip_prefix('#') {
            r.trim_start_matches('#').trim_start()
        } else if let Some(r) = trimmed.strip_prefix('>') {
            r.trim_start()
        } else if let Some(r) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
            .or_else(|| trimmed.strip_prefix("• "))
        {
            r.trim_start()
        } else if let Some(r) = strip_ordered_marker(trimmed) {
            r.trim_start()
        } else {
            return trimmed;
        };
        rest = after;
    }
}

/// `1. text` and `1) text` are bullets too; `2.5 kg` is not, because the digit
/// run is followed by a dot and then another digit.
fn strip_ordered_marker(line: &str) -> Option<&str> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    // `get` rather than an index slice: this file sits inside the byte-slice
    // ratchet, and the digits are ASCII so the offset is always a boundary.
    let rest = line.get(digits..)?;
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    // `1.5` is a number, not a list item.
    if rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
        Some(rest)
    } else {
        None
    }
}

/// Remove the emphasis, code and strike markers from a line, keeping its words.
///
/// Only the paired markers go: a lone `*` or `_` is left where it is, because
/// `a*b` and `snake_case` are words and cutting them would be a worse lie than
/// the asterisk.
fn strip_inline_markers(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '`' => {}
            '~' if chars.peek() == Some(&'~') => {
                chars.next();
            }
            '*' | '_' if chars.peek() == Some(&ch) => {
                chars.next();
            }
            _ => out.push(ch),
        }
    }
    out
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

    /// The bug Daniel reported on 2026-11-21: the rail printed the markers.
    #[test]
    fn the_preview_is_words_not_markup() {
        assert_eq!(
            flatten("**A bateria** está nos 87%"),
            "A bateria está nos 87%"
        );
        assert_eq!(flatten("`cargo test` passa"), "cargo test passa");
        assert_eq!(flatten("## Resumo\n\ntudo bem"), "Resumo tudo bem");
        assert_eq!(flatten("- um\n- dois"), "um dois");
        assert_eq!(flatten("1. primeiro\n2. segundo"), "primeiro segundo");
        assert_eq!(flatten("> citado"), "citado");
        assert_eq!(flatten("a~~riscado~~b"), "ariscadob");
        assert_eq!(flatten("---"), "");
        assert_eq!(flatten("```bash\ncargo test\n```"), "cargo test");
    }

    /// A tool ledger is markup the reader must never see, and the reasoning
    /// markers are not prose either.
    #[test]
    fn a_ledger_and_a_reasoning_block_never_reach_the_card() {
        let raw = "<!-- tools [{\"name\":\"bash\"}] -->\n<!-- reasoning -->\nI should look.\n<!-- /reasoning -->\nThe answer is 4.";
        let line = flatten(raw);
        assert!(!line.contains("<!--"), "markup leaked: {line}");
        assert!(!line.contains("tools"), "the ledger leaked: {line}");
        assert!(line.contains("answer is 4"), "the words stayed: {line}");
    }

    /// Code is words too, but its indentation is not: a preview is one line.
    #[test]
    fn line_breaks_and_indentation_collapse() {
        assert_eq!(flatten("one\n\n   two\t\nthree"), "one two three");
    }

    /// A number that is a number, not a list item, keeps its dot.
    #[test]
    fn a_decimal_is_not_a_bullet() {
        assert_eq!(flatten("2.5 kg"), "2.5 kg");
        assert_eq!(flatten("2026. foi um ano"), "2026. foi um ano");
    }

    /// snake_case and a lone asterisk survive: cutting them would be a worse
    /// lie than leaving the marker.
    #[test]
    fn a_word_with_an_underscore_or_a_lone_star_is_left_alone() {
        assert_eq!(flatten("instance_strip.rs"), "instance_strip.rs");
        assert_eq!(flatten("a * b"), "a * b");
    }
}
