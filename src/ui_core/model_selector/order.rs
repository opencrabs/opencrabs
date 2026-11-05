//! Newest-first ordering for the model picker (#1057).
//!
//! The picker rendered `config.models` in array order, and whatever registers
//! a newly discovered model appends it, so every new release landed at the
//! bottom of the list - the one entry the user is most likely reaching for,
//! in the position hardest to find.
//!
//! Plain alphabetical does not fix it. Ascending lexicographic order puts
//! `glm-5.3s` after `glm-4.5` anyway, reproducing the complaint, and it breaks
//! outright across a version boundary: `glm-5.10` sorts before `glm-5.2` as
//! text while being the newer model. Version segments have to compare as
//! numbers.
//!
//! So ordering is "natural" (digit runs compared numerically, everything else
//! lexicographically) and descending, which puts the highest version first
//! regardless of how many digits it has.

/// One run of a model id: a number or the text between numbers.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    Num(u64),
    Text(String),
}

/// Split a model id into alternating digit and non-digit runs, lowercased.
///
/// Lowercasing here is what lets `GLM 5.1` and `glm-5.1` compare as the same
/// version, since a catalogue display name and a raw id can both appear in
/// the same list.
fn tokenize(name: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.peek().copied() {
        if c.is_ascii_digit() {
            let mut n: u64 = 0;
            while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                // Saturate rather than wrap on an absurdly long digit run: a
                // wrong order is better than a panic in a picker.
                n = n.saturating_mul(10).saturating_add(d as u64);
                chars.next();
            }
            out.push(Token::Num(n));
        } else {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    break;
                }
                // Separators are dropped, not compared. A catalogue display
                // name ("GLM 5.1") and a raw id ("glm-5.1") differ only in
                // separator, and comparing those runs as text let ' ' vs '-'
                // outrank the version number itself.
                if c.is_ascii_alphanumeric() {
                    s.push(c.to_ascii_lowercase());
                }
                chars.next();
            }
            // An all-separator run yields an empty string; emitting it would
            // put a Text token where the other id has its next version digit,
            // so `glm-5-turbo` ("turbo" vs "") outranked `glm-5.3s`. Skipping
            // it lets the Num-vs-Text rule below compare 3 against "turbo",
            // which is the comparison that matters.
            if !s.is_empty() {
                out.push(Token::Text(s));
            }
        }
    }
    out
}

/// Compare two model ids naturally: numbers as numbers, text as text.
///
/// A number outranks text at the same position, which is what makes a deeper
/// version win over a variant name: `glm-5.3s` has a `3` where `glm-5-turbo`
/// has `turbo`, and 5.3 is the newer model.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (ta, tb) = (tokenize(a), tokenize(b));
    for (x, y) in ta.iter().zip(tb.iter()) {
        let ord = match (x, y) {
            (Token::Num(m), Token::Num(n)) => m.cmp(n),
            (Token::Text(m), Token::Text(n)) => m.cmp(n),
            (Token::Num(_), Token::Text(_)) => Ordering::Greater,
            (Token::Text(_), Token::Num(_)) => Ordering::Less,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ta.len().cmp(&tb.len())
}

/// Order model names newest-first, in place.
///
/// Display-only: the caller's `config.models` array is never rewritten, so a
/// user's hand-ordered config stays exactly as they wrote it on disk.
pub fn sort_newest_first(models: &mut [&str]) {
    models.sort_by(|a, b| natural_cmp(b, a));
}
