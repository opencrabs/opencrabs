//! Usage dashboard layout and state
//!
//! A full-screen, responsive dashboard (#2073). Geometry lives in
//! [`super::layout`]; the key decisions live in
//! `crate::ui_core::usage::input` as terminal-neutral logic shared with any
//! future GUI, and the crossterm translation sits in [`super::keymap`].
//! This module holds the draw call only.

use super::cards;
use super::layout;
use crate::tui::render::theme::{self, Role};
use crate::ui_core::usage::view_model::UsageViewModel;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

/// The dashboard's state and its key decisions are terminal-neutral; the
/// TUI-facing signatures that take crossterm events live in
/// [`super::keymap`]. Both are re-exported so existing callers keep their
/// `crate::usage::dashboard::` paths.
pub use super::keymap::{advertised_keys, advertises, decide};
pub use crate::ui_core::usage::input::{DashboardState, UsageKeyOutcome};

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

    // One prepared view per draw: all presentation decisions (cells,
    // markers, ratios, bands) come out of the shared view model, so a GUI
    // rendering the same model cannot drift from this screen.
    let view = UsageViewModel::build(&state.data, state.period);

    // Summary bar
    cards::render_summary(f, &view, layout.summary);

    // The six cards, in focus order. Column count and row count come from
    // the layout, so the grid reflows with the terminal.
    cards::render_daily(f, &view.daily, layout.cards[0], state.focused_card == 0);
    cards::render_projects(f, &view.projects, layout.cards[1], state.focused_card == 1);
    cards::render_models(f, &view.models, layout.cards[2], state.focused_card == 2);
    cards::render_tools(f, &view.tools, layout.cards[3], state.focused_card == 3);
    cards::render_activities(
        f,
        &view.activities,
        layout.cards[4],
        state.focused_card == 4,
    );
    cards::render_cache_efficiency(
        f,
        view.cache.as_ref(),
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
        cards::render_cache_breakdown(f, view.cache.as_ref(), overlay);
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
