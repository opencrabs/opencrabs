//! Mission Control top-level dispatch — paints the backdrop, computes
//! the panel layout, calls each panel renderer, then overlays the help
//! bar (and detail popup, when open).

use super::layout::{self, McLayout};
use super::theme;
use super::{activity_panel, analytics_panel, detail_popup, inbox_panel, schedule_panel};

use crate::tui::app::App;
use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};
use crate::tui::app::mission_control::McPanel;
use crate::tui::render::hints;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Render Mission Control over the full content area `area`. Inherits
/// the terminal background — no dark wash — to match the `Sessions`
/// and `Help` screens.
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    // The log viewer is full-screen: it replaces the panels rather than
    // overlaying them, so nothing below it is drawn or laid out (#1528).
    if app.mc.log_viewer.is_some() {
        super::log_viewer::draw(frame, app, area);
        return;
    }

    let focus = app.mc.focused_panel;
    let McLayout {
        inbox,
        analytics,
        activity,
        schedule,
        help_bar,
        panels,
        too_small,
    } = layout::compute_focused(area, focus);

    if too_small {
        render_too_small(frame, panels);
        if help_bar.height > 0 {
            draw_help_bar(frame, app, help_bar);
        }
        return;
    }

    // A zero-height rect means "this panel is not on screen": in narrow
    // mode only the focused one is, so the rest are skipped rather than
    // drawn into slivers (#2074).
    if inbox.width > 0 {
        inbox_panel::draw(frame, app, inbox, focus == McPanel::Inbox);
    }
    if analytics.width > 0 {
        analytics_panel::draw(frame, app, analytics, focus == McPanel::Analytics);
    }
    if activity.width > 0 {
        activity_panel::draw(frame, app, activity, focus == McPanel::Activity);
    }
    if schedule.width > 0 {
        schedule_panel::draw(frame, app, schedule, focus == McPanel::Schedule);
    }

    if help_bar.height > 0 {
        draw_help_bar(frame, app, help_bar);
    }

    if app.mc.detail_open {
        detail_popup::draw(frame, app, area);
    }
}

/// The floor: one honest line naming the size the screen needs, instead of
/// panels rendered into slivers.
fn render_too_small(frame: &mut Frame, area: Rect) {
    let text = vec![
        Line::from(Span::styled(
            "Terminal too small for Mission Control",
            Style::default()
                .fg(theme::teal())
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "Needs at least {} columns and {} rows.",
                layout::MIN_WIDTH,
                layout::MIN_PANEL_ROWS + 2
            ),
            Style::default().fg(theme::text_dim()),
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
    frame.render_widget(
        Paragraph::new(text).alignment(ratatui::layout::Alignment::Center),
        rows[1],
    );
}

/// Bottom bar (2 rows): the canonical dialog footer from the scoped
/// keymap on the first row, the active analytics window on the second
/// (#1775, #900). Footer and input handler both answer to
/// `dialog_keys(MissionControl)`, and the drift test in
/// `dialog_keys.rs` replays every advertised event through `decide()`
/// to prove the pair cannot drift.
fn draw_help_bar(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    hints::render_footer(frame, rows[0], dialog_keys(DialogScope::MissionControl));

    let active = window_word(app.mc.analytics_window);
    let filter = Line::from(vec![
        Span::styled("active: ", theme::dim()),
        Span::styled(
            active,
            Style::default()
                .fg(theme::border_analytics_focus())
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    if rows.len() > 1 {
        frame.render_widget(Paragraph::new(filter), rows[1]);
    }
}

/// Human-readable window name for the commands bar (#900).
fn window_word(window: crate::brain::mission_control::TimeWindow) -> &'static str {
    use crate::brain::mission_control::TimeWindow as W;
    match window {
        W::Day => "Day",
        W::Week => "Week",
        W::Month => "Month",
        W::All => "All",
    }
}
