//! Usage dashboard layout and state
//!
//! A full-screen, responsive dashboard (#2073). Geometry lives in
//! [`super::layout`]; this module holds the state, the key decisions and
//! the draw call.

use super::cards;
use super::data::{DashboardData, Period};
use super::layout;
use crate::tui::app::dialog_keys::{DialogKey, DialogScope, dialog_keys};
use crate::tui::render::theme::{self, Role};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

/// Number of focusable cards. It is a layout fact (it decides the grid),
/// so it lives in [`super::layout`] and is re-exported here for the focus
/// maths and the tests.
pub(crate) use super::layout::CARD_COUNT;

/// Grid index of the Cache Efficiency card: 0=daily, 1=project, 2=model,
/// 3=tools, 4=activity, 5=cache.
pub(crate) const CACHE_CARD: usize = 5;

/// Dashboard UI state (lives in App)
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
        self.focused_card = (self.focused_card + 1) % CARD_COUNT;
    }

    pub fn focus_prev(&mut self) {
        self.focused_card = if self.focused_card == 0 {
            CARD_COUNT - 1
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
/// The dashboard's own state changes (focus moves, opening and closing the
/// breakdown) happen inside [`decide`]; the variants carry only what needs
/// the App: re-fetching after a period change, and leaving the mode. Splitting
/// it this way makes every key decision testable with no App, no database and
/// no terminal (#2027).
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

/// The dashboard's answer to `event`, applying the state changes that belong
/// to the decision itself.
///
/// The render side asks [`advertises`] which keys are live, so the footer a
/// user reads and the keys the handler honours come from the same rules and
/// cannot drift (#2027).
pub fn decide(state: &mut DashboardState, event: KeyEvent) -> UsageKeyOutcome {
    if state.cache_detail_open {
        // The breakdown sits over the grid: Esc backs out of it, and every
        // other key is deliberately inert so the dashboard behind it cannot
        // move while the prices are being read.
        if event.code == KeyCode::Esc {
            state.cache_detail_open = false;
        }
        return UsageKeyOutcome::Consumed;
    }

    match event.code {
        KeyCode::Esc => UsageKeyOutcome::Close,
        KeyCode::Tab => {
            state.focus_next();
            UsageKeyOutcome::Consumed
        }
        KeyCode::BackTab => {
            state.focus_prev();
            UsageKeyOutcome::Consumed
        }
        KeyCode::Enter if state.cache_card_focused() => {
            state.cache_detail_open = true;
            UsageKeyOutcome::Consumed
        }
        KeyCode::Char('t') | KeyCode::Char('T') => UsageKeyOutcome::Period(Period::Today),
        KeyCode::Char('w') | KeyCode::Char('W') => UsageKeyOutcome::Period(Period::Week),
        KeyCode::Char('m') | KeyCode::Char('M') => UsageKeyOutcome::Period(Period::Month),
        KeyCode::Char('a') | KeyCode::Char('A') => UsageKeyOutcome::Period(Period::AllTime),
        _ => UsageKeyOutcome::NotConsumed,
    }
}

/// Does `code` do, in this state, what the footer says it does?
///
/// Deliberately narrower than [`decide`]: with the breakdown open every key is
/// consumed but only Esc performs an action, and a key that performs nothing
/// must not be advertised.
pub fn advertises(state: &DashboardState, code: KeyCode) -> bool {
    if state.cache_detail_open {
        return code == KeyCode::Esc;
    }
    match code {
        KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => true,
        KeyCode::Char('t' | 'T' | 'w' | 'W' | 'm' | 'M' | 'a' | 'A') => true,
        // Enter opens the breakdown, so it is live only where there is one
        // to open (#2027).
        KeyCode::Enter => state.cache_card_focused(),
        _ => false,
    }
}

/// The dashboard's footer entries for this state: the scoped keymap filtered
/// to the keys that are live.
///
/// The rendered footer and the drift tests both come through here, so what a
/// user is told about and what the handler honours are decided by
/// [`advertises`] alone (#2027). Entries are handed back by value
/// ([`DialogKey`] is `Copy`) so the footer takes them as a plain slice.
pub fn advertised_keys(state: &DashboardState) -> Vec<DialogKey> {
    dialog_keys(DialogScope::UsageDashboard)
        .iter()
        .filter(|key| key.events.iter().any(|(code, _)| advertises(state, *code)))
        .copied()
        .collect()
}

/// Render the usage dashboard across the whole content area.
///
/// The screen is full width now, so the grid reflows with the terminal
/// instead of being pinned to a 2x2 (#2073):
/// ```text
/// [Summary Bar ────────────────────── full width]
/// [Daily] [By Project] [By Model]
/// [Tools] [By Activity] [Cache]
/// [Footer ─────────────────────────── full width]
/// ```
/// Below the floor the layout reports `too_small` and one honest line
/// replaces the grid, instead of cards clipped narrower than their text.
pub fn render(f: &mut Frame, state: &DashboardState, area: Rect) {
    // The outer border is the screen frame now, not dialog chrome.
    let border = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::role(Role::AccentTeal)))
        .title(Span::styled(
            " Usage Dashboard ",
            Style::default()
                .fg(theme::role(Role::AccentTeal))
                .add_modifier(Modifier::BOLD),
        ));
    let inner = border.inner(area);
    f.render_widget(border, area);

    let layout = layout::compute(inner);

    if layout.too_small {
        render_too_small(f, inner);
        return;
    }

    // Summary bar
    cards::render_summary(f, &state.data, layout.summary, state.period.label());

    // The six cards, in focus order. Column count and row count come from
    // the layout, so the grid reflows with the terminal.
    cards::render_daily(
        f,
        &state.data.daily,
        layout.cards[0],
        state.focused_card == 0,
    );
    cards::render_projects(
        f,
        &state.data.projects,
        layout.cards[1],
        state.focused_card == 1,
    );
    cards::render_models(
        f,
        &state.data.models,
        layout.cards[2],
        state.focused_card == 2,
    );
    cards::render_tools(
        f,
        &state.data.tools,
        layout.cards[3],
        state.focused_card == 3,
    );
    cards::render_activities(
        f,
        &state.data.activities,
        layout.cards[4],
        state.focused_card == 4,
    );
    cards::render_cache_efficiency(
        f,
        &state.data.cache,
        layout.cards[5],
        state.cache_card_focused(),
    );

    // Footer: the keys that are live in this state (#2027).
    cards::render_footer(f, layout.footer, state);

    // The price breakdown, if Enter opened it. Drawn last so it covers the
    // grid, and sized to stop one row above the footer so the key that
    // dismisses it stays on screen.
    if state.cache_detail_open {
        let top = layout.cards[0].y;
        let overlay = Rect {
            x: layout.summary.x,
            y: top,
            width: layout.summary.width,
            height: layout.footer.y.saturating_sub(top),
        };
        cards::render_cache_breakdown(f, &state.data.cache, overlay);
    }
}

/// The floor: one honest line naming the size the screen needs, instead of
/// cards squeezed narrower than their own text.
fn render_too_small(f: &mut Frame, area: Rect) {
    let text = vec![
        Line::from(Span::styled(
            "Terminal too small for the usage dashboard",
            Style::default()
                .fg(theme::role(Role::AccentTeal))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "Needs at least {} columns and {} rows.",
                layout::MIN_WIDTH,
                layout::required_height(1)
            ),
            Style::default().fg(theme::role(Role::TextDim)),
        )),
    ];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(text.len() as u16),
            Constraint::Min(0),
        ])
        .split(area);
    f.render_widget(Paragraph::new(text).alignment(Alignment::Center), rows[1]);
}
