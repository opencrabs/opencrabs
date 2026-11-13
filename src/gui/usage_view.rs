//! The six usage cards, drawn from the view model.
//!
//! Pure render functions over [`UsageViewModel`]: every string comes from the
//! view model's own formatting, every bar width from its `bar_ratio`, every
//! cache colour from its `HitBand`. Nothing here holds state or touches the
//! ledger, so the numbers on screen can only be the numbers the TUI would
//! report for the same period.
//!
//! Layout rules, learned the hard way on the first real window:
//! - Nothing is sized in `px`. Card widths, bars and text are rem-based, so
//!   the window's zoom scales the whole dashboard instead of leaving fixed
//!   islands behind.
//! - Cards wrap instead of clipping: each one asks for a 24rem column and
//!   grows into it, so a wide window shows two abreast and a narrow one shows
//!   one, with no breakpoint table to keep in sync.
//! - Names keep their full text and wrap when necessary. Project and model
//!   ledgers use real columns instead of one clipped label/value string.

use crate::palette;
use gpui::{Div, div, prelude::*, rems, rgb};

use opencrabs::ui_core::usage::view_model::{
    ActivityRow, DailyRow, HitBand, ModelRow, ProjectRow, ToolRow, UsageViewModel,
};

use crate::theme::{self, Size};

/// A card's preferred column width. Two fit abreast in the default 980px
/// window; below that they wrap to one.
const CARD_BASIS_REM: f32 = 24.0;
/// The track a bar draws inside. Fixed so ratios are comparable across rows.
const BAR_REM: f32 = 7.0;

fn text(s: impl Into<gpui::SharedString>, size: Size, color: u32) -> Div {
    theme::text(s, size, color)
}

/// A horizontal ratio bar: muted track, filled portion scaled by `ratio`
/// (already computed by the view model's own ratio() helper).
fn bar(ratio: f64, color: u32) -> Div {
    let filled = rems(BAR_REM * ratio.clamp(0.0, 1.0) as f32);
    div()
        .w(rems(BAR_REM))
        .h(rems(0.32))
        .rounded_sm()
        .bg(rgb(palette::current().track))
        .flex_shrink_0()
        .child(div().w(filled).h_full().rounded_sm().bg(rgb(color)))
}

/// Card shell: dark panel, muted title, content stacked under it. The card
/// itself clips, so a child that mis-measures can never paint past the panel
/// onto its neighbour.
fn card_shell(title: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(rems(0.5))
        .flex_basis(rems(CARD_BASIS_REM))
        .flex_grow()
        .flex_shrink()
        .bg(rgb(palette::current().panel))
        .rounded_md()
        .overflow_hidden()
        .p(rems(1.0))
        .child(text(
            title.to_string(),
            Size::Sm,
            palette::current().text_title,
        ))
}

/// A `label … value` line. The row wraps, so a narrow card stacks the value
/// under the label instead of clipping it; the label is the part that
/// ellipsizes, because the numbers are what the card is for.
fn row(left: Div, right: Div) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(rems(0.5))
        .child(left.flex_grow().flex_shrink().min_w(rems(4.0)))
        .child(right.flex_shrink_0().truncate())
}

fn empty_note(what: &str) -> Div {
    text(
        format!("no {what} recorded yet"),
        Size::Xs,
        palette::current().text_faint,
    )
}

/// Daily Activity: date, tokens, bar. Most recent day first (view model
/// guarantees the order; this renders it).
fn daily_card(rows: &[DailyRow]) -> Div {
    let mut body = card_shell("Daily activity");
    if rows.is_empty() {
        return body.child(empty_note("days"));
    }
    for d in rows {
        body = body.child(row(
            text(
                d.short_date.clone(),
                Size::Xs,
                palette::current().text_muted,
            ),
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(rems(0.5))
                .child(bar(d.bar_ratio, palette::current().green))
                .child(text(
                    d.tokens_str.clone(),
                    Size::Xs,
                    palette::current().text_normal,
                )),
        ));
    }
    body
}

/// A plain table model consumed directly by the GUI renderer. The first
/// column is the full name; later columns are numeric and right aligned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<TableRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub cells: Vec<String>,
    pub faint: bool,
    pub estimated: bool,
}

impl TableRow {
    fn new(cells: Vec<String>) -> Self {
        Self {
            cells,
            faint: false,
            estimated: false,
        }
    }
}

fn project_table(rows: &[ProjectRow]) -> Table {
    Table {
        headers: ["Project", "Cost", "Tokens", "Sessions"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        rows: rows
            .iter()
            .map(|project| {
                TableRow::new(vec![
                    project.name.clone(),
                    project.cost_str.clone(),
                    project.tokens_str.clone(),
                    project.sessions_str.clone(),
                ])
            })
            .collect(),
    }
}

fn model_table(rows: &[ModelRow]) -> Table {
    // Match the terminal: an entirely unmeasured cache column is omitted,
    // while a row without its own split stays blank rather than claiming 0%.
    let show_cache = rows.iter().any(|model| model.cache_cell.is_some());
    let mut headers = vec!["Model", "Cost", "Tokens"];
    if show_cache {
        headers.push("Cache");
    }
    headers.push("Requests");
    let mut painted = Vec::new();
    for model in rows {
        let mut cells = vec![
            model.name.clone(),
            model.cost_str.clone(),
            model.tokens_str.clone(),
        ];
        if show_cache {
            cells.push(
                model
                    .cache_cell
                    .as_deref()
                    .and_then(|cell| cell.split_whitespace().next())
                    .unwrap_or("")
                    .to_string(),
            );
        }
        cells.push(model.calls_str.clone());
        painted.push(TableRow {
            estimated: model.estimated,
            ..TableRow::new(cells)
        });
        for (index, variant) in model.variants.iter().enumerate() {
            let prefix = if index + 1 == model.variants.len() {
                "└─ "
            } else {
                "├─ "
            };
            let mut cells = vec![
                format!("{prefix}{}", variant.name),
                variant.cost_str.clone(),
                variant.tokens_str.clone(),
            ];
            if show_cache {
                cells.push(String::new());
            }
            cells.push(variant.calls_str.clone());
            painted.push(TableRow {
                faint: true,
                ..TableRow::new(cells)
            });
        }
    }
    Table {
        headers: headers.into_iter().map(str::to_string).collect(),
        rows: painted,
    }
}

fn table_line(cells: &[String], faint: bool, estimated: bool, header: bool) -> Div {
    let mut line = div()
        .flex()
        .flex_row()
        .items_start()
        .gap(rems(0.5))
        .py(rems(0.15));
    for (index, value) in cells.iter().enumerate() {
        let color = if header {
            palette::current().text_muted
        } else if index == 1 && estimated {
            palette::current().amber
        } else if faint {
            palette::current().text_faint
        } else if index == 0 {
            palette::current().text_strong
        } else {
            palette::current().text_muted
        };
        let cell = text(value.clone(), Size::Xs, color);
        line = line.child(if index == 0 {
            // No truncation: a long project or model name wraps in its own
            // flexible column while every numeric column stays put.
            cell.flex_1().min_w_0()
        } else {
            cell.w(rems(6.0)).flex_shrink_0().text_right()
        });
    }
    line
}

fn table(table: &Table) -> Div {
    let mut body = div()
        .flex()
        .flex_col()
        .child(table_line(&table.headers, false, false, true));
    for row in &table.rows {
        body = body.child(table_line(&row.cells, row.faint, row.estimated, false));
    }
    body
}

fn wide_card_shell(title: &str) -> Div {
    // Project/model tables need the whole row at the standard window width;
    // squeezing five columns into a half-width card was what produced the
    // clipped names and overlapping values.
    card_shell(title).flex_basis(rems(46.0))
}

/// By Project: full name, cost, tokens and sessions in separate columns.
fn projects_card(rows: &[ProjectRow]) -> Div {
    let body = wide_card_shell("By project");
    if rows.is_empty() {
        return body.child(empty_note("projects"));
    }
    body.child(table(&project_table(rows)))
}

/// By Model: parent and variant rows share aligned columns, including the
/// measured cache percentage when the ledger has one.
fn models_card(rows: &[ModelRow]) -> Div {
    let body = wide_card_shell("By model");
    if rows.is_empty() {
        return body.child(empty_note("models"));
    }
    body.child(table(&model_table(rows)))
}

/// Core Tools: name, calls, bar against the top tool.
fn tools_card(rows: &[ToolRow]) -> Div {
    let mut body = card_shell("Core tools");
    if rows.is_empty() {
        return body.child(empty_note("tool calls"));
    }
    for t in rows {
        body = body.child(row(
            text(t.name.clone(), Size::Xs, palette::current().text_normal),
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(rems(0.5))
                .child(bar(t.bar_ratio, palette::current().blue))
                .child(text(
                    t.call_count.to_string(),
                    Size::Xs,
                    palette::current().text_muted,
                )),
        ));
    }
    body
}

/// By Activity: category, cost, turns, one-shot share, bar against the
/// costliest category.
fn activities_card(rows: &[ActivityRow]) -> Div {
    let mut body = card_shell("By activity");
    if rows.is_empty() {
        return body.child(empty_note("activity"));
    }
    for a in rows {
        body = body.child(row(
            text(a.category.clone(), Size::Xs, palette::current().text_normal),
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(rems(0.5))
                .child(bar(a.bar_ratio, palette::current().amber))
                .child(text(
                    format!(
                        "{} · {} turns · {} one-shot",
                        a.cost_str, a.turns, a.one_shot_str
                    ),
                    Size::Xs,
                    palette::current().text_muted,
                )),
        ));
    }
    body
}

fn band_color(b: HitBand) -> u32 {
    match b {
        HitBand::High => palette::current().green,
        HitBand::Mid => palette::current().amber,
        HitBand::Low => palette::current().red,
    }
}

/// Cache Efficiency: overall hit rate in its traffic-light colour, per-model
/// lines, or the fallback `X / Y cached` line for ledgers without splits.
fn cache_card(cache: &Option<opencrabs::ui_core::usage::view_model::CacheView>) -> Div {
    let mut body = card_shell("Cache efficiency");
    let Some(c) = cache else {
        return body.child(empty_note("cache splits"));
    };
    body = body.child(row(
        text(
            "overall".to_string(),
            Size::Xs,
            palette::current().text_muted,
        ),
        text(
            format!("{:.0}%", c.overall_pct),
            Size::Sm,
            band_color(c.overall_band),
        ),
    ));
    if c.models.is_empty() {
        if let Some(line) = &c.fallback_line {
            body = body.child(text(line.clone(), Size::Xs, palette::current().text_muted));
        }
    } else {
        for m in &c.models {
            body = body.child(row(
                text(m.model.clone(), Size::Xs, palette::current().text_normal),
                text(
                    format!(
                        "{:.0}% · {} cached · {} saved",
                        m.hit_pct, m.cached_str, m.saved_str
                    ),
                    Size::Xs,
                    band_color(m.band),
                ),
            ));
        }
    }
    body.child(text(
        c.footnote1.clone(),
        Size::Xs,
        palette::current().text_faint,
    ))
}

/// One visible period-switcher segment. The window attaches its click and
/// keyboard handling; this only paints the active state.
pub fn period_tab(label: &str, active: bool) -> Div {
    div()
        .px(rems(0.75))
        .py(rems(0.35))
        .rounded_md()
        .bg(rgb(if active {
            palette::current().panel_hi
        } else {
            palette::current().panel
        }))
        .border_1()
        .border_color(rgb(if active {
            palette::current().focus
        } else {
            palette::current().track
        }))
        .child(text(
            label.to_string(),
            Size::Xs,
            if active {
                palette::current().text_strong
            } else {
                palette::current().text_muted
            },
        ))
}

/// The whole dashboard body: the four pinned summary cells, then the six
/// wrapping cards. The screen's name, period and version live in the window
/// chrome above as one line, so nothing here repeats them: the second logo
/// and the second "Usage" were the duplication the owner called out.
/// `focused` is the grid index the key layer moved to, ringed so Tab/BackTab
/// are visible.
pub fn dashboard(vm: &UsageViewModel, focused: usize) -> Div {
    let cards = [
        daily_card(&vm.daily),
        projects_card(&vm.projects),
        models_card(&vm.models),
        tools_card(&vm.tools),
        activities_card(&vm.activities),
        cache_card(&vm.cache),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, card)| {
        if i == focused {
            card.border_1().border_color(rgb(palette::current().focus))
        } else {
            card.border_1().border_color(rgb(palette::current().panel))
        }
    });
    div()
        .flex()
        .flex_col()
        .gap(rems(1.0))
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(rems(0.75))
                .child(cell("Tokens", &vm.summary.tokens_str, None))
                .child(cell(
                    "Cost",
                    &vm.summary.cost_str,
                    vm.summary.unpriced_marker.as_deref(),
                ))
                .child(cell("Sessions", &vm.summary.sessions_str, None))
                .child(cell("Calls", &vm.summary.calls_str, None)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_start()
                .gap(rems(0.75))
                .children(cards),
        )
}

/// One pinned summary cell: muted label over the view model's formatted
/// value; the optional footnote (the unpriced marker) rides under the cost.
fn cell(label: &str, value: &str, footnote: Option<&str>) -> Div {
    let mut c = div()
        .flex()
        .flex_col()
        .gap(rems(0.25))
        .flex_basis(rems(9.0))
        .flex_grow()
        .flex_shrink()
        .bg(rgb(crate::palette::current().panel_hi))
        .rounded_md()
        .px(rems(1.0))
        .py(rems(0.6))
        .child(text(
            label.to_string(),
            Size::Xs,
            palette::current().text_muted,
        ))
        .child(
            div()
                .truncate()
                .child(text(value.to_string(), Size::Xl, palette::current().text)),
        );
    if let Some(note) = footnote {
        c = c.child(text(note.to_string(), Size::Xs, palette::current().warn));
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::ui_core::usage::view_model::ModelVariantRow;

    #[test]
    fn usage_render_includes_cache_header_without_truncating_model_names() {
        let long_name = "development/qwen3.7-max-20260520-preview".to_string();
        let table = model_table(&[ModelRow {
            name: long_name.clone(),
            cost_str: "$1.23".into(),
            estimated: false,
            tokens_str: "45.6K".into(),
            calls_str: "12".into(),
            cache_cell: Some("87% cache".into()),
            variants: vec![ModelVariantRow {
                name: "qwen3.7-max-20260520".into(),
                cost_str: "$1.00".into(),
                tokens_str: "40.0K".into(),
                calls_str: "10".into(),
            }],
        }]);
        assert_eq!(
            table.headers,
            ["Model", "Cost", "Tokens", "Cache", "Requests"]
        );
        assert_eq!(table.rows[0].cells[0], long_name);
        assert_eq!(table.rows[0].cells[3], "87%");
        assert_eq!(table.rows[1].cells[3], "");
        assert!(!table.rows[0].cells[0].contains('…'));
    }

    #[test]
    fn usage_render_omits_empty_cache_column_and_keeps_project_names() {
        let table = model_table(&[ModelRow {
            name: "model-without-cache-data".into(),
            cost_str: "$0.00".into(),
            estimated: false,
            tokens_str: "0".into(),
            calls_str: "0".into(),
            cache_cell: None,
            variants: Vec::new(),
        }]);
        assert_eq!(table.headers, ["Model", "Cost", "Tokens", "Requests"]);

        let project = "Development / truelens regulatory compliance".to_string();
        let projects = project_table(&[ProjectRow {
            name: project.clone(),
            cost_str: "$2.34".into(),
            tokens_str: "67.8K".into(),
            sessions_str: "3".into(),
        }]);
        assert_eq!(projects.rows[0].cells[0], project);
        assert!(!projects.rows[0].cells[0].contains("..."));
    }
}
