//! Analytics panel (full-height right column). Brain sizes plus tool usage /
//! reliability, RSI application counts, and the #897 provider/model-agnostic
//! stats (phantom detection, streaming recoveries, brain-verify gate, per-model
//! reliability), read from the cached `app.mc.analytics` snapshot. Read-only:
//! the renderer never hits disk or the DB itself.
//!
//! This is the native home for what the external `opencrabs-analytics` HTML
//! tool produced (discussion #178).
//!
//! Layout (#900): the D/W/M/All filter tabs are pinned on the top line, and
//! the sections below render as individual bordered cards flowed into a
//! responsive grid — 3 columns when the column is wide, 2 when medium, 1 when
//! narrow. Each non-empty section is one card; empty sections are skipped so
//! the grid stays tight. Cards fill the viewport and clip their own overflow
//! (dashboard style, like btop/htop) rather than scrolling as one text block.
//! Card order: Totals (+ recovery/verify counters), Model reliability (with a
//! per-model ✓/⚠/✗ status glyph), Flakiest, Phantoms, RSI applied, Brain
//! files, Top tools.

use super::super::theme::{self as render_theme, Role};
use super::theme;
use crate::brain::mission_control::{McAnalytics, McToolStat, TimeWindow};
use crate::tui::app::App;
use crate::tui::render::responsive::grid_cols;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

pub fn draw(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    render(
        frame,
        &app.mc.analytics,
        app.mc.analytics_window,
        app.mc.scroll_offset,
        area,
        focused,
    );
}

/// Render the analytics view (block + pinned filter tabs + responsive card
/// grid) into any rect. Shared by the Mission Control panel and the Enter
/// detail popup, so the popup is the same rich view, just larger.
///
/// `_scroll` is kept in the signature for the shared caller contract but is
/// unused: the card grid fills the viewport and clips per card, so there is
/// no whole-panel scroll offset to apply (#900).
pub(crate) fn render(
    frame: &mut Frame,
    a: &McAnalytics,
    window: TimeWindow,
    _scroll: u16,
    area: Rect,
    focused: bool,
) {
    let border_color = if focused {
        theme::border_analytics_focus()
    } else {
        theme::border_idle()
    };
    let block = Block::default()
        .title(" Analytics ")
        .title_style(theme::title_style(theme::border_analytics_focus()))
        .borders(Borders::ALL)
        .border_set(symbols::border::ROUNDED)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Pin the D/W/M/All filter tabs on the top line; the card grid fills the
    // space beneath them (#900).
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    frame.render_widget(Paragraph::new(tabs_line(window)), rows[0]);
    let body = rows[1];

    let sections = active_sections(a, window);
    if sections.is_empty() || body.height < 3 || body.width < 4 {
        return;
    }

    let cols = grid_cols(body.width);
    let nrows = sections.len().div_ceil(cols);
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Ratio(1, nrows as u32); nrows])
        .split(body);

    for (i, (title, kind)) in sections.iter().enumerate() {
        let r = i / cols;
        let c = i % cols;
        let col_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Ratio(1, cols as u32); cols])
            .split(row_areas[r]);
        render_card(frame, title, *kind, a, col_areas[c]);
    }
}

/// The non-empty sections in display order, each paired with its card kind.
/// Totals always renders; the rest appear only when they have data, so the
/// grid stays tight (Phantoms, for example, hides itself until there is a
/// detection to show).
fn active_sections(a: &McAnalytics, window: TimeWindow) -> Vec<(String, CardKind)> {
    let mut out: Vec<(String, CardKind)> = vec![("Totals".to_string(), CardKind::Totals)];
    if !a.model_tools.is_empty() {
        out.push(("Model reliability".to_string(), CardKind::Model));
    }
    if !a.flakiest_tools.is_empty() {
        out.push((
            format!("Flakiest ({})", window_label(window)),
            CardKind::Flakiest,
        ));
    }
    if a.phantom.total > 0 {
        out.push(("Phantoms".to_string(), CardKind::Phantom));
    }
    if !a.rsi_top_dimensions.is_empty() {
        out.push(("RSI applied".to_string(), CardKind::Rsi));
    }
    if !a.brain_files.is_empty() {
        out.push(("Brain files".to_string(), CardKind::Brain));
    }
    if !a.top_tools.is_empty() {
        out.push(("Top tools".to_string(), CardKind::TopTools));
    }
    out
}

/// One bordered card: the section title sits in the border, the body lines
/// fill the interior and clip if they overflow the card's height.
fn render_card(frame: &mut Frame, title: &str, kind: CardKind, a: &McAnalytics, rect: Rect) {
    if rect.width < 4 || rect.height < 3 {
        return;
    }
    let block = Block::default()
        .title(format!(" {title} "))
        .title_style(
            Style::default()
                .fg(theme::border_analytics_focus())
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_set(symbols::border::ROUNDED)
        .border_style(Style::default().fg(theme::border_idle()));
    let card_inner = block.inner(rect);
    frame.render_widget(block, rect);

    let w = card_inner.width as usize;
    let lines = card_body(kind, a, w);
    frame.render_widget(Paragraph::new(lines), card_inner);
}

/// Which section a card renders.
#[derive(Clone, Copy)]
enum CardKind {
    Totals,
    Model,
    Flakiest,
    Phantom,
    Rsi,
    Brain,
    TopTools,
}

fn card_body(kind: CardKind, a: &McAnalytics, w: usize) -> Vec<Line<'static>> {
    match kind {
        CardKind::Totals => summary_body(a),
        CardKind::Model => model_body(a, w),
        CardKind::Flakiest => flakiest_body(a, w),
        CardKind::Phantom => phantom_body(a, w),
        CardKind::Rsi => rsi_body(a, w),
        CardKind::Brain => brain_body(a, w),
        CardKind::TopTools => top_tools_body(a, w),
    }
}

/// D/W/M/All filter tabs, active tab highlighted in the accent color (#900).
/// The hotkeys themselves live in the Mission Control footer table (#1775),
/// so no trailing hint is needed here — the footer already says D/W/M/A.
fn tabs_line(active: TimeWindow) -> Line<'static> {
    let tabs = [
        (TimeWindow::Day, "D"),
        (TimeWindow::Week, "W"),
        (TimeWindow::Month, "M"),
        (TimeWindow::All, "All"),
    ];
    let mut spans = vec![Span::raw(" ")];
    for (i, (window, label)) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let style = if *window == active {
            Style::default()
                .fg(theme::border_analytics_focus())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text_dim())
        };
        spans.push(Span::styled(format!("[{label}]"), style));
    }
    Line::from(spans)
}

/// Totals card: calls / fails / recovery + verify counters / RSI / brain.
fn summary_body(a: &McAnalytics) -> Vec<Line<'static>> {
    let fail_pct = if a.tool_total_calls > 0 {
        (a.tool_total_fails as f64 / a.tool_total_calls as f64) * 100.0
    } else {
        0.0
    };
    vec![
        summary_row("Tools", format!("{} calls", a.tool_total_calls)),
        summary_row(
            "Fails",
            format!("{} ({:.1}%)", a.tool_total_fails, fail_pct),
        ),
        summary_row("Recov", format!("{} recovered", a.streaming.total)),
        summary_row(
            "Verify",
            format!(
                "{} ok / {} rollbk",
                a.brain_verify.passes, a.brain_verify.rollbacks
            ),
        ),
        summary_row("RSI", format!("{} applied", a.rsi_applied_total)),
        summary_row(
            "RSI live",
            crate::brain::mission_control::staleness::rsi_staleness_line(
                chrono::Utc::now().timestamp(),
                a.rsi_last_call_ts,
                a.tool_events_since_rsi,
            ),
        ),
        summary_row(
            "Brain",
            format!("{:.1} KB / {} files", a.brain_total_kb, a.brain_files.len()),
        ),
    ]
}

/// Model reliability card: per-model fail rate + phantom rate, most calls
/// first, each row prefixed with a ✓/⚠/✗ status glyph (#900). Provider-agnostic
/// — shows the model name.
fn model_body(a: &McAnalytics, w: usize) -> Vec<Line<'static>> {
    a.model_tools
        .iter()
        .take(8)
        .map(|m| {
            // Join the per-model phantom count onto the model's call volume; a
            // model with no recorded phantoms reads as 0%.
            let phantoms = a
                .phantom
                .by_model
                .iter()
                .find(|(name, _, _)| name == &m.model)
                .map(|(_, total, _)| *total)
                .unwrap_or(0);
            let phantom_rate = if m.total > 0 {
                (phantoms as f64 / m.total as f64) * 100.0
            } else {
                0.0
            };
            model_row(&m.model, m.fail_rate, phantom_rate, w)
        })
        .collect()
}

/// Flakiest card: highest failure rates, values right-aligned to the card
/// width. The window label lives in the card title (#900).
fn flakiest_body(a: &McAnalytics, w: usize) -> Vec<Line<'static>> {
    a.flakiest_tools
        .iter()
        .take(8)
        .map(|t| {
            value_row(
                &t.name,
                format!("{:.1}%", t.fail_rate),
                fail_color(t.fail_rate),
                w,
            )
        })
        .collect()
}

/// Phantoms card: detected / resolved / ratio plus a per-model breakdown (#900).
fn phantom_body(a: &McAnalytics, w: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!(
                "{} detected, {} resolved ({:.1}%)",
                a.phantom.total, a.phantom.resolved, a.phantom.resolved_pct
            ),
            Style::default().fg(theme::green()),
        ),
    ])];
    for (model, total, resolved) in a.phantom.by_model.iter().take(6) {
        lines.push(value_row(
            model,
            format!("{total} ({resolved} resolved)"),
            theme::text_secondary(),
            w,
        ));
    }
    lines
}

/// RSI-by-dimension card, counts right-aligned to the card width.
fn rsi_body(a: &McAnalytics, w: usize) -> Vec<Line<'static>> {
    a.rsi_top_dimensions
        .iter()
        .take(8)
        .map(|(dim, n)| value_row(dim, n.to_string(), theme::green(), w))
        .collect()
}

/// Brain-files card, sizes right-aligned to the card width.
fn brain_body(a: &McAnalytics, w: usize) -> Vec<Line<'static>> {
    a.brain_files
        .iter()
        .take(14)
        .map(|f| {
            value_row(
                &f.name,
                format!("{:.1} KB", f.kb),
                theme::text_secondary(),
                w,
            )
        })
        .collect()
}

/// Top tools card, with proportional bars scaled to the card width.
fn top_tools_body(a: &McAnalytics, w: usize) -> Vec<Line<'static>> {
    let max = a
        .top_tools
        .iter()
        .map(|t| t.total)
        .max()
        .unwrap_or(1)
        .max(1);
    a.top_tools
        .iter()
        .take(10)
        .map(|t| tool_bar_row(t, max, w))
        .collect()
}

fn summary_row(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!("{label:<6}"),
            Style::default().fg(theme::text_dim()),
        ),
        Span::styled(value, Style::default().fg(theme::green())),
    ])
}

/// `✓ model<spaces>fail X%  ph Y%` with the values flush against the right
/// edge. The leading glyph summarises the model's health (#900).
fn model_row(model: &str, fail_rate: f64, phantom_rate: f64, w: usize) -> Line<'static> {
    let (icon, icon_color) = model_status(fail_rate, phantom_rate);
    let fail = format!("fail {:.1}%", fail_rate);
    let ph = format!("  ph {:.1}%", phantom_rate);
    let value_len = fail.chars().count() + ph.chars().count();
    // Leading " ✓ " takes ~3 cells; reserve room for the icon plus gaps.
    let name_room = w.saturating_sub(value_len + 5);
    let name = trunc(model, name_room.max(4));
    let gap = w
        .saturating_sub(3 + name.chars().count() + value_len + 1)
        .max(1);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(icon, Style::default().fg(icon_color)),
        Span::raw(" "),
        Span::styled(name, Style::default().fg(theme::text_primary())),
        Span::raw(" ".repeat(gap)),
        Span::styled(fail, Style::default().fg(fail_color(fail_rate))),
        Span::styled(ph, Style::default().fg(theme::text_secondary())),
    ])
}

/// Health glyph for a model row (#900): ✓ healthy, ⚠ warning, ✗ bad. A model
/// is bad at a >=15% fail rate or a >=10% phantom rate; warning at >=5% fails
/// or any phantoms; healthy otherwise.
fn model_status(fail_rate: f64, phantom_rate: f64) -> (&'static str, Color) {
    if fail_rate >= 15.0 || phantom_rate >= 10.0 {
        ("✗", render_theme::role(Role::Error))
    } else if fail_rate >= 5.0 || phantom_rate > 0.0 {
        ("⚠", theme::orange())
    } else {
        ("✓", theme::green())
    }
}

/// `name  ███████▌ count rate%` with the bar filling the column width.
fn tool_bar_row(t: &McToolStat, max: i64, w: usize) -> Line<'static> {
    let name_w = 12usize;
    let count_w = 8usize; // "  24304 " style trailing block
    // Whatever is left after the name, count, and a 4% rate column is the bar.
    let bar_w = w.saturating_sub(name_w + count_w + 6).clamp(4, 60);
    let filled = ((t.total as f64 / max as f64) * bar_w as f64).round() as usize;
    let bar: String = "█".repeat(filled.min(bar_w));
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            pad(&trunc(&t.name, name_w), name_w),
            Style::default().fg(theme::text_primary()),
        ),
        Span::styled(
            format!("{bar:<bar_w$}"),
            Style::default().fg(theme::green()),
        ),
        Span::styled(
            format!("{:>6}", t.total),
            Style::default().fg(theme::text_secondary()),
        ),
        Span::styled(
            format!(" {:>3.0}%", t.fail_rate),
            Style::default().fg(fail_color(t.fail_rate)),
        ),
    ])
}

/// `name<spaces>value` with `value` flush against the right edge of width `w`.
fn value_row(name: &str, value: String, value_color: Color, w: usize) -> Line<'static> {
    let value_len = value.chars().count();
    let name_room = w.saturating_sub(value_len + 3);
    let name = trunc(name, name_room.max(4));
    let gap = w
        .saturating_sub(name.chars().count() + value_len + 2)
        .max(1);
    Line::from(vec![
        Span::raw("  "),
        Span::styled(name, Style::default().fg(theme::text_primary())),
        Span::raw(" ".repeat(gap)),
        Span::styled(value, Style::default().fg(value_color)),
    ])
}

fn window_label(window: TimeWindow) -> &'static str {
    match window {
        TimeWindow::Day => "24h",
        TimeWindow::Week => "7d",
        TimeWindow::Month => "30d",
        TimeWindow::All => "all-time",
    }
}

fn fail_color(rate: f64) -> Color {
    if rate >= 25.0 {
        render_theme::role(Role::Error)
    } else if rate >= 10.0 {
        theme::orange()
    } else {
        theme::text_secondary()
    }
}

fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

fn trunc(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
