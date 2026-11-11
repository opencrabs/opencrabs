//! Mission Control: the whole frame, its cards, and the detail popup.
//!
//! Everything here reads the snapshot the four services produced when the
//! screen opened and draws it. A card that invents a row is worse than one
//! that admits it has none (#2426), so every panel takes plain data and says
//! what it has. The grid reflows with the pane (#2459): the terminal draws a
//! fixed 2x2 and hides what does not fit, a window has room for all of it, so
//! the column count follows the width it actually has.
//!
//! Moved out of the crate root (#2817 follow-up) as pure code motion: the
//! functions and their comments are unchanged, only the module boundary is
//! new.

use gpui::prelude::*;
use gpui::{App, ClickEvent, Div, IntoElement, SharedString, Stateful, Window, div, px, rgb, rgba};

use opencrabs::brain::mission_control::{McActivity, McAnalytics, McInboxItem, McScheduleItem};
use opencrabs::tui::app::mission_control::log_viewer::LogViewerState;
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};

use crate::mission_control;
use crate::palette;
use crate::theme::{Size, text};

/// One Mission Control frame: a responsive grid of cards drawn from the
/// snapshot the four services produced when the screen opened. Every row here
/// was read from a service; a quiet instance says so, and a failed read says
/// that instead, because a card that invents a row is worse than one that
/// admits it has none (#2426).
///
/// The grid reflows with the pane (#2459). The terminal draws a fixed 2x2 and
/// hides the panels that do not fit; a window has the room to draw all of
/// them, so the column count follows the width it actually has and a resize
/// never pushes a card out of the frame. Each card is sized by the grid and
/// clips its own rows: a twenty-five item inbox must not decide how tall the
/// screen is.
pub(crate) fn mission_control_body(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    err: Option<&SharedString>,
    loading: bool,
    shown: palette::Palette,
    cols: usize,
) -> impl IntoElement {
    let cards = mission_control_cards(snapshot, nav, shown);
    let count = cards.len();
    let mut grid = div()
        .id("mission-control-body")
        .flex()
        .flex_col()
        .flex_1()
        .w_full()
        .min_w_0()
        .min_h_0()
        .gap_2()
        .px_4()
        .pb_2();
    if let Some(err) = err {
        grid = grid.child(text(format!("snapshot failed: {err}"), Size::Xs, shown.red));
    } else if loading && snapshot.is_none() {
        grid = grid.child(text("reading the services…", Size::Xs, shown.text_muted));
    }
    let mut cards = cards.into_iter();
    for slots in mission_control::grid_rows(count, cols) {
        let mut row = div()
            .flex()
            .flex_row()
            .flex_1()
            .w_full()
            .min_w_0()
            .min_h_0()
            .gap_2();
        for slot in slots {
            match slot {
                Some(_) => {
                    if let Some(card) = cards.next() {
                        row = row.child(card);
                    }
                }
                // An empty slot holds the last row's cards to the same width
                // as the rows above instead of letting one card stretch over
                // the whole pane.
                None => row = row.child(div().flex_1().min_w_0().min_h_0()),
            }
        }
        grid = grid.child(row);
    }
    grid
}

/// The cards the screen can show, in reading order: the three lists the
/// terminal keeps on its left, then the analytics breakdown, one card per
/// section (#2459). An analytics section with nothing in it is skipped, so a
/// new instance shows four cards rather than seven empty boxes.
fn mission_control_cards(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    shown: palette::Palette,
) -> Vec<Div> {
    let mut cards = vec![
        mc_inbox_panel(snapshot, nav, shown),
        mc_activity_panel(snapshot, nav, shown),
        mc_schedule_panel(snapshot, nav, shown),
    ];
    cards.extend(mc_analytics_cards(
        snapshot,
        nav.panel == mission_control::Panel::Analytics,
        shown,
    ));
    cards
}

/// The analytics sections shared by the dashboard and its detail popup.
/// Reusing this list keeps the popup from becoming a second, stale renderer.
fn mc_analytics_cards(
    snapshot: Option<&mission_control::Snapshot>,
    focused: bool,
    shown: palette::Palette,
) -> Vec<Div> {
    let analytics = snapshot.map(|s| &s.analytics);
    let mut cards = vec![mc_totals_panel(snapshot, focused, shown)];
    let Some(a) = analytics else {
        return cards;
    };
    if !a.model_tools.is_empty() {
        cards.push(mc_models_panel(a, shown));
    }
    if !a.flakiest_tools.is_empty() {
        cards.push(mc_flakiest_panel(
            a,
            snapshot.map(|s| s.window).unwrap_or_default(),
            shown,
        ));
    }
    if a.phantom.total > 0 {
        cards.push(mc_phantoms_panel(a, shown));
    }
    if !a.rsi_top_dimensions.is_empty() {
        cards.push(mc_rsi_panel(a, shown));
    }
    if !a.brain_files.is_empty() {
        cards.push(mc_brain_panel(a, shown));
    }
    if !a.top_tools.is_empty() {
        cards.push(mc_top_tools_panel(a, shown));
    }
    cards
}

/// One row inside a card. The lists stay one truncated line each; the
/// analytics numbers are a label and a value, so the value sits flush right
/// and the label ellipsizes before it, which keeps the columns readable at any
/// card width.
///
/// `pub(crate)` because the phantom card's rows cross the module boundary: a
/// helper that hands back `Vec<McRow>` is reachable from the crate root, so
/// the row type has to be too or the lint gate stops the build.
pub(crate) enum McRow {
    /// A whole line, truncated when the card is narrower than it.
    Line(String, u32),
    /// A label on the left, a value flush against the right.
    Pair(String, String, u32),
}

impl McRow {
    fn color(&self) -> u32 {
        match self {
            Self::Line(_, color) | Self::Pair(_, _, color) => *color,
        }
    }
}

/// One card: a title, then the rows the services produced. The card is a grid
/// cell first, so it takes the width and height the grid gives it and never
/// grows to fit its content: `min_w_0` stops a long shell command from
/// widening its column, `min_h_0` plus `overflow_hidden` stop a long list from
/// pushing the row below it out of the frame, and the row area scrolls when
/// there is more than the card can show (#2459).
fn mc_panel(
    id: &'static str,
    title: String,
    rows: Vec<McRow>,
    empty: &'static str,
    focused: bool,
    selected: Option<usize>,
    shown: palette::Palette,
) -> Div {
    let title_color = if focused {
        shown.focus
    } else {
        shown.text_title
    };
    let mut rows_view = div()
        .id(SharedString::from(format!("mc-rows-{id}")))
        .flex()
        .flex_col()
        .flex_1()
        .w_full()
        .min_w_0()
        .min_h_0()
        .gap_1()
        .overflow_y_scroll();
    if rows.is_empty() {
        rows_view = rows_view.child(text(empty, Size::Xs, shown.text_faint));
    }
    for (index, row) in rows.into_iter().enumerate() {
        let armed = selected == Some(index);
        let color = if armed { shown.focus } else { row.color() };
        let mut cell = div().w_full().min_w_0().text_color(rgb(color));
        if armed {
            cell = cell.bg(rgb(shown.selection)).rounded(px(4.0)).px_1();
        }
        match row {
            // A line is laid out through an explicit flex row with a growing,
            // shrinkable text child (#2642): a truncated text element inside a
            // plain width-full block div is measured once at the grid's
            // flex-basis zero and stays an ellipsis forever (gpui caches the
            // nowrap layout on wrap_width, which never changes here). The
            // label/value arm below already uses this shape and renders.
            McRow::Line(line, _) => {
                cell = cell
                    .flex()
                    .flex_row()
                    .child(text(line, Size::Xs, color).flex_1().min_w_0().truncate());
            }
            McRow::Pair(label, value, _) => {
                cell = cell
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(text(label, Size::Xs, color).flex_1().min_w_0().truncate())
                    .child(text(value, Size::Xs, color).flex_none());
            }
        }
        rows_view = rows_view.child(cell);
    }
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        // The focused card wears the ring the rest of the window uses, so the
        // keyboard's target is never a guess.
        .border_color(rgb(if focused { shown.focus } else { shown.panel_hi }))
        .bg(glass_fill(shown.panel))
        .child(text(title, Size::Sm, title_color))
        .child(rows_view)
}

/// The RSI proposals nobody has decided yet: kind and label, then the summary
/// the agent wrote for it. Six fit the card and the window slides with the
/// selection instead of hiding whatever fell past the sixth, so `j` never walks
/// off the screen.
fn mc_inbox_panel(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    shown: palette::Palette,
) -> Div {
    let items = snapshot.map(|s| s.inbox.as_slice()).unwrap_or_default();
    let selected = nav.selected.min(items.len().saturating_sub(1));
    let (start, len) = mission_control::visible_window(items.len(), Some(selected), 6);
    let mut rows = Vec::new();
    for item in items.iter().skip(start).take(len) {
        rows.push(McRow::Line(
            format!("{} \u{b7} {}", item.kind.label(), item.label),
            shown.text_normal,
        ));
        rows.push(McRow::Line(format!("  {}", item.summary), shown.text_faint));
        rows.push(McRow::Line(
            format!(
                "  {} \u{b7} {}",
                item.source,
                mc_relative_time(Some(item.created_at))
            ),
            shown.text_faint,
        ));
    }
    let focused = nav.panel == mission_control::Panel::Inbox;
    mc_panel(
        "inbox",
        format!("Inbox ({})", items.len()),
        rows,
        "No pending proposals.",
        focused,
        // The selected item's first line, in the rows actually drawn.
        (focused && !items.is_empty()).then(|| (selected - start) * 3),
        shown,
    )
}

/// The analytics tab line: the terminal's D/W/M/All, the active one bracketed.
fn mc_window_tabs(window: mission_control::TimeWindow) -> String {
    use mission_control::TimeWindow;
    [
        (TimeWindow::Day, "D"),
        (TimeWindow::Week, "W"),
        (TimeWindow::Month, "M"),
        (TimeWindow::All, "All"),
    ]
    .iter()
    .map(|(w, label)| {
        if *w == window {
            format!("[{label}]")
        } else {
            (*label).to_string()
        }
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// The label the flakiest card carries, the same words the terminal uses.
fn mc_window_label(window: mission_control::TimeWindow) -> &'static str {
    use mission_control::TimeWindow;
    match window {
        TimeWindow::Day => "24h",
        TimeWindow::Week => "7d",
        TimeWindow::Month => "30d",
        TimeWindow::All => "all-time",
    }
}

/// A failure rate's colour at both ends of the scale, so the flakiest card and
/// the model card agree about what "bad" looks like.
fn mc_rate_color(rate: f64, shown: palette::Palette) -> u32 {
    if rate >= 25.0 {
        shown.red
    } else if rate >= 10.0 {
        shown.warn
    } else {
        shown.text_muted
    }
}

/// Usage and reliability for the selected window: the filter tabs, the totals
/// the terminal leads with, the RSI liveness line (#469), and the brain size.
fn mc_totals_panel(
    snapshot: Option<&mission_control::Snapshot>,
    focused: bool,
    shown: palette::Palette,
) -> Div {
    let mut rows: Vec<McRow> = Vec::new();
    if let Some(snap) = snapshot {
        let a = &snap.analytics;
        let fail_pct = if a.tool_total_calls > 0 {
            (a.tool_total_fails as f64 / a.tool_total_calls as f64) * 100.0
        } else {
            0.0
        };
        // The snapshot carries the window it was read for; the tabs show that
        // one, so a slow read can never label November numbers as September's.
        rows.push(McRow::Line(mc_window_tabs(snap.window), shown.teal));
        rows.push(McRow::Pair(
            "Tools".to_string(),
            format!("{} calls", a.tool_total_calls),
            shown.text_normal,
        ));
        rows.push(McRow::Pair(
            "Fails".to_string(),
            format!("{} ({fail_pct:.1}%)", a.tool_total_fails),
            if a.tool_total_fails > 0 {
                shown.warn
            } else {
                shown.text_muted
            },
        ));
        rows.push(McRow::Pair(
            "Recov".to_string(),
            format!("{} recovered", a.streaming.total),
            shown.text_muted,
        ));
        rows.push(McRow::Pair(
            "Verify".to_string(),
            format!(
                "{} ok / {} rollbk",
                a.brain_verify.passes, a.brain_verify.rollbacks
            ),
            shown.text_muted,
        ));
        rows.push(McRow::Pair(
            "RSI".to_string(),
            format!("{} applied", a.rsi_applied_total),
            shown.text_muted,
        ));
        rows.push(McRow::Pair(
            "RSI live".to_string(),
            opencrabs::brain::mission_control::staleness::rsi_staleness_line(
                chrono::Utc::now().timestamp(),
                a.rsi_last_call_ts,
                a.tool_events_since_rsi,
            ),
            shown.text_faint,
        ));
        rows.push(McRow::Pair(
            "Brain".to_string(),
            format!("{:.1} KB / {} files", a.brain_total_kb, a.brain_files.len()),
            shown.text_muted,
        ));
    }
    mc_panel(
        "totals",
        "Totals".to_string(),
        rows,
        "No numbers yet.",
        focused,
        None,
        shown,
    )
}

/// Per-model tool reliability, most calls first, each row carrying the
/// terminal's ✓/⚠/✗ health glyph and its phantom rate (#900).
fn mc_models_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    let rows = a
        .model_tools
        .iter()
        .take(8)
        .map(|m| {
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
            let (glyph, color) = if m.fail_rate >= 15.0 || phantom_rate >= 10.0 {
                ("\u{2717}", shown.red)
            } else if m.fail_rate >= 5.0 || phantom_rate > 0.0 {
                ("\u{26a0}", shown.warn)
            } else {
                ("\u{2713}", shown.green)
            };
            McRow::Pair(
                m.model.clone(),
                format!(
                    "{glyph} {:.1}% fail \u{b7} {:.1}% ph",
                    m.fail_rate, phantom_rate
                ),
                color,
            )
        })
        .collect();
    mc_panel(
        "models",
        "Model reliability".to_string(),
        rows,
        "No models yet.",
        false,
        None,
        shown,
    )
}

/// Highest failure rates, the window named in the title the way the terminal
/// names it.
fn mc_flakiest_panel(
    a: &McAnalytics,
    window: mission_control::TimeWindow,
    shown: palette::Palette,
) -> Div {
    let rows = a
        .flakiest_tools
        .iter()
        .take(8)
        .map(|t| {
            McRow::Pair(
                t.name.clone(),
                format!("{:.1}%", t.fail_rate),
                mc_rate_color(t.fail_rate, shown),
            )
        })
        .collect();
    mc_panel(
        "flakiest",
        format!("Flakiest ({})", mc_window_label(window)),
        rows,
        "No flaky tools.",
        false,
        None,
        shown,
    )
}

/// Rows for the phantom card. An empty model breakdown stays empty and lets
/// the shared panel render its empty state instead. (#2642 fixed the line
/// rows that used to ellipsize into three dots under the header; this guard
/// is about the data, not the rendering.)
pub(crate) fn mc_phantoms_rows(a: &McAnalytics, shown: palette::Palette) -> Vec<McRow> {
    if a.phantom.by_model.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![McRow::Line(
        format!(
            "{} detected, {} resolved ({:.1}%)",
            a.phantom.total, a.phantom.resolved, a.phantom.resolved_pct
        ),
        shown.green,
    )];
    for (model, total, resolved) in a.phantom.by_model.iter().take(6) {
        rows.push(McRow::Pair(
            model.clone(),
            format!("{total} ({resolved} resolved)"),
            shown.text_muted,
        ));
    }
    rows
}

/// Phantom tool calls: detected, resolved, then the per-model breakdown the
/// terminal shows.
fn mc_phantoms_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    mc_panel(
        "phantoms",
        "Phantoms".to_string(),
        mc_phantoms_rows(a, shown),
        "No phantoms detected.",
        false,
        None,
        shown,
    )
}

/// RSI improvements applied, counted per dimension.
fn mc_rsi_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    let rows = a
        .rsi_top_dimensions
        .iter()
        .take(8)
        .map(|(dim, n)| McRow::Pair(dim.clone(), n.to_string(), shown.green))
        .collect();
    mc_panel(
        "rsi",
        "RSI applied".to_string(),
        rows,
        "Nothing applied yet.",
        false,
        None,
        shown,
    )
}

/// The brain files and their sizes, largest first.
fn mc_brain_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    let rows = a
        .brain_files
        .iter()
        .take(14)
        .map(|f| McRow::Pair(f.name.clone(), format!("{:.1} KB", f.kb), shown.text_muted))
        .collect();
    mc_panel(
        "brain",
        "Brain files".to_string(),
        rows,
        "No brain files.",
        false,
        None,
        shown,
    )
}

const MC_TOOL_BAR_WIDTH: usize = 12;

/// The same proportional usage bar as the terminal, kept fixed-width so the
/// call count and failure rate remain aligned in a GUI card.
pub(crate) fn mc_tool_bar(total: i64, max: i64) -> String {
    let max = max.max(1);
    let filled = ((total.max(0) as f64 / max as f64) * MC_TOOL_BAR_WIDTH as f64)
        .round()
        .clamp(0.0, MC_TOOL_BAR_WIDTH as f64) as usize;
    format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(MC_TOOL_BAR_WIDTH - filled)
    )
}

/// The busiest tools, with the proportional bar, call count and failure rate
/// the terminal pairs with each name.
fn mc_top_tools_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    let max = a
        .top_tools
        .iter()
        .map(|t| t.total)
        .max()
        .unwrap_or(1)
        .max(1);
    let rows = a
        .top_tools
        .iter()
        .take(10)
        .map(|t| {
            let color = if t.failures > 0 {
                shown.text_normal
            } else {
                shown.text_muted
            };
            McRow::Pair(
                t.name.clone(),
                format!(
                    "{} {} calls \u{b7} {} failed \u{b7} {:.1}%",
                    mc_tool_bar(t.total, max),
                    t.total,
                    t.failures,
                    t.fail_rate
                ),
                color,
            )
        })
        .collect();
    mc_panel(
        "top-tools",
        "Top tools".to_string(),
        rows,
        "No tools yet.",
        false,
        None,
        shown,
    )
}

/// The RSI journal, newest first, each row carrying the level glyph the
/// terminal paints, and the selection that follows the keyboard.
fn mc_activity_panel(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    shown: palette::Palette,
) -> Div {
    use opencrabs::brain::mission_control::McActivityLevel;
    let entries = snapshot.map(|s| s.activity.as_slice()).unwrap_or_default();
    let focused = nav.panel == mission_control::Panel::Activity;
    let mut rows = Vec::new();
    for entry in entries {
        let (glyph, color) = match entry.level {
            McActivityLevel::Error => ("\u{2717}", shown.red),
            McActivityLevel::Warn => ("\u{26a0}", shown.warn),
            McActivityLevel::Success => ("\u{2713}", shown.green),
            McActivityLevel::Info => ("\u{b7}", shown.text_muted),
        };
        rows.push(McRow::Line(
            format!(
                "{glyph} {} \u{b7} {}",
                mc_relative_time(entry.timestamp),
                entry.detail
            ),
            color,
        ));
    }
    let (start, len) = mission_control::visible_window(
        rows.len(),
        focused.then_some(nav.selected),
        mission_control::ACTIVITY_ROWS,
    );
    let selected = focused.then(|| nav.selected.saturating_sub(start));
    let rows: Vec<McRow> = rows.into_iter().skip(start).take(len).collect();
    let selected = selected.filter(|_| !rows.is_empty());
    mc_panel(
        "activity",
        format!("Activity ({})", entries.len()),
        rows,
        "No activity yet.",
        focused,
        selected,
        shown,
    )
}

/// Cron jobs and pending approvals: label, expression, and the state the
/// terminal marks (awaiting you, or paused), with the focused row selected.
fn mc_schedule_panel(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    shown: palette::Palette,
) -> Div {
    let jobs = snapshot.map(|s| s.schedule.as_slice()).unwrap_or_default();
    let focused = nav.panel == mission_control::Panel::Schedule;
    let mut rows = Vec::new();
    for job in jobs {
        let color = if job.awaiting_user {
            shown.amber
        } else if job.enabled {
            shown.text_normal
        } else {
            shown.text_faint
        };
        let state = if job.awaiting_user {
            " \u{b7} awaiting you"
        } else if !job.enabled {
            " \u{b7} paused"
        } else {
            ""
        };
        rows.push(McRow::Line(
            format!(
                "{} \u{b7} {} \u{b7} {}{}",
                job.kind.label(),
                job.label,
                job.schedule,
                state
            ),
            color,
        ));
    }
    let (start, len) = mission_control::visible_window(
        rows.len(),
        focused.then_some(nav.selected),
        mission_control::ACTIVITY_ROWS,
    );
    let selected = focused.then(|| nav.selected.saturating_sub(start));
    let rows: Vec<McRow> = rows.into_iter().skip(start).take(len).collect();
    let selected = selected.filter(|_| !rows.is_empty());
    mc_panel(
        "schedule",
        format!("Schedule ({})", jobs.len()),
        rows,
        "No scheduled jobs.",
        focused,
        selected,
        shown,
    )
}

/// The focused panel's full record or analytics dashboard, drawn over
/// everything. Field order follows `tui/render/mission_control/detail_popup.rs`
/// and every value comes from the snapshot already on hand. Enter opens it,
/// Esc closes it, and a click on the card is inert so reading never throws it
/// away.
pub(crate) fn mission_control_detail(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let shown = palette::current();
    let title = match nav.panel {
        mission_control::Panel::Inbox => snapshot
            .and_then(|s| s.inbox.get(nav.selected))
            .map(|item| format!("Inbox detail \u{b7} {}", item.kind.label())),
        _ => None,
    }
    .unwrap_or_else(|| format!("{} detail", nav.panel.title()));
    let close = std::rc::Rc::new(close);
    let scrim_close = close.clone();
    let button_close = close.clone();
    let mut card = div()
        .id("mission-control-detail")
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .w_full()
        .max_w(px(720.0))
        .max_h(px(560.0))
        // The field area scrolls inside this bounded card.
        .overflow_hidden()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(shown.panel_hi))
        .bg(glass_fill(shown.panel))
        // Inert: the click stops here instead of reaching the scrim.
        .on_click(|_ev, _window, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .mb_1()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(shown.focus))
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .id("mission-control-detail-close")
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .cursor_pointer()
                        .child("esc")
                        .on_click(move |ev, window, cx| button_close(ev, window, cx)),
                ),
        );
    let body = match nav.panel {
        mission_control::Panel::Inbox => mc_inbox_detail(
            snapshot.and_then(|s| s.inbox.get(nav.selected)).cloned(),
            shown,
        ),
        mission_control::Panel::Activity => mc_activity_detail(
            snapshot.and_then(|s| s.activity.get(nav.selected)).cloned(),
            shown,
        ),
        mission_control::Panel::Schedule => mc_schedule_detail(
            snapshot.and_then(|s| s.schedule.get(nav.selected)).cloned(),
            shown,
        ),
        mission_control::Panel::Analytics => mc_analytics_detail(snapshot, shown),
    };
    card = card.child(body);
    div()
        .id("mission-control-detail-scrim")
        // Modal: the dim scrim owns the pointer, so a click meant for a panel
        // behind it cannot move the selection under the popup.
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0add))
        .on_click(move |ev, window, cx| scrim_close(ev, window, cx))
        .child(card)
}

/// The scrollable body shared by all four detail views.
fn mc_detail_body() -> Stateful<Div> {
    div()
        .id("mission-control-detail-fields")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .gap_1()
        .overflow_y_scroll()
}

fn mc_detail_heading(title: &'static str, shown: palette::Palette) -> Div {
    div()
        .mt_2()
        .text_xs()
        .text_color(rgb(shown.text_title))
        .child(title)
}

fn mc_inbox_detail(item: Option<McInboxItem>, shown: palette::Palette) -> Stateful<Div> {
    let mut body = mc_detail_body();
    let Some(item) = item else {
        return body.child(text("No proposal selected.", Size::Xs, shown.text_faint));
    };
    body = body
        .child(mc_kv("Kind", item.kind.label().to_string(), shown))
        .child(mc_kv("Label", item.label.clone(), shown))
        .child(mc_kv("Source", item.source.clone(), shown))
        .child(mc_kv(
            "Filed",
            item.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
            shown,
        ))
        .child(mc_kv("ID", item.id.clone(), shown))
        .child(mc_detail_heading("Summary", shown));
    for line in item.summary.split('\n') {
        body = body.child(text(line.to_string(), Size::Xs, shown.text_normal));
    }
    body
}

fn mc_activity_level_label(
    level: opencrabs::brain::mission_control::McActivityLevel,
) -> &'static str {
    use opencrabs::brain::mission_control::McActivityLevel;
    match level {
        McActivityLevel::Success => "success",
        McActivityLevel::Info => "info",
        McActivityLevel::Warn => "warn",
        McActivityLevel::Error => "error",
    }
}

fn mc_activity_detail(entry: Option<McActivity>, shown: palette::Palette) -> Stateful<Div> {
    let mut body = mc_detail_body();
    let Some(entry) = entry else {
        return body.child(text("No activity selected.", Size::Xs, shown.text_faint));
    };
    body = body
        .child(mc_kv(
            "When",
            entry
                .timestamp
                .map(|ts| ts.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "undated".into()),
            shown,
        ))
        .child(mc_kv("Source", entry.source.clone(), shown))
        .child(mc_kv(
            "Level",
            mc_activity_level_label(entry.level).to_string(),
            shown,
        ))
        .child(mc_detail_heading("Detail", shown));
    for line in entry.detail.split('\n') {
        body = body.child(text(line.to_string(), Size::Xs, shown.text_normal));
    }
    body
}

pub(crate) fn mc_schedule_duration(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn mc_schedule_detail(job: Option<McScheduleItem>, shown: palette::Palette) -> Stateful<Div> {
    let mut body = mc_detail_body();
    let Some(job) = job else {
        return body.child(text(
            "No schedule item selected.",
            Size::Xs,
            shown.text_faint,
        ));
    };
    let kind = match job.kind {
        opencrabs::brain::mission_control::McScheduleKind::Cron => "cron job",
        opencrabs::brain::mission_control::McScheduleKind::PendingApproval => "pending approval",
    };
    let state = if job.awaiting_user {
        "awaiting user"
    } else if !job.enabled {
        "paused"
    } else {
        "active"
    };

    body = body
        .child(mc_kv("Kind", kind.to_string(), shown))
        .child(mc_kv("Label", job.label.clone(), shown))
        .child(mc_kv("State", state.to_string(), shown))
        .child(mc_detail_heading("Schedule", shown))
        .child(mc_kv("Cron", job.schedule.clone(), shown))
        .child(mc_kv(
            "Next",
            mc_timestamp(job.next_run_at).unwrap_or_else(|| "not scheduled".into()),
            shown,
        ))
        .child(mc_detail_heading("Last run", shown))
        .child(mc_kv(
            "At",
            mc_timestamp(job.last_run_at).unwrap_or_else(|| "never".into()),
            shown,
        ));
    if let Some(status) = job.last_run_status.as_deref() {
        let status = match status {
            "running" => "running...",
            other => other,
        };
        body = body.child(mc_kv("Status", status.to_string(), shown));
    }
    if let Some(cost) = job.last_run_cost {
        body = body.child(mc_kv("Cost", format!("${cost:.4}"), shown));
    }
    if let Some(secs) = job.last_run_duration_secs {
        body = body.child(mc_kv("Duration", mc_schedule_duration(secs), shown));
    }
    if let Some(target) = job.deliver_to.as_deref() {
        body = body
            .child(mc_detail_heading("Delivery", shown))
            .child(mc_kv("Target", target.to_string(), shown));
    }
    if !job.prompt.is_empty() {
        let prompt = if job.prompt.len() > 300 {
            format!(
                "{}...",
                opencrabs::utils::string::truncate_str(&job.prompt, 300)
            )
        } else {
            job.prompt.clone()
        };
        body = body.child(mc_detail_heading("Prompt", shown));
        for line in prompt.split('\n') {
            body = body.child(text(line.to_string(), Size::Xs, shown.text_normal));
        }
    }
    body = body.child(mc_detail_heading("Meta", shown)).child(mc_kv(
        "Created",
        job.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
        shown,
    ));
    if let Some(profile) = job.profile_name.as_deref() {
        body = body.child(mc_kv("Profile", profile.to_string(), shown));
    }
    body.child(mc_kv("ID", job.id.clone(), shown))
}

fn mc_analytics_detail(
    snapshot: Option<&mission_control::Snapshot>,
    shown: palette::Palette,
) -> Stateful<Div> {
    let cards = mc_analytics_cards(snapshot, true, shown);
    let count = cards.len();
    let mut cards = cards.into_iter();
    let mut grid = div().flex().flex_col().w_full().gap_2().pr_1();
    for slots in mission_control::grid_rows(count, 2) {
        let mut row = div().flex().flex_row().w_full().h(px(180.0)).gap_2();
        for slot in slots {
            match slot {
                Some(_) => {
                    if let Some(card) = cards.next() {
                        row = row.child(card);
                    }
                }
                None => row = row.child(div().flex_1().min_w_0()),
            }
        }
        grid = grid.child(row);
    }
    mc_detail_body().child(grid)
}

/// How many log lines the window draws at once: a page key moves by a screen,
/// and a 512 KB tail never becomes thousands of elements in one frame.
const MC_LOG_ROWS: usize = 44;

/// The log reader, drawn over the screen it was opened from. The state, its
/// filters and its file paging are the terminal's own type, so the two
/// clients read the same lines with the same parser and the same levels.
pub(crate) fn mission_control_logs(
    viewer: &mut LogViewerState,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    use opencrabs::logging::reader::LogLevel;
    let shown = palette::current();
    // The state clamps scrolling to what fits; the renderer is the part that
    // knows how many rows fit, exactly as in the terminal.
    viewer.viewport_rows = MC_LOG_ROWS;
    let scroll = viewer.scroll.min(viewer.max_scroll());
    let file = viewer.file_name();
    let level = viewer.min_level.label();
    let total = viewer.entries.len();
    let filtered = viewer.visible().len();
    let search = viewer.search.clone();
    let searching = viewer.search_active;
    let error = viewer.error.clone();

    // The rows on screen: each entry's header, then its continuation lines,
    // starting at the scroll offset and stopping at the bottom.
    let mut rows: Vec<(String, u32)> = Vec::new();
    let mut row = 0usize;
    'outer: for entry in viewer.visible() {
        let color = match entry.level {
            LogLevel::Error => shown.red,
            LogLevel::Warn => shown.warn,
            LogLevel::Info => shown.text_normal,
            LogLevel::Debug => shown.text_muted,
            LogLevel::Trace => shown.text_faint,
        };
        let lines = std::iter::once((
            format!(
                "{} {:<5} {} \u{b7} {}",
                entry.timestamp,
                entry.level.label(),
                entry.target,
                entry.message
            ),
            color,
        ))
        .chain(
            entry
                .continuation
                .iter()
                .map(|line| (format!("  {line}"), shown.text_faint)),
        );
        for (line, color) in lines {
            if row >= scroll {
                rows.push((line, color));
            }
            row += 1;
            if rows.len() >= MC_LOG_ROWS {
                break 'outer;
            }
        }
    }

    let close = std::rc::Rc::new(close);
    let scrim_close = close.clone();
    let mut card = div()
        .id("mission-control-logs")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .gap_1()
        .p_3()
        .w_full()
        .overflow_hidden()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(shown.panel_hi))
        .bg(glass_fill(shown.panel))
        // Inert: a click inside the reader never closes it by accident.
        .on_click(|_ev, _window, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(shown.focus))
                        .truncate()
                        .child(format!(
                            "{file} \u{b7} level {level} \u{b7} {filtered}/{total} entries"
                        )),
                )
                .child(
                    div()
                        .id("mission-control-logs-close")
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .cursor_pointer()
                        .child("esc")
                        .on_click(move |ev, window, cx| scrim_close(ev, window, cx)),
                ),
        );
    if !search.is_empty() || searching {
        // The search box, with a caret while the keys are going into it.
        card = card.child(text(
            format!("/{search}{}", if searching { "\u{258f}" } else { "" }),
            Size::Xs,
            if searching {
                shown.focus
            } else {
                shown.text_muted
            },
        ));
    }
    if let Some(err) = error {
        card = card.child(text(err, Size::Xs, shown.red));
    } else if rows.is_empty() {
        card = card.child(text("No entries match.", Size::Xs, shown.text_faint));
    }
    let mut body = div().flex().flex_col().flex_1().min_h_0();
    for (line, color) in rows {
        body = body.child(
            div()
                .text_xs()
                .truncate()
                .text_color(rgb(color))
                .child(line),
        );
    }
    card = card.child(body).child(text(
        "j/k scroll \u{b7} PgUp/PgDn page \u{b7} g/G ends \u{b7} / search \u{b7} [ ] file \u{b7} 1-5 level \u{b7} Esc close",
        Size::Xs,
        shown.text_muted,
    ));
    div()
        .id("mission-control-logs-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .p_4()
        .bg(rgba(0x0a0a0add))
        .on_click(move |ev, window, cx| close(ev, window, cx))
        .child(card)
}

/// One label/value line in the detail popup, in the terminal's columns.
fn mc_kv(key: &str, value: String, shown: palette::Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .gap_2()
        .child(
            div()
                .w(px(56.0))
                .text_xs()
                .text_color(rgb(shown.text_title))
                .child(key.to_string()),
        )
        .child(
            div()
                .flex_1()
                .text_xs()
                .text_color(rgb(shown.text_normal))
                .truncate()
                .child(value),
        )
}

/// Format an optional timestamp for the schedule detail fields.
fn mc_timestamp(ts: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    ts.map(|ts| ts.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// An event's age in the terminal's own shorthand, and "undated" when the
/// journal had no parseable time: a missing date must never read as now.
fn mc_relative_time(ts: Option<chrono::DateTime<chrono::Utc>>) -> String {
    let Some(ts) = ts else {
        return "undated".to_string();
    };
    let secs = (chrono::Utc::now() - ts).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}
