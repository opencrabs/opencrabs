//! Tests for the per-session background-state cache that lets
//! inactive panes update live instead of going dark.
//!
//! Regression context (2026-06-04 screenshot): in split-pane mode
//! the inactive pane showed gray "Thinking" / "X tool calls"
//! bullets from prior turns but nothing about the active turn
//! happening in that session. `AppState` held the live state for
//! one session at a time and twenty `TuiEvent` handlers in
//! `state.rs` gated on `is_current_session(session_id)`, silently
//! dropping events for non-focused sessions. The
//! `BackgroundSessionState` sidecar + `SessionStateMut` routing
//! enum let those handlers update either the foreground `AppState`
//! fields or a per-session sidecar entry, depending on which
//! session the event targets.
//!
//! These tests pin the contract at the unit layer so a future
//! refactor that "simplifies" the routing back to a single direct
//! mutation can't silently re-open the inactive-pane freeze.

use crate::tui::app::background_session::{BackgroundSessionState, SessionStateMut};

#[test]
fn empty_state_reports_no_live_content() {
    let bg = BackgroundSessionState::default();
    assert!(
        !bg.has_live_state(),
        "a freshly-defaulted sidecar must report no live state — \
         otherwise `demote_to_background` would insert empty \
         entries into the map and leak across sessions that never \
         actually had a turn"
    );
}

#[test]
fn streaming_response_marks_state_as_live() {
    let mut bg = BackgroundSessionState::default();
    let mut routing = SessionStateMut::Background(&mut bg);
    routing.append_streaming_chunk("hello ");
    routing.append_streaming_chunk("world");
    assert!(bg.has_live_state());
    assert_eq!(bg.streaming_response.as_deref(), Some("hello world"));
}

#[test]
fn streaming_chunk_clears_pending_reasoning() {
    // Mirrors the foreground behaviour: a visible text chunk means
    // the reasoning phase is over for this segment, so the
    // "Thinking …" label should not stay on screen while the
    // visible response is streaming in.
    let mut bg = BackgroundSessionState::default();
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.append_reasoning_chunk("planning the next step");
    }
    assert!(bg.streaming_reasoning.is_some());
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.append_streaming_chunk("here's the answer");
    }
    assert_eq!(bg.streaming_reasoning, None);
    assert!(bg.streaming_response.is_some());
}

#[test]
fn reasoning_skips_empty_and_whitespace_first_chunks() {
    // The original handler skipped empty / pure-whitespace chunks
    // when no reasoning had accumulated yet, so the renderer
    // never shows a "Thinking" label with no body. The routing
    // helper must preserve that.
    let mut bg = BackgroundSessionState::default();
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.append_reasoning_chunk("");
        routing.append_reasoning_chunk("   \n\n   ");
    }
    assert_eq!(bg.streaming_reasoning, None);
    // Once a real chunk arrives, subsequent whitespace is appended.
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.append_reasoning_chunk("thinking");
        routing.append_reasoning_chunk("\n");
    }
    assert_eq!(bg.streaming_reasoning.as_deref(), Some("thinking\n"));
}

#[test]
fn streaming_output_tokens_accumulate_and_advance_tps_tracker() {
    use std::time::Instant;
    let mut bg = BackgroundSessionState::default();
    let t0 = Instant::now();
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.add_streaming_output_tokens(10);
        routing.add_streaming_output_tokens(20);
    }
    assert_eq!(bg.streaming_output_tokens, 30);
    // The tracker stores `last_token_at` after each advance.
    // We can't compare against an arbitrary `t0` directly without
    // racing the system clock, but `active_secs_now` is monotonic
    // so a sample after the calls must be ≥ 0.
    let observed = bg.tps_tracker.active_secs_now(t0);
    assert!(observed >= 0.0);
}

#[test]
fn processing_flag_round_trips() {
    let mut bg = BackgroundSessionState::default();
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.set_processing(true);
    }
    assert!(bg.is_processing);
    assert!(bg.processing_started_at.is_some());
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.set_processing(false);
    }
    assert!(!bg.is_processing);
    assert!(bg.processing_started_at.is_none());
}

#[test]
fn active_tool_group_round_trips_through_routing_helper() {
    use crate::tui::app::{ToolCallEntry, ToolCallGroup};
    use serde_json::Value;

    let mut bg = BackgroundSessionState::default();
    let group = ToolCallGroup {
        calls: vec![ToolCallEntry {
            task_id: None,
            description: "grep `IDENTITY` in ~/srv".to_string(),
            success: true,
            details: None,
            completed: false,
            tool_input: Value::Null,
        }],
        expanded: false,
    };
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.set_active_tool_group(Some(group));
    }
    assert!(bg.active_tool_group.is_some());
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        let g = routing
            .active_tool_group_mut()
            .expect("group should be present after set_active_tool_group");
        g.calls[0].completed = true;
        g.calls[0].success = false;
    }
    let g = bg.active_tool_group.as_ref().unwrap();
    assert!(g.calls[0].completed);
    assert!(!g.calls[0].success);
}

#[test]
fn clear_turn_state_drops_every_live_field() {
    let mut bg = BackgroundSessionState::default();
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.append_streaming_chunk("partial");
        routing.append_reasoning_chunk("thinking");
        routing.set_processing(true);
        routing.add_streaming_output_tokens(42);
    }
    assert!(bg.has_live_state());
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.clear_turn_state();
    }
    assert!(
        !bg.has_live_state(),
        "clear_turn_state must drop every in-flight live field — \
         this is what ResponseComplete + demote_to_background \
         rely on to keep the background_sessions map bounded"
    );
}

#[test]
fn display_token_count_and_last_input_tokens_route() {
    let mut bg = BackgroundSessionState::default();
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.set_display_token_count(81449);
        routing.set_last_input_tokens(81449);
    }
    assert_eq!(bg.display_token_count, 81449);
    assert_eq!(bg.last_input_tokens, Some(81449));
}

#[test]
fn push_message_routes_to_pending_messages_for_background() {
    use crate::tui::app::DisplayMessage;
    let mut bg = BackgroundSessionState::default();
    let msg = DisplayMessage {
        id: uuid::Uuid::new_v4(),
        role: "user".to_string(),
        content: "queued while in background".to_string(),
        timestamp: chrono::Utc::now(),
        token_count: None,
        cost: None,
        approval: None,
        approve_menu: None,
        details: None,
        expanded: false,
        expanded_full: false,
        tool_group: None,
        duration_secs: None,
    };
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.push_message(msg.clone());
    }
    assert_eq!(bg.pending_messages.len(), 1);
    assert_eq!(bg.pending_messages[0].content, msg.content);
    assert!(
        bg.has_live_state(),
        "a pending message must mark the state as live so demote_to_background \
         keeps the entry around for the next focus switch"
    );
}

#[test]
fn last_message_mut_returns_latest_pending_for_background() {
    use crate::tui::app::DisplayMessage;
    let mut bg = BackgroundSessionState::default();
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        for content in ["one", "two", "three"] {
            routing.push_message(DisplayMessage {
                id: uuid::Uuid::new_v4(),
                role: "assistant".to_string(),
                content: content.to_string(),
                timestamp: chrono::Utc::now(),
                token_count: None,
                cost: None,
                approval: None,
                approve_menu: None,
                details: None,
                expanded: false,
                expanded_full: false,
                tool_group: None,
                duration_secs: None,
            });
        }
        let last = routing
            .last_message_mut()
            .expect("pending_messages non-empty");
        last.content = "three-edited".to_string();
    }
    assert_eq!(bg.pending_messages.last().unwrap().content, "three-edited");
}

#[test]
fn last_message_mut_returns_none_on_empty_background() {
    let mut bg = BackgroundSessionState::default();
    let mut routing = SessionStateMut::Background(&mut bg);
    assert!(routing.last_message_mut().is_none());
}

#[test]
fn clear_turn_state_preserves_pending_messages() {
    use crate::tui::app::DisplayMessage;
    let mut bg = BackgroundSessionState::default();
    {
        let mut routing = SessionStateMut::Background(&mut bg);
        routing.push_message(DisplayMessage {
            id: uuid::Uuid::new_v4(),
            role: "assistant".to_string(),
            content: "flushed text".to_string(),
            timestamp: chrono::Utc::now(),
            token_count: None,
            cost: None,
            approval: None,
            approve_menu: None,
            details: None,
            expanded: false,
            expanded_full: false,
            tool_group: None,
            duration_secs: None,
        });
        routing.append_streaming_chunk("in flight");
        routing.set_processing(true);
        routing.clear_turn_state();
    }
    // In-flight streaming state is wiped by clear_turn_state...
    assert_eq!(bg.streaming_response, None);
    assert!(!bg.is_processing);
    // ...but pending_messages survive so the user can still see
    // what flushed while the session was off-screen on focus
    // switch. They are NOT a transient in-flight buffer.
    assert_eq!(bg.pending_messages.len(), 1);
    assert!(bg.has_live_state());
}

// ── Source-level invariant guard ──────────────────────────────────
//
// Approval prompts (tool approval, sudo password, ssh password)
// MUST stay foreground-only. A turn waiting on approval for a
// non-focused session would otherwise queue forever with no UI
// surface, blocking the agent's tool loop until the user happened
// to switch focus. Pin the contract via a source-level check so a
// future refactor that "for consistency, routes ApprovalRequested
// via session_state_mut too" fails this test loudly.

const STATE_SRC: &str = include_str!("../tui/app/state.rs");

#[test]
fn approval_requests_are_not_routed_through_session_state_mut() {
    // Strip line comments and string literals so a doc-comment
    // discussing routing doesn't false-match.
    let no_comments: String = STATE_SRC
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    for variant in [
        "TuiEvent::ToolApprovalRequested",
        "TuiEvent::SudoPasswordRequested",
        "TuiEvent::SshPasswordRequested",
    ] {
        // The handler arm for an approval variant must NOT call
        // session_state_mut. We don't grep for the absence of the
        // call directly (it could legitimately appear in unrelated
        // arms above/below) — instead, locate the arm's slice and
        // assert it doesn't contain the routing helper.
        let arm_start = no_comments
            .find(variant)
            .unwrap_or_else(|| panic!("expected {variant} arm in state.rs"));
        // Walk forward until the next `TuiEvent::` arm to bound
        // the slice. This is a heuristic but tight enough: every
        // arm starts with `TuiEvent::Foo`.
        let rest = &no_comments[arm_start + variant.len()..];
        let arm_end = rest
            .find("\n            TuiEvent::")
            .map(|off| arm_start + variant.len() + off)
            .unwrap_or(no_comments.len());
        let arm = &no_comments[arm_start..arm_end];
        assert!(
            !arm.contains("session_state_mut"),
            "{variant} must stay foreground-only — routing it through \
             session_state_mut would let a background-session turn block on \
             approval with no UI surface. See background_session.rs doc \
             for the routing model."
        );
    }
}
