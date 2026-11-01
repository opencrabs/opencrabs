//! The plan checklist strip, above the input box.
//!
//! The TUI draws a live checklist panel over the editor
//! (`tui::render::plan_widget`) so a running plan is always visible without
//! asking `/plan` again. This is the same information, in the same order:
//! a header with the plan's title, `done/total`, a ten-cell progress bar and
//! the percentage, then the tasks themselves with their status marks.
//!
//! Data comes from the plan file the agent's own plan tool writes, loaded
//! through `opencrabs::utils::plan_files::load_plan_from_path` (the TUI's
//! loader, so the legacy statuses are normalized the same way). Nothing is
//! cached behind a fake model: the file's mtime is the cache key, and a
//! rewitten plan redraws on the next frame.

use crate::palette;
use std::path::PathBuf;
use std::time::SystemTime;

use gpui::{Div, SharedString, div, prelude::*, rgb};
use opencrabs::tui::plan::{PlanDocument, PlanStatus, TaskStatus};

/// The strip's state: the plan file it is showing, the mtime it last read,
/// and the parsed document.
#[derive(Default)]
pub struct PlanStripState {
    /// Resolved by the caller, because the plan's location depends on what
    /// the session is bound to (project > profile), which is a DB lookup.
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
    /// `None` means no plan for this session (or an unreadable one); the
    /// strip then draws nothing at all rather than an empty frame.
    doc: Option<PlanDocument>,
}

impl PlanStripState {
    /// Point the strip at a session's plan file, or at nothing. A different
    /// path drops the cached document: the old plan is another session's.
    pub fn set_path(&mut self, path: Option<PathBuf>) {
        if self.path == path {
            return;
        }
        self.path = path;
        self.mtime = None;
        self.doc = None;
        self.refresh();
    }

    /// Re-read the plan when its file moved. Called from the render path,
    /// which is the only place that knows the frame is being built anyway;
    /// the mtime is the cache key, so a rewritten plan redraws next frame.
    pub fn refresh(&mut self) {
        let mtime = self
            .path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok().and_then(|m| m.modified().ok()));
        if self.mtime == mtime && self.doc.is_some() {
            return;
        }
        self.mtime = mtime;
        self.doc = self
            .path
            .as_deref()
            .and_then(opencrabs::utils::plan_files::load_plan_from_path);
    }

    /// The strip, or `None` when there is no plan to show.
    pub fn view(&self) -> Option<Div> {
        let plan = self.doc.as_ref()?;
        // An Editing plan is a design draft: nothing is executing yet, so a
        // checklist would be a lie about state. The TUI keeps its card in the
        // same place, and it only becomes a strip once it is Active.
        if !matches!(plan.status, PlanStatus::Active) {
            return None;
        }
        Some(if plan.tasks.is_empty() {
            // Approved design whose checklist is still being seeded, or a
            // seed that failed. Same two states the TUI names.
            div()
                .flex()
                .flex_row()
                .gap_2()
                .p_2()
                .rounded_sm()
                .bg(rgb(palette::current().panel))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().warn))
                        .child(SharedString::from(format!(
                            "📋 {} · checklist not seeded yet",
                            fit(&plan.title, 60)
                        ))),
                )
        } else {
            checklist(plan)
        })
    }
}

/// The checklist body: header line, progress bar, then the visible tasks
/// windowed around the running one so a long plan cannot push the editor off
/// the screen. Six task rows is what fits above the editor at the default
/// window height without squeezing the chat pane.
fn checklist(plan: &PlanDocument) -> Div {
    let total = plan.tasks.len();
    let done = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
        .count();
    let percent = (done * 100).checked_div(total).unwrap_or(0);
    let filled = (done * 10).checked_div(total).unwrap_or(0);
    let bar: String = format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled));

    let current = plan
        .tasks
        .iter()
        .position(|t| matches!(t.status, TaskStatus::InProgress));
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_sm()
        .bg(rgb(palette::current().panel))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(format!(
                            "📋 {} · {}/{}",
                            fit(&plan.title, 60),
                            done,
                            total
                        ))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().teal))
                        .child(bar),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_faint))
                        .child(SharedString::from(format!("{percent}%"))),
                ),
        );

    let window = visible_window(total, current);
    let (start, len) = window;
    for task in plan.tasks.iter().skip(start).take(len) {
        let (mark, color) = match &task.status {
            TaskStatus::Completed | TaskStatus::Skipped => ("☑", palette::current().teal),
            TaskStatus::InProgress => ("▶", palette::current().accent),
            TaskStatus::Failed => ("✗", palette::current().warn),
            TaskStatus::Blocked(_) => ("⊘", palette::current().warn),
            TaskStatus::Pending => ("☐", palette::current().text_faint),
        };
        col = col.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(div().text_xs().text_color(rgb(color)).child(mark))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(if matches!(task.status, TaskStatus::InProgress) {
                            palette::current().text
                        } else {
                            palette::current().text_muted
                        }))
                        .child(SharedString::from(format!(
                            "{}. {}",
                            task.order,
                            fit(&task.title, 70)
                        ))),
                ),
        );
    }
    col
}

/// The task window: six rows, centred on the running task, or the first six
/// when nothing is running.
fn visible_window(total: usize, current: Option<usize>) -> (usize, usize) {
    const ROWS: usize = 6;
    if total <= ROWS {
        return (0, total);
    }
    match current {
        Some(i) => {
            let start = i.saturating_sub(ROWS / 2).min(total - ROWS);
            (start, ROWS)
        }
        None => (0, ROWS),
    }
}

/// Char-safe truncation, the same rule the rail and the transcript follow
/// (#2271: never slice bytes).
fn fit(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::visible_window;

    #[test]
    fn window_centres_on_the_running_task_and_never_overruns() {
        assert_eq!(visible_window(3, Some(1)), (0, 3));
        assert_eq!(visible_window(20, Some(0)), (0, 6));
        assert_eq!(visible_window(20, Some(10)), (7, 6));
        assert_eq!(visible_window(20, None), (0, 6));
        // Every case stays inside the task list, so no row is ever dropped
        // by a window that starts past the end.
        for total in 1..40 {
            for current in [None, Some(0), Some(total / 2), Some(total - 1)] {
                let (start, len) = visible_window(total, current);
                assert!(start + len <= total, "{total} {current:?} -> {start} {len}");
            }
        }
    }
}
