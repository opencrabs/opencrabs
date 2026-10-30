//! Per-session live-state cache for non-focused panes.
//!
//! ## Problem
//!
//! Before this module existed, `AppState` held the live state for
//! **one session at a time** — `streaming_response`,
//! `streaming_reasoning`, `active_tool_group`, `is_processing`,
//! `processing_started_at`, `streaming_output_tokens`,
//! `last_input_tokens`, `display_token_count`, `tps_tracker`. Twenty
//! `TuiEvent` handlers in `state.rs` were gated on
//! `is_current_session(session_id)` and silently dropped events for
//! non-focused sessions. The split-pane renderer then showed an
//! inactive pane frozen at whatever was last loaded from the DB —
//! no live thinking, no in-progress tool calls, no streaming chunks
//! until the user tabbed back and `preload_pane_session` re-read
//! from disk. The inactive pane in the 2026-06-04 screenshot showed
//! gray "Thinking" and "X tool calls" markers from prior turns but
//! nothing about the active turn taking place in that session.
//!
//! ## Shape of the fix
//!
//! `BackgroundSessionState` is a sidecar struct that carries the
//! exact same live fields the focused `AppState` holds, but only
//! for sessions that aren't currently shown in the focused pane.
//! Event handlers route every incoming `TuiEvent` to either
//! `AppState` (focused) or
//! `AppState.background_sessions[session_id]` (background) via the
//! `SessionStateMut` enum returned by `App::session_state_mut`.
//!
//! On focus switch, `App::demote_to_background(old_sid)` saves the
//! current `AppState`'s live fields into a fresh
//! `BackgroundSessionState`, and `App::promote_to_foreground(
//! new_sid)` pops the new pane's entry into `AppState`. A
//! `ResponseComplete` arriving for a background session finalizes
//! its `streaming_response` into a `DisplayMessage` in
//! `pane_message_cache` and drops the background entry — so when
//! the user eventually focuses that pane, the finalized message is
//! already there waiting for them.
//!
//! ## What lives here vs. in DB
//!
//! - DB-persisted (`MessageRepository`): every committed message,
//!   token/cost stats, plan documents, session metadata. These get
//!   re-read on focus switch via `preload_pane_session`.
//! - `pane_message_cache` (already in `AppState`): a snapshot of
//!   `DisplayMessage`s for the inactive pane, refreshed whenever
//!   focus leaves a pane. Read-only.
//! - **This module**: the *in-flight turn* state that hasn't yet
//!   landed in either the DB or `pane_message_cache` — streaming
//!   chunks, the current tool-call group, the thinking buffer, live
//!   token counters. Without it the inactive pane misses every
//!   chunk between turns.

use crate::tui::app::state::{DisplayMessage, StreamingTpsTracker, ToolCallEntry, ToolCallGroup};
use std::time::Instant;

/// Sidecar live-state cache for a non-focused session.
///
/// Mirrors the live-turn fields on `App` so an inactive pane can be
/// rendered with up-to-date thinking/tool/streaming state and
/// promoted back to foreground without losing accumulated work.
/// Fields are intentionally kept narrow to the *in-flight* turn —
/// finalized messages live in `pane_message_cache` or the DB.
#[derive(Debug, Default, Clone)]
pub struct BackgroundSessionState {
    /// Streaming response buffer for the in-progress turn. `None`
    /// between turns. Drained into a finalized `DisplayMessage` on
    /// `ResponseComplete` before this entry is dropped.
    pub streaming_response: Option<String>,
    /// Streaming reasoning ("Thinking …") buffer for the in-
    /// progress turn. Cleared at the start of every visible response
    /// chunk (same shape as `App.streaming_reasoning`).
    pub streaming_reasoning: Option<String>,
    /// Tool calls executing this turn. The renderer collapses these
    /// into the gray "X tool calls" bullet on the inactive pane.
    pub active_tool_group: Option<ToolCallGroup>,
    /// Whether the agent is mid-turn for this session. Drives the
    /// "[processing...]" status label on the inactive pane border.
    pub is_processing: bool,
    /// Instant the current turn started — used to render elapsed
    /// time and tok/s on the inactive pane footer when the user
    /// switches focus.
    pub processing_started_at: Option<Instant>,
    /// Last input-token count seen for this session (drives the
    /// `ctx: NK/200K` indicator on the inactive pane footer).
    pub last_input_tokens: Option<u32>,
    /// Running output-token count for the in-progress turn.
    pub streaming_output_tokens: u32,
    /// Current ctx-token display for the inactive pane.
    pub display_token_count: usize,
    /// Live tok/s tracker for this session — accumulates active
    /// streaming windows the same way the foreground tracker does
    /// so the rate stays accurate across focus switches. See
    /// `StreamingTpsTracker` doc for the active-window invariant.
    pub tps_tracker: StreamingTpsTracker,
    /// Per-session message delta accumulated while the session is
    /// in background. Each `IntermediateText` / `QueuedUserMessage`
    /// flush for a non-focused session appends here instead of to
    /// `App.messages` (which is foreground-only). On
    /// `promote_to_foreground` the delta is merged into the
    /// reloaded `App.messages` so the user sees everything that
    /// flushed while they were on the other pane, in chronological
    /// order. The inactive-pane renderer also reads from this Vec
    /// for the live preview rows so the user sees text/tool-group
    /// flushes appear in real time rather than only on focus
    /// switch.
    pub pending_messages: Vec<DisplayMessage>,
}

impl BackgroundSessionState {
    /// True when there is *any* live state worth rendering. Empty
    /// background entries are dropped at cleanup time to keep the
    /// map bounded.
    pub fn has_live_state(&self) -> bool {
        self.streaming_response.is_some()
            || self.streaming_reasoning.is_some()
            || self.active_tool_group.is_some()
            || self.is_processing
            || self.streaming_output_tokens > 0
            || !self.pending_messages.is_empty()
    }
}

/// Routing handle for an event handler.
///
/// Returned by `App::session_state_mut(session_id)`. The variant
/// carried tells the handler where to write — foreground (the
/// focused `App` fields) or background (a sidecar entry). Both
/// variants expose the same set of mutator methods below so the
/// 20 event-handler call sites stay one-liners regardless of
/// which session they're targeting.
///
/// Lifetime is tied to the borrow on `App`, so no clone is
/// performed and the handler can mutate either path in place.
pub enum SessionStateMut<'a> {
    Foreground(&'a mut crate::tui::app::App),
    Background(&'a mut BackgroundSessionState),
}

impl SessionStateMut<'_> {
    /// Append a streaming-response chunk. Foreground: also nudges
    /// the auto-scroll flag and invalidates the render cache.
    /// Background: just appends; render happens on focus switch.
    pub fn append_streaming_chunk(&mut self, text: &str) {
        match self {
            Self::Foreground(app) => app.append_streaming_chunk(text.to_string()),
            Self::Background(bg) => {
                bg.streaming_reasoning = None;
                bg.streaming_response
                    .get_or_insert_with(String::new)
                    .push_str(text);
            }
        }
    }

    /// Append a reasoning ("Thinking …") chunk. Both variants skip
    /// empty / whitespace-only chunks so the renderer never shows
    /// a "Thinking" label with no content.
    pub fn append_reasoning_chunk(&mut self, text: &str) {
        let is_whitespace = text.trim().is_empty();
        if text.is_empty() {
            return;
        }
        match self {
            Self::Foreground(app) => {
                if let Some(ref mut existing) = app.streaming_reasoning {
                    existing.push_str(text);
                } else if !is_whitespace {
                    app.streaming_reasoning = Some(text.to_string());
                }
                if app.auto_scroll {
                    app.scroll_offset = 0;
                }
            }
            Self::Background(bg) => {
                if let Some(ref mut existing) = bg.streaming_reasoning {
                    existing.push_str(text);
                } else if !is_whitespace {
                    bg.streaming_reasoning = Some(text.to_string());
                }
            }
        }
    }

    /// Mark the session as actively processing a turn.
    pub fn set_processing(&mut self, processing: bool) {
        match self {
            Self::Foreground(app) => {
                app.is_processing = processing;
                if processing && app.processing_started_at.is_none() {
                    app.processing_started_at = Some(Instant::now());
                } else if !processing {
                    app.processing_started_at = None;
                }
            }
            Self::Background(bg) => {
                bg.is_processing = processing;
                if processing && bg.processing_started_at.is_none() {
                    bg.processing_started_at = Some(Instant::now());
                } else if !processing {
                    bg.processing_started_at = None;
                }
            }
        }
    }

    /// Increment the streaming-output token counter and feed the
    /// tps tracker.
    pub fn add_streaming_output_tokens(&mut self, tokens: u32) {
        match self {
            Self::Foreground(app) => {
                app.streaming_output_tokens += tokens;
                app.advance_streaming_window();
            }
            Self::Background(bg) => {
                bg.streaming_output_tokens += tokens;
                bg.tps_tracker.advance(Instant::now());
            }
        }
    }

    /// Replace the active tool-call group. Used when a new batch of
    /// tool calls starts inside the same turn.
    pub fn set_active_tool_group(&mut self, group: Option<ToolCallGroup>) {
        match self {
            Self::Foreground(app) => app.active_tool_group = group,
            Self::Background(bg) => bg.active_tool_group = group,
        }
    }

    /// Borrow the active tool group mutably so the caller can flip
    /// individual entries from "executing" → "completed" without
    /// rebuilding the whole group.
    pub fn active_tool_group_mut(&mut self) -> Option<&mut ToolCallGroup> {
        match self {
            Self::Foreground(app) => app.active_tool_group.as_mut(),
            Self::Background(bg) => bg.active_tool_group.as_mut(),
        }
    }

    /// Find the detached-task row carrying `task_id` so a finish updates it in
    /// place (#2146).
    ///
    /// The live turn's active group is checked first, then the settled
    /// `tool_group` messages, because a finish can land after the turn ended
    /// and its group was already flushed into the transcript. Identity is the
    /// task id, never the label: two detached tasks can share a label, and a
    /// label match would settle the wrong row.
    pub fn find_detached_row(&mut self, task_id: &str) -> Option<&mut ToolCallEntry> {
        let (group, messages) = match self {
            Self::Foreground(app) => (&mut app.active_tool_group, &mut app.messages),
            Self::Background(bg) => (&mut bg.active_tool_group, &mut bg.pending_messages),
        };
        if let Some(group) = group.as_mut()
            && let Some(entry) = group
                .calls
                .iter_mut()
                .rev()
                .find(|c| c.task_id.as_deref() == Some(task_id))
        {
            return Some(entry);
        }
        messages
            .iter_mut()
            .rev()
            .filter_map(|m| m.tool_group.as_mut())
            .flat_map(|g| g.calls.iter_mut().rev())
            .find(|c| c.task_id.as_deref() == Some(task_id))
    }

    /// Update the display-token counter (drives the footer
    /// `ctx: NK/200K` indicator).
    pub fn set_display_token_count(&mut self, count: usize) {
        match self {
            Self::Foreground(app) => app.display_token_count = count,
            Self::Background(bg) => bg.display_token_count = count,
        }
    }

    /// Record the last-input-token count for the footer.
    pub fn set_last_input_tokens(&mut self, count: u32) {
        match self {
            Self::Foreground(app) => app.last_input_tokens = Some(count),
            Self::Background(bg) => bg.last_input_tokens = Some(count),
        }
    }

    /// Take the active tool-call group out of the routed state,
    /// leaving `None` behind. Used by handlers that flush the
    /// group into a finalised `DisplayMessage` (e.g.
    /// `IntermediateText`) so the next tool batch starts with a
    /// clean group.
    pub fn take_active_tool_group(&mut self) -> Option<ToolCallGroup> {
        match self {
            Self::Foreground(app) => app.active_tool_group.take(),
            Self::Background(bg) => bg.active_tool_group.take(),
        }
    }

    /// Append a finalised `DisplayMessage` to the routed message
    /// list. Foreground pushes to `App.messages`; background
    /// pushes to the sidecar's `pending_messages` Vec, which
    /// `promote_to_foreground` later drains into `App.messages`
    /// when this session becomes focused. Either way the inactive-
    /// pane renderer can read both paths for its preview rows.
    pub fn push_message(&mut self, msg: DisplayMessage) {
        match self {
            Self::Foreground(app) => app.messages.push(msg),
            Self::Background(bg) => bg.pending_messages.push(msg),
        }
    }

    /// Borrow the last message of the routed list mutably so the
    /// `IntermediateText` reasoning-merge path can append to an
    /// existing thinking-only assistant entry rather than pushing
    /// a duplicate. Returns `None` when the list is empty.
    ///
    /// For background sessions, only `pending_messages` is
    /// considered — the cached snapshot in `pane_message_cache` is
    /// read-only at this point. If the last message the user would
    /// see is actually in the snapshot, the merge is skipped and a
    /// fresh entry is pushed; the next focus-switch re-reads from
    /// DB which holds the authoritative shape.
    pub fn last_message_mut(&mut self) -> Option<&mut DisplayMessage> {
        match self {
            Self::Foreground(app) => app.messages.last_mut(),
            Self::Background(bg) => bg.pending_messages.last_mut(),
        }
    }

    /// Reset the foreground processing-started clock used by the
    /// `[processing...]` footer. No-op for background — the
    /// `processing_started_at` instant on the sidecar is set by
    /// `set_processing`.
    pub fn reset_processing_clock(&mut self) {
        if let Self::Foreground(app) = self {
            app.processing_started_at = Some(Instant::now());
        }
    }

    /// Clear all in-flight turn state — used at `ResponseComplete`
    /// when the turn has been finalized into the message store.
    pub fn clear_turn_state(&mut self) {
        match self {
            Self::Foreground(app) => {
                app.streaming_response = None;
                app.streaming_reasoning = None;
                app.active_tool_group = None;
                app.is_processing = false;
                app.processing_started_at = None;
                app.streaming_output_tokens = 0;
            }
            Self::Background(bg) => {
                bg.streaming_response = None;
                bg.streaming_reasoning = None;
                bg.active_tool_group = None;
                bg.is_processing = false;
                bg.processing_started_at = None;
                bg.streaming_output_tokens = 0;
                // Keep pending_messages — they're a delta of FLUSHED
                // messages from the just-ended turn, not in-flight
                // state. The caller (ResponseComplete cleanup or
                // promote_to_foreground) decides whether to merge
                // them into App.messages or drop them along with the
                // sidecar entry. Wiping them here would erase the
                // user-visible turn before they had a chance to see
                // it after focus switch.
            }
        }
    }
}
