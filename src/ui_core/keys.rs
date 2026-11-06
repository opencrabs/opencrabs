//! Terminal-neutral key surface for shared UI logic.
//!
//! Every frontend has keys; no two frontends spell them the same way. A
//! crossterm `KeyEvent` carries a `KeyCode` plus `KeyModifiers`, a GPUI
//! keystroke carries a platform key name, and a web frontend carries a
//! `KeyboardEvent.code`. Shared logic that matched on any one of those
//! spellings could not be reused by the others, and re-implementing the
//! decision per frontend is how two surfaces drift apart.
//!
//! [`UiKey`] is the smallest vocabulary the usage dashboard's decision
//! logic needs today. It is deliberately not a general keyboard model: a
//! key outside this set maps to [`UiKey::Other`], and a surface that needs
//! more keys adds them here with the same rule. The migration of the rest
//! of the TUI is a later phase; this module is the pilot's shape.
//!
//! Modifiers are NOT part of the vocabulary on purpose. The dashboard's
//! existing rules match on the code alone (Ctrl+T and T have always done
//! the same thing there), so carrying modifiers through would change
//! behaviour while pretending to preserve it. A frontend that wants
//! modifier-sensitive rules adds a variant that says so explicitly.

/// A key press, spelled without any terminal or windowing dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiKey {
    /// The Escape key: leave the surface, or back out of an overlay.
    Esc,
    /// Forward focus movement.
    Tab,
    /// Backward focus movement (Shift+Tab on most terminals).
    BackTab,
    /// Activate the focused element, where the surface has something to
    /// activate.
    Enter,
    /// A printable character, exactly as produced by the input layer.
    /// Case is preserved: callers that treat `t` and `T` alike say so.
    Char(char),
    /// Any key this vocabulary does not model. Always unhandled by
    /// decision logic written against this enum, which keeps an unknown
    /// key from being silently reinterpreted as a known one.
    Other,
}

/// Whether a keystroke can only be text (#2611).
///
/// The window logs one debug line per keystroke, which is what tells a chord
/// that never fired apart from a chord that never arrived (#2356). A typed
/// character answers neither question: it is no chord candidate, and every
/// surface types characters all day, so the line becomes a keystroke
/// recorder. Control, platform and alt are the modifiers that turn a key into
/// a command, and a named key (`tab`, `escape`, `enter`, `up`) is never a
/// character, so both keep the line.
///
/// Shift is deliberately not a term. `shift+t` is a capital letter, and the
/// chords that need shift spell themselves as a named key (`backtab`) or as a
/// modifier the window reads on its own.
pub fn is_typing_key(key: &str, control: bool, platform: bool, alt: bool) -> bool {
    if control || platform || alt {
        return false;
    }
    key == "space" || key.chars().count() == 1
}
