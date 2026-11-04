//! Usage dashboard card renderers
//!
//! Each card is a self-contained render function that takes data + area and draws into a Frame.

use super::dashboard::{DashboardState, advertised_keys};
use super::data::{
    ActivityStats, CacheStats, DailyStats, DashboardData, ModelEntry, ProjectStats, ToolStats,
    fmt_cost, fmt_tokens,
};
use crate::tui::render::theme::{self, Role};
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

pub fn render_summary(f: &mut Frame, data: &DashboardData, area: Rect, period_label: &str) {
    let s = &data.summary;
    let version = crate::VERSION;
    let line = Line::from(vec![
        Span::styled(format!("v{version}  "), accent()),
        Span::styled("Tokens: ", LABEL),
        Span::styled(fmt_tokens(s.total_tokens), BOLD),
        Span::styled("  Cost: ", LABEL),
        Span::styled(fmt_cost(s.total_cost), BOLD),
        Span::styled("  Sessions: ", LABEL),
        Span::styled(format!("{}", s.session_count), BOLD),
        Span::styled("  Calls: ", LABEL),
        Span::styled(format!("{}", s.call_count), BOLD),
        Span::styled(format!("  [{}]", period_label), accent()),
    ]);
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(vec![line])
        .block(block)
        .alignment(Alignment::Center);
    f.render_widget(paragraph, area);
}

// ── Daily Activity ───────────────────────────────────────────────────────────

pub fn render_daily(f: &mut Frame, daily: &[DailyStats], area: Rect, focused: bool) {
    let block = card_block("Daily Activity", focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if daily.is_empty() {
        let p = Paragraph::new(" No data").style(DIM);
        f.render_widget(p, inner);
        return;
    }

    let max_tokens = daily.iter().map(|d| d.tokens).max().unwrap_or(1);

    // Compute actual cost column width from data
    let max_cost_len = daily
        .iter()
        .map(|d| fmt_tokens(d.tokens).len())
        .max()
        .unwrap_or(8);
    let date_width = 6usize; // " 04-15 "
    let data_cols = max_cost_len + 2; // " {cost}"
    let bar_width = inner.width.saturating_sub((date_width + data_cols) as u16) as usize;

    let mut lines: Vec<Line> = Vec::new();
    // Show most recent days first (reversed), N that fit
    let visible = (inner.height as usize).min(daily.len());
    let start = daily.len().saturating_sub(visible);

    for day in daily[start..].iter().rev() {
        let bar_len = if max_tokens > 0 && bar_width > 0 {
            ((day.tokens as f64 / max_tokens as f64) * bar_width as f64).ceil() as usize
        } else {
            0
        };
        let bar_len = bar_len.max(1).min(bar_width);
        let bar: String = "\u{2584}".repeat(bar_len); // ▄ lower-half block for visual separation
        let pad: String = " ".repeat(bar_width.saturating_sub(bar_len));
        // Show short date (MM-DD)
        let short_date = if day.date.len() >= 10 {
            &day.date[5..10]
        } else {
            &day.date
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {:>5} ", short_date), DIM),
            Span::styled(bar, accent()),
            Span::raw(pad),
            Span::styled(
                format!(" {:>width$}", fmt_tokens(day.tokens), width = max_cost_len),
                LABEL,
            ),
        ]));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

// ── By Project ───────────────────────────────────────────────────────────────

pub fn render_projects(f: &mut Frame, projects: &[ProjectStats], area: Rect, focused: bool) {
    let block = card_block("By Project", focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if projects.is_empty() {
        let p = Paragraph::new(" No data").style(DIM);
        f.render_widget(p, inner);
        return;
    }

    // Compute column widths from actual data
    let max_cost_len = projects
        .iter()
        .map(|p| fmt_cost(p.cost).len())
        .max()
        .unwrap_or(6);
    let max_tok_len = projects
        .iter()
        .map(|p| fmt_tokens(p.tokens).len())
        .max()
        .unwrap_or(6);
    let max_sess_len = projects
        .iter()
        .map(|p| p.sessions.to_string().len())
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
        let name = if proj.project.len() > name_width {
            format!(
                "{}...",
                proj.project
                    .chars()
                    .take(name_width.saturating_sub(3))
                    .collect::<String>()
            )
        } else {
            proj.project.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<width$}", name, width = name_width), BOLD),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}", fmt_cost(proj.cost), width = cost_width),
                LABEL,
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}", fmt_tokens(proj.tokens), width = tok_width),
                DIM,
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}s", proj.sessions, width = sess_width),
                DIM,
            ),
        ]));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

// ── By Model ─────────────────────────────────────────────────────────────────

pub fn render_models(f: &mut Frame, models: &[ModelEntry], area: Rect, focused: bool) {
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
    let all_models: Vec<&ModelEntry> = models.iter().take(visible).collect();

    let max_cost_len = all_models
        .iter()
        .flat_map(|m| {
            let parent_cost = fmt_cost(m.cost);
            let mut costs = if m.estimated {
                vec![format!("~{}", parent_cost)]
            } else {
                vec![parent_cost]
            };
            for v in &m.variants {
                costs.push(fmt_cost(v.cost));
            }
            costs.iter().map(|s| s.len()).collect::<Vec<_>>()
        })
        .max()
        .unwrap_or(6);

    let max_tok_len = all_models
        .iter()
        .flat_map(|m| {
            let mut lens = vec![fmt_tokens(m.tokens).len()];
            for v in &m.variants {
                lens.push(fmt_tokens(v.tokens).len());
            }
            lens
        })
        .max()
        .unwrap_or(6);

    let max_calls_len = all_models
        .iter()
        .flat_map(|m| {
            let mut lens = vec![m.calls.to_string().len()];
            for v in &m.variants {
                lens.push(v.calls.to_string().len());
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
    let cache_cell = |m: &ModelEntry| m.cache_read_pct().map(|pct| format!("{pct:.0}% cache"));
    let show_cache = all_models.iter().any(|m| m.cache_read_tokens.is_some());
    let cache_width = if show_cache {
        all_models
            .iter()
            .filter_map(|m| cache_cell(m).map(|s| s.len()))
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
        let display = m.model.clone();
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

        // Parent row (bold model name, no prefix)
        let cost_style = if m.estimated { accent() } else { LABEL };
        let cost_str = if m.estimated {
            format!("~{}", fmt_cost(m.cost))
        } else {
            fmt_cost(m.cost)
        };
        let mut spans = vec![
            Span::styled(format!(" {:<width$}", name, width = name_width), BOLD),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}", cost_str, width = cost_width),
                cost_style,
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}", fmt_tokens(m.tokens), width = tok_width),
                DIM,
            ),
            Span::raw("  "),
        ];
        // The cache share, when the model has one recorded (#2027). A row with
        // no split stays blank rather than reading 0%.
        if show_cache {
            let (text, style) = match cache_cell(m) {
                Some(s) => (s, LABEL),
                None => (String::new(), DIM),
            };
            spans.push(Span::styled(
                format!("{text:<width$}", width = cache_width),
                style,
            ));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{:>width$} req", m.calls, width = calls_width),
            DIM,
        ));
        lines.push(Line::from(spans));

        // Child rows (variant breakdown) — only show the tree when
        // there's something MEANINGFUL to break down. A variant
        // whose name is a cosmetic alias of the parent (only
        // separator/case differences, e.g. `qwen3.7-max` vs
        // `qwen-3.7-max`) carries zero information and just
        // duplicates the parent line visually.
        let meaningful_variants: Vec<&_> = m
            .variants
            .iter()
            .filter(|v| {
                crate::usage::data::normalize_model_for_grouping(&v.name) != m.model
                    && !crate::usage::data::is_cosmetic_alias_of_parent(&v.name, &m.model)
            })
            .collect();
        let has_real_variants = !meaningful_variants.is_empty();

        if has_real_variants {
            // Tree prefix: box-drawing glyph + 1 trailing space = 3 display
            // chars. The `├─` / `└─` glyphs are themselves the depth signal;
            // adding leading whitespace just shoves the name off the natural
            // alignment with the parent and wastes horizontal room without
            // clarifying anything. Reserve those 3 chars from `name_width`
            // so child cost / tokens / calls columns still share the parent
            // row's right-hand column edges.
            const TREE_PREFIX_CHARS: usize = 3;
            let max_child_name = name_width.saturating_sub(TREE_PREFIX_CHARS);
            for (vidx, v) in meaningful_variants.iter().enumerate() {
                let is_last = vidx == meaningful_variants.len() - 1;
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
                    Span::styled(
                        format!("{:>width$}", fmt_cost(v.cost), width = cost_width),
                        LABEL,
                    ),
                    Span::raw("  "),
                    Span::styled(
                        format!("{:>width$}", fmt_tokens(v.tokens), width = tok_width),
                        DIM,
                    ),
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
                    Span::styled(format!("{:>width$} req", v.calls, width = calls_width), DIM),
                ]));
            }
        }
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

// ── Core Tools ───────────────────────────────────────────────────────────────

pub fn render_tools(f: &mut Frame, tools: &[ToolStats], area: Rect, focused: bool) {
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
        .map(|t| t.tool_name.len())
        .max()
        .unwrap_or(8);
    let max_count_len = tools
        .iter()
        .take(visible)
        .map(|t| t.call_count.to_string().len())
        .max()
        .unwrap_or(1);
    let max_count = tools.first().map(|t| t.call_count).unwrap_or(1);

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
        let bar_len = if max_count > 0 && bar_width > 0 {
            ((tool.call_count as f64 / max_count as f64) * bar_width as f64).ceil() as usize
        } else {
            0
        };
        let bar_len = bar_len.max(1).min(bar_width);
        let bar: String = "\u{2584}".repeat(bar_len);
        let pad: String = " ".repeat(bar_width.saturating_sub(bar_len));
        let name = if tool.tool_name.len() > name_width {
            format!(
                "{}...",
                tool.tool_name
                    .chars()
                    .take(name_width.saturating_sub(3))
                    .collect::<String>()
            )
        } else {
            tool.tool_name.clone()
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

/// Pure column-width calculator for the "By Activity" card. Pulled out
/// so `src/tests/usage_activity_columns_test.rs` can exercise the
/// header-label floor that prevents "1-shot" / "Turns" from being
/// clipped when their data values are narrower than the header.
pub(crate) fn activity_column_widths(activities: &[ActivityStats]) -> ActivityColumnWidths {
    let max_cat_len = activities
        .iter()
        .map(|a| a.category.len())
        .max()
        .unwrap_or(8);
    let max_cost_len = activities
        .iter()
        .map(|a| fmt_cost(a.cost).len())
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

pub fn render_activities(f: &mut Frame, activities: &[ActivityStats], area: Rect, focused: bool) {
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

    let max_cost = activities.iter().map(|a| a.cost).fold(0.0_f64, f64::max);

    for act in activities.iter().take(visible) {
        let bar_len = if max_cost > 0.0 && bar_width > 0 {
            ((act.cost / max_cost) * bar_width as f64).ceil() as usize
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
        let one_shot = format!("{}%", act.one_shot_pct as u32);
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<width$}", category, width = cat_width), BOLD),
            Span::styled(bar, accent()),
            Span::raw(pad),
            Span::styled(
                format!(" {:>width$}", fmt_cost(act.cost), width = cost_width),
                LABEL,
            ),
            Span::styled(format!(" {:>width$}", act.turns, width = turns_width), DIM),
            Span::styled(format!(" {:>width$}", one_shot, width = pct_width), DIM),
        ]));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

// ── Cache Efficiency ─────────────────────────────────────────────────────────

pub fn render_cache_efficiency(
    f: &mut Frame,
    cache: &Option<CacheStats>,
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

    // Color-code by hit rate: green >=60%, yellow 30-60%, red <30%.
    let pct_color = |p: f64| {
        if p >= 60.0 {
            Color::Green
        } else if p >= 30.0 {
            Color::Yellow
        } else {
            Color::Red
        }
    };

    let mut lines: Vec<Line> = Vec::new();

    // Overall efficiency — one compact header line.
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:.0}%", cache.cache_hit_pct),
            Style::new()
                .fg(pct_color(cache.cache_hit_pct))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" overall", DIM),
    ]));

    // Per-model breakdown, highest hit-rate first. Each row: model … NN%.
    // No blank spacer — vertical space in this card is scarce, every row is a
    // model we'd otherwise drop.
    if !cache.per_model.is_empty() {
        let width = inner.width as usize;
        let name_w = width.saturating_sub(5).max(6); // leave room for " 100%"
        for ms in &cache.per_model {
            if lines.len() as u16 >= inner.height {
                break;
            }
            let name: String = ms.model.chars().take(name_w).collect();
            lines.push(Line::from(vec![
                Span::raw(format!("{name:<name_w$}")),
                Span::styled(
                    format!("{:>3}%", ms.cache_hit_pct.round() as i64),
                    Style::new().fg(pct_color(ms.cache_hit_pct)),
                ),
            ]));
        }
    } else {
        // No per-model rows (e.g. only one provider) — keep the cached/total line.
        lines.push(Line::from(Span::styled(
            format!(
                "{} / {} cached",
                fmt_tokens(cache.cached_tokens),
                fmt_tokens(cache.total_input_tokens)
            ),
            DIM,
        )));
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
pub fn render_cache_breakdown(f: &mut Frame, cache: &Option<CacheStats>, area: Rect) {
    let block = card_block("Cache Efficiency: prices", true);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let cache = match cache {
        Some(c) if !c.per_model.is_empty() => c,
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
    // model list never pushes the explanation off the panel.
    let max_rows = (inner.height as usize).saturating_sub(4).max(1);
    for ms in cache.per_model.iter().take(max_rows) {
        let name: String = ms.model.chars().take(name_w.saturating_sub(1)).collect();
        // The By Model card's marker for default-table prices (#1717): `~`,
        // in the accent color, so nobody reads an estimate as a quoted rate.
        let money_style = if ms.estimated { accent() } else { DIM };
        let money = |c: f64| {
            if ms.estimated {
                format!("~{}", fmt_cost(c))
            } else {
                fmt_cost(c)
            }
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {name:<name_w$}"), LABEL),
            Span::styled(format!("{:>8}", fmt_tokens(ms.cached_tokens)), DIM),
            Span::styled(
                format!("{:>8}", format!("${:.2}", ms.cache_read_per_m)),
                money_style,
            ),
            Span::styled(format!("{:>9}", money(ms.cost.read_cost)), money_style),
            Span::styled(format!("{:>9}", money(ms.cost.full_rate_cost)), money_style),
            Span::styled(format!("{:>9}", money(ms.cost.saved)), money_style),
        ]));
    }

    let totals = cache.cost_totals();
    let any_estimated = cache.per_model.iter().any(|m| m.estimated);
    let total_money = |c: f64| {
        if any_estimated {
            format!("~{}", fmt_cost(c))
        } else {
            fmt_cost(c)
        }
    };
    lines.push(Line::from(vec![
        Span::styled(format!(" {:<name_w$}", "TOTAL"), BOLD),
        Span::styled(format!("{:>8}", fmt_tokens(cache.cached_tokens)), BOLD),
        Span::raw(" ".repeat(8)),
        Span::styled(format!("{:>9}", total_money(totals.read_cost)), BOLD),
        Span::styled(format!("{:>9}", total_money(totals.full_rate_cost)), BOLD),
        Span::styled(format!("{:>9}", total_money(totals.saved)), BOLD),
    ]));
    lines.push(Line::from(Span::styled(
        format!(
            " {:.0}% of input tokens were cache reads; writes cost {} on top",
            cache.cache_hit_pct,
            fmt_cost(totals.write_cost)
        ),
        DIM,
    )));
    lines.push(Line::from(Span::styled(
        " saved = full rate - read rate; ~ = default table price",
        DIM,
    )));

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
