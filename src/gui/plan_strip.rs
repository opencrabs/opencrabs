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
use crate::transcript::{
    ClickHandler, SMALL_TEXT_SIZE, SelectionContext, selectable_styled_text, small_style,
};
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};
use opencrabs::ui_core::plan_history::{COMPLETED_PLAN_FOOTER, CompletedPlanView};
use std::path::PathBuf;
use std::time::SystemTime;

use gpui::{Div, MouseButton, SharedString, Stateful, div, prelude::*, px, rgb};
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

/// The two taps a plan waiting on approval offers (#2755).
///
/// The window mints them: only its own view can build a listener, and only it
/// knows which session the card belongs to. The card stays a pure draw.
pub struct PlanActions {
    /// `✅ Approve plan`: sends `/execute`, the command the terminal and
    /// Telegram send.
    pub on_approve: ClickHandler,
    /// `🗑 Discard`: sends `/discard`.
    pub on_discard: ClickHandler,
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

    /// The plan this strip last read (#2691). The window's chime pass walks
    /// every strip through here and hands the documents to
    /// `completion_sound::newly_resolved`, which dedupes by task id, so the
    /// strip keeps ownership of its cache and the caller only reads it.
    pub fn document(&self) -> Option<&PlanDocument> {
        self.doc.as_ref()
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

    /// The approval card (#2755): the plan that is waiting on the user.
    ///
    /// `Editing` is the one state that carries a decision, and the window drew
    /// nothing at all in it. A GUI user was never told a plan was waiting, let
    /// alone handed the button that approves it, while the same plan in the
    /// terminal or on Telegram grew an Approve button. The taps send the two
    /// commands those surfaces send, so a tap and a typed `/execute` are one
    /// code path.
    ///
    /// `actions` is `None` only when the caller has no session to send through:
    /// the card still names the plan and the commands, it just offers no tap.
    pub fn editing_view(&self, actions: Option<&PlanActions>) -> Option<Div> {
        let plan = self.doc.as_ref()?;
        if !matches!(plan.status, PlanStatus::Editing) {
            return None;
        }
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .min_w_0()
            .rounded(px(CARD_RADIUS))
            .bg(glass_fill(palette::current().panel));
        for (kind, line) in editing_lines(plan) {
            let color = match kind {
                LineKind::Title => palette::current().text_muted,
                _ => palette::current().text_faint,
            };
            col = col.child(
                div().flex().flex_row().w_full().min_w_0().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(color))
                        .child(SharedString::from(line)),
                ),
            );
        }
        if let Some(actions) = actions {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(action_chip(
                        format!("plan-approve-{}", plan.id),
                        APPROVE_LABEL,
                        true,
                        actions.on_approve.clone(),
                    ))
                    .child(action_chip(
                        format!("plan-discard-{}", plan.id),
                        DISCARD_LABEL,
                        false,
                        actions.on_discard.clone(),
                    )),
            );
        }
        Some(col)
    }

    /// The plan as a split pane draws it: the task in flight and the next one
    /// or two while there is room, the count and the bar when there is less,
    /// and a bare bar in a wall of panes (#2622, #2624).
    ///
    /// The terminal compacts too (`tui::render::panes::pane_strip_lines`), but
    /// it knows one split width. A window has to survive anything from two
    /// panes to eight and more, so the shape is a parameter: the caller turns
    /// the pane count and the pane's own width into a [`PaneDensity`].
    pub fn pane_view(&self, density: PaneDensity, expanded: bool) -> Option<Div> {
        let plan = self.doc.as_ref()?;
        // Same rule as the full card: an Editing plan is a design draft, not a
        // checklist, so a pane shows nothing rather than a lie about state.
        if !matches!(plan.status, PlanStatus::Active) {
            return None;
        }
        // `flex_none` on the card itself: inside a pane the transcript above
        // it takes `flex_1`, and a shrinkable card was squeezed to zero height
        // in a short pane while its composer survived (#2624).
        //
        // `w_full` is what makes the rows legible at all (#2624): a column
        // sized from its own content contains truncating text, so the text was
        // measured against the width it was about to define. gpui resolved
        // that circle by giving every line one character of room, which is why
        // a split pane showed a wide card of nothing but ellipses.
        let mut col: Div = div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .gap_1()
            .px_2()
            .py_1()
            .min_w_0()
            .rounded(px(CARD_RADIUS))
            .bg(glass_fill(palette::current().panel));
        for (kind, line) in pane_lines(plan, density, expanded) {
            let color = match kind {
                LineKind::Title | LineKind::Bar | LineKind::Done => palette::current().teal,
                LineKind::Running => palette::current().text,
                LineKind::Next => palette::current().text_faint,
            };
            // A flex row with a growing, shrinkable text child, not a
            // width-full block div holding truncating text (#2624). gpui
            // measures a nowrap child once, at the flex basis it is handed, and
            // caches that on wrap_width: inside a plain block div the basis is
            // zero, so the line is laid out one character wide and stays an
            // ellipsis no matter how wide the card gets. The full card above
            // already renders through this shape (#2642); the pane card did not,
            // which is why every pane line was a dot in a wide empty card.
            col = col.child(
                div().flex().flex_row().w_full().min_w_0().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(color))
                        .child(SharedString::from(line)),
                ),
            );
        }
        Some(col)
    }
}

/// What the approval card calls the plan: the same words the Telegram chrome
/// uses, so one plan reads the same on both surfaces.
const APPROVE_LABEL: &str = "✅ Approve plan";
const DISCARD_LABEL: &str = "🗑 Discard";
/// What the card tells the user to do, under the plan it names.
pub const APPROVE_HINT: &str = "Approve with /execute, or discard with /discard.";

/// The lines the approval card prints, split out of the view so the copy is
/// testable without a frame (#2755).
pub fn editing_lines(plan: &PlanDocument) -> Vec<(LineKind, String)> {
    let header = if plan.tasks.is_empty() {
        format!("✍️ Editing plan · {}", fit(&plan.title, 60))
    } else {
        format!(
            "✍️ Editing plan · {} · {} tasks",
            fit(&plan.title, 60),
            plan.tasks.len()
        )
    };
    vec![
        (LineKind::Title, header),
        (LineKind::Next, APPROVE_HINT.to_string()),
    ]
}

/// One tap on the approval card. The id comes from the plan, so two strips
/// showing the same plan never mint the same element twice.
fn action_chip(id: String, label: &str, accent: bool, handler: ClickHandler) -> Stateful<Div> {
    let color = if accent {
        palette::current().teal
    } else {
        palette::current().text_muted
    };
    div()
        .id(SharedString::from(id))
        .flex_none()
        .px_2()
        .py_1()
        .rounded(px(CARD_RADIUS))
        .bg(rgb(palette::current().panel_hi))
        .cursor_pointer()
        .text_xs()
        .text_color(rgb(color))
        .child(SharedString::from(label.to_string()))
        // A tap on a chip is not a tap on the card it sits in: the pane's card
        // toggles its checklist on a press, and approving a plan must not also
        // rewrite that pane's layout (#2755).
        .on_mouse_down(MouseButton::Left, |_ev, _window, cx| {
            cx.stop_propagation();
        })
        .on_click(move |event, window, cx| handler(event, window, cx))
}

/// How much of a plan one pane shows (#2624). A split pane is a fraction of
/// the window and a wall of eight is a fraction of a fraction, so the card
/// gives up its detail in the order the eye wants it last: the upcoming
/// tasks first, then the title, then everything but the bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaneDensity {
    /// Up to three panes at a readable width: the running task and the next
    /// one or two.
    Tasks,
    /// Four to seven panes: the bar and the task in flight.
    Summary,
    /// Eight or more panes, or a pane too narrow to read: the bar alone. The
    /// user asked for exactly this: at eight splits a pane has room for its
    /// conversation, and the plan expands on a click.
    Bar,
}

/// Which shape a pane's plan takes: the pane count first, then the pane's own
/// width, because one narrow column of a three-pane window clips as badly as a
/// leaf of an eight-pane grid.
pub fn pane_density(panes: usize, width_px: f32) -> PaneDensity {
    if panes >= 8 || width_px < 220.0 {
        PaneDensity::Bar
    } else if panes >= 4 || width_px < 340.0 {
        PaneDensity::Summary
    } else {
        PaneDensity::Tasks
    }
}

/// What one line of a pane's plan is, so the view can colour it without
/// re-reading the plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Title,
    Bar,
    Running,
    Next,
    Done,
}

/// The lines a pane draws, split out of the view so the shape is testable
/// without a frame (#2624). Collapsed, a dense pane shows the bar and nothing
/// else; expanded, it shows the checklist the terminal shows, windowed so a
/// twenty-task plan cannot push the composer off the pane.
pub fn pane_lines(
    plan: &PlanDocument,
    density: PaneDensity,
    expanded: bool,
) -> Vec<(LineKind, String)> {
    let (done, total, percent, bar) = progress(plan);
    if total == 0 {
        // Approved design whose checklist is still being seeded, or a seed
        // that failed. Same two states the terminal names.
        return vec![
            (LineKind::Title, format!("📋 {}", fit(&plan.title, 32))),
            (LineKind::Next, CHECKLIST_NOT_SEEDED.to_string()),
        ];
    }
    if expanded {
        let mut out = vec![
            (
                LineKind::Title,
                format!("📋 {} · {done}/{total}", fit(&plan.title, 32)),
            ),
            (LineKind::Bar, format!("{bar} {percent}%")),
        ];
        let current = plan
            .tasks
            .iter()
            .position(|t| matches!(t.status, TaskStatus::InProgress));
        let (start, len) = visible_window(total, current);
        for task in plan.tasks.iter().skip(start).take(len) {
            let kind = match task.status {
                TaskStatus::Completed | TaskStatus::Skipped => LineKind::Done,
                TaskStatus::InProgress => LineKind::Running,
                TaskStatus::Failed | TaskStatus::Blocked(_) => LineKind::Running,
                TaskStatus::Pending => LineKind::Next,
            };
            out.push((kind, format!("{}. {}", task.order, fit(&task.title, 34))));
        }
        return out;
    }
    let running = plan
        .tasks
        .iter()
        .find(|t| matches!(t.status, TaskStatus::InProgress));
    let running_line = match running {
        Some(task) => format!("▸ {}", fit(&task.title, 32)),
        None if done == total => "✓ complete".to_string(),
        None => "▸ waiting".to_string(),
    };
    match density {
        // A wall of panes gets the bar and the count, and the click expands
        // it. Anything more and eight panes leave no room for conversations.
        PaneDensity::Bar => vec![(LineKind::Bar, format!("{bar} {percent}% · {done}/{total}"))],
        PaneDensity::Summary => vec![
            (LineKind::Bar, format!("{bar} {percent}%")),
            (LineKind::Running, running_line),
        ],
        PaneDensity::Tasks => {
            // The owner's shape for a split: the task in flight and the next
            // one or two, and nothing else. The plan title stays in the
            // unsplit card, where there is room for it, and the pane's own
            // header already says which session this is.
            let mut out = vec![
                (LineKind::Bar, format!("{bar} {percent}%")),
                (LineKind::Running, running_line),
            ];
            // "The next one or two" means after the task in flight, never a
            // pending row the running task already walked past.
            let after = plan
                .tasks
                .iter()
                .position(|t| matches!(t.status, TaskStatus::InProgress))
                .map(|i| i + 1)
                .unwrap_or(0);
            for task in plan
                .tasks
                .iter()
                .skip(after)
                .filter(|t| matches!(t.status, TaskStatus::Pending))
                .take(2)
            {
                out.push((LineKind::Next, format!("· {}", fit(&task.title, 32))));
            }
            out
        }
    }
}

/// The line a pane prints while an approved plan has no checklist yet.
const CHECKLIST_NOT_SEEDED: &str = "checklist not seeded";

/// `done`, `total`, the percentage and the ten-cell bar. One place, so the
/// full card and the compact strip can never disagree about the same plan.
fn progress(plan: &PlanDocument) -> (usize, usize, usize, String) {
    let (done, total) = progress_counts(plan);
    let (percent, bar) = progress_bar(done, total);
    (done, total, percent, bar)
}

/// The counting rule: Completed and Skipped both count as done. Shared with
/// the rail card, so a row's bar and the composer strip agree about the same
/// plan without a second copy of the filter.
pub(crate) fn progress_counts(plan: &PlanDocument) -> (usize, usize) {
    // One implementation, in `ui_core`: the TUI session row draws the same
    // bar for the same plan (#2701), and two copies would drift.
    opencrabs::ui_core::plan_index::progress_counts(plan)
}

/// The percentage and the ten-cell bar for a done/total pair. The rail card
/// draws through this helper too, so the bar shape lives in exactly one place.
pub(crate) fn progress_bar(done: usize, total: usize) -> (usize, String) {
    opencrabs::ui_core::plan_index::progress_bar(done, total)
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
pub fn completed_plan_card(
    message_id: uuid::Uuid,
    plan: &CompletedPlanView,
    selection: Option<&SelectionContext>,
) -> Stateful<Div> {
    let mut card = div()
        .id(SharedString::from(format!("completed-plan-{message_id}")))
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().panel))
        .child(selectable_styled_text(
            format!("\u{2611} {}", plan.header()),
            small_style(SMALL_TEXT_SIZE, palette::current().teal),
            Vec::new(),
            &[],
            selection,
        ));

    for task in &plan.tasks {
        // One line per task, spelled exactly as `copy_text` spells it, so a
        // drag over one title copies that title alone (#2678).
        card = card.child(selectable_styled_text(
            format!("  \u{2611} {}. {}", task.order, task.title),
            small_style(SMALL_TEXT_SIZE, palette::current().text_muted),
            Vec::new(),
            &[],
            selection,
        ));
    }

    card.child(selectable_styled_text(
        COMPLETED_PLAN_FOOTER.to_string(),
        small_style(SMALL_TEXT_SIZE, palette::current().text_faint),
        Vec::new(),
        &[],
        selection,
    ))
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
    use super::{LineKind, PaneDensity, pane_density, pane_lines, visible_window};
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

    /// #2691: the GUI had no chime call site at all, so the notification the
    /// terminal rings was silence in the window. The window's pass reads the
    /// strips through `document()` and dedupes with one shared seen set. This
    /// pins the data half of that wiring against a real plan file: a settled
    /// task counts once, an unchanged re-read counts zero, and the same plan
    /// drawn by two strips still counts once.
    #[test]
    fn a_settled_task_rings_once_across_every_strip_showing_it() {
        use super::PlanStripState;
        use opencrabs::utils::completion_sound::newly_resolved;
        use std::collections::HashSet;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("plan.json");
        let mut plan = plan_with(2, 1, Some(1));
        std::fs::write(
            &path,
            serde_json::to_string(&plan).expect("plan serializes"),
        )
        .expect("plan file written");

        let mut window_strip = PlanStripState::default();
        window_strip.set_path(Some(path.clone()));
        let mut pane_strip = PlanStripState::default();
        pane_strip.set_path(Some(path.clone()));
        assert!(
            window_strip.document().is_some(),
            "the strip loads the plan the chime pass reads"
        );

        // One seen set for the window, exactly as the Dashboard holds it.
        let mut seen: HashSet<uuid::Uuid> = HashSet::new();
        assert_eq!(
            newly_resolved(&mut seen, &[window_strip.document(), pane_strip.document()]),
            1,
            "the same settled task in two strips is one chime, not two"
        );
        window_strip.refresh();
        pane_strip.refresh();
        assert_eq!(
            newly_resolved(&mut seen, &[window_strip.document(), pane_strip.document()]),
            0,
            "an unchanged re-read must not ring again"
        );

        // The second task settles and the tool rewrites the file.
        plan.tasks[1].status = TaskStatus::Completed;
        std::fs::write(
            &path,
            serde_json::to_string(&plan).expect("plan serializes"),
        )
        .expect("plan file rewritten");
        window_strip.refresh();
        pane_strip.refresh();
        assert_eq!(
            newly_resolved(&mut seen, &[window_strip.document(), pane_strip.document()]),
            1,
            "the newly settled task rings once"
        );
        assert_eq!(seen.len(), 2, "both settled tasks are recorded");
    }

    /// #2624: the pane card was a wide box of ellipses in a split. Two faults
    /// stacked. A column sized from its own truncating content made gpui
    /// measure each line against the width it was about to define, and each
    /// line was a width-full block div holding truncating text, which gpui
    /// lays out once at a zero flex basis and caches as an ellipsis (#2642).
    /// Both halves are pinned: the card owns its width, and each row is a flex
    /// row with a growing, shrinkable, truncated child.
    #[test]
    fn the_pane_card_takes_the_panes_width() {
        const SRC: &str = include_str!("plan_strip.rs");
        let body = SRC
            .split_once("pub fn pane_view(")
            .expect("pane_view exists")
            .1
            .split_once("/// How much of a plan one pane shows")
            .expect("pane_view ends")
            .0;
        assert!(
            body.contains(".w_full()"),
            "the pane card must own its width, not derive it from the text it truncates"
        );
        let row = body
            .split_once("col = col.child(")
            .expect("pane rows are built in a loop")
            .1;
        let flat: String = row.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains(".flex_row()"),
            "each line is a flex row: {flat}"
        );
        assert!(
            flat.contains(".flex_1()"),
            "the text child grows and shrinks inside that row: {flat}"
        );
        assert!(
            !flat.contains(".w_full() .min_w_0() .overflow_hidden() .truncate()"),
            "the block-div truncate shape is back: gpui lays it out one character wide"
        );
    }

    /// #2755: `Editing` is the state that owns the approval, and the window
    /// drew nothing at all in it while the terminal and Telegram grew an
    /// Approve button for the same plan.
    #[test]
    fn an_editing_plan_gets_a_card_naming_the_plan_and_the_commands() {
        let mut plan = PlanDocument::new(uuid::Uuid::new_v4(), "Ship the card".to_string());
        plan.status = PlanStatus::Editing;
        let lines = super::editing_lines(&plan);
        assert_eq!(lines[0].0, LineKind::Title);
        assert!(
            lines[0].1.contains("Editing plan") && lines[0].1.contains("Ship the card"),
            "{:?}",
            lines[0]
        );
        assert_eq!(lines[1], (LineKind::Next, super::APPROVE_HINT.to_string()));
        assert!(super::APPROVE_HINT.contains("/execute"));
        assert!(super::APPROVE_HINT.contains("/discard"));

        // A plan with work says how much; an empty one still says it waits.
        let sized = plan_with(3, 0, None);
        assert!(super::editing_lines(&sized)[0].1.contains("3 tasks"));
        assert!(
            super::editing_lines(&plan)[0]
                .1
                .starts_with("\u{270d}\u{fe0f} Editing plan"),
            "an empty plan is still waiting: {:?}",
            super::editing_lines(&plan)[0]
        );
    }

    /// The card is drawn for Editing only, in both draw paths, and its two
    /// taps send the literal commands the terminal sends rather than calling
    /// an approval implementation of their own.
    #[test]
    fn the_approval_card_is_drawn_where_the_taps_can_be_pressed() {
        let src = include_str!("plan_strip.rs");
        let body = src
            .split("pub fn editing_view(")
            .nth(1)
            .and_then(|rest| rest.split("/// The plan as a split pane draws it").next())
            .expect("the editing view body");
        assert!(
            body.contains("matches!(plan.status, PlanStatus::Editing)"),
            "only a waiting plan gets the approval card"
        );
        assert!(
            body.contains("actions.on_approve") && body.contains("actions.on_discard"),
            "the card renders both taps it is handed"
        );

        let main = include_str!("main.rs");
        let panes = main
            .split("fn panes_body(")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .expect("panes body");
        assert!(
            panes.contains("editing_view(Some(&actions))"),
            "a split pane draws the waiting plan, not just the running one"
        );
        let unsplit = main
            .split("if !self.window_is_split() {")
            .nth(1)
            .and_then(|rest| rest.split("let waiting = self.queued_for_strip();").next())
            .expect("the unsplit strip block");
        assert!(
            unsplit.contains("editing_view(actions.as_ref())"),
            "the unsplit window draws the waiting plan"
        );

        let taps = main
            .split("fn plan_actions(")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .expect("the tap builder");
        assert!(
            taps.contains("\"/execute\".to_string()"),
            "Approve sends the command the terminal sends"
        );
        assert!(
            taps.contains("\"/discard\".to_string()"),
            "Discard sends the command the terminal sends"
        );
        assert!(
            taps.contains("send_turn_to(session,"),
            "a tap goes through the composer path that parses the command"
        );
    }

    #[test]
    fn a_wide_pane_shows_the_running_task_and_the_next_two() {
        // #2624: three panes at a readable width keep the detail the user
        // asked for: what is running, and the one or two tasks after it.
        let lines = pane_lines(&plan_with(8, 3, Some(4)), PaneDensity::Tasks, false);
        assert_eq!(lines.len(), 4, "bar, running, two next: {lines:?}");
        assert_eq!(lines[0].1, "███░░░░░░░ 37%");
        assert_eq!(lines[1], (LineKind::Running, "▸ task 4".to_string()));
        assert_eq!(lines[2].0, LineKind::Next);
        assert!(lines[2].1.starts_with("· task 5"), "{:?}", lines[2]);
        assert!(lines[3].1.starts_with("· task 6"), "{:?}", lines[3]);
        assert!(
            !lines.iter().any(|(kind, _)| *kind == LineKind::Title),
            "a pane spends its rows on the work, not on the plan's title"
        );
    }

    #[test]
    fn a_split_grid_drops_to_the_bar_alone() {
        // Eight panes have no room for task titles: one line, the bar and the
        // count, and the click expands the rest.
        let lines = pane_lines(&plan_with(8, 3, Some(4)), PaneDensity::Bar, false);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].0, LineKind::Bar);
        assert!(lines[0].1.ends_with("· 3/8"), "{:?}", lines[0]);
        // Four to seven keeps the task in flight, and no title.
        let summary = pane_lines(&plan_with(8, 3, Some(4)), PaneDensity::Summary, false);
        assert_eq!(summary.len(), 2, "{summary:?}");
        assert_eq!(summary[1], (LineKind::Running, "▸ task 4".to_string()));
        assert!(
            !summary.iter().any(|(kind, _)| *kind == LineKind::Title),
            "a summary pane spends its rows on the bar and the task"
        );
    }

    #[test]
    fn the_density_follows_the_pane_count_and_the_pane_width() {
        // The count decides first, then the measured width, so a narrow
        // column of a three-pane window compacts like a grid leaf.
        assert_eq!(pane_density(2, 600.0), PaneDensity::Tasks);
        assert_eq!(pane_density(3, 400.0), PaneDensity::Tasks);
        assert_eq!(pane_density(4, 400.0), PaneDensity::Summary);
        assert_eq!(pane_density(7, 400.0), PaneDensity::Summary);
        assert_eq!(pane_density(8, 400.0), PaneDensity::Bar);
        assert_eq!(pane_density(2, 300.0), PaneDensity::Summary);
        assert_eq!(pane_density(2, 200.0), PaneDensity::Bar);
    }

    #[test]
    fn an_expanded_pane_shows_the_checklist_windowed() {
        // A click expands: the same six-row window the full card uses, so a
        // long plan cannot push the composer out of the pane.
        let lines = pane_lines(&plan_with(20, 3, Some(4)), PaneDensity::Bar, true);
        assert_eq!(lines[0].0, LineKind::Title);
        assert_eq!(lines[1].0, LineKind::Bar);
        assert_eq!(lines.len(), 8, "header, bar and the six-row window");
        assert!(lines[2].1.starts_with("2. "), "{:?}", lines[2]);
        assert!(lines.iter().skip(2).any(|(k, _)| *k == LineKind::Running));
    }

    #[test]
    fn a_pane_names_the_idle_states() {
        let complete = plan_with(4, 4, None);
        let lines = pane_lines(&complete, PaneDensity::Tasks, false);
        assert_eq!(lines[1].1, "✓ complete");
        let waiting = plan_with(4, 1, None);
        assert_eq!(
            pane_lines(&waiting, PaneDensity::Tasks, false)[1].1,
            "▸ waiting"
        );
        let mut seed = PlanDocument::new(uuid::Uuid::new_v4(), "Landing".to_string());
        seed.status = PlanStatus::Active;
        let lines = pane_lines(&seed, PaneDensity::Tasks, false);
        assert_eq!(lines.len(), 2, "a plan with no checklist is two lines");
        assert!(lines[1].1.contains("checklist not seeded"));
    }

    #[test]
    fn a_pane_truncates_a_long_task_title() {
        // A pane can be narrow; a long task title is cut instead of wrapped
        // into rows the pane does not have.
        let mut plan = plan_with(3, 1, Some(1));
        plan.tasks[1].title = "x".repeat(120);
        plan.tasks[2].title = "y".repeat(120);
        let lines = pane_lines(&plan, PaneDensity::Tasks, false);
        assert_eq!(lines[1].0, LineKind::Running);
        assert!(
            lines[1].1.chars().count() <= 40,
            "the running line is cut: {}",
            lines[1].1
        );
        assert!(
            lines[1].1.ends_with('…'),
            "a cut line says it was cut: {:?}",
            lines[1]
        );
        assert!(
            lines[2].1.chars().count() <= 40,
            "the next line is cut too: {}",
            lines[2].1
        );
        // The bar keeps its count whatever the titles do.
        assert!(lines[0].1.ends_with("33%"), "{:?}", lines[0]);
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

    /// #2678: a finished plan's card is text the user reads, so each of its
    /// lines has to be a selectable segment, spelled exactly as `copy_text`
    /// spells it. Otherwise the only thing a drag can copy is the whole card.
    #[test]
    fn the_completed_card_draws_one_selectable_line_per_copy_line() {
        let src = include_str!("plan_strip.rs");
        let body = src
            .split("pub fn completed_plan_card(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("the completed card body");
        assert_eq!(
            body.matches(["selectable_", "styled_text("].concat().as_str())
                .count(),
            3,
            "the header, the task rows and the footer are all selectable"
        );
        // The draw spells the tick as an escape while `copy_text` spells it as
        // the character, so compare the way the code renders it rather than the
        // bytes it happens to be written with.
        let rendered = body.replace("\\u{2611}", "\u{2611}");
        assert!(
            rendered.contains("\u{2611} {}. {}") && body.contains("task.order, task.title"),
            "a task line is spelled the way copy_text spells it"
        );
        assert!(
            !body.contains(".text_xs()"),
            "a selectable line carries its size in the style, not on the div"
        );
    }
}
