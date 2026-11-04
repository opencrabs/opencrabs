//! The TUI edge for the usage dashboard's key decisions.
//!
//! The rules themselves live in `crate::ui_core::usage::input` and speak
//! [`UiKey`], so a GUI frontend can share them. This module is the only
//! place that knows the TUI spells those keys as crossterm types: it
//! translates `KeyEvent`/`KeyCode` into `UiKey` and hands the decision to
//! the shared logic.
//!
//! Modifiers are deliberately dropped in the translation. The dashboard's
//! rules have always matched on the code alone (Ctrl+T and T did the same
//! thing before this split), so reading modifiers here would change
//! behaviour under the banner of a refactor.

use crate::tui::app::dialog_keys::{DialogKey, DialogScope, dialog_keys};
use crate::ui_core::keys::UiKey;
use crate::ui_core::usage::input::{
    DashboardState, UsageKeyOutcome, advertises as advertises_key, decide as decide_key,
};
use crossterm::event::{KeyCode, KeyEvent};

/// Translate a crossterm key code into the neutral vocabulary.
///
/// Anything the dashboard does not model becomes [`UiKey::Other`], which no
/// rule matches: an unhandled terminal key can never be mistaken for one
/// that is handled.
pub fn to_ui_key(code: KeyCode) -> UiKey {
    match code {
        KeyCode::Esc => UiKey::Esc,
        KeyCode::Tab => UiKey::Tab,
        KeyCode::BackTab => UiKey::BackTab,
        KeyCode::Enter => UiKey::Enter,
        KeyCode::Char(c) => UiKey::Char(c),
        _ => UiKey::Other,
    }
}

/// The dashboard's answer to a crossterm event, applying the state changes
/// that belong to the decision itself.
pub fn decide(state: &mut DashboardState, event: KeyEvent) -> UsageKeyOutcome {
    decide_key(state, to_ui_key(event.code))
}

/// Does this key code do, in this state, what the footer says it does?
pub fn advertises(state: &DashboardState, code: KeyCode) -> bool {
    advertises_key(state, to_ui_key(code))
}

/// The dashboard's footer entries for this state: the scoped keymap filtered
/// to the keys that are live.
///
/// The rendered footer and the drift tests both come through here, so what a
/// user is told about and what the handler honours are decided by the shared
/// [`advertises`](crate::ui_core::usage::input::advertises) alone (#2027).
/// Entries are handed back by value ([`DialogKey`] is `Copy`) so the footer
/// takes them as a plain slice.
pub fn advertised_keys(state: &DashboardState) -> Vec<DialogKey> {
    dialog_keys(DialogScope::UsageDashboard)
        .iter()
        .filter(|key| key.events.iter().any(|(code, _)| advertises(state, *code)))
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_core::usage::data::Period;
    use crossterm::event::KeyModifiers;

    #[test]
    fn translation_covers_the_dashboard_vocabulary() {
        assert_eq!(to_ui_key(KeyCode::Esc), UiKey::Esc);
        assert_eq!(to_ui_key(KeyCode::Tab), UiKey::Tab);
        assert_eq!(to_ui_key(KeyCode::BackTab), UiKey::BackTab);
        assert_eq!(to_ui_key(KeyCode::Enter), UiKey::Enter);
        assert_eq!(to_ui_key(KeyCode::Char('a')), UiKey::Char('a'));
    }

    #[test]
    fn unmodelled_terminal_keys_map_to_other() {
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::F(1),
            KeyCode::Home,
            KeyCode::Delete,
            KeyCode::Null,
        ] {
            assert_eq!(to_ui_key(code), UiKey::Other, "{code:?}");
        }
    }

    #[test]
    fn crossterm_decide_delegates_to_the_shared_rules() {
        let mut state = DashboardState::default();
        let event = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(
            decide(&mut state, event),
            UsageKeyOutcome::Period(Period::Week)
        );
    }
}
