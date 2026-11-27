//! One running task opened into a view of its own.
//!
//! The card above the composer shows the last handful of lines a task wrote,
//! which answers "is it moving". This is the rest of the answer: Enter on a
//! row (or the row's `open` door) lifts the task into a modal that scrolls
//! through everything it has written, follows the newest line until the
//! reader scrolls away, and can stop the task after a confirmation.
//!
//! The modal holds its own copy of what it last read. A task that ends takes
//! its row, and often its log file, with it; the view stays open on the last
//! output and says the task ended, because the moment a task finishes is
//! exactly when its final lines are worth reading.
//!
//! Every action has a key and a pointer door: arrows, PageUp/PageDown and the
//! wheel scroll, End or a click on the notice follows again, `s` or the
//! button asks to stop, Enter or a second click confirms, Esc or the scrim
//! goes back to the list.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use gpui::{ClickEvent, IntoElement, ScrollHandle, SharedString, div, prelude::*, px, rgb};
use uuid::Uuid;

use crate::fonts;
use crate::modal_shell;
use crate::palette;
use crate::task_list::elapsed_text;
use crate::transcript::ClickHandler;

/// How often the open view repaints, so a new line and the pin to the bottom
/// land without waiting on some other part of the window to change.
pub(crate) const REPAINT_TICK: Duration = Duration::from_millis(300);

/// How often the task's file is looked at again.
pub(crate) const READ_TICK: Duration = Duration::from_secs(1);

/// One arrow press, in pixels.
pub(crate) const LINE_STEP: f32 = 48.0;

/// One PageUp or PageDown, in pixels.
pub(crate) const PAGE_STEP: f32 = 400.0;

/// How close to the end still counts as being at the end.
const FOLLOW_SLACK: f32 = 4.0;

/// The open view of one task.
pub(crate) struct TaskDetail {
    pub(crate) session: Uuid,
    pub(crate) task: Uuid,
    pub(crate) label: String,
    /// The full command as handed to the shell, printed above the output so a
    /// reader can see what the strip only had room to name (#3207). `None`
    /// for a mirrored cli task, which never handed us a command.
    pub(crate) command: Option<String>,
    pub(crate) tag: &'static str,
    /// Whether this process spawned the task and can therefore stop it.
    pub(crate) ours: bool,
    /// When the task started, so the header keeps counting.
    pub(crate) started: Instant,
    /// The file the output is read from, once it was found.
    pub(crate) path: Option<PathBuf>,
    /// Size and modification time of the file at the last read. A file that
    /// has not changed is not read again.
    pub(crate) stamp: Option<(u64, SystemTime)>,
    /// What was last read. Kept when the file disappears.
    pub(crate) text: SharedString,
    pub(crate) lines: usize,
    pub(crate) read_at: Option<Instant>,
    /// Whether the view is glued to the newest line.
    pub(crate) following: bool,
    /// The scroll range the view was last pinned to, which is how a reader
    /// scrolling up is told apart from new output arriving.
    pub(crate) pinned_max: f32,
    /// Lines that arrived while the reader was scrolled away.
    pub(crate) unseen: usize,
    /// The stop was asked for and waits on its confirmation.
    pub(crate) confirm_stop: bool,
    /// The stop was signalled; the task ends through its own exit path.
    pub(crate) stop_sent: bool,
    /// How long the task had run when it left the roster.
    pub(crate) ended: Option<u64>,
}

impl TaskDetail {
    pub(crate) fn new(
        session: Uuid,
        task: Uuid,
        label: String,
        command: Option<String>,
        tag: &'static str,
        ours: bool,
        secs: u64,
    ) -> Self {
        let now = Instant::now();
        Self {
            session,
            task,
            label,
            command,
            tag,
            ours,
            started: now.checked_sub(Duration::from_secs(secs)).unwrap_or(now),
            path: None,
            stamp: None,
            text: SharedString::default(),
            lines: 0,
            read_at: None,
            following: true,
            pinned_max: 0.0,
            unseen: 0,
            confirm_stop: false,
            stop_sent: false,
            ended: None,
        }
    }

    /// Seconds the task has run, frozen once it ended.
    pub(crate) fn secs(&self) -> u64 {
        self.ended
            .unwrap_or_else(|| self.started.elapsed().as_secs())
    }

    /// Whether a stop can be asked for right now.
    pub(crate) fn stoppable(&self) -> bool {
        self.ours && self.ended.is_none() && !self.stop_sent
    }

    /// Take a fresh read of the output. An empty read after the task ended
    /// keeps what was already there: a spawned command's log is removed when
    /// it exits, and the last lines must not vanish with it.
    pub(crate) fn absorb(&mut self, text: String) {
        if text.trim().is_empty() && (self.ended.is_some() || !self.text.is_empty()) {
            return;
        }
        if text.as_str() == AsRef::<str>::as_ref(&self.text) {
            return;
        }
        let lines = text.lines().count();
        if !self.following {
            // At the line cap the count stops moving while the text still
            // does, so a changed read is always worth at least one line.
            self.unseen += lines.saturating_sub(self.lines).max(1);
        }
        self.lines = lines;
        self.text = SharedString::from(text);
    }
}

/// What one key does while the view holds the keyboard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DetailStep {
    /// Back to the task list.
    Close,
    /// Scroll by this many pixels, positive towards the end.
    Scroll(f32),
    /// Jump to the first line.
    Top,
    /// Jump to the newest line and follow it.
    Follow,
    /// Ask whether to stop the task.
    AskStop,
    /// Stop the task.
    ConfirmStop,
    /// Take the stop question back.
    DropConfirm,
    /// The view owns the keyboard, so a key it has no use for goes nowhere.
    Ignore,
}

/// The key decision. `confirm` is the stop question being open, `stoppable`
/// whether the task is one this process can stop and is still running.
///
/// While the question is open only Enter answers yes: every other key takes
/// it back, so a stray press can never stop a build.
pub(crate) fn detail_key(key: &str, confirm: bool, stoppable: bool) -> DetailStep {
    if confirm {
        return match key {
            "enter" if stoppable => DetailStep::ConfirmStop,
            _ => DetailStep::DropConfirm,
        };
    }
    match key {
        "escape" => DetailStep::Close,
        "up" => DetailStep::Scroll(-LINE_STEP),
        "down" => DetailStep::Scroll(LINE_STEP),
        "pageup" => DetailStep::Scroll(-PAGE_STEP),
        "pagedown" => DetailStep::Scroll(PAGE_STEP),
        "home" => DetailStep::Top,
        "end" => DetailStep::Follow,
        "s" if stoppable => DetailStep::AskStop,
        _ => DetailStep::Ignore,
    }
}

/// Reconcile the follow flag with where the reader is.
///
/// `at` is how far the view is scrolled from the top, `max` the furthest it
/// can go now, `pinned_max` the furthest it could go when it was last pinned.
/// Returns whether the view follows, and the offset to pin it to when it does.
///
/// New output grows `max` and leaves `at` where it was, so a following view
/// that still sits at its old pin is simply behind and is pinned again. Only
/// a view that moved above its old pin was scrolled by the reader. A reader
/// who scrolls back down to the end is following again without pressing End.
pub(crate) fn follow_step(
    following: bool,
    at: f32,
    max: f32,
    pinned_max: f32,
) -> (bool, Option<f32>) {
    if following {
        if at < pinned_max - FOLLOW_SLACK {
            (false, None)
        } else {
            (true, Some(max))
        }
    } else if at >= max - FOLLOW_SLACK {
        (true, Some(max))
    } else {
        (false, None)
    }
}

/// The line under the title: what kind of task, how long, and where its
/// output comes from.
pub(crate) fn subtitle_text(tag: &str, secs: u64, ended: bool, path: Option<&str>) -> String {
    let time = if ended {
        format!("ended after {}", elapsed_text(secs))
    } else {
        format!("running {}", elapsed_text(secs))
    };
    match path {
        Some(path) => format!("{tag} \u{b7} {time} \u{b7} {path}"),
        None => format!("{tag} \u{b7} {time}"),
    }
}

/// The notice under the output while the reader is scrolled away.
pub(crate) fn unseen_text(unseen: usize) -> String {
    match unseen {
        0 => "scrolled up \u{b7} End or click here to follow the newest line".to_string(),
        1 => "1 new line \u{b7} End or click here to follow".to_string(),
        n => format!("{n} new lines \u{b7} End or click here to follow"),
    }
}

/// The key hints at the foot of the view.
pub(crate) fn footer_text(stoppable: bool, ours: bool, ended: bool) -> &'static str {
    if ended {
        "\u{2191}\u{2193} PgUp PgDn scroll \u{b7} End newest \u{b7} Esc back \u{b7} the task has ended"
    } else if stoppable {
        "\u{2191}\u{2193} PgUp PgDn scroll \u{b7} End follow \u{b7} s stop \u{b7} Esc back"
    } else if ours {
        "\u{2191}\u{2193} PgUp PgDn scroll \u{b7} End follow \u{b7} Esc back \u{b7} stopping"
    } else {
        "\u{2191}\u{2193} PgUp PgDn scroll \u{b7} End follow \u{b7} Esc back \u{b7} runs inside claude, not ours to stop"
    }
}

/// The doors of the view, minted by the window.
pub(crate) struct DetailDoors {
    pub(crate) close: ClickHandler,
    pub(crate) stop: ClickHandler,
    pub(crate) follow: ClickHandler,
}

/// The modal. `viewport` is the window's size, so the card fits a small
/// window instead of running off it.
pub(crate) fn detail_modal(
    detail: &TaskDetail,
    scroll: &ScrollHandle,
    viewport: (f32, f32),
    doors: DetailDoors,
) -> impl IntoElement {
    let pal = palette::current();
    let ended = detail.ended.is_some();
    let path = detail.path.as_ref().map(|p| p.display().to_string());
    let width = (viewport.0 - 64.0).clamp(320.0, 900.0);
    let out_height = (viewport.1 - 280.0).clamp(140.0, 560.0);

    let shown: SharedString = if detail.text.trim().is_empty() {
        SharedString::from(if detail.path.is_none() {
            "no output available for this task"
        } else {
            "no output yet"
        })
    } else {
        detail.text.clone()
    };

    let stop_door = doors.stop;
    let actions = if ended {
        div()
            .flex_none()
            .text_xs()
            .text_color(rgb(pal.text_faint))
            .child("ended")
            .into_any_element()
    } else if detail.stop_sent {
        div()
            .flex_none()
            .text_xs()
            .text_color(rgb(pal.warn))
            .child("stopping\u{2026}")
            .into_any_element()
    } else if !detail.ours {
        div()
            .flex_none()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(pal.track))
            .text_xs()
            .text_color(rgb(pal.text_faint))
            .child("Stop")
            .into_any_element()
    } else {
        let confirm = detail.confirm_stop;
        div()
            .id("task-detail-stop")
            .flex_none()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(pal.red))
            .text_xs()
            .text_color(rgb(if confirm { pal.bg } else { pal.red }))
            .when(confirm, |d| d.bg(rgb(pal.red)))
            .cursor_pointer()
            .child(if confirm {
                "Stop this task? Enter or click"
            } else {
                "Stop  s"
            })
            .on_click(move |ev: &ClickEvent, window, app| stop_door(ev, window, app))
            .into_any_element()
    };

    let mut body = div().flex().flex_col().gap_2().w_full().min_w_0();
    // What the strip only had room to name (#3207). Wrapped mono text, capped
    // so an inline-heredoc launch cannot push the output off the card, and
    // scrollable once it outgrows the cap.
    if let Some(command) = detail.command.as_ref() {
        body = body.child(
            div()
                .id("task-detail-command")
                .w_full()
                .min_w_0()
                .max_h(px(120.0))
                .overflow_y_scroll()
                .p_3()
                .rounded_md()
                .bg(rgb(pal.panel_hi))
                .font_family(fonts::current_mono())
                .text_xs()
                .text_color(rgb(pal.text_normal))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .child(SharedString::from(command.clone())),
                ),
        );
    }
    body = body.child(
        div()
            .id("task-detail-output")
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .h(px(out_height))
            .overflow_y_scroll()
            .track_scroll(scroll)
            .p_3()
            .rounded_md()
            .bg(rgb(pal.bg))
            .font_family(fonts::current_mono())
            .text_xs()
            .text_color(rgb(pal.text_normal))
            // One text element for the whole output: it wraps long lines and
            // keeps the blank ones, and a build log of two thousand lines is
            // one thing to lay out instead of two thousand.
            .child(div().w_full().min_w_0().child(shown)),
    );
    if !detail.following {
        let follow_door = doors.follow;
        body = body.child(
            div()
                .id("task-detail-follow")
                .w_full()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel_hi))
                .text_xs()
                .text_color(rgb(pal.teal))
                .cursor_pointer()
                .child(SharedString::from(unseen_text(detail.unseen)))
                .on_click(move |ev: &ClickEvent, window, app| follow_door(ev, window, app)),
        );
    }

    let close_door = doors.close;
    modal_shell::render(
        modal_shell::ModalView {
            id: "task-detail",
            title: detail.label.clone(),
            subtitle: subtitle_text(detail.tag, detail.secs(), ended, path.as_deref()),
            actions: Some(actions),
            header_close: true,
            body: body.into_any_element(),
            footer: Some(
                modal_shell::footer(
                    footer_text(detail.stoppable(), detail.ours, ended).to_string(),
                )
                .into_any_element(),
            ),
            width,
            max_height: viewport.1 - 48.0,
        },
        move |ev: &ClickEvent, window, app| close_door(ev, window, app),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(ours: bool) -> TaskDetail {
        TaskDetail::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "cargo build".into(),
            None,
            "detached",
            ours,
            5,
        )
    }

    #[test]
    fn the_arrows_and_pages_scroll_and_escape_goes_back() {
        assert_eq!(
            detail_key("up", false, true),
            DetailStep::Scroll(-LINE_STEP)
        );
        assert_eq!(
            detail_key("down", false, true),
            DetailStep::Scroll(LINE_STEP)
        );
        assert_eq!(
            detail_key("pageup", false, true),
            DetailStep::Scroll(-PAGE_STEP)
        );
        assert_eq!(
            detail_key("pagedown", false, true),
            DetailStep::Scroll(PAGE_STEP)
        );
        assert_eq!(detail_key("home", false, true), DetailStep::Top);
        assert_eq!(detail_key("end", false, true), DetailStep::Follow);
        assert_eq!(detail_key("escape", false, true), DetailStep::Close);
    }

    #[test]
    fn stop_is_asked_first_and_only_enter_confirms_it() {
        assert_eq!(detail_key("s", false, true), DetailStep::AskStop);
        assert_eq!(detail_key("enter", true, true), DetailStep::ConfirmStop);
        // Any other key takes the question back, Esc included: it must not
        // close the view and stop nothing in the same press.
        assert_eq!(detail_key("escape", true, true), DetailStep::DropConfirm);
        assert_eq!(detail_key("s", true, true), DetailStep::DropConfirm);
        // Enter with no question open stops nothing.
        assert_eq!(detail_key("enter", false, true), DetailStep::Ignore);
    }

    #[test]
    fn a_task_that_is_not_ours_has_no_stop_key() {
        assert_eq!(detail_key("s", false, false), DetailStep::Ignore);
        assert_eq!(detail_key("enter", true, false), DetailStep::DropConfirm);
    }

    #[test]
    fn a_key_the_view_has_no_use_for_goes_nowhere() {
        assert_eq!(detail_key("a", false, true), DetailStep::Ignore);
        assert_eq!(detail_key("tab", false, true), DetailStep::Ignore);
    }

    #[test]
    fn new_output_keeps_a_following_view_pinned() {
        // Pinned at 100, the content grew to 180, the view did not move.
        assert_eq!(follow_step(true, 100.0, 180.0, 100.0), (true, Some(180.0)));
        // The first frames, before anything was laid out.
        assert_eq!(follow_step(true, 0.0, 0.0, 0.0), (true, Some(0.0)));
    }

    #[test]
    fn scrolling_up_stops_the_follow_and_reaching_the_end_resumes_it() {
        assert_eq!(follow_step(true, 40.0, 180.0, 100.0), (false, None));
        assert_eq!(follow_step(false, 40.0, 180.0, 100.0), (false, None));
        assert_eq!(follow_step(false, 178.0, 180.0, 100.0), (true, Some(180.0)));
    }

    #[test]
    fn lines_that_arrive_while_scrolled_away_are_counted() {
        let mut d = detail(true);
        d.absorb("a\nb".into());
        assert_eq!((d.lines, d.unseen), (2, 0));
        d.following = false;
        d.absorb("a\nb\nc\nd".into());
        assert_eq!((d.lines, d.unseen), (4, 2));
        // The same text again is not new output.
        d.absorb("a\nb\nc\nd".into());
        assert_eq!(d.unseen, 2);
    }

    #[test]
    fn the_last_output_survives_the_log_being_removed() {
        let mut d = detail(true);
        d.absorb("Compiling x\nFinished".into());
        d.ended = Some(12);
        d.absorb(String::new());
        assert_eq!(d.text.to_string(), "Compiling x\nFinished");
        assert_eq!(d.secs(), 12);
    }

    #[test]
    fn a_task_can_be_stopped_only_while_it_is_ours_and_running() {
        assert!(detail(true).stoppable());
        assert!(!detail(false).stoppable());
        let mut d = detail(true);
        d.stop_sent = true;
        assert!(!d.stoppable());
        let mut d = detail(true);
        d.ended = Some(3);
        assert!(!d.stoppable());
    }

    #[test]
    fn the_subtitle_and_the_notice_read_as_sentences() {
        assert_eq!(
            subtitle_text("claude", 65, false, Some("x.output")),
            "claude \u{b7} running 1m 05s \u{b7} x.output"
        );
        assert_eq!(
            subtitle_text("detached", 9, true, None),
            "detached \u{b7} ended after 9s"
        );
        assert!(unseen_text(1).starts_with("1 new line "));
        assert!(unseen_text(7).starts_with("7 new lines "));
        assert!(unseen_text(0).starts_with("scrolled up"));
    }
}
