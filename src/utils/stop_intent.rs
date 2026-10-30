//! "Stop what you are doing" detection for every channel and the TUI (#965).
//!
//! Each channel used to gate its fast-cancel on an exact, case-insensitive
//! match of the bare word `stop`. That failed on everything a person actually
//! types under pressure: `STOP!!!` (trailing punctuation), `stop.`, `hold on`,
//! `wait a sec`, and every non-English equivalent. A miss fell through to
//! normal dispatch and became ordinary mid-turn input, so whether anything
//! stopped depended on the model noticing while tools kept executing. That is
//! a suggestion, not a kill switch.
//!
//! Vocabulary lives in the six per-language TOMLs behind
//! [`crate::utils::prompt_analyzer::all_packs`] and is scanned across ALL of
//! them. Users are worldwide and code-switch freely, so a language guess would
//! silently disarm the kill switch for whoever it guessed wrong about.
//!
//! # Why single-word and multi-word entries match differently
//!
//! The dangerous failure here is not a missed stop, it is a false one:
//! cancelling `stop the docker container` silently drops work the user asked
//! for. So the two shapes are treated differently:
//!
//! - **Single-word** entries (`stop`, `wait`, `cancel`) match only when they
//!   are the WHOLE message. `stop` cancels; `stop the docker container` does
//!   not.
//! - **Multi-word** entries (`hold on`, `wait a sec`) may also open a message,
//!   because they do not begin ordinary instructions. `hold on, let me check`
//!   cancels; nobody writes `hold on the container`.
//!
//! Callers are responsible for only acting when a turn is actually in flight.

use crate::utils::prompt_analyzer::all_packs;

/// Longest a message can be and still be read as a bare interrupt. Anything
/// wordier is prose that merely mentions stopping, and a leading multi-word
/// phrase is the only way in.
const MAX_BARE_WORDS: usize = 3;

/// Casefold, drop punctuation and symbols, collapse whitespace.
///
/// This is what makes `STOP!!!`, `stop.`, and `  Stop  ` all reduce to `stop`.
/// Apostrophes are kept so contractions survive intact.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = true;
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '\'' {
            out.extend(ch.to_lowercase());
            last_was_space = false;
        } else if !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
    }
    out.trim_end().to_string()
}

/// True when `phrase` opens `text` at a word boundary.
fn opens_with(text: &str, phrase: &str) -> bool {
    text.strip_prefix(phrase)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// Drop a trailing run of address terms, so "stop crab", "hold on crabs" and
/// "stop please" reduce to the interrupt itself.
///
/// Only the tail is stripped. "stop the bot container" keeps every word,
/// because `bot` is not final there and the object is what makes it an
/// instruction rather than an interrupt.
fn strip_trailing_address(normalized: &str) -> &str {
    let mut end = normalized.len();
    'outer: loop {
        let head = normalized[..end].trim_end();
        if head.is_empty() {
            return head;
        }
        for lang in all_packs() {
            for term in &lang.stop_address {
                let term = term.trim().to_lowercase();
                if term.is_empty() {
                    continue;
                }
                if let Some(rest) = head.strip_suffix(&term)
                    && (rest.is_empty() || rest.ends_with(' '))
                {
                    // Never strip everything: a message that is only an
                    // address term is not an interrupt.
                    if rest.trim_end().is_empty() {
                        return head;
                    }
                    end = rest.len();
                    continue 'outer;
                }
            }
        }
        return head;
    }
}

/// Does this message mean "stop right now"?
///
/// Scans every supported language. See the module docs for why single-word and
/// multi-word entries are matched differently.
pub fn is_stop_intent(text: &str) -> bool {
    let full = normalize(text);
    if full.is_empty() {
        return false;
    }
    let normalized = strip_trailing_address(&full);
    if normalized.is_empty() {
        return false;
    }
    let word_count = normalized.split_whitespace().count();

    for lang in all_packs() {
        for phrase in &lang.stop_intent {
            let phrase = normalize(phrase);
            if phrase.is_empty() {
                continue;
            }
            if phrase.contains(' ') {
                // Multi-word: whole message or leading clause.
                if opens_with(normalized, &phrase) {
                    return true;
                }
            } else if normalized == phrase && word_count <= MAX_BARE_WORDS {
                // Single word: whole message only, never a prefix.
                return true;
            }
        }
    }
    false
}

/// Strip a leading slash so `/stop` and `stop` take the same path, then test.
///
/// Channels accept both spellings; the slash form is the documented command
/// and must keep working regardless of the phrase tables.
pub fn is_stop_command_or_intent(text: &str) -> bool {
    let trimmed = text.trim();
    let without_slash = trimmed.strip_prefix('/').unwrap_or(trimmed);
    // Bot-suffixed commands ("/stop@somebot") are the group-chat spelling.
    let without_mention = without_slash
        .split_once('@')
        .map_or(without_slash, |(head, _)| head);
    is_stop_intent(without_mention)
}
