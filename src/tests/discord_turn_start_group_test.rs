//! #1845: the Discord flow group opens at turn start, not on the first
//! tool call, so the clock covers the thinking window (Slack #1808
//! parity). The turn-start shell renders bare zero counts with a rolling
//! clock, carries no expand components, and is posted before the ticker
//! spawns and the turn dispatches.

use crate::channels::discord::tool_group::{GroupState, render_components, render_content};
use std::time::Instant;

fn turn_start_shell() -> GroupState {
    GroupState {
        page: 0,
        entries: Vec::new(),
        notes: Vec::new(),
        expanded: false,
        started_at: Instant::now(),
        settled: None,
    }
}

#[test]
fn turn_start_shell_renders_bare_zero_counts_with_live_clock() {
    let rendered = render_content(&turn_start_shell());
    assert!(
        rendered.starts_with("✅ **0 tool calls** · 🕒 "),
        "turn-start shell must render bare counts plus the live clock, got: {rendered}"
    );
}

#[test]
fn turn_start_shell_has_no_expand_components() {
    assert!(
        render_components(&turn_start_shell(), 42).is_empty(),
        "an empty shell has nothing to expand: no buttons"
    );
}

#[test]
fn shell_is_posted_before_the_ticker_spawns() {
    let handler = include_str!("../channels/discord/handler.rs");
    let shell_at = handler
        .find("Turn-start group shell (#1845")
        .expect("turn-start shell block present in handler.rs");
    let ticker_at = handler
        .find("Spawns after the turn-start")
        .expect("ticker spawn site comment present in handler.rs");
    assert!(
        shell_at < ticker_at,
        "the shell must post before spawn_flow_ticker is called, so the \
         ticker finds an existing group and the clock covers thinking"
    );
}

/// The #1808 lesson: the pre-tool gap shows the LIVE status line (rolling
/// clock), never a static "thinking" placeholder text posted to the
/// channel. Discord's native typing dots are fine; static text is not.
#[test]
fn no_static_thinking_placeholder_is_ever_posted() {
    let handler = include_str!("../channels/discord/handler.rs");
    for literal in ["_thinking..._", "Thinking...", "⏳ thinking"] {
        assert!(
            !handler.contains(literal),
            "static placeholder literal {literal:?} must not exist: the \
             turn-start shell IS the pre-tool feedback (#1808 lesson)"
        );
    }
}
