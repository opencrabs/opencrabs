//! Usage dashboard card renderers
//!
//! Each card is a self-contained render function that takes data + area and draws into a Frame.

use super::dashboard::{DashboardState, advertised_keys};
use crate::tui::render::theme::{self, Role};
use crate::ui_core::usage::view_model::{
    ActivityRow, CacheView, DailyRow, HitBand, ModelRow, ProjectRow, ToolRow, UsageViewModel,
};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

const LABEL: Style = Style::new().fg(Color::DarkGray);
const BOLD: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().fg(Color::DarkGray);
fn accent() -> Style {
    Style::new().fg(theme::role(Role::Accent))
}

fn card_block(title: &str, focused: bool) -> Block<'_> {
    let border_color = if focused {
        theme::role(Role::Accent)
    } else {
        Color::DarkGray
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" {} ", title),
            if focused { accent() } else { LABEL },
        ))
}

// ── Summary Bar ──────────────────────────────────────────────────────────────

pub fn render_summary(f: &mut Frame, view: &UsageViewModel, area: Rect) {
    let s = &view.summary;
    // The cost cell carries the `~` estimate marker itself (#2249): `~` when
    // any part of the figure is an estimate, so it never reads as total
    // spend. The unpriced share marker is the first thing dropped when the
    // bar would have to clip: the line is centred, so an overflow eats the
    // version off the left and the period label off the right.
    let mut head = vec![
        Span::styled(format!("v{}  ", view.version), accent()),
        Span::styled("Tokens: ", LABEL),
        Span::styled(s.tokens_str.clone(), BOLD),
        Span::styled("  Cost: ", LABEL),
        Span::styled(s.cost_str.clone(), BOLD),
    ];
    let tail = [
        Span::styled("  Sessions: ", LABEL),
        Span::styled(s.sessions_str.clone(), BOLD),
        Span::styled("  Calls: ", LABEL),
        Span::styled(s.calls_str.clone(), BOLD),
        Span::styled(format!("  [{}]", view.period_label), accent()),
    ];
    if let Some(marker) = &s.unpriced_marker {
        let used: usize = head
            .iter()
            .chain(tail.iter())
            .map(|sp| sp.content.chars().count())
            .sum();
        if used + marker.chars().count() <= area.width as usize {
            head.push(Span::styled(marker.clone(), DIM));
        }
    }
    head.extend(tail);
    let line = Line::from(head);
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(vec![line])
        .block(block)
        .alignment(Alignment::Center);
    f.render_widget(paragraph, area);
}

// ── Daily Activity ───────────────────────────────────────────────────────────

pub fn render_daily(f: &mut Frame, daily: &[DailyRow], area: Rect, focused: bool) {
    let block = card_block("Daily Activity", focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if daily.is_empty() {
        let p = Paragraph::new(" No data").style(DIM);
        f.render_widget(p, inner);
        return;
    }

    // Rows arrive most recent first; the model already scaled every bar
    // against the busiest day.
    let max_cost_len = daily.iter().map(|d| d.tokens_str.len()).max().unwrap_or(8);
    let date_width = 6usize; // " 04-15 "
    let data_cols = max_cost_len + 2; // " {cost}"
    let bar_width = inner.width.saturating_sub((date_width + data_cols) as u16) as usize;

    let mut lines: Vec<Line> = Vec::new();
    let visible = (inner.height as usize).min(daily.len());

    for day in daily.iter().take(visible) {
        let bar_len = if bar_width > 0 {
            (day.bar_ratio * bar_width as f64).ceil() as usize
        } else {
            0
        };
        let bar_len = bar_len.max(1).min(bar_width);
        let bar: String = "\u{2584}".repeat(bar_len); // ▄ lower-half block for visual separation
        let pad: String = " ".repeat(bar_width.saturating_sub(bar_len));
        lines.push(Line::from(vec![
            Span::styled(format!(" {:>5} ", day.short_date), DIM),
            Span::styled(bar, accent()),
            Span::raw(pad),
            Span::styled(
                format!(" {:>width$}", day.tokens_str, width = max_cost_len),
                LABEL,
            ),
        ]));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

// ── By Project ───────────────────────────────────────────────────────────────

pub fn render_projects(f: &mut Frame, projects: &[ProjectRow], area: Rect, focused: bool) {
    let block = card_block("By Project", focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if projects.is_empty() {
        let p = Paragraph::new(" No data").style(DIM);
        f.render_widget(p, inner);
        return;
    }

    // Compute column widths from actual data
    let max_cost_len = projects.iter().map(|p| p.cost_str.len()).max().unwrap_or(6);
    let max_tok_len = projects
        .iter()
        .map(|p| p.tokens_str.len())
        .max()
        .unwrap_or(6);
    let max_sess_len = projects
        .iter()
        .map(|p| p.sessions_str.len())
        .max()
        .unwrap_or(1);

    // Data columns get guaranteed space; name column fills whatever is left
    let spacing = 2;
    let cost_width = max_cost_len;
    let tok_width = max_tok_len;
    let sess_width = max_sess_len;
    // +1 for leading space on name, +1 for trailing 's' on sessions
    let fixed = cost_width + tok_width + sess_width + spacing * 3 + 2;
    let name_width = (inner.width as usize).saturating_sub(fixed).max(4);

    let mut lines: Vec<Line> = Vec::new();
    let visible = (inner.height as usize).min(projects.len());
    for proj in projects.iter().take(visible) {
        let name = if proj.name.len() > name_width {
            format!(
                "{}...",
                proj.name
                    .chars()
                    .take(name_width.saturating_sub(3))
                    .collect::<String>()
            )
        } else {
            proj.name.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<width$}", name, width = name_width), BOLD),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}", proj.cost_str, width = cost_width),
                LABEL,
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}", proj.tokens_str, width = tok_width),
                DIM,
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}s", proj.sessions_str, width = sess_width),
                DIM,
            ),
        ]));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

// ── By Model ─────────────────────────────────────────────────────────────────

pub fn render_models(f: &mut Frame, models: &[ModelRow], area: Rect, focused: bool) {
    let block = card_block("By Model", focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if models.is_empty() {
        let p = Paragraph::new(" No data").style(DIM);
        f.render_widget(p, inner);
        return;
    }

    // Compute column widths from ALL visible data (parent + child rows)
    let visible = (inner.height as usize).min(models.len());
    let all_models: Vec<&ModelRow> = models.iter().take(visible).collect();

    let max_cost_len = all_models
        .iter()
        .flat_map(|m| {
            let mut costs = vec![m.cost_str.len()];
            for v in &m.variants {
                costs.push(v.cost_str.len());
            }
            costs
        })
        .max()
        .unwrap_or(6);

    let max_tok_len = all_models
        .iter()
        .flat_map(|m| {
            let mut lens = vec![m.tokens_str.len()];
            for v in &m.variants {
                lens.push(v.tokens_str.len());
            }
            lens
        })
        .max()
        .unwrap_or(6);

    let max_calls_len = all_models
        .iter()
        .flat_map(|m| {
            let mut lens = vec![m.calls_str.len()];
            for v in &m.variants {
                lens.push(v.calls_str.len());
            }
            lens
        })
        .max()
        .unwrap_or(1);

    // Cache column (#2027): of this model's recorded tokens, how many were
    // cache reads. The column appears only when at least one visible model
    // carries a split, because a column of blanks on a ledger written before
    // the cache-read column existed would be pure noise. A model with no
    // recorded split renders blank there, never a zero it did not measure.
    let show_cache = all_models.iter().any(|m| m.cache_cell.is_some());
    let cache_width = if show_cache {
        all_models
            .iter()
            .filter_map(|m| m.cache_cell.as_ref().map(|s| s.len()))
            .max()
            .unwrap_or(0)
    } else {
        0
    };

    // Data columns get guaranteed space; name column fills whatever is left
    let spacing = 2;
    let cost_width = max_cost_len;
    let tok_width = max_tok_len;
    let calls_width = max_calls_len;
    let cache_cols = if show_cache { cache_width + spacing } else { 0 };
    let fixed = cost_width + tok_width + calls_width + cache_cols + spacing * 3 + 1;
    let name_width = (inner.width as usize).saturating_sub(fixed).max(4);

    let mut lines: Vec<Line> = Vec::new();
    for m in all_models.iter() {
        // Render the parent-row name as the normalized kebab-case id so
        // every entry in the dashboard follows one consistent pattern
        // (`family-X.Y-tier`, all lowercase, dashes between segments).
        // Prettified labels ("Qwen 3.7 Max") belong in /models and
        // onboarding, not the usage ledger.
        let display = m.name.clone();
        let name = if display.len() > name_width {
            format!(
                "{}...",
                display
                    .chars()
                    .take(name_width.saturating_sub(3))
                    .collect::<String>()
            )
        } else {
            display
        };

        // Parent row (bold model name, no prefix). The estimate state picks
        // the style; the `~` itself already rides in the cost cell.
        let cost_style = if m.estimated { accent() } else { LABEL };
        let mut spans = vec![
            Span::styled(format!(" {:<width$}", name, width = name_width), BOLD),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}", m.cost_str, width = cost_width),
                cost_style,
            ),
            Span::raw("  "),
            Span::styled(format!("{:>width$}", m.tokens_str, width = tok_width), DIM),
            Span::raw("  "),
        ];
        // The cache share, when the model has one recorded (#2027). A row with
        // no split stays blank rather than reading 0%.
        if show_cache {
            let (text, style) = match &m.cache_cell {
                Some(s) => (s.clone(), LABEL),
                None => (String::new(), DIM),
            };
            spans.push(Span::styled(
                format!("{text:<width$}", width = cache_width),
                style,
            ));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{:>width$} req", m.calls_str, width = calls_width),
            DIM,
        ));
        lines.push(Line::from(spans));

        // Child rows (variant breakdown). The view model already dropped
        // cosmetic aliases, so every variant here carries real information.
        // Tree prefix: box-drawing glyph + 1 trailing space = 3 display
        // chars. The `├─` / `└─` glyphs are themselves the depth signal;
        // adding leading whitespace just shoves the name off the natural
        // alignment with the parent and wastes horizontal room without
        // clarifying anything. Reserve those 3 chars from `name_width`
        // so child cost / tokens / calls columns still share the parent
        // row's right-hand column edges.
        if !m.variants.is_empty() {
            const TREE_PREFIX_CHARS: usize = 3;
            let max_child_name = name_width.saturating_sub(TREE_PREFIX_CHARS);
            for (vidx, v) in m.variants.iter().enumerate() {
                let is_last = vidx == m.variants.len() - 1;
                let prefix = if is_last { "└─ " } else { "├─ " };
                // Keep child variant names as the raw API id so the
                // operator sees exactly what was recorded (e.g.
                // `qwen3.7-max-20260520`), matching the parent row's
                // lowercase-kebab convention.
                let v_display = v.name.clone();
                let v_name = if v_display.chars().count() > max_child_name {
                    format!(
                        "{}…",
                        v_display
                            .chars()
                            .take(max_child_name.saturating_sub(1))
                            .collect::<String>()
                    )
                } else {
                    v_display
                };
                let v_name = format!("{prefix}{v_name}");
                lines.push(Line::from(vec![
                    // Same total span width as the parent row — leading space
                    // + name padded to name_width — so cost/tokens/calls
                    // start at the same column for both.
                    Span::styled(format!(" {:<width$}", v_name, width = name_width), DIM),
                    Span::raw("  "),
                    Span::styled(format!("{:>width$}", v.cost_str, width = cost_width), LABEL),
                    Span::raw("  "),
                    Span::styled(format!("{:>width$}", v.tokens_str, width = tok_width), DIM),
                    Span::raw("  "),
                    // A variant row carries no cache split of its own (the
                    // ledger records one per turn, not per spelling), so the
                    // cell stays blank to keep the columns aligned with the
                    // parent row.
                    Span::raw(if show_cache {
                        format!("{:<width$}", "", width = cache_width + spacing)
                    } else {
                        String::new()
                    }),
                    Span::styled(
                        format!("{:>width$} req", v.calls_str, width = calls_width),
                        DIM,
                    ),
                ]));
            }
        }
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

// ── Core Tools ───────────────────────────────────────────────────────────────

pub fn render_tools(f: &mut Frame, tools: &[ToolRow], area: Rect, focused: bool) {
    let block = card_block("Core Tools", focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if tools.is_empty() {
        let p = Paragraph::new(" No data").style(DIM);
        f.render_widget(p, inner);
        return;
    }

    let visible = (inner.height as usize).min(tools.len());

    // Compute widths from actual data
    let max_name_len = tools
        .iter()
        .take(visible)
        .map(|t| t.name.len())
        .max()
        .unwrap_or(8);
    let max_count_len = tools
        .iter()
        .take(visible)
        .map(|t| t.call_count.to_string().len())
        .max()
        .unwrap_or(1);

    // Reserve space for actual name and count, bar gets the rest
    let data_cols = max_name_len + max_count_len + 3; // 3 spacer chars
    let total_needed = (inner.width as usize).min(data_cols);
    let name_width = total_needed
        .saturating_sub(max_count_len + 2)
        .max(max_name_len.min(4));
    let count_width = total_needed
        .saturating_sub(name_width + 2)
        .max(max_count_len);
    let bar_width = inner
        .width
        .saturating_sub((name_width + count_width + 3) as u16) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for tool in tools.iter().take(visible) {
        let bar_len = if bar_width > 0 {
            (tool.bar_ratio * bar_width as f64).ceil() as usize
        } else {
            0
        };
        let bar_len = bar_len.max(1).min(bar_width);
        let bar: String = "\u{2584}".repeat(bar_len);
        let pad: String = " ".repeat(bar_width.saturating_sub(bar_len));
        let name = if tool.name.len() > name_width {
            format!(
                "{}...",
                tool.name
                    .chars()
                    .take(name_width.saturating_sub(3))
                    .collect::<String>()
            )
        } else {
            tool.name.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<width$}", name, width = name_width), BOLD),
            Span::styled(bar, accent()),
            Span::raw(pad),
            Span::styled(
                format!(" {:>width$}", tool.call_count, width = count_width),
                DIM,
            ),
        ]));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

// ── By Activity ──────────────────────────────────────────────────────────────

/// Column widths for the "By Activity" card. Each width is the larger
/// of the widest data value and its header label, so headers like
/// "1-shot" or "Turns" never overflow a column sized only for the
/// values (the 2026-04-25 "1-sho" clip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivityColumnWidths {
    pub cat: usize,
    pub cost: usize,
    pub turns: usize,
    /// Pct width includes one slot for the trailing '%' so values like
    /// "60%" line up at the same right edge as the bare-number widest.
    pub pct: usize,
}

/// Pure column-width calculator for the "By Activity" card. Consumes the
/// view model's rows (raw cost and pct ride on them for exactly this
/// math). Pulled out so `src/tests/usage_activity_columns_test.rs` can
/// exercise the header-label floor that prevents "1-shot" / "Turns" from
/// being clipped when their data values are narrower than the header.
pub(crate) fn activity_column_widths(activities: &[ActivityRow]) -> ActivityColumnWidths {
    let max_cat_len = activities
        .iter()
        .map(|a| a.category.len())
        .max()
        .unwrap_or(8);
    let max_cost_len = activities
        .iter()
        .map(|a| a.cost_str.len())
        .max()
        .unwrap_or(6);
    let max_turns_len = activities
        .iter()
        .map(|a| a.turns.to_string().len())
        .max()
        .unwrap_or(1);
    let max_pct_len = activities
        .iter()
        .map(|a| a.one_shot_pct.to_string().len())
        .max()
        .unwrap_or(1);

    ActivityColumnWidths {
        cat: max_cat_len,
        cost: max_cost_len.max("Cost".len()),
        turns: max_turns_len.max("Turns".len()),
        // +1 for the trailing '%' the values render with.
        pct: (max_pct_len + 1).max("1-shot".len()),
    }
}

pub fn render_activities(f: &mut Frame, activities: &[ActivityRow], area: Rect, focused: bool) {
    let block = card_block("By Activity", focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if activities.is_empty() {
        let p = Paragraph::new(" No data").style(DIM);
        f.render_widget(p, inner);
        return;
    }

    // Compute column widths from actual data
    let visible = (inner.height.saturating_sub(1) as usize).min(activities.len());
    let widths = activity_column_widths(&activities[..visible]);
    let max_cat_len = widths.cat;
    let cost_width = widths.cost;
    let turns_width = widths.turns;
    let pct_width = widths.pct;

    // Data columns get guaranteed space; category+bar fill whatever is left
    let spacing = 1; // single space between data cols
    let fixed_data = cost_width + turns_width + pct_width + spacing * 3;
    let cat_bar_width = (inner.width as usize).saturating_sub(fixed_data + 1); // +1 leading space
    let cat_width = max_cat_len.min(cat_bar_width / 3).max(4);
    let bar_width = cat_bar_width.saturating_sub(cat_width);

    let header_line = Line::from(vec![
        Span::styled(format!(" {:<width$}", "Category", width = cat_width), LABEL),
        Span::raw(" ".repeat(bar_width)),
        Span::styled(format!(" {:>width$}", "Cost", width = cost_width), LABEL),
        Span::styled(format!(" {:>width$}", "Turns", width = turns_width), LABEL),
        Span::styled(format!(" {:>width$}", "1-shot", width = pct_width), LABEL),
    ]);
    let mut lines: Vec<Line> = vec![header_line];

    for act in activities.iter().take(visible) {
        let bar_len = if bar_width > 0 {
            (act.bar_ratio * bar_width as f64).ceil() as usize
        } else {
            0
        };
        let bar_len = bar_len
            .max(if act.cost > 0.0 { 1 } else { 0 })
            .min(bar_width);
        let bar: String = "\u{2584}".repeat(bar_len);
        let pad: String = " ".repeat(bar_width.saturating_sub(bar_len));
        let category = if act.category.len() > cat_width {
            format!(
                "{}...",
                act.category
                    .chars()
                    .take(cat_width.saturating_sub(3))
                    .collect::<String>()
            )
        } else {
            act.category.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<width$}", category, width = cat_width), BOLD),
            Span::styled(bar, accent()),
            Span::raw(pad),
            Span::styled(
                format!(" {:>width$}", act.cost_str, width = cost_width),
                LABEL,
            ),
            Span::styled(format!(" {:>width$}", act.turns, width = turns_width), DIM),
            Span::styled(
                format!(" {:>width$}", act.one_shot_str, width = pct_width),
                DIM,
            ),
        ]));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

// ── Cache Efficiency ─────────────────────────────────────────────────────────

pub fn render_cache_efficiency(
    f: &mut Frame,
    cache: Option<&CacheView>,
    area: Rect,
    focused: bool,
) {
    let block = card_block("Cache Efficiency", focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cache = match cache {
        Some(c) => c,
        None => {
            let p = Paragraph::new(" No cache data").style(DIM);
            f.render_widget(p, inner);
            return;
        }
    };

    // Color-code by band: the thresholds live in the view model, the
    // palette lives here (green high, yellow mid, red low).
    let band_color = |band: HitBand| match band {
        HitBand::High => Color::Green,
        HitBand::Mid => Color::Yellow,
        HitBand::Low => Color::Red,
    };

    let mut lines: Vec<Line> = Vec::new();

    // Overall efficiency — one compact header line.
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:.0}%", cache.overall_pct),
            Style::new()
                .fg(band_color(cache.overall_band))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" overall", DIM),
    ]));

    // Per-model breakdown, highest hit-rate first. Each row: model … NN%.
    // No blank spacer — vertical space in this card is scarce, every row is a
    // model we'd otherwise drop.
    if !cache.models.is_empty() {
        let width = inner.width as usize;
        let name_w = width.saturating_sub(5).max(6); // leave room for " 100%"
        for ms in &cache.models {
            if lines.len() as u16 >= inner.height {
                break;
            }
            let name: String = ms.model.chars().take(name_w).collect();
            lines.push(Line::from(vec![
                Span::raw(format!("{name:<name_w$}")),
                Span::styled(
                    format!("{:>3}%", ms.hit_pct.round() as i64),
                    Style::new().fg(band_color(ms.band)),
                ),
            ]));
        }
    } else {
        // No per-model rows (e.g. only one provider) — keep the cached/total line.
        if let Some(line) = &cache.fallback_line {
            lines.push(Line::from(Span::styled(line.clone(), DIM)));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

// ── Cache price breakdown (#2027) ────────────────────────────────────────────

/// Fixed widths of the money and count columns, plus the leading space.
/// `model` takes whatever is left, so the table fits any terminal.
const BREAKDOWN_COLS: usize = 1 + 8 + 8 + 9 + 9 + 9;

/// The cache price breakdown, drawn where the Cache Efficiency card was when
/// Enter opened it (#2027).
///
/// One row per model: the cached tokens, the cache-read rate they were billed
/// at, what those reads cost, what they would have cost as ordinary input, and
/// the difference. The TOTAL row adds the rows up, and the footnote says what
/// the columns mean, so the money here answers the question the card raised.
pub fn render_cache_breakdown(f: &mut Frame, cache: Option<&CacheView>, area: Rect) {
    let block = card_block("Cache Efficiency: prices", true);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let cache = match cache {
        Some(c) if !c.models.is_empty() => c,
        _ => {
            let p = Paragraph::new(" No cache data").style(DIM);
            f.render_widget(p, inner);
            return;
        }
    };

    let width = inner.width as usize;
    let name_w = width.saturating_sub(BREAKDOWN_COLS).max(8);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            " {:<name_w$}{:>8}{:>8}{:>9}{:>9}{:>9}",
            "model", "cached", "$/M", "read", "full", "saved"
        ),
        LABEL,
    )));

    // The TOTAL row and the two footnote lines are reserved first, so a long
    // model list never pushes the explanation off the panel. The `~` on
    // money cells is already baked into the view model's cells; the
    // estimated flag only picks the accent style.
    let max_rows = (inner.height as usize).saturating_sub(4).max(1);
    for ms in cache.models.iter().take(max_rows) {
        let name: String = ms.model.chars().take(name_w.saturating_sub(1)).collect();
        let money_style = if ms.estimated { accent() } else { DIM };
        lines.push(Line::from(vec![
            Span::styled(format!(" {name:<name_w$}"), LABEL),
            Span::styled(format!("{:>8}", ms.cached_str), DIM),
            Span::styled(format!("{:>8}", ms.rate_str), money_style),
            Span::styled(format!("{:>9}", ms.read_str), money_style),
            Span::styled(format!("{:>9}", ms.full_rate_str), money_style),
            Span::styled(format!("{:>9}", ms.saved_str), money_style),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled(format!(" {:<name_w$}", "TOTAL"), BOLD),
        Span::styled(format!("{:>8}", cache.total.cached_str), BOLD),
        Span::raw(" ".repeat(8)),
        Span::styled(format!("{:>9}", cache.total.read_str), BOLD),
        Span::styled(format!("{:>9}", cache.total.full_rate_str), BOLD),
        Span::styled(format!("{:>9}", cache.total.saved_str), BOLD),
    ]));
    lines.push(Line::from(Span::styled(cache.footnote1.clone(), DIM)));
    lines.push(Line::from(Span::styled(cache.footnote2, DIM)));

    f.render_widget(Paragraph::new(lines), inner);
}

// ── Footer ───────────────────────────────────────────────────────────────────

/// The dashboard footer: the keys that are live in this state (#1775 / #2027).
///
/// Enter is only advertised on the card that has a breakdown to open, and once
/// the breakdown is up Esc is the only key left, so the row a user reads is
/// always a key the handler honours.
pub fn render_footer(f: &mut Frame, area: Rect, state: &DashboardState) {
    use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};

    if state.cache_detail_open {
        crate::tui::render::hints::render_footer(
            f,
            area,
            dialog_keys(DialogScope::UsageCacheBreakdown),
        );
    } else {
        crate::tui::render::hints::render_footer(f, area, &advertised_keys(state));
    }
}
