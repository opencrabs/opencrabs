//! Neutral input logic for the usage dashboard.
//!
//! The dashboard's own state changes (focus moves, opening and closing the
//! price breakdown, switching period) happen in [`decide`], which matches on
//! [`UiKey`] and never on a terminal type. A frontend converts its native
//! key into a `UiKey` at its own edge and calls this; the TUI adapter lives
//! in `crate::usage::keymap`.
//!
//! Why this split: the rules that say "Esc closes, Tab moves focus, Enter
//! opens the breakdown only on the Cache card" are product behaviour, not
//! rendering. Keeping them here means a GUI gets the same behaviour for
//! free, and the TUI's footer and handler cannot drift because both read
//! [`advertises`].

use super::data::{DashboardData, Period};
use super::view_model::USAGE_CARD_COUNT;
use crate::ui_core::keys::UiKey;

/// Grid index of the Cache Efficiency card: 0=daily, 1=project, 2=model,
/// 3=tools, 4=activity, 5=cache.
pub const CACHE_CARD: usize = 5;

/// Dashboard UI state, owned by the frontend that shows it.
#[derive(Debug, Clone)]
pub struct DashboardState {
    pub period: Period,
    pub focused_card: usize,
    pub data: DashboardData,
    /// The cache price breakdown is open over the grid (#2027).
    pub cache_detail_open: bool,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            period: Period::AllTime,
            focused_card: 0,
            data: DashboardData::default(),
            cache_detail_open: false,
        }
    }
}

impl DashboardState {
    pub fn focus_next(&mut self) {
        self.focused_card = (self.focused_card + 1) % USAGE_CARD_COUNT;
    }

    pub fn focus_prev(&mut self) {
        self.focused_card = if self.focused_card == 0 {
            USAGE_CARD_COUNT - 1
        } else {
            self.focused_card - 1
        };
    }

    /// Set period and return true if changed (caller should re-fetch data)
    pub fn set_period(&mut self, period: Period) -> bool {
        if self.period != period {
            self.period = period;
            true
        } else {
            false
        }
    }

    /// Is the Cache Efficiency card the focused one? Only that card has a
    /// price breakdown to open (#2027).
    pub fn cache_card_focused(&self) -> bool {
        self.focused_card == CACHE_CARD
    }
}

/// What the dashboard does with a key press.
///
/// The dashboard's own state changes happen inside [`decide`]; the variants
/// carry only what needs the host app: re-fetching after a period change,
/// and leaving the mode. Splitting it this way makes every key decision
/// testable with no App, no database and no terminal (#2027).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageKeyOutcome {
    /// Handled inside the dashboard.
    Consumed,
    /// Switch the reporting period and re-fetch.
    Period(Period),
    /// Leave the dashboard.
    Close,
    /// Not a dashboard key; the caller keeps it.
    NotConsumed,
}

/// The dashboard's answer to `key`, applying the state changes that belong
/// to the decision itself.
///
/// The render side asks [`advertises`] which keys are live, so the footer a
/// user reads and the keys the handler honours come from the same rules and
/// cannot drift (#2027).
pub fn decide(state: &mut DashboardState, key: UiKey) -> UsageKeyOutcome {
    if state.cache_detail_open {
        // The breakdown sits over the grid: Esc backs out of it, and every
        // other key is deliberately inert so the dashboard behind it cannot
        // move while the prices are being read.
        if key == UiKey::Esc {
            state.cache_detail_open = false;
        }
        return UsageKeyOutcome::Consumed;
    }

    match key {
        UiKey::Esc => UsageKeyOutcome::Close,
        UiKey::Tab => {
            state.focus_next();
            UsageKeyOutcome::Consumed
        }
        UiKey::BackTab => {
            state.focus_prev();
            UsageKeyOutcome::Consumed
        }
        UiKey::Enter if state.cache_card_focused() => {
            state.cache_detail_open = true;
            UsageKeyOutcome::Consumed
        }
        UiKey::Char('t' | 'T') => UsageKeyOutcome::Period(Period::Today),
        UiKey::Char('w' | 'W') => UsageKeyOutcome::Period(Period::Week),
        UiKey::Char('m' | 'M') => UsageKeyOutcome::Period(Period::Month),
        UiKey::Char('a' | 'A') => UsageKeyOutcome::Period(Period::AllTime),
        _ => UsageKeyOutcome::NotConsumed,
    }
}

/// Does `key` do, in this state, what the footer says it does?
///
/// Deliberately narrower than [`decide`]: with the breakdown open every key
/// is consumed but only Esc performs an action, and a key that performs
/// nothing must not be advertised.
pub fn advertises(state: &DashboardState, key: UiKey) -> bool {
    if state.cache_detail_open {
        return key == UiKey::Esc;
    }
    match key {
        UiKey::Esc | UiKey::Tab | UiKey::BackTab => true,
        UiKey::Char('t' | 'T' | 'w' | 'W' | 'm' | 'M' | 'a' | 'A') => true,
        // Enter opens the breakdown, so it is live only where there is one
        // to open (#2027).
        UiKey::Enter => state.cache_card_focused(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_on(card: usize) -> DashboardState {
        DashboardState {
            focused_card: card,
            ..DashboardState::default()
        }
    }

    #[test]
    fn tab_and_backtab_cycle_focus_with_wrap() {
        let mut s = state_on(USAGE_CARD_COUNT - 1);
        assert_eq!(decide(&mut s, UiKey::Tab), UsageKeyOutcome::Consumed);
        assert_eq!(s.focused_card, 0, "Tab wraps forward");
        assert_eq!(decide(&mut s, UiKey::BackTab), UsageKeyOutcome::Consumed);
        assert_eq!(s.focused_card, USAGE_CARD_COUNT - 1, "BackTab wraps back");
    }

    #[test]
    fn esc_closes_the_dashboard() {
        let mut s = DashboardState::default();
        assert_eq!(decide(&mut s, UiKey::Esc), UsageKeyOutcome::Close);
    }

    #[test]
    fn enter_opens_the_breakdown_only_on_the_cache_card() {
        let mut on_cache = state_on(CACHE_CARD);
        assert_eq!(
            decide(&mut on_cache, UiKey::Enter),
            UsageKeyOutcome::Consumed
        );
        assert!(on_cache.cache_detail_open);

        let mut elsewhere = state_on(0);
        assert_eq!(
            decide(&mut elsewhere, UiKey::Enter),
            UsageKeyOutcome::NotConsumed
        );
        assert!(!elsewhere.cache_detail_open);
    }

    #[test]
    fn breakdown_eats_every_key_but_esc() {
        let mut s = state_on(CACHE_CARD);
        s.cache_detail_open = true;
        for key in [
            UiKey::Tab,
            UiKey::BackTab,
            UiKey::Enter,
            UiKey::Char('a'),
            UiKey::Other,
        ] {
            assert_eq!(
                decide(&mut s, key),
                UsageKeyOutcome::Consumed,
                "{key:?} must be inert, not fall through to the grid"
            );
            assert!(s.cache_detail_open, "{key:?} must not close the breakdown");
            assert!(
                !advertises(&s, key),
                "{key:?} performs nothing: not advertised"
            );
        }
        assert!(advertises(&s, UiKey::Esc));
        assert_eq!(decide(&mut s, UiKey::Esc), UsageKeyOutcome::Consumed);
        assert!(!s.cache_detail_open);
    }

    #[test]
    fn period_keys_are_case_insensitive() {
        for (lower, upper, period) in [
            ('t', 'T', Period::Today),
            ('w', 'W', Period::Week),
            ('m', 'M', Period::Month),
            ('a', 'A', Period::AllTime),
        ] {
            let mut s = DashboardState::default();
            assert_eq!(
                decide(&mut s, UiKey::Char(lower)),
                UsageKeyOutcome::Period(period)
            );
            assert_eq!(
                decide(&mut s, UiKey::Char(upper)),
                UsageKeyOutcome::Period(period)
            );
        }
    }

    #[test]
    fn unmodelled_keys_are_never_consumed() {
        let mut s = DashboardState::default();
        for key in [UiKey::Other, UiKey::Char('x'), UiKey::Char(' ')] {
            assert_eq!(decide(&mut s, key), UsageKeyOutcome::NotConsumed);
            assert!(!advertises(&s, key));
        }
    }

    #[test]
    fn advertising_matches_handling_on_every_card() {
        for card in 0..USAGE_CARD_COUNT {
            let s = state_on(card);
            let mut probe = s.clone();
            for key in [
                UiKey::Esc,
                UiKey::Tab,
                UiKey::BackTab,
                UiKey::Enter,
                UiKey::Char('t'),
                UiKey::Char('w'),
                UiKey::Char('m'),
                UiKey::Char('a'),
                UiKey::Other,
            ] {
                if advertises(&s, key) {
                    let mut fresh = probe.clone();
                    assert_ne!(
                        decide(&mut fresh, key),
                        UsageKeyOutcome::NotConsumed,
                        "card {card}: {key:?} is advertised but ignored"
                    );
                }
                probe = s.clone();
            }
        }
    }
}
