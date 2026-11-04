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
