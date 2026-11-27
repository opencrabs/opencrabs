//! Tasks overlay: the terminal's view of what is running in the background.
//!
//! The composer border names one task and counts the rest (`cargo test 32s
//! +2`), which answers "is something running" and nothing more. This overlay
//! is the rest of the answer: every running task of the session the composer
//! sends into, and the live tail of the selected one's output.
//!
//! It reads the same roster and the same scrubbed tail the GUI's task card
//! reads, so the two surfaces cannot disagree about what a task is doing.
//! Opened with `/tasks`; it closes itself on the tick once the session has
//! nothing left running.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::brain::agent::service::background_tasks::{TaskOutput, output_tail};
use crate::tui::app::App;
use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};
use crate::tui::render::theme::{self, Role};

/// The fewest output rows the overlay asks for, however short the terminal.
/// Below this the tail stops answering "what is it doing right now".
pub(crate) const MIN_TAIL_ROWS: usize = 3;

/// Keep `selected` on a real row. The roster shrinks under the overlay as
/// tasks finish, so a selection that was valid a second ago may point past
/// the end; it falls back to the last row rather than to nothing.
pub(crate) fn clamp_selection(selected: usize, len: usize) -> usize {
    if len == 0 { 0 } else { selected.min(len - 1) }
}

/// How many output rows fit under a roster of `tasks` rows in a box whose
/// inside is `inner_height` rows tall. One row goes to the gap between the
/// roster and the output.
pub(crate) fn tail_rows(inner_height: usize, tasks: usize) -> usize {
    inner_height
        .saturating_sub(tasks.saturating_add(1))
        .max(MIN_TAIL_ROWS)
}

pub(super) fn render_tasks_overlay(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(Clear, area);

    // A task opened with Enter takes the whole area until Esc.
    if let Some(detail) = app.tasks_detail.as_ref() {
        render_task_detail(f, detail, area);
        return;
    }

    // The roster of the session the composer sends into (#2576): in split
    // view that is the focused pane's session, not the window bind.
    let tasks = app
        .send_target_session_id()
        .zip(app.agent_service.background_manager())
        .map(|(session_id, mgr)| mgr.running_tasks(session_id))
        .unwrap_or_default();
    let selected = clamp_selection(app.tasks_overlay_selected, tasks.len());

    let dim = Style::default().fg(theme::role(Role::GrayDim));
    let picked = Style::default()
        .fg(theme::role(Role::Warning))
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if tasks.is_empty() {
        lines.push(Line::from(Span::styled(
            "No background tasks running.",
            dim,
        )));
    }
    for (i, task) in tasks.iter().enumerate() {
        let (marker, style) = if i == selected {
            ("▸ ", picked)
        } else {
            ("  ", Style::default())
        };
        let elapsed = super::background::humanize_elapsed(task.started.elapsed().as_secs());
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{}", task.label), style),
            Span::styled(format!("  {} · {elapsed}", task.source.tag()), dim),
        ]));
    }

    if let Some(task) = tasks.get(selected) {
        lines.push(Line::default());
        let inner_height = area.height.saturating_sub(2) as usize;
        let rows = tail_rows(inner_height, tasks.len());
        match task.output() {
            Some(output) => {
                let tail = output_tail(&output, rows);
                if tail.trim().is_empty() {
                    lines.push(Line::from(Span::styled("(no output yet)", dim)));
                } else {
                    for line in tail.lines() {
                        lines.push(Line::from(line.to_string()));
                    }
                }
            }
            None => lines.push(Line::from(Span::styled(
                "(this task has no output file to read)",
                dim,
            ))),
        }
    }

    let title = Line::from(format!(" ⏳ Tasks · {} running ", tasks.len()));
    let mut footer =
        crate::tui::render::hints::footer_line(" ", dialog_keys(DialogScope::TasksOverlay));
    footer.spans.push(Span::raw(" "));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(footer);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ---------------------------------------------------------------------------
// One task opened into its full view.
//
// Enter on a row lifts that task out of the roster into a view of everything
// it has written: the arrows and PageUp/PageDown scroll, End follows the
// newest line again, `s` asks to stop it and Enter confirms, Esc goes back to
// the roster. The view keeps its own copy of the last read, so it stays on
// the final lines when the task ends and takes its row and log with it.
// ---------------------------------------------------------------------------

/// How often the open task's file is looked at again.
pub(crate) const DETAIL_READ_TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// How many lines one PageUp or PageDown moves.
pub(crate) const DETAIL_PAGE: usize = 10;

/// The open view of one task.
pub(crate) struct TaskDetail {
    pub(crate) session: uuid::Uuid,
    pub(crate) task: uuid::Uuid,
    pub(crate) label: String,
    pub(crate) tag: &'static str,
    /// Whether this process spawned the task and can therefore stop it.
    pub(crate) ours: bool,
    pub(crate) started: std::time::Instant,
    pub(crate) path: Option<TaskOutput>,
    /// Size and modification time at the last read: an unchanged file is not
    /// read again.
    pub(crate) stamp: Option<(u64, std::time::SystemTime)>,
    /// What was last read. Kept when the file disappears.
    pub(crate) text: String,
    pub(crate) lines: usize,
    pub(crate) read_at: Option<std::time::Instant>,
    /// How many lines above the newest one the view sits. Zero follows.
    pub(crate) up: usize,
    /// Lines that arrived while the reader was scrolled away.
    pub(crate) unseen: usize,
    pub(crate) confirm_stop: bool,
    pub(crate) stop_sent: bool,
    /// How long the task had run when it left the roster.
    pub(crate) ended: Option<u64>,
}

impl TaskDetail {
    pub(crate) fn new(
        session: uuid::Uuid,
        task: uuid::Uuid,
        label: String,
        tag: &'static str,
        ours: bool,
        started: std::time::Instant,
    ) -> Self {
        Self {
            session,
            task,
            label,
            tag,
            ours,
            started,
            path: None,
            stamp: None,
            text: String::new(),
            lines: 0,
            read_at: None,
            up: 0,
            unseen: 0,
            confirm_stop: false,
            stop_sent: false,
            ended: None,
        }
    }

    pub(crate) fn secs(&self) -> u64 {
        self.ended
            .unwrap_or_else(|| self.started.elapsed().as_secs())
    }

    /// Whether a stop can be asked for right now.
    pub(crate) fn stoppable(&self) -> bool {
        self.ours && self.ended.is_none() && !self.stop_sent
    }

    /// Take a fresh read. A reader who scrolled away keeps the lines they
    /// were looking at: the view moves up by as many lines as arrived, and
    /// counts them. An empty read never wipes what was already shown, since
    /// a spawned command's log is removed the moment it exits.
    pub(crate) fn absorb(&mut self, text: String) {
        if text.trim().is_empty() && (self.ended.is_some() || !self.text.is_empty()) {
            return;
        }
        if text == self.text {
            return;
        }
        let lines = text.lines().count();
        if self.up > 0 {
            let arrived = lines.saturating_sub(self.lines);
            self.up += arrived;
            self.unseen += arrived.max(1);
        }
        self.lines = lines;
        self.text = text;
    }
}

/// What one key does while the full view is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailStep {
    Close,
    Up(usize),
    Down(usize),
    Top,
    Follow,
    AskStop,
    ConfirmStop,
    DropConfirm,
    Ignore,
}

/// The key decision. While the stop question is open only Enter answers yes;
/// every other key takes it back, so a stray press never stops a build.
pub(crate) fn detail_key(
    event: &crossterm::event::KeyEvent,
    confirm: bool,
    stoppable: bool,
) -> DetailStep {
    use crate::tui::events::keys;
    use crossterm::event::KeyCode;
    if confirm {
        return if keys::is_enter(event) && stoppable {
            DetailStep::ConfirmStop
        } else {
            DetailStep::DropConfirm
        };
    }
    if keys::is_cancel(event) {
        DetailStep::Close
    } else if keys::is_up(event) {
        DetailStep::Up(1)
    } else if keys::is_down(event) {
        DetailStep::Down(1)
    } else if keys::is_page_up(event) {
        DetailStep::Up(DETAIL_PAGE)
    } else if keys::is_page_down(event) {
        DetailStep::Down(DETAIL_PAGE)
    } else if event.code == KeyCode::Home {
        DetailStep::Top
    } else if event.code == KeyCode::End {
        DetailStep::Follow
    } else if event.code == KeyCode::Char('s') && event.modifiers.is_empty() && stoppable {
        DetailStep::AskStop
    } else {
        DetailStep::Ignore
    }
}

/// The slice of `total` lines a view `height` rows tall shows when it sits
/// `up` lines above the newest one. `up` past the first line stops there.
pub(crate) fn view_window(total: usize, height: usize, up: usize) -> (usize, usize) {
    let height = height.max(1);
    let up = up.min(total.saturating_sub(height));
    let end = total - up;
    (end.saturating_sub(height), end)
}

/// The key hints on the bottom border.
pub(crate) fn detail_footer(d: &TaskDetail) -> String {
    if d.confirm_stop {
        " Stop this task?  Enter stops it \u{b7} any other key keeps it running ".to_string()
    } else if d.ended.is_some() {
        " \u{2191}\u{2193} PgUp PgDn scroll \u{b7} End newest \u{b7} Esc back \u{b7} the task has ended ".to_string()
    } else if d.stoppable() {
        " \u{2191}\u{2193} PgUp PgDn scroll \u{b7} End follow \u{b7} s stop \u{b7} Esc back "
            .to_string()
    } else if d.ours {
        " \u{2191}\u{2193} PgUp PgDn scroll \u{b7} End follow \u{b7} Esc back \u{b7} stopping\u{2026} ".to_string()
    } else {
        " \u{2191}\u{2193} PgUp PgDn scroll \u{b7} End follow \u{b7} Esc back \u{b7} runs inside claude, not ours to stop ".to_string()
    }
}

impl App {
    /// Lift row `at` of the roster into its full view.
    pub(crate) fn open_task_detail(&mut self, at: usize) {
        let Some((session, mgr)) = self
            .send_target_session_id()
            .zip(self.agent_service.background_manager())
        else {
            return;
        };
        let Some(task) = mgr.running_tasks(session).into_iter().nth(at) else {
            return;
        };
        self.tasks_detail = Some(TaskDetail::new(
            session,
            task.task_id,
            task.label.clone(),
            task.source.tag(),
            task.cancel.is_some(),
            task.started,
        ));
        self.refresh_task_detail();
    }

    /// Look at the open task again, at most once a [`DETAIL_READ_TICK`]:
    /// whether it still runs, where its output is, what it wrote since.
    /// Called from the tick, so the renderer stays a pure read.
    pub(crate) fn refresh_task_detail(&mut self) {
        let Some(mgr) = self.agent_service.background_manager() else {
            return;
        };
        let Some(d) = self.tasks_detail.as_mut() else {
            return;
        };
        if d.read_at.is_some_and(|at| at.elapsed() < DETAIL_READ_TICK) {
            return;
        }
        d.read_at = Some(std::time::Instant::now());
        let running = mgr
            .running_tasks(d.session)
            .iter()
            .any(|t| t.task_id == d.task);
        if !running && d.ended.is_none() {
            d.ended = Some(d.started.elapsed().as_secs());
            d.confirm_stop = false;
        }
        if d.path.is_none() && running {
            d.path = mgr.output_for(d.session, d.task);
        }
        let Some(output) = d.path.clone() else {
            return;
        };
        let stamp = std::fs::metadata(output.path())
            .ok()
            .map(|m| (m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
        if stamp.is_some() && stamp == d.stamp {
            return;
        }
        d.stamp = stamp;
        d.absorb(crate::brain::agent::service::background_tasks::output_history(&output));
    }

    /// One key while the full view is open (see [`detail_key`]).
    pub(crate) fn task_detail_key(&mut self, event: &crossterm::event::KeyEvent) {
        let Some(d) = self.tasks_detail.as_mut() else {
            return;
        };
        match detail_key(event, d.confirm_stop, d.stoppable()) {
            DetailStep::Close => self.tasks_detail = None,
            DetailStep::Up(n) => d.up = (d.up + n).min(d.lines.saturating_sub(1)),
            DetailStep::Down(n) => {
                d.up = d.up.saturating_sub(n);
                if d.up == 0 {
                    d.unseen = 0;
                }
            }
            DetailStep::Top => d.up = d.lines.saturating_sub(1),
            DetailStep::Follow => {
                d.up = 0;
                d.unseen = 0;
            }
            DetailStep::AskStop => d.confirm_stop = true,
            DetailStep::DropConfirm => d.confirm_stop = false,
            DetailStep::ConfirmStop => {
                d.confirm_stop = false;
                let (session, task) = (d.session, d.task);
                let outcome = self
                    .agent_service
                    .background_manager()
                    .map(|mgr| mgr.cancel(session, task));
                if let Some(d) = self.tasks_detail.as_mut() {
                    d.stop_sent = outcome.is_some_and(|o| o.signalled());
                }
            }
            DetailStep::Ignore => {}
        }
    }
}

fn render_task_detail(f: &mut Frame, d: &TaskDetail, area: Rect) {
    let dim = Style::default().fg(theme::role(Role::GrayDim));
    let inner_height = area.height.saturating_sub(2) as usize;
    let noticed = d.up > 0;
    let height = inner_height.saturating_sub(usize::from(noticed)).max(1);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if d.text.trim().is_empty() {
        lines.push(Line::from(Span::styled(
            if d.path.is_none() {
                "(this task has no output file to read)"
            } else {
                "(no output yet)"
            },
            dim,
        )));
    } else {
        let all: Vec<&str> = d.text.lines().collect();
        let (start, end) = view_window(all.len(), height, d.up);
        for line in &all[start..end] {
            lines.push(Line::from((*line).to_string()));
        }
    }
    if noticed {
        while lines.len() < height {
            lines.push(Line::default());
        }
        let note = match d.unseen {
            0 => "\u{2193} scrolled up \u{b7} End follows the newest line".to_string(),
            1 => "\u{2193} 1 new line \u{b7} End follows".to_string(),
            n => format!("\u{2193} {n} new lines \u{b7} End follows"),
        };
        lines.push(Line::from(Span::styled(
            note,
            Style::default().fg(theme::role(Role::AccentTeal)),
        )));
    }

    let state = if d.ended.is_some() {
        "ended after"
    } else {
        "running"
    };
    let elapsed = super::background::humanize_elapsed(d.secs());
    let room = (area.width as usize).saturating_sub(34).max(8);
    let label: String = if d.label.chars().count() > room {
        let mut cut: String = d.label.chars().take(room.saturating_sub(1)).collect();
        cut.push('\u{2026}');
        cut
    } else {
        d.label.clone()
    };
    let title = Line::from(format!(
        " \u{23f3} {label} \u{b7} {} \u{b7} {state} {elapsed} ",
        d.tag
    ));
    let footer_style = if d.confirm_stop {
        Style::default()
            .fg(theme::role(Role::Error))
            .add_modifier(Modifier::BOLD)
    } else {
        dim
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::from(Span::styled(detail_footer(d), footer_style)));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod detail_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn detail(ours: bool) -> TaskDetail {
        TaskDetail::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "cargo build".into(),
            "detached",
            ours,
            std::time::Instant::now(),
        )
    }

    #[test]
    fn the_view_shows_the_newest_lines_until_scrolled() {
        assert_eq!(view_window(100, 10, 0), (90, 100));
        assert_eq!(view_window(100, 10, 5), (85, 95));
        // Past the first line it stops at the first line.
        assert_eq!(view_window(100, 10, 500), (0, 10));
        // Fewer lines than rows shows them all.
        assert_eq!(view_window(3, 10, 0), (0, 3));
        assert_eq!(view_window(0, 10, 2), (0, 0));
    }

    #[test]
    fn the_keys_scroll_follow_and_go_back() {
        assert_eq!(
            detail_key(&key(KeyCode::Up), false, true),
            DetailStep::Up(1)
        );
        assert_eq!(
            detail_key(&key(KeyCode::Down), false, true),
            DetailStep::Down(1)
        );
        assert_eq!(
            detail_key(&key(KeyCode::PageUp), false, true),
            DetailStep::Up(DETAIL_PAGE)
        );
        assert_eq!(
            detail_key(&key(KeyCode::Home), false, true),
            DetailStep::Top
        );
        assert_eq!(
            detail_key(&key(KeyCode::End), false, true),
            DetailStep::Follow
        );
        assert_eq!(
            detail_key(&key(KeyCode::Esc), false, true),
            DetailStep::Close
        );
    }

    #[test]
    fn stop_is_asked_first_and_only_enter_confirms_it() {
        assert_eq!(
            detail_key(&key(KeyCode::Char('s')), false, true),
            DetailStep::AskStop
        );
        assert_eq!(
            detail_key(&key(KeyCode::Enter), true, true),
            DetailStep::ConfirmStop
        );
        assert_eq!(
            detail_key(&key(KeyCode::Esc), true, true),
            DetailStep::DropConfirm
        );
        assert_eq!(
            detail_key(&key(KeyCode::Enter), false, true),
            DetailStep::Ignore
        );
        // A task that is not ours has no stop key at all.
        assert_eq!(
            detail_key(&key(KeyCode::Char('s')), false, false),
            DetailStep::Ignore
        );
    }

    #[test]
    fn a_reader_scrolled_away_keeps_their_place_as_lines_arrive() {
        let mut d = detail(true);
        d.absorb("a\nb\nc".into());
        d.up = 2;
        d.absorb("a\nb\nc\nd\ne".into());
        assert_eq!((d.up, d.unseen, d.lines), (4, 2, 5));
        // Following, nothing is counted.
        let mut d = detail(true);
        d.absorb("a".into());
        d.absorb("a\nb".into());
        assert_eq!((d.up, d.unseen), (0, 0));
    }

    #[test]
    fn the_last_output_survives_the_log_being_removed() {
        let mut d = detail(true);
        d.absorb("Compiling x\nFinished".into());
        d.ended = Some(12);
        d.absorb(String::new());
        assert_eq!(d.text, "Compiling x\nFinished");
        assert_eq!(d.secs(), 12);
        assert!(!d.stoppable());
    }

    #[test]
    fn the_footer_names_the_stop_only_when_it_can_happen() {
        assert!(detail_footer(&detail(true)).contains("s stop"));
        assert!(detail_footer(&detail(false)).contains("not ours to stop"));
        let mut d = detail(true);
        d.confirm_stop = true;
        assert!(detail_footer(&d).contains("Enter stops it"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_past_the_end_falls_back_to_the_last_row() {
        assert_eq!(clamp_selection(5, 2), 1);
        assert_eq!(clamp_selection(1, 2), 1);
        assert_eq!(clamp_selection(0, 2), 0);
    }

    #[test]
    fn an_empty_roster_selects_row_zero_without_underflow() {
        assert_eq!(clamp_selection(3, 0), 0);
    }

    #[test]
    fn the_tail_takes_what_the_roster_leaves() {
        // 20 rows inside, 2 tasks, 1 gap row.
        assert_eq!(tail_rows(20, 2), 17);
    }

    #[test]
    fn a_short_terminal_still_asks_for_the_minimum_tail() {
        assert_eq!(tail_rows(4, 6), MIN_TAIL_ROWS);
        assert_eq!(tail_rows(0, 0), MIN_TAIL_ROWS);
    }
}
