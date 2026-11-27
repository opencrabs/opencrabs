//! The running tasks of one session, opened out from the strip above the
//! composer.
//!
//! The strip names the oldest task and counts the rest. A click opens it into
//! one row per task, and a click on a row shows the last lines that task has
//! written. It follows the plan strip and the queued list beside it: the card
//! sits in the column flow, takes its room from the conversation only while it
//! is open, and is gone the moment the session has nothing running.
//!
//! At rest it is always the one line, on the right above the composer. The
//! card opens on a click, or on Down from the composer's last row, which also
//! hands it the keyboard: the arrows walk the rows, Esc stops the row they
//! stand on, one task per press, and Up off the first row gives the keyboard
//! back. Up from the composer already belongs to attachments, so the tasks
//! sit on the other arrow.

use gpui::{Div, SharedString, div, prelude::*, px, rgb};
use uuid::Uuid;

use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};

use crate::fonts;
use crate::palette;
use crate::transcript::ClickHandler;

/// How many lines of a task's output the card shows.
pub(crate) const TAIL_LINES: usize = 8;

/// How often an open task's output is re-read.
pub(crate) const TAIL_TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// What one key does while the task list holds the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyStep {
    /// Stand on this row.
    Select(usize),
    /// Stop the task on this row.
    Cancel(usize),
    /// Open the task on this row into its full view.
    Open(usize),
    /// Give the keyboard back to the composer.
    Leave,
}

/// The key decision, with `at` the row the cursor stands on and `count` the
/// rows there are.
///
/// Down stops at the last row instead of wrapping, so holding it never
/// loops past the task the user was reaching for. Up off the first row is the
/// way out, the mirror of the Down that came in. Esc stops the row it stands
/// on and stays, so several tasks are cancelled one press each. Any other key
/// is the user going back to typing.
pub(crate) fn key_step(key: &str, at: Option<usize>, count: usize) -> KeyStep {
    if count == 0 {
        return KeyStep::Leave;
    }
    let at = at.unwrap_or(0).min(count - 1);
    match key {
        "down" => KeyStep::Select((at + 1).min(count - 1)),
        "up" if at == 0 => KeyStep::Leave,
        "up" => KeyStep::Select(at - 1),
        "escape" => KeyStep::Cancel(at),
        "enter" => KeyStep::Open(at),
        _ => KeyStep::Leave,
    }
}

/// The hint on the right of the card's header.
///
/// With the keyboard on the list it names the keys, and it says outright when
/// the row under the cursor is one this process cannot stop, so Esc never
/// looks like it failed.
pub(crate) fn hint_text(focused: bool, selected_ours: Option<bool>) -> &'static str {
    match (focused, selected_ours) {
        (false, _) => "click to collapse",
        (true, Some(false)) => {
            "\u{2191}\u{2193} select \u{b7} enter opens \u{b7} not ours to stop, esc leaves"
        }
        (true, _) => "\u{2191}\u{2193} select \u{b7} enter opens \u{b7} esc cancels this task",
    }
}

/// How much of the oldest task's name the collapsed strip prints.
///
/// The strip is a status line, not a command line: it exists to say WHAT is
/// running and for HOW LONG in as little width as possible (#3207), which
/// matters most in split panes where several strips share one column. The
/// label that reaches it is already the payload statement with its paths
/// folded, so 32 characters name the work; the full command lives in the
/// detail modal, one click away.
pub(crate) const STRIP_LABEL_CHARS: usize = 32;

/// One running task, as the card prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskRow {
    pub(crate) id: Uuid,
    pub(crate) label: String,
    /// The full command as handed to the shell, when this process spawned it.
    /// The label is cut for the strip (#3207), so this is what the detail
    /// modal prints in full. `None` for mirrored cli tasks, whose command was
    /// never ours.
    pub(crate) command: Option<String>,
    pub(crate) tag: &'static str,
    pub(crate) secs: u64,
    /// Whether this process spawned the task and can therefore stop it. A
    /// mirrored claude-cli task runs inside the CLI and is not ours to kill.
    pub(crate) ours: bool,
}

/// `42s`, `3m 05s`, `1h 12m`: the seconds stay while they still say
/// something, and drop once the task is measured in hours.
pub(crate) fn elapsed_text(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// `1 task running` or `3 tasks running`.
pub(crate) fn header_text(count: usize) -> String {
    if count == 1 {
        "1 task running".to_string()
    } else {
        format!("{count} tasks running")
    }
}

/// What the output block says for an open task: its tail, or a line that
/// explains the silence. A task with no file to read and one that has not
/// written yet look the same on disk, and both are ordinary.
pub(crate) fn tail_or_note(tail: Option<&str>) -> String {
    match tail {
        Some(text) if !text.trim().is_empty() => text.to_string(),
        Some(_) => "no output yet".to_string(),
        None => "no output available for this task".to_string(),
    }
}

/// The card's click handlers, minted by the window, which is the only one
/// that knows the session. `rows` and `open` are index-aligned with the
/// task rows: one picks the task, the other opens its detail view.
pub(crate) struct CardClicks {
    pub(crate) header: ClickHandler,
    pub(crate) rows: Vec<ClickHandler>,
    pub(crate) open: Vec<ClickHandler>,
}

/// The open card: a header that closes it, one row per task, and under the
/// open row the last lines it wrote.
pub(crate) fn task_card(
    session: Uuid,
    rows: &[TaskRow],
    open: Option<Uuid>,
    focused: bool,
    tail: Option<&str>,
    clicks: CardClicks,
) -> Div {
    let CardClicks {
        header: on_header,
        rows: on_rows,
        open: on_open,
    } = clicks;
    let pal = palette::current();
    let selected_ours = rows
        .iter()
        .find(|row| open == Some(row.id))
        .map(|row| row.ours);
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .w_full()
        .min_w_0()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(pal.panel))
        .child(
            div()
                .id(SharedString::from(format!("task-card-head-{session}")))
                .flex()
                .flex_row()
                .justify_between()
                .gap_2()
                .text_xs()
                .cursor_pointer()
                .child(
                    div()
                        .text_color(rgb(pal.teal))
                        .child(SharedString::from(format!(
                            "\u{23f3} {}",
                            header_text(rows.len())
                        ))),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_faint))
                        .child(SharedString::from(hint_text(focused, selected_ours))),
                )
                .on_click(move |ev, window, app| on_header(ev, window, app)),
        );
    for ((row, pick), open_door) in rows.iter().zip(on_rows).zip(on_open) {
        let is_open = open == Some(row.id);
        let arrow = if is_open { "\u{25be}" } else { "\u{25b8}" };
        col = col.child(
            div()
                .id(SharedString::from(format!("task-row-{}", row.id)))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .w_full()
                .min_w_0()
                .px_1()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                // The row the keyboard stands on is lit, so Esc is never
                // aimed at a task the user cannot see is selected.
                .when(focused && is_open, |d| d.bg(rgb(pal.panel_hi)))
                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(pal.text_muted))
                        .child(SharedString::from(arrow)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .truncate()
                        .text_color(rgb(if is_open { pal.text } else { pal.text_normal }))
                        .child(SharedString::from(row.label.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(pal.text_faint))
                        .child(SharedString::from(row.tag)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(pal.teal))
                        .child(SharedString::from(elapsed_text(row.secs))),
                )
                // The pointer's way into the full view, the same door Enter
                // is for the keyboard. Its click stops here, so it never
                // also toggles the row it sits on.
                .child(
                    div()
                        .id(SharedString::from(format!("task-open-{}", row.id)))
                        .flex_none()
                        .px_1()
                        .rounded_md()
                        .text_color(rgb(pal.text_faint))
                        .cursor_pointer()
                        .hover(|d| d.text_color(rgb(palette::current().text)))
                        .child("open \u{2922}")
                        .on_click(move |ev, window, app| open_door(ev, window, app)),
                )
                .on_click(move |ev, window, app| pick(ev, window, app)),
        );
        if is_open {
            let mut block = div()
                .flex()
                .flex_col()
                .min_w_0()
                .overflow_hidden()
                .ml_4()
                .p_2()
                .rounded_md()
                .bg(rgb(pal.bg))
                .font_family(fonts::current_mono())
                .text_xs()
                .text_color(rgb(pal.text_muted));
            for line in tail_or_note(tail).lines() {
                // An empty child collapses to no height, which would swallow
                // the blank lines a build prints between its sections.
                let shown = if line.is_empty() { " " } else { line };
                // A flex row with a growing text child, never a width-full
                // block div holding the truncating text: gpui measures that
                // shape at a zero basis and lays the line out one character
                // wide (#2624, the same trap the plan strip fell into).
                block = block.child(
                    div().flex().flex_row().w_full().min_w_0().child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .truncate()
                            .child(SharedString::from(shown.to_string())),
                    ),
                );
            }
            col = col.child(block);
        }
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn down_walks_the_rows_and_stops_at_the_last() {
        assert_eq!(key_step("down", Some(0), 3), KeyStep::Select(1));
        assert_eq!(key_step("down", Some(1), 3), KeyStep::Select(2));
        assert_eq!(key_step("down", Some(2), 3), KeyStep::Select(2));
    }

    #[test]
    fn up_off_the_first_row_gives_the_keyboard_back() {
        assert_eq!(key_step("up", Some(2), 3), KeyStep::Select(1));
        assert_eq!(key_step("up", Some(0), 3), KeyStep::Leave);
    }

    #[test]
    fn escape_stops_the_row_under_the_cursor_and_stays() {
        assert_eq!(key_step("escape", Some(1), 3), KeyStep::Cancel(1));
        assert_eq!(key_step("escape", None, 3), KeyStep::Cancel(0));
    }

    #[test]
    fn enter_opens_the_row_under_the_cursor() {
        assert_eq!(key_step("enter", Some(1), 3), KeyStep::Open(1));
        assert_eq!(key_step("enter", None, 3), KeyStep::Open(0));
        assert_eq!(key_step("enter", Some(0), 0), KeyStep::Leave);
    }

    #[test]
    fn a_cursor_past_the_end_is_pulled_back_onto_a_row() {
        // The row it stood on finished between two keys.
        assert_eq!(key_step("escape", Some(7), 2), KeyStep::Cancel(1));
        assert_eq!(key_step("up", Some(7), 2), KeyStep::Select(0));
    }

    #[test]
    fn any_other_key_or_an_empty_list_leaves() {
        assert_eq!(key_step("a", Some(0), 3), KeyStep::Leave);
        assert_eq!(key_step("tab", Some(0), 3), KeyStep::Leave);
        assert_eq!(key_step("down", None, 0), KeyStep::Leave);
        assert_eq!(key_step("escape", Some(0), 0), KeyStep::Leave);
    }

    #[test]
    fn the_hint_names_the_keys_only_while_the_list_holds_them() {
        assert_eq!(hint_text(false, Some(true)), "click to collapse");
        assert!(hint_text(true, Some(true)).contains("esc cancels"));
        assert!(hint_text(true, None).contains("esc cancels"));
        assert!(hint_text(true, Some(false)).contains("not ours"));
    }

    #[test]
    fn elapsed_keeps_seconds_until_the_hour() {
        assert_eq!(elapsed_text(0), "0s");
        assert_eq!(elapsed_text(59), "59s");
        assert_eq!(elapsed_text(60), "1m 00s");
        assert_eq!(elapsed_text(185), "3m 05s");
        assert_eq!(elapsed_text(3600), "1h 00m");
        assert_eq!(elapsed_text(4330), "1h 12m");
    }

    #[test]
    fn the_header_counts_in_the_singular_and_the_plural() {
        assert_eq!(header_text(1), "1 task running");
        assert_eq!(header_text(3), "3 tasks running");
    }

    #[test]
    fn a_silent_task_says_why_it_is_silent() {
        assert_eq!(tail_or_note(Some("Compiling x")), "Compiling x");
        assert_eq!(tail_or_note(Some("  \n")), "no output yet");
        assert_eq!(tail_or_note(None), "no output available for this task");
    }
}
