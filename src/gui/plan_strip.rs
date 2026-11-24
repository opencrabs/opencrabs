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

use super::icon::disclosure_icon;
use crate::palette;
use crate::transcript::{
    ClickHandler, MESSAGE_RADIUS, SMALL_TEXT_SIZE, SelectionContext, message_card_surface,
    selectable_styled_text, small_style,
};
use opencrabs::ui_core::card::{glass_fill, session_edge};
use opencrabs::ui_core::plan_history::{COMPLETED_PLAN_FOOTER, CompletedPlanView};
use std::path::PathBuf;
use std::time::SystemTime;

use gpui::{
    Div, MouseButton, SharedString, Stateful, TextRun, Window, div, prelude::*, px, rgb, rgba,
};
use opencrabs::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus};

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
    /// `✅ Approve`: sends `/execute`, the command the terminal and
    /// Telegram send.
    pub on_approve: ClickHandler,
    /// `🗑️ Discard`: sends `/discard`.
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

    /// The file's last write, the key this strip caches on (#2805).
    ///
    /// A split pane is inserted through gpui's cached view path, so a pane that
    /// was not notified replays its last frame and never reaches `refresh` on
    /// its own. The walk refreshes the strip for it and compares this around
    /// the call, which is how a rewritten plan still redraws the pane.
    pub fn mtime(&self) -> Option<SystemTime> {
        self.mtime
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
    ///
    /// `window` is here to measure (#3124): every card in this file caps itself
    /// at the width of its own longest row, and that number only the live text
    /// system can give.
    pub fn view(&self, window: &Window) -> Option<Div> {
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
            let text = format!("📋 {} · checklist not seeded yet", plan.title);
            // As wide as the sentence, no wider (#3124): the row is the whole
            // card, so stretching it to the column paints a slab of empty
            // surface behind eight words.
            let width = card_width([line_width(window, &text, SMALL_TEXT_SIZE)]);
            div()
                .flex()
                .flex_row()
                .gap_2()
                .p_3()
                .w_full()
                .max_w(px(width))
                // The message card's curve and fill (#3114): the strips above
                // the composer stand in the conversation's column, so they are
                // one family with the bubbles rather than a second set of
                // panels.
                .rounded(px(MESSAGE_RADIUS))
                .bg(glass_fill(message_card_surface()))
                .border_1()
                .border_color(session_edge(palette::current().panel_hi))
                .child(card_line(text, palette::current().warn))
        } else {
            checklist(plan, window)
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
    pub fn editing_view(&self, actions: Option<&PlanActions>, window: &Window) -> Option<Div> {
        let plan = self.doc.as_ref()?;
        if !matches!(plan.status, PlanStatus::Editing) {
            return None;
        }
        let mut lines = editing_lines(plan).into_iter();
        let head = lines.next().map(|(_, line)| line).unwrap_or_default();
        let tail: Vec<(LineKind, String)> = lines.collect();
        // The card is as wide as the widest thing it draws (#3124): the head
        // row, every visible task, the two command lines, and the two chips at
        // the bottom. Anything narrower would cut the row it was measured
        // from; anything wider is surface the card never fills.
        let mut widths = vec![if plan.tasks.is_empty() {
            line_width(window, &head, SMALL_TEXT_SIZE)
        } else {
            header_width(window, plan, &head)
        }];
        for task in windowed_tasks(plan) {
            widths.push(task_row_width(window, task));
        }
        for (_, line) in &tail {
            widths.push(line_width(window, line, SMALL_TEXT_SIZE));
        }
        if actions.is_some() {
            widths.push(
                action_chip_width(window, APPROVE_LABEL)
                    + ROW_GAP
                    + action_chip_width(window, DISCARD_LABEL),
            );
        }
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .min_w_0()
            .w_full()
            .max_w(px(card_width(widths)))
            // The same curve and fill as the messages around it (#3114).
            .rounded(px(MESSAGE_RADIUS))
            .bg(glass_fill(message_card_surface()))
            .border_1()
            .border_color(session_edge(palette::current().panel_hi));
        // A plan that already carries its checklist shows what is being
        // approved: the same `done/total`, the same bar and the same six-row
        // window the live card draws once it is running. A bare count hid the
        // progress and every task behind it (#2970).
        if plan.tasks.is_empty() {
            col = col.child(card_line(head, palette::current().text_muted));
        } else {
            col = col.child(progress_header_row(plan, head));
            col = task_rows(col, plan);
        }
        for (kind, line) in tail {
            let color = match kind {
                LineKind::Title => palette::current().text_muted,
                _ => palette::current().text_faint,
            };
            col = col.child(card_line(line, color));
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
                        actions.on_approve.clone(),
                    ))
                    .child(action_chip(
                        format!("plan-discard-{}", plan.id),
                        DISCARD_LABEL,
                        actions.on_discard.clone(),
                    )),
            );
        }
        Some(col)
    }

    /// The plan as a split pane draws it: the task in flight and the next one
    /// or two while there is room, the count and the bar when there is less,
    /// and a leading chevron with the count in a wall of panes (#2622, #2624).
    ///
    /// The terminal compacts too (`tui::render::panes::pane_strip_lines`), but
    /// it knows one split width. A window has to survive anything from two
    /// panes to eight and more, so the shape is a parameter: the caller turns
    /// the pane count and the pane's own width into a [`PaneDensity`].
    pub fn pane_view(&self, density: PaneDensity, expanded: bool, window: &Window) -> Option<Div> {
        let plan = self.doc.as_ref()?;
        // Same rule as the full card: an Editing plan is a design draft, not a
        // checklist, so a pane shows nothing rather than a lie about state.
        if !matches!(plan.status, PlanStatus::Active) {
            return None;
        }
        let lines = pane_lines(plan, density, expanded);
        // The bar row draws a track and the percentage (#3131), so the card
        // has to budget for both. The wall-of-panes count keeps its compact
        // shape: no track there, the click expands it.
        let percent = progress(plan).2;
        let percent_width = line_width(window, &format!("{percent}%"), SMALL_TEXT_SIZE);
        let track_wide = |line: &str, lead: f32, wide: bool| {
            if wide {
                line_width(window, line, SMALL_TEXT_SIZE)
                    + lead
                    + PROGRESS_TRACK_WIDTH
                    + ROW_GAP
                    + percent_width
            } else {
                line_width(window, line, SMALL_TEXT_SIZE) + lead
            }
        };
        // The pane card is as wide as its longest line (#3124). A pane is the
        // place this matters most: the card was `w_full` inside a column that
        // can be 600 points, so a task name was cut to forty characters with
        // most of the card empty beside it, and the ellipsis was read as "the
        // title is long" when the truth was "the card is short of nothing but
        // its own text".
        let mut widths = Vec::with_capacity(lines.len());
        for (index, (kind, line)) in lines.iter().enumerate() {
            // The leading chevron and the gap after it belong to the first row
            // alone; the rest start at the card's padding.
            let lead = if index == 0 { 16.0 + ROW_GAP } else { 0.0 };
            let wide = matches!(kind, LineKind::Bar) && (expanded || density != PaneDensity::Bar);
            widths.push(track_wide(line, lead, wide));
        }
        // `flex_none` on the card itself: inside a pane the transcript above
        // it takes `flex_1`, and a shrinkable card was squeezed to zero height
        // in a short pane while its composer survived (#2624).
        //
        // `w_full` is what makes the rows legible at all (#2624): a column
        // sized from its own content contains truncating text, so the text was
        // measured against the width it was about to define. gpui resolved
        // that circle by giving every line one character of room, which is why
        // a split pane showed a wide card of nothing but ellipses. The cap
        // added in #3124 is what stops the other half of that: full width for
        // a line that needs a third of it.
        //
        // The padding is the family's (#3117). It was a bare 8 by 4, so the
        // card's own lines started on its edge and the card read as a strip of
        // text pressed against two borders rather than as the plan card the
        // unsplit chat draws. Every other plan card pads 12, and a pane's card
        // is the same object on a narrower column.
        let mut col: Div = div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .max_w(px(card_width(widths)))
            .gap_1()
            .p_3()
            .min_w_0()
            // The message card's curve and fill (#3117), the same pair the
            // unseeded row, the approval card, the live checklist and the
            // finished card wear since #3114. This was the last plan card on
            // the larger card curve over the raised panel, so in a split the
            // plan read as a panel from another window wedged in above the
            // composer.
            .rounded(px(MESSAGE_RADIUS))
            .bg(glass_fill(message_card_surface()))
            // Same edge as the unsplit card (#3110): a pane paints the same
            // session surface, so its card cannot be the one without a border.
            .border_1()
            .border_color(session_edge(palette::current().panel_hi))
            .cursor_pointer()
            // The hover is the one thing that keeps the raised tone (#3117):
            // the card is a tap target in a pane (it folds the checklist open),
            // so it still has to answer the pointer, and the message fill has
            // nothing lighter to step to.
            .hover(|d| d.bg(rgb(palette::current().panel_hi)));
        // The expanded checklist scrolls (#3124). It used to be clipped to the
        // six rows `visible_window` returns, so a ten-task plan in a pane was
        // a card that stopped mid-list with no way to see the rest, while the
        // terminal showed all of them. The head and the bar stay put and only
        // the task rows move, so the count is always on screen.
        let mut scroll: Vec<Div> = Vec::new();
        for (index, (kind, line)) in lines.into_iter().enumerate() {
            let color = match kind {
                LineKind::Title | LineKind::Bar | LineKind::Done => palette::current().teal,
                LineKind::Running => palette::current().text,
                LineKind::Next => palette::current().text_faint,
            };
            // The bar row draws the track and the percentage (#3131): the
            // same drawn bar the composer header paints, never the glyph
            // string. The wall-of-panes count stays plain text, because a
            // wall has room for the count alone and the click expands it.
            let draws_track =
                matches!(kind, LineKind::Bar) && (expanded || density != PaneDensity::Bar);
            // A flex row with a growing, shrinkable text child, not a
            // width-full block div holding truncating text (#2624). gpui
            // measures a nowrap child once, at the flex basis it is handed, and
            // caches that on wrap_width: inside a plain block div the basis is
            // zero, so the line is laid out one character wide and stays an
            // ellipsis no matter how wide the card gets. The full card above
            // already renders through this shape (#2642); the pane card did not,
            // which is why every pane line was a dot in a wide empty card.
            let row = div()
                .flex()
                .flex_row()
                .w_full()
                .min_w_0()
                .when(index == 0, |d| {
                    d.child(
                        div()
                            .flex_shrink_0()
                            .w(px(16.0))
                            .child(disclosure_icon(color, 1.0)),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(color))
                        .child(SharedString::from(line)),
                );
            let row = if draws_track {
                row.child(progress_bar_view(percent, color)).child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(color))
                        .child(SharedString::from(format!("{percent}%"))),
                )
            } else {
                row
            };
            if expanded && index >= 2 {
                scroll.push(row);
            } else {
                col = col.child(row);
            }
        }
        if scroll.is_empty() {
            return Some(col);
        }
        Some(
            col.child(
                div()
                    .id(SharedString::from(format!("plan-pane-scroll-{}", plan.id)))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .max_h(px(PANE_SCROLL_HEIGHT))
                    .overflow_y_scroll()
                    .children(scroll),
            ),
        )
    }
}

/// What the approval card calls the plan: the same two decisions Telegram
/// offers, in fewer words (#3124). The card already names the plan one line
/// up, so repeating "plan" inside the button only made the label longer than
/// the decision it carries.
const APPROVE_LABEL: &str = "✅ Approve";
const DISCARD_LABEL: &str = "🗑️ Discard";
/// What the card tells the user to do, under the plan it names.
pub const APPROVE_HINT: &str = "Approve with /execute, or discard with /discard.";

/// The lines the approval card prints, split out of the view so the copy is
/// testable without a frame (#2755).
///
/// A plan that already carries its checklist says how much of it is done, not
/// just how many rows it has. "6 tasks" told the reader the size of the plan
/// and nothing else, while the live card two states later prints `done/total`,
/// Telegram prints every row, and Discord prints `done/total` beside them
/// (#2970). A design plan with no checklist keeps the bare header, because
/// there is nothing to count.
pub fn editing_lines(plan: &PlanDocument) -> Vec<(LineKind, String)> {
    let header = if plan.tasks.is_empty() {
        format!("✍️ Editing plan · {}", plan.title)
    } else {
        let (done, total) = progress_counts(plan);
        format!("✍️ Editing plan · {} · {done}/{total}", plan.title)
    };
    vec![
        (LineKind::Title, header),
        (LineKind::Next, APPROVE_HINT.to_string()),
    ]
}

/// One tap on the approval card. The id comes from the plan, so two strips
/// showing the same plan never mint the same element twice.
///
/// The chrome is the offer rows' chrome (#3124): the same curve one step
/// inside the message card, a 1px edge, the body size, and a wash instead of
/// the raised panel. Drawn as bare text on `panel_hi` at `text_xs` the two
/// decisions read as lines flying loose inside the card, not as the buttons
/// they are, and the card was the only place in the window that asked for a
/// click without showing an edge to click.
fn action_chip(id: String, label: &str, handler: ClickHandler) -> Stateful<Div> {
    let p = palette::current();
    // Both chips wear the plain offer row: the quiet wash, the quiet edge and
    // the same lift on hover. The accent belongs to an offer row the keyboard
    // is already on, and a plan card has no such row, so painting Approve
    // accent made it look chosen before anything had been chosen and left
    // Discard reading as a different kind of control (#3124).
    div()
        .id(SharedString::from(id))
        .flex_none()
        .px_3()
        .py_1p5()
        .rounded(px(MESSAGE_RADIUS - 4.0))
        .border_1()
        .border_color(rgba(0xffffff1a))
        .bg(rgba(0xffffff0d))
        .hover(|d| d.bg(rgba(0xffffff14)).border_color(rgba(0xffffff33)))
        .cursor_pointer()
        // The card's own size, not the answer's body size (#3140). These two
        // chips sit in the plan card, so they follow the title and the task
        // rows it holds; at the words of a message they came out larger than
        // the plan they were approving, which is the one thing on the card a
        // reader should not notice first. The offer rows keep the body size
        // for the opposite reason (#3124): they stand on their own, with no
        // card around them to set the measure.
        .text_size(px(SMALL_TEXT_SIZE))
        .text_color(rgb(p.text_normal))
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
/// tasks first, then the title, then everything but the disclosure and count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaneDensity {
    /// Up to three panes at a readable width: the running task and the next
    /// one or two.
    Tasks,
    /// Four to seven panes: the bar and the task in flight.
    Summary,
    /// Eight or more panes, or a pane too narrow to read: the disclosure and
    /// count alone. At eight splits a pane has room for its conversation, and
    /// the plan expands on a click.
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
/// without a frame (#2624). Collapsed, a dense pane shows the disclosure and
/// count; expanded, it shows the checklist the terminal shows, windowed so a
/// twenty-task plan cannot push the composer off the pane.
pub fn pane_lines(
    plan: &PlanDocument,
    density: PaneDensity,
    expanded: bool,
) -> Vec<(LineKind, String)> {
    let (done, total, _) = progress(plan);
    if total == 0 {
        // Approved design whose checklist is still being seeded, or a seed
        // that failed. Same two states the terminal names.
        return vec![
            (LineKind::Title, plan.title.clone()),
            (LineKind::Next, CHECKLIST_NOT_SEEDED.to_string()),
        ];
    }
    if expanded {
        let mut out = vec![
            (LineKind::Title, format!("{} · {done}/{total}", plan.title)),
            // The bar row carries no text: the view draws the track and the
            // percentage itself (#3131), and the count already rides the
            // title beside it.
            (LineKind::Bar, String::new()),
        ];
        // Every task, not the six-row window (#3124). The card that draws this
        // list scrolls, so the window only ever hid work: a twenty-task plan in
        // a pane stopped at six rows with no way to reach the rest, while the
        // terminal showed all of them. The head and the bar stay put and the
        // rows move under them.
        for task in &plan.tasks {
            let kind = match task.status {
                TaskStatus::Completed | TaskStatus::Skipped => LineKind::Done,
                TaskStatus::InProgress => LineKind::Running,
                TaskStatus::Failed | TaskStatus::Blocked(_) => LineKind::Running,
                TaskStatus::Pending => LineKind::Next,
            };
            out.push((kind, format!("{}. {}", task.order, task.title)));
        }
        return out;
    }
    let running = plan
        .tasks
        .iter()
        .find(|t| matches!(t.status, TaskStatus::InProgress));
    let running_line = match running {
        Some(task) => format!("▸ {}", task.title),
        None if done == total => "✓ complete".to_string(),
        None => "▸ waiting".to_string(),
    };
    let progress_line = format!("{done}/{total}");
    match density {
        // A wall of panes gets the chevron and count. The click expands it,
        // and anything more leaves no room for conversations.
        PaneDensity::Bar => vec![(LineKind::Bar, format!("{done}/{total}"))],
        PaneDensity::Summary => vec![
            (LineKind::Bar, progress_line.clone()),
            (LineKind::Running, running_line),
        ],
        PaneDensity::Tasks => {
            // The owner's shape for a split: the task in flight and the next
            // one or two, and nothing else. The plan title stays in the
            // unsplit card, where there is room for it, and the pane's own
            // header already says which session this is.
            let mut out = vec![
                (LineKind::Bar, progress_line),
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
                out.push((LineKind::Next, format!("· {}", task.title)));
            }
            out
        }
    }
}

/// The line a pane prints while an approved plan has no checklist yet.
const CHECKLIST_NOT_SEEDED: &str = "checklist not seeded";

/// `done`, `total` and the percentage. One place, so the full card and the
/// compact strip can never disagree about the same plan.
fn progress(plan: &PlanDocument) -> (usize, usize, usize) {
    let (done, total) = progress_counts(plan);
    let (percent, _) = progress_bar(done, total);
    (done, total, percent)
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

/// The plan bar as a drawn track, not a glyph string (#3131).
///
/// The ten `█`/`░` cells were typed text with no font family, so every
/// platform resolved the block elements through its own fallback chain: on
/// Linux with Inter neither codepoint exists, so the cell width was whatever
/// fontconfig answered, `░` dithered differently per face, and there was no
/// track at all. A recessed slot with a fill child at the done percentage is
/// font independent, resolution independent, and gives the bar a real track
/// for free. The cell count disappears with the glyphs; `percent` is exact.
pub(crate) fn progress_bar_view(percent: usize, colour: u32) -> Div {
    div()
        .flex_none()
        .w(px(PROGRESS_TRACK_WIDTH))
        .h(px(6.0))
        .rounded_full()
        .bg(rgb(palette::current().panel))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative((percent.min(100) as f32) / 100.0))
                .bg(rgb(colour)),
        )
}

/// The header row of a plan card: the line the caller wrote, then the bar and
/// the percentage at the trailing edge (#2496). The title takes the slack and
/// cuts, so a long plan name never pushes the metrics out of the card.
fn progress_header_row(plan: &PlanDocument, text: String) -> Div {
    let (_, _, percent, _) = progress(plan);
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
                .child(SharedString::from(text)),
        )
        .child(progress_bar_view(percent, palette::current().teal))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(rgb(palette::current().text_faint))
                .child(SharedString::from(format!("{percent}%"))),
        )
}

/// One task row: the status mark and the numbered title. Shared by the live
/// checklist and the approval card, so the six tasks a plan is approved on are
/// spelled exactly the way that plan then reports its progress.
fn task_row(task: &PlanTask) -> Div {
    let (mark, color) = task_mark(task);
    div()
        .flex()
        .flex_row()
        .gap_2()
        .child(div().text_xs().text_color(rgb(color)).child(mark))
        .child(
            // One row whatever the title does (#3124): the card is measured
            // from this string, so it only cuts when the pane itself is
            // narrower than the words, and then it says so with an ellipsis
            // rather than spilling past the card's edge.
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .truncate()
                .text_xs()
                .text_color(rgb(if matches!(task.status, TaskStatus::InProgress) {
                    palette::current().text
                } else {
                    palette::current().text_muted
                }))
                .child(SharedString::from(task_line(task))),
        )
}

/// The mark a task's status draws and the colour it draws in.
fn task_mark(task: &PlanTask) -> (&'static str, u32) {
    match &task.status {
        TaskStatus::Completed | TaskStatus::Skipped => ("☑", palette::current().teal),
        TaskStatus::InProgress => ("▶", palette::current().accent),
        TaskStatus::Failed => ("✗", palette::current().warn),
        TaskStatus::Blocked(_) => ("⊘", palette::current().warn),
        TaskStatus::Pending => ("☐", palette::current().text_faint),
    }
}

/// What a task row prints beside its mark. Untruncated on purpose (#3124): the
/// card is measured from this string, so a character cap here would cut a title
/// and then measure the cut, which is the bug the cap was meant to prevent.
fn task_line(task: &PlanTask) -> String {
    format!("{}. {}", task.order, task.title)
}

/// One plain line of a card: the text owns the row and cuts when it is long.
fn card_line(text: String, color: u32) -> Div {
    div().flex().flex_row().w_full().min_w_0().child(
        div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .truncate()
            .text_xs()
            .text_color(rgb(color))
            .child(SharedString::from(text)),
    )
}

/// The task window a card draws: six rows centred on the running task, the
/// same window the terminal and the compact pane use. `visible_window` returns
/// the slice; this walks it into the column.
fn task_rows(col: Div, plan: &PlanDocument) -> Div {
    let mut col = col;
    for task in windowed_tasks(plan) {
        col = col.child(task_row(task));
    }
    col
}

/// The tasks the six-row window shows, in order. One place, so the rows a card
/// draws and the rows it measures can never be different sets (#3124).
fn windowed_tasks(plan: &PlanDocument) -> impl Iterator<Item = &PlanTask> + '_ {
    let current = plan
        .tasks
        .iter()
        .position(|t| matches!(t.status, TaskStatus::InProgress));
    let (start, len) = visible_window(plan.tasks.len(), current);
    plan.tasks.iter().skip(start).take(len)
}

/// The checklist body: header line, progress bar, then the visible tasks
/// windowed around the running one so a long plan cannot push the editor off
/// the screen. Six task rows is what fits above the editor at the default
/// window height without squeezing the chat pane.
fn checklist(plan: &PlanDocument, window: &Window) -> Div {
    let (done, total) = progress_counts(plan);
    let header = format!("📋 {} · {done}/{total}", plan.title);
    // As wide as the widest row it draws (#3124): the header with its bar and
    // percentage, or whichever visible task runs longest. The card is a column
    // of short lines, so without the cap a plan called "Fix the offer card"
    // painted 900 points of surface for 300 points of words.
    let mut widths = vec![header_width(window, plan, &header)];
    for task in windowed_tasks(plan) {
        widths.push(task_row_width(window, task));
    }
    let col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .min_w_0()
        .w_full()
        .max_w(px(card_width(widths)))
        // The message card's curve and fill (#3114), so the card does not change
        // family when the plan stops waiting and starts running.
        .rounded(px(MESSAGE_RADIUS))
        .bg(glass_fill(message_card_surface()))
        .border_1()
        .border_color(session_edge(palette::current().panel_hi))
        .child(progress_header_row(plan, header));
    task_rows(col, plan)
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
        // A row of the transcript takes the message card's curve and fill
        // (#3114): it is drawn between two bubbles, so anything else reads as a
        // card from another window wedged into the history.
        .rounded(px(MESSAGE_RADIUS))
        .bg(glass_fill(message_card_surface()))
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
pub(crate) fn visible_window(total: usize, current: Option<usize>) -> (usize, usize) {
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

/// The card's own step inside its border: `p_3`, both sides.
const CARD_PAD_X: f32 = 24.0;
/// The 1px border, both sides.
const CARD_EDGE: f32 = 2.0;
/// `gap_2`, the step a row keeps between its parts.
const ROW_GAP: f32 = 8.0;

/// The drawn track's width in points (#3131). The pane card's width budget
/// and the header's share it, so the card is sized for the bar it actually
/// draws.
const PROGRESS_TRACK_WIDTH: f32 = 60.0;
/// `px_3`, the padding inside one of the approval card's chips.
const CHIP_PAD_X: f32 = 24.0;
/// A few points of slack, so a measured line is never a pixel short of the box
/// that has to draw it and cut it with an ellipsis.
const CARD_SLACK: f32 = 6.0;
/// How tall the expanded pane checklist gets before it scrolls: about seven
/// rows of small type with the gaps between them (#3124).
const PANE_SCROLL_HEIGHT: f32 = 160.0;

/// The width one line paints at, shaped by the live text system in the card's
/// own family and size (#3124).
///
/// A card that spans the column leaves most of its surface empty when its rows
/// are short, so every card in this file caps itself at the width of its
/// longest row. That number has to come from the shaper: a character count is a
/// guess, and the guess is what cut a task title at forty characters inside a
/// pane with two thirds of its card still blank. A shaper that refuses the run
/// falls back to the same per-character estimate `sessions::text_width` uses,
/// which keeps the card placeable rather than collapsing it.
fn line_width(window: &Window, text: &str, size: f32) -> f32 {
    let style = window.text_style();
    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_text(
            SharedString::from(text.to_string()),
            px(size),
            &[run],
            None,
            None,
        )
        .ok()
        .and_then(|lines| lines.into_iter().next())
        .map(|line| f32::from(line.width()))
        .unwrap_or_else(|| text.chars().count() as f32 * size * 0.7)
}

/// What a card caps itself at: its widest row, plus the padding, the border and
/// the slack it draws in (#3124).
fn card_width(rows: impl IntoIterator<Item = f32>) -> f32 {
    rows.into_iter().fold(0.0_f32, f32::max) + CARD_PAD_X + CARD_EDGE + CARD_SLACK
}

/// The width of a card's header row: the line the caller wrote, then the drawn
/// track and the percentage at the trailing edge with the row's own 8 between
/// them (#2496, #3124, #3131).
fn header_width(window: &Window, plan: &PlanDocument, text: &str) -> f32 {
    let (_, _, percent) = progress(plan);
    line_width(window, text, SMALL_TEXT_SIZE)
        + ROW_GAP
        + PROGRESS_TRACK_WIDTH
        + ROW_GAP
        + line_width(window, &format!("{percent}%"), SMALL_TEXT_SIZE)
}

/// The width of one task row: the status mark, the gap, then the numbered
/// title.
fn task_row_width(window: &Window, task: &PlanTask) -> f32 {
    let (mark, _) = task_mark(task);
    line_width(window, mark, SMALL_TEXT_SIZE)
        + ROW_GAP
        + line_width(window, &task_line(task), SMALL_TEXT_SIZE)
}

/// The width of one approval chip: its label at the card's size, the padding
/// inside the pill, and the 1px edge on both sides (#3124, #3140).
///
/// The size has to be the one the chip draws with. Measuring at the answer's
/// body size while the chip drew at the card's would budget a pixel per
/// character of slack the label never needs, and the card is sized from this.
fn action_chip_width(window: &Window, label: &str) -> f32 {
    line_width(window, label, SMALL_TEXT_SIZE) + CHIP_PAD_X + CARD_EDGE
}
