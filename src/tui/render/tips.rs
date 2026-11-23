//! The hint row: one line above the input box, under the conversation (#2279,
//! #3102).
//!
//! It used to sit between the input box and the status bar, which put it below
//! the thing it describes. It reads as part of the conversation now: directly
//! under the last message, directly above the field it is about, which is where
//! the eye already is when it wonders what to do next.
//!
//! The row is always there, even when it has nothing to say. A row that appears
//! only when it has a hint moves the whole transcript up and down as the
//! conversation crosses a threshold, and the one thing nobody wants moving
//! while they read is the text they are reading.
//!
//! What it prints has an intent (#3102): one line naming what the screen is
//! asking for at this moment, not a rotation. The keys that act on the field
//! itself are painted in the field, where the caret is.
//!
//! What it prints comes from [`crate::ui_core::hints`], which the desktop app
//! draws too. This module only turns the answer into cells and reads the
//! terminal's own state into the shape that function expects.

use crate::ui_core::hints::{CONTEXT_TIGHT, Hint, HintState, dismissible, task_line_hint};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::super::app::App;
use super::theme::{self, Role};

/// One row, always reserved.
pub(crate) const HINT_ROW: u16 = 1;

/// The most rows the hint may take.
///
/// The hints are one long sentence each, so two or three rows cover every
/// column a real terminal has. The cap is what stops a pathological width (a
/// 20 column terminal holding a 116 character hint) from eating the
/// transcript: past the cap the tail is cut, which is the one case a wrap
/// cannot save (#3161).
const MAX_HINT_ROWS: u16 = 5;

/// The gap between the sentence and the chord, so the two never touch.
const GAP: u16 = 3;

/// The chord that hides the line, or nothing when the line may not be hidden.
///
/// Only advice carries the chord: a warning cannot be hidden, so printing a
/// key that would do nothing is worse than printing nothing.
fn dismiss_chord(hint: &Hint) -> &'static str {
    if dismissible(hint.id) {
        "alt+h hides"
    } else {
        ""
    }
}

/// The width the sentence has, once the chord and its gap are taken out.
///
/// The chord lives in its own cell at the right edge, so the text stops
/// before it instead of running under it.
fn text_width_for(hint: &Hint, width: u16) -> u16 {
    let chord_width = UnicodeWidthStr::width(dismiss_chord(hint)) as u16;
    if chord_width == 0 {
        width.max(1)
    } else {
        width.saturating_sub(chord_width + GAP).max(1)
    }
}

/// The rows the hint takes at this width, wrapped and counted the same way it
/// is drawn.
///
/// The row used to be exactly one row tall and the sentence was cut at the
/// edge, so a narrow terminal lost the tail of every hint, and the tail is
/// where the second command and the joke live (#3161). The count comes from
/// the widget that does the wrapping, so the rows reserved and the rows
/// painted can never disagree.
pub(crate) fn rows_for(hint: &Hint, width: u16) -> u16 {
    let rows = Paragraph::new(Line::from(Span::raw(hint.text.clone())))
        .wrap(Wrap { trim: false })
        .line_count(text_width_for(hint, width));
    (rows.min(usize::from(MAX_HINT_ROWS)) as u16).max(1)
}

/// The live facts the row is decided from.
///
/// Read from state the app already keeps for its own footer, so the hint costs
/// no bookkeeping of its own. `turns` counts the reader's own rows: one per
/// exchange, which is what the shared rule is about. `clear_base` is left at
/// zero here and filled in only when the context rung is live: counting it
/// means serialising and tokenising the whole tool registry, which is not a
/// cost a redraw pays on every keystroke.
fn state_of(app: &App) -> HintState {
    HintState {
        processing: app.is_processing,
        context_used: u64::from(app.last_input_tokens.unwrap_or(0)),
        context_max: u64::from(app.context_max_tokens),
        clear_base: 0,
        update_version: app.update_available_version.clone(),
        turns: app
            .messages
            .iter()
            .filter(|message| message.role == "user")
            .count(),
        // The offer the last turn made, waiting above the field. The terminal
        // already walks it with the arrows (#596), so the row only has to say
        // that it is there.
        offers: app.followup_suggestions.len(),
        // The terminal has no rail holding the keyboard: its session list is a
        // screen of its own, and the composer is not drawn behind it.
        rail_focused: false,
        rail_split: false,
        // Filled from the dismissal store by the terminal itself; the engine
        // stays free of the database.
        dismissed: app.hint_dismissed.clone(),
        // Read from the live config, the same pointer clone the window asks
        // before it offers the record chord: a tip pointing at `/whisper` with
        // no engine behind it would bounce.
        voice: crate::config::Config::current().voice_config().stt_enabled,
    }
}

/// The hint the row is printing right now, if any (#3122).
///
/// Shared by the paint path and the hide chord, so the key hides exactly the
/// line on screen. Two functions deciding separately what is showing is how a
/// dismissal ends up hiding a hint nobody can see.
pub(crate) fn current(app: &App) -> Option<Hint> {
    let mut state = state_of(app);
    // What `/clear` leaves behind is the system prompt and the tool schemas.
    // The provider reports the whole prompt, so the base is the only part the
    // reader cannot get back by clearing, and the footer's own
    // `base_context_tokens()` is zero by design. Measured here rather than in
    // `state_of` because it tokenises every tool schema, and only the context
    // rung ever prints the number.
    if state.context_percent() >= CONTEXT_TIGHT {
        state.clear_base = u64::from(app.agent_service.base_context_tokens_raw());
    }
    // The terminal dispatches the whole shared table, so a name the table
    // knows is a name it answers. `support` is the window's narrower list, and
    // asking it here would hide a command this surface runs fine. A command the
    // terminal cannot run must never be printed: it bounces with "Unknown
    // command", which is the failure the issue was filed about.
    task_line_hint(&state, |command| {
        crate::ui_core::commands::lookup(command).is_some()
    })
}

/// Paint the card, or leave it blank when the work has nothing to say.
///
/// One line, chosen by what the screen is asking for right now (#3102): the
/// rail holding the keyboard, an offer waiting above the field, a queue on its
/// way out, or a rung of the ladder. It used to cycle through all of them on a
/// timer, and a line that changes on its own reads as noise.
///
/// The chord that hides the line is painted at the row's right edge, in its own
/// cell, instead of being appended to the sentence (#3122). Appending it works
/// until the tip is long, and the roast lines are long on purpose: the chord
/// then falls off the right edge and the reader is left with a key nobody told
/// them about. In its own cell it survives any width, and the tip clips under
/// it the way it always clipped at the edge of the row.
///
/// The sentence itself is never cut (#3161). It is painted with wrapping on,
/// over as many rows as it needs, and the rows it needs were reserved before
/// the layout was solved, so a narrow terminal shows the whole sentence
/// instead of its first line.
pub(crate) fn render_hint(f: &mut Frame, hint: Option<&Hint>, area: Rect) {
    let Some(hint) = hint else {
        return;
    };
    let chord = dismiss_chord(hint);
    let background = Style::default().bg(theme::background().unwrap_or_default());
    let chord_width = UnicodeWidthStr::width(chord) as u16;
    let text_width = text_width_for(hint, area.width);
    // The whole line is the accent, not the muted chrome the status bar uses.
    // The row exists to be noticed by someone who does not yet know the keys,
    // and the quiet ink that makes the footer recede is exactly the wrong
    // colour for that (#3102). The accent comes from the theme, so it follows
    // whichever palette the reader picked.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint.text.clone(), accent())))
            .wrap(Wrap { trim: false })
            .style(background),
        Rect {
            width: text_width,
            ..area
        },
    );
    if chord_width > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                chord,
                Style::default().fg(theme::role(Role::TextMuted)),
            )))
            .style(background),
            Rect {
                x: area.x + area.width.saturating_sub(chord_width),
                width: chord_width,
                // The chord belongs to the first row, the one the reader reads
                // first, and the sentence breaks under it.
                height: 1,
                ..area
            },
        );
    }
}

/// The theme's accent: present, and impossible to mistake for the transcript.
fn accent() -> Style {
    Style::default().fg(theme::role(Role::Accent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The longest line the shared ladder prints, taken from the fresh-start
    /// rung (`src/ui_core/hints.rs`).
    const LONG: &str = "/new when the job changes · /clear when the model starts making things up · one drops the baggage, keeps the history";

    fn hint_with(text: &str) -> Hint {
        Hint {
            id: "fresh-start",
            text: text.to_string(),
            command: "/new",
        }
    }

    /// Paint the hint into a terminal of the given size and give back the
    /// rows, each one joined by a space so a sentence that broke across a row
    /// still reads as one sentence.
    fn painted(hint: &Hint, width: u16, rows: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, rows)).unwrap();
        terminal
            .draw(|f| render_hint(f, Some(hint), Rect::new(0, 0, width, rows)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// A long hint in a narrow terminal used to be cut at the edge of the row:
    /// the tail was simply gone, and the tail is where the promise lives
    /// (#3161). It breaks onto a second row now, and the whole sentence is on
    /// screen.
    #[test]
    fn a_narrow_hint_keeps_its_tail() {
        let hint = hint_with(LONG);
        let width = 60;
        let rows = rows_for(&hint, width);
        assert!(
            rows >= 2,
            "a {} character hint does not fit in one {} column row",
            LONG.chars().count(),
            width
        );
        let rows_painted = painted(&hint, width, rows);
        let joined = rows_painted.join(" ");
        assert!(
            joined.contains("keeps the history"),
            "the tail of the hint is missing: {joined:?}"
        );
    }

    /// The chord that hides the line stays in its own cell at the right edge
    /// of the FIRST row, and the sentence breaks under it (#3122, #3161).
    #[test]
    fn the_chord_stays_on_the_first_row() {
        let hint = hint_with(LONG);
        let width = 60;
        let rows = rows_for(&hint, width);
        let painted = painted(&hint, width, rows);
        assert!(
            painted[0].contains("alt+h hides"),
            "the chord is not on the first row: {painted:?}"
        );
        assert!(
            painted[1..].iter().all(|row| !row.contains("alt+h")),
            "the chord was painted twice: {painted:?}"
        );
    }

    /// A hint that fits the row it has takes exactly one row, so the wide
    /// terminal keeps the layout it always had.
    #[test]
    fn a_wide_hint_takes_one_row() {
        let hint = hint_with(LONG);
        assert_eq!(rows_for(&hint, 200), 1);
    }

    /// A hint that cannot fit in any sane number of rows is capped, so it can
    /// never eat the transcript.
    #[test]
    fn a_pathological_width_is_capped() {
        let hint = hint_with(LONG);
        assert_eq!(rows_for(&hint, 8), MAX_HINT_ROWS);
    }
}
