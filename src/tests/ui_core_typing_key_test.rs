//! Which keystrokes are text and which are commands (#2611).
//!
//! The window writes one debug line per keystroke, and that line is the only
//! way to tell a chord that never fired from a chord that never arrived. It
//! was also recording every typed character, which made the daily log a
//! transcript of the composer. The split is a pure predicate so the rule can
//! be checked without a window.

use crate::ui_core::keys::is_typing_key;

#[test]
fn a_plain_character_is_typing() {
    for key in ["a", "z", "0", "9", ":", ".", ",", "/", "!", "T"] {
        assert!(
            is_typing_key(key, false, false, false),
            "{key:?} is text and must not be logged per keystroke"
        );
    }
}

#[test]
fn a_space_is_typing() {
    assert!(
        is_typing_key("space", false, false, false),
        "the window spells the space bar `space`, and it is text"
    );
}

#[test]
fn a_named_key_is_not_typing() {
    for key in [
        "tab",
        "backtab",
        "escape",
        "enter",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "backspace",
        "delete",
        "f1",
    ] {
        assert!(
            !is_typing_key(key, false, false, false),
            "{key:?} is a command candidate and keeps its log line"
        );
    }
}

#[test]
fn a_character_under_a_command_modifier_is_not_typing() {
    assert!(
        !is_typing_key("t", true, false, false),
        "ctrl+t is a chord, not a letter"
    );
    assert!(
        !is_typing_key("t", false, true, false),
        "cmd+t is a chord, not a letter"
    );
    assert!(
        !is_typing_key("tab", false, false, true),
        "alt+tab is a chord, and it is the rotation"
    );
}

#[test]
fn a_capital_letter_is_still_typing() {
    // Shift is not a term in the rule: `shift+t` is `T`, and the chords that
    // need shift spell themselves as a named key or as a modifier the window
    // reads on its own.
    assert!(
        is_typing_key("T", false, false, false),
        "a capital is text, and logging it would be the same flood"
    );
}
