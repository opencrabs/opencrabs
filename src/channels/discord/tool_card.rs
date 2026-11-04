//! Tool card for Discord (#1880): the paged, collapsible view of one turn's
//! tool rows. Everything here is pure: repeats collapse into one row, rows
//! page five at a time, the bar color and header follow the rows, and the
//! Log file is built from the full, uncollapsed list. The message state and
//! the sends live in [`super::tool_group`].

use super::tool_group::{GroupEntry, entry_icon};

/// Rows per page in the card. A page fits an embed without scrolling.
pub(crate) const PAGE_SIZE: usize = 5;

/// Bar color while any tool is still running (yellow).
pub(crate) const COLOR_RUNNING: u32 = 0xF0B232;
/// Bar color when every tool finished clean (green).
pub(crate) const COLOR_CLEAN: u32 = 0x57F287;
/// Bar color when a tool failed and nothing is still running (red).
pub(crate) const COLOR_FAILED: u32 = 0xED4245;

/// One displayed row: a run of identical consecutive calls, collapsed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CardRow {
    pub name: String,
    pub context: String,
    /// How many consecutive calls this row stands for.
    pub count: usize,
    /// None = running, Some(true) = all finished ok, Some(false) = any failed.
    pub status: Option<bool>,
    /// Set only on a detached-task row (#2148). Two detached tasks can share a
    /// label, so they must never collapse into one `x2` row: each settles on
    /// its own.
    pub task_id: Option<String>,
}

/// Whether a completion belongs to this row. By call id when both sides have
/// one, so each of two parallel calls of one tool finishes its own row.
/// Otherwise by name, as before (#1880).
pub(crate) fn entry_matches_completion(entry: &GroupEntry, name: &str, call_id: &str) -> bool {
    if !call_id.is_empty() && !entry.call_id.is_empty() {
        entry.call_id == call_id
    } else {
        entry.name == name
    }
}

/// Merge two statuses for one collapsed row. Running wins, then failed, so a
/// row never shows a finished icon while one of its calls is still running.
fn merge_status(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (None, _) | (_, None) => None,
        (Some(false), _) | (_, Some(false)) => Some(false),
        _ => Some(true),
    }
}

/// Collapse runs of consecutive calls with the same name and arguments
/// (`cargo check ×3`). Non-adjacent repeats stay separate rows, so the order
/// of the work is kept.
pub(crate) fn collapse_repeats(entries: &[GroupEntry]) -> Vec<CardRow> {
    let mut rows: Vec<CardRow> = Vec::new();
    for entry in entries {
        match rows.last_mut() {
            Some(row)
                if row.name == entry.name
                    && row.context == entry.context
                    && row.task_id.is_none()
                    && entry.task_id.is_none() =>
            {
                row.count += 1;
                row.status = merge_status(row.status, entry.status);
            }
            _ => rows.push(CardRow {
                name: entry.name.clone(),
                context: entry.context.clone(),
                count: 1,
                status: entry.status,
                task_id: entry.task_id.clone(),
            }),
        }
    }
    rows
}

/// Number of pages for `rows` displayed rows; an empty card is one page.
pub(crate) fn page_count(rows: usize) -> usize {
    rows.div_ceil(PAGE_SIZE).max(1)
}

/// The page to show, clamped into range when the rows shrank under it.
pub(crate) fn clamp_page(page: usize, rows: usize) -> usize {
    page.min(page_count(rows) - 1)
}

/// The rows on `page` (zero-based, clamped), in order.
pub(crate) fn page_slice(rows: &[CardRow], page: usize) -> &[CardRow] {
    let page = clamp_page(page, rows.len());
    let start = (page * PAGE_SIZE).min(rows.len());
    let end = (start + PAGE_SIZE).min(rows.len());
    &rows[start..end]
}

/// Longest row line in the card. A row is a glance, not a transcript: the
/// full arguments are in the Log file.
pub(crate) const ROW_MAX_CHARS: usize = 160;

/// Longest header line, so a long tool context cannot break the embed title.
pub(crate) const TITLE_MAX_CHARS: usize = 200;

/// `text` cut to `max` chars, with `…` when anything was cut.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// One row as it reads in the card: icon, bold name, the arguments, and
/// `×N` when the row stands for several calls.
pub(crate) fn row_line(row: &CardRow) -> String {
    let icon = entry_icon(row.status);
    let repeats = if row.count > 1 {
        format!(" ×{}", row.count)
    } else {
        String::new()
    };
    let label = crate::channels::background_work::detached_row_label(
        &row.name,
        row.task_id.as_deref(),
        row.status,
    );
    clip(
        &format!("{icon} **{label}**{}{repeats}", row.context),
        ROW_MAX_CHARS,
    )
}

/// Bar color from the rows: yellow while anything runs, red when a tool
/// failed, green when everything finished clean.
pub(crate) fn card_color(rows: &[CardRow]) -> u32 {
    if rows.iter().any(|r| r.status.is_none()) {
        COLOR_RUNNING
    } else if rows.iter().any(|r| r.status == Some(false)) {
        COLOR_FAILED
    } else {
        COLOR_CLEAN
    }
}

/// Header line. Names the tool that is running right now (the latest running
/// row), or the finished totals once nothing runs.
pub(crate) fn card_title(rows: &[CardRow]) -> String {
    clip(&card_title_text(rows), TITLE_MAX_CHARS)
}

fn card_title_text(rows: &[CardRow]) -> String {
    match rows.iter().rev().find(|r| r.status.is_none()) {
        Some(row) => {
            let ctx = row.context.trim();
            if ctx.is_empty() {
                format!("Running: {}", row.name)
            } else {
                format!("Running: {} {ctx}", row.name)
            }
        }
        None => {
            let failed = rows.iter().filter(|r| r.status == Some(false)).count();
            if failed > 0 {
                format!("Done · {} rows · {failed} failed", rows.len())
            } else {
                format!("Done · {} rows", rows.len())
            }
        }
    }
}

/// Footer under the rows: where the page sits, and how many calls the rows
/// stand for in total.
pub(crate) fn card_footer(rows: &[CardRow], page: usize, total_calls: usize) -> String {
    let page = clamp_page(page, rows.len());
    let shown = page_slice(rows, page).len();
    let start = page * PAGE_SIZE + 1;
    let end = page * PAGE_SIZE + shown;
    format!(
        "Page {}/{} · rows {start}-{end} of {} ({total_calls} calls, repeats collapsed)",
        page + 1,
        page_count(rows.len()),
        rows.len(),
    )
}

/// The Log file: every call, uncollapsed, in order, as a Markdown list. A
/// file has no length cap, so nothing is dropped here.
pub(crate) fn log_markdown(entries: &[GroupEntry]) -> String {
    let failed = entries.iter().filter(|e| e.status == Some(false)).count();
    let mut out = format!(
        "# Tool calls\n\n{} calls · {failed} failed\n\n",
        entries.len()
    );
    for (i, entry) in entries.iter().enumerate() {
        out.push_str(&format!(
            "{}. {} **{}**{}\n",
            i + 1,
            entry_icon(entry.status),
            entry.name,
            entry.context
        ));
    }
    out
}
