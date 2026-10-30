//! #2220: `ends_with_url` derived a byte offset from a character index.
//!
//! `str::rfind` returns a byte index. The old code added one byte
//! unconditionally, which assumes the matched separator is one byte wide. Any
//! reply whose last whitespace was multi-byte (U+202F, U+00A0, U+2009,
//! U+3000) sliced inside that character and panicked with
//! `start byte index N is not a char boundary`.
//!
//! On the Telegram path the panic killed the turn before the reply was sent,
//! so the bot looked dead while it was in fact polling and running turns
//! normally. Reported by VincentFeliciano with three panics in one evening,
//! all from a model that emits U+202F around task numbers.
//!
//! `ends_with_url` is private, so these drive it through the public entry
//! point that calls it, `looks_truncated_mid_sentence`.

use crate::brain::agent::service::phantom::looks_truncated_mid_sentence;

/// A reply long enough to clear the 40-character floor, with no code fence and
/// no trailing pipe, so every structural exemption before the URL check is
/// passed and the check itself is what runs.
const LEAD: &str = "The quick brown fox jumps over the lazy dog.";

#[test]
fn test_multibyte_whitespace_before_a_digit_does_not_panic() {
    // The reported shape: `Task 7` written with U+202F. It is the last
    // whitespace in the string, so `rfind` names it and the old `i + 1`
    // landed one byte inside a three-byte character.
    let text = format!("{LEAD}\u{202f}7");
    assert!(
        looks_truncated_mid_sentence(&text),
        "#2220: a reply ending mid-sentence on a digit is truncated"
    );
}

#[test]
fn test_every_multibyte_separator_the_report_named() {
    for sep in ['\u{202f}', '\u{00a0}', '\u{2009}', '\u{3000}'] {
        let text = format!("{LEAD}{sep}7");
        assert!(
            looks_truncated_mid_sentence(&text),
            "#2220: U+{:04X} as the last separator must not panic",
            sep as u32
        );
    }
}

#[test]
fn test_url_after_a_multibyte_separator_still_exempts_the_reply() {
    // The URL check has to keep working when the separator before it is
    // multi-byte: the tail must still start at the URL, not inside the
    // separator.
    let text = format!("{LEAD} docs:\u{202f}https://docs.opencrabs.com");
    assert!(
        !looks_truncated_mid_sentence(&text),
        "#2220: a reply ending in a URL is not truncated, multi-byte separator or not"
    );
}

#[test]
fn test_ascii_behaviour_is_unchanged() {
    // No behaviour change for ASCII input: the separator is one byte, so the
    // offset arithmetic is identical before and after the fix.
    let text = "Read the docs at https://docs.opencrabs.com";
    assert!(
        !looks_truncated_mid_sentence(text),
        "#2220: ASCII URL tail is still exempt"
    );
}
