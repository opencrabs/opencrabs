//! The hint row: one line between the input box and the status bar (#2279).
//!
//! The row is always there, even when it has nothing to say. A row that appears
//! only when it has a hint moves the whole transcript up and down as the
//! conversation crosses a threshold, and the one thing nobody wants moving
//! while they read is the text they are reading.
//!
//! What it prints comes from [`crate::ui_core::hints`], which the desktop
//! app draws too. This module only turns the answer into cells and reads the
//! terminal's own state into the shape that function expects.

use crate::ui_core::hints::{CONTEXT_TIGHT, HintState, hint};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
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
    }
}

/// Paint the row, or leave it blank when there is nothing to suggest.
pub(crate) fn render_hint(f: &mut Frame, app: &App, area: Rect) {
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
    let Some(hint) = hint(&state, |command| {
        crate::ui_core::commands::lookup(command).is_some()
    }) else {
        return;
    };
    // A command in the text is the actionable part, so it is the part that is
    // emphasised. Splitting on the command keeps the styling honest: the whole
    // line bright would read as an error, and none of it bright reads as
    // chrome nobody notices. The pieces are taken owned first so the split's
    // borrow of the text ends before the text is handed to a span.
    let command = hint.command;
    let (before, after) = match hint.text.split_once(command) {
        Some((before, after)) => (before.to_string(), after.to_string()),
        None => (hint.text.clone(), String::new()),
    };
    let mut spans = Vec::new();
    if !before.is_empty() {
        spans.push(Span::styled(before, muted()));
    }
    spans.push(Span::styled(
        command,
        Style::default()
            .fg(theme::role(Role::Accent))
            .add_modifier(Modifier::BOLD),
    ));
    if !after.is_empty() {
        spans.push(Span::styled(after, muted()));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().bg(theme::background().unwrap_or_default())),
        area,
    );
}

/// The chrome colour: present, never competing with the transcript.
fn muted() -> Style {
    Style::default().fg(theme::role(Role::TextMuted))
}
