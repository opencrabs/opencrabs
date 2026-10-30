//! #2086: a turn halted by `suggest_options` ends there. Its text is the answer
//! and stays visible, and no follow-up model request is made.

use crate::tui::app::{DisplayMessage, ToolCallEntry, ToolCallGroup};
use crate::tui::render::chat::{final_answer_idx, turn_ranges};
use uuid::Uuid;

fn msg(role: &str, content: &str) -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: role.to_string(),
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
    }
}

fn option_surface_group() -> DisplayMessage {
    let mut m = msg("tool_group", "");
    m.tool_group = Some(ToolCallGroup {
        calls: vec![ToolCallEntry {
            task_id: None,
            description: "suggest_options".to_string(),
            success: true,
            details: None,
            completed: true,
            tool_input: serde_json::Value::Null,
        }],
        expanded: false,
    });
    m
}

#[test]
fn the_text_before_the_option_surface_is_the_answer() {
    let msgs = vec![
        msg("user", "hello"),
        msg(
            "assistant",
            "Brain's online. Two things: 1. Who am I working with? 2. What do you do?",
        ),
        option_surface_group(),
    ];
    let turns = turn_ranges(&msgs);
    assert_eq!(final_answer_idx(&msgs, turns[0]), Some(1));
}

#[test]
fn a_follow_up_text_after_the_option_surface_would_hide_the_answer() {
    // Documents why the follow-up request must not run: its text would
    // become the turn's answer and the greeting would fold.
    let msgs = vec![
        msg("user", "hello"),
        msg(
            "assistant",
            "Brain's online. Two things: 1. Who am I working with?",
        ),
        option_surface_group(),
        msg("assistant", "Name and location/timezone, hit me."),
    ];
    let turns = turn_ranges(&msgs);
    assert_eq!(final_answer_idx(&msgs, turns[0]), Some(3));
}

#[test]
fn the_halt_stops_the_loop_before_another_request() {
    let src = include_str!("../brain/agent/service/tool_loop.rs");
    assert!(
        src.contains("if option_surface_halt_seen && option_halt_iteration_has_text {\n                tracing::info!(\"🛑 Turn ended after option-surface halt"),
        "the outer loop must end on an option-surface halt"
    );
}

#[test]
fn a_halt_without_text_still_asks_the_model_for_a_reply() {
    let src = include_str!("../brain/agent/service/tool_loop.rs");
    assert!(
        src.contains("option_surface_halt_seen && option_halt_iteration_has_text"),
        "the halt must end the turn only when the halting iteration has text"
    );
    assert!(
        src.contains("option_halt_iteration_has_text = !iteration_text.trim().is_empty()"),
        "the halting iteration's text must be recorded at the halt"
    );
}
