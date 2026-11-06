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
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};
use opencrabs::ui_core::plan_history::{COMPLETED_PLAN_FOOTER, CompletedPlanView};
use std::path::PathBuf;
use std::time::SystemTime;

use gpui::{Div, SharedString, Stateful, div, prelude::*, px, rgb};
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

    /// One compact line for the pane footer (#2593): `3/12` plus the task in
    /// flight. The strip above the composer already carries the detail; this
    /// is the version that survives inside a split, where the strip of a
    /// session is not always the one the user is reading.
    pub fn progress_label(&self) -> Option<String> {
        let plan = self.doc.as_ref()?;
        if plan.tasks.is_empty() {
            return Some(format!("📋 {} · not seeded", plan.title));
        }
        let done = plan
            .tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
            .count();
        let running = plan
            .tasks
            .iter()
            .find(|t| t.status == TaskStatus::InProgress)
            .map(|t| format!(" · ▸ {}", t.title))
            .unwrap_or_default();
        Some(format!("📋 {done}/{}{running}", plan.tasks.len()))
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
                .p_3()
                .rounded(px(CARD_RADIUS))
                .bg(glass_fill(palette::current().panel))
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

    /// The three-line strip a split pane draws (#2622).
    ///
    /// The terminal's pane strip is the reference
    /// (`tui::render::panes::pane_strip_lines`): the plan's name and count,
    /// the bar with its percentage, and the task in flight. A pane holds a
    /// fraction of the window, so the six-row checklist is not drawn there;
    /// it clipped mid-word and pushed its header past the pane's edge.
    pub fn compact_view(&self) -> Option<Div> {
        let plan = self.doc.as_ref()?;
        // Same rule as the full card: an Editing plan is a draft, not a
        // checklist, so a split pane shows nothing rather than a lie.
        if !matches!(plan.status, PlanStatus::Active) {
            return None;
        }
        let mut col: Div = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .min_w_0()
            .rounded(px(CARD_RADIUS))
            .bg(glass_fill(palette::current().panel));
        for (line, color) in compact_lines(plan).into_iter().zip(compact_colors()) {
            col = col.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(color))
                    .child(SharedString::from(line)),
            );
        }
        Some(col)
    }
}

/// The colours of the compact strip's three lines, in order: the name and the
/// bar in the palette's cyan (the terminal draws the bar in its success
/// colour, which is the same calm teal), the task in flight in muted ink.
fn compact_colors() -> [u32; 3] {
    [
        palette::current().teal,
        palette::current().teal,
        palette::current().text_muted,
    ]
}

/// The compact strip's three lines (#2622), split out of the view so the
/// shape is testable without a frame. Same three facts the terminal puts in a
/// pane, in the same order, and no checklist.
fn compact_lines(plan: &PlanDocument) -> Vec<String> {
    let (done, total, percent, bar) = progress(plan);
    if total == 0 {
        // Approved design still being seeded, or a seed that failed.
        return vec![format!(
            "📋 {} · checklist not seeded",
            fit(&plan.title, 40)
        )];
    }
    let running = match plan
        .tasks
        .iter()
        .find(|t| matches!(t.status, TaskStatus::InProgress))
    {
        Some(task) => format!("▸ {}", fit(&task.title, 40)),
        None if done == total => "✓ complete".to_string(),
        None => "▸ waiting".to_string(),
    };
    vec![
        format!("📋 {} · {done}/{total}", fit(&plan.title, 40)),
        format!("{bar} {percent}%"),
        running,
    ]
}

/// `done`, `total`, the percentage and the ten-cell bar. One place, so the
/// full card and the compact strip can never disagree about the same plan.
fn progress(plan: &PlanDocument) -> (usize, usize, usize, String) {
    let total = plan.tasks.len();
    let done = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
        .count();
    let percent = (done * 100).checked_div(total).unwrap_or(0);
    let filled = (done * 10).checked_div(total).unwrap_or(0);
    (
        done,
        total,
        percent,
        format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled)),
    )
}

/// The checklist body: header line, progress bar, then the visible tasks
/// windowed around the running one so a long plan cannot push the editor off
/// the screen. Six task rows is what fits above the editor at the default
/// window height without squeezing the chat pane.
fn checklist(plan: &PlanDocument) -> Div {
    let (done, total, percent, bar) = progress(plan);

    let current = plan
        .tasks
        .iter()
        .position(|t| matches!(t.status, TaskStatus::InProgress));
    // The header row fills the column (#2496): the title takes the slack and
    // truncates, the metric group keeps its width at the trailing edge. Before
    // this, the row packed to the left and left a dead band on the right, and
    // a long title pushed the bar out of the card.
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .min_w_0()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .min_w_0()
                .w_full()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .truncate()
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
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(palette::current().teal))
                        .child(bar),
                )
                .child(
                    div()
                        .flex_none()
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

/// A historical checklist in the transcript. Unlike the live strip above the
/// editor, this card has no six-row window and no interactive controls. Its
/// DOM id comes from the durable message, not from its position in the turn.
pub fn completed_plan_card(message_id: uuid::Uuid, plan: &CompletedPlanView) -> Stateful<Div> {
    let mut card = div()
        .id(SharedString::from(format!("completed-plan-{message_id}")))
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(palette::current().teal))
                .child(SharedString::from(format!("☑ {}", plan.header()))),
        );

    for task in &plan.tasks {
        card = card.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().teal))
                        .child("☑"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(format!(
                            "{}. {}",
                            task.order, task.title
                        ))),
                ),
        );
    }

    card.child(
        div()
            .text_xs()
            .italic()
            .text_color(rgb(palette::current().text_faint))
            .child(COMPLETED_PLAN_FOOTER),
    )
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
    use super::{compact_lines, visible_window};
    use opencrabs::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};

    fn plan_with(tasks: usize, done: usize, running: Option<usize>) -> PlanDocument {
        let mut plan = PlanDocument::new(uuid::Uuid::new_v4(), "Landing".to_string());
        plan.status = PlanStatus::Active;
        for i in 0..tasks {
            let mut task = PlanTask::new(i + 1, format!("task {i}"), String::new(), TaskType::Edit);
            if i < done {
                task.status = TaskStatus::Completed;
            }
            plan.add_task(task);
        }
        if let Some(i) = running {
            plan.tasks[i].status = TaskStatus::InProgress;
        }
        plan
    }

    #[test]
    fn the_compact_strip_is_three_lines_like_the_terminal() {
        // #2622: a split pane draws the terminal's three-line strip, never the
        // six-row checklist that clipped mid-word in a narrow column.
        let lines = compact_lines(&plan_with(8, 3, Some(4)));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "📋 Landing · 3/8");
        assert_eq!(lines[1], "███░░░░░░░ 37%");
        assert_eq!(lines[2], "▸ task 4");
    }

    #[test]
    fn the_compact_strip_names_the_idle_states() {
        let complete = plan_with(4, 4, None);
        assert_eq!(compact_lines(&complete)[2], "✓ complete");
        let waiting = plan_with(4, 1, None);
        assert_eq!(compact_lines(&waiting)[2], "▸ waiting");
        let mut seed = PlanDocument::new(uuid::Uuid::new_v4(), "Landing".to_string());
        seed.status = PlanStatus::Active;
        let lines = compact_lines(&seed);
        assert_eq!(lines.len(), 1, "a plan with no checklist is one line");
        assert!(lines[0].contains("checklist not seeded"));
    }

    #[test]
    fn the_compact_strip_truncates_a_long_title() {
        // A pane can be narrow; the title gives way instead of pushing the
        // count off the row.
        let mut plan = plan_with(2, 1, None);
        plan.title = "x".repeat(120);
        let lines = compact_lines(&plan);
        assert!(
            lines[0].chars().count() <= 60,
            "the title is cut, not the count: {}",
            lines[0]
        );
        assert!(lines[0].ends_with("· 1/2"));
    }

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

    #[test]
    fn the_checklist_header_gives_the_title_the_slack() {
        // #2496: the header row packed to the left, so a wide card carried a
        // dead band on the right and a long title pushed the bar out of it.
        // The title takes the slack and truncates; the metrics keep their
        // width at the trailing edge.
        let src = include_str!("plan_strip.rs");
        let body = src
            .split("fn checklist(plan: &PlanDocument) -> Div {")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("the checklist body");
        assert!(body.contains(".flex_1()"), "the title takes the slack");
        assert!(
            body.contains(".flex_none()"),
            "the metric group keeps its width"
        );
        assert!(body.contains(".truncate()"), "a long title is cut");
    }
}
