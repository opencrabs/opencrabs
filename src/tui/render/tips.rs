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

use crate::ui_core::hints::{CONTEXT_TIGHT, HintState, task_line};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use super::super::app::App;
use super::theme::{self, Role};

/// One row, always reserved.
pub(crate) const HINT_ROW: u16 = 1;

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
        // The terminal queues a message typed while a turn runs, the same as
        // the window does, so the row names the same keys (#3102).
        queued: queued_count(app),
        // The terminal has no rail holding the keyboard: its session list is a
        // screen of its own, and the composer is not drawn behind it.
        rail_focused: false,
        rail_split: false,
        // Filled from the dismissal store by the terminal itself, once it has
        // one; the engine stays free of the database.
        dismissed: Vec::new(),
        // No engine is known to this surface yet, so the tip that names
        // `/whisper` stays quiet rather than pointing at a command with
        // nothing behind it.
        voice: false,
    }
}

/// How many messages wait on the session on screen.
///
/// The map is shared with every channel, so the guard is taken and dropped
/// before anything is drawn: holding it across render work would let a
/// background send block the frame (#2244).
fn queued_count(app: &App) -> usize {
    let Some(session) = &app.current_session else {
        return 0;
    };
    app.queued_messages
        .lock()
        .ok()
        .and_then(|map| map.get(&session.id).map(Vec::len))
        .unwrap_or(0)
}

/// Paint the card, or leave it blank when the work has nothing to say.
///
/// One line, chosen by what the screen is asking for right now (#3102): the
/// rail holding the keyboard, an offer waiting above the field, a queue on its
/// way out, or a rung of the ladder. It used to cycle through all of them on a
/// timer, and a line that changes on its own reads as noise.
pub(crate) fn render_hint(f: &mut Frame, app: &mut App, area: Rect) {
    // The terminal dispatches the whole shared table, so a name the table
    // knows is a name it answers. `support` is the window's narrower list, and
    // asking it here would hide a command this surface runs fine. A command the
    // terminal cannot run must never be printed: it bounces with "Unknown
    // command", which is the failure the issue was filed about.
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
    let Some(line) = task_line(&state, |command| {
        crate::ui_core::commands::lookup(command).is_some()
    }) else {
        return;
    };
    // The whole line is the accent, not the muted chrome the status bar uses.
    // The row exists to be noticed by someone who does not yet know the keys,
    // and the quiet ink that makes the footer recede is exactly the wrong
    // colour for that (#3102). The accent comes from the theme, so it follows
    // whichever palette the reader picked.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(line, accent())))
            .style(Style::default().bg(theme::background().unwrap_or_default())),
        area,
    );
}

/// The theme's accent: present, and impossible to mistake for the transcript.
fn accent() -> Style {
    Style::default().fg(theme::role(Role::Accent))
}
