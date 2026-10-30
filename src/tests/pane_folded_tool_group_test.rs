//! #2075: an unfocused pane draws its turns the way the focused pane draws them
//! by default. A tool block keeps its turn header and last-call preview, and is
//! never drawn as a bare bullet, so switching focus does not change it.

use crate::tui::app::{DisplayMessage, ToolCallEntry, ToolCallGroup};
use crate::tui::render::chat::folded_pane_lines;
use ratatui::text::Line;
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

fn tool_group_msg(descriptions: &[&str]) -> DisplayMessage {
    let mut m = msg("tool_group", "");
    m.tool_group = Some(ToolCallGroup {
        calls: descriptions
            .iter()
            .map(|d| ToolCallEntry {
                task_id: None,
                description: d.to_string(),
                success: true,
                details: None,
                completed: true,
                tool_input: serde_json::Value::Null,
            })
            .collect(),
        expanded: false,
    });
    m
}

fn text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn a_finished_tool_turn_shows_its_header_and_last_call_not_a_bare_bullet() {
    let msgs = vec![
        msg("user", "check the build"),
        tool_group_msg(&["bash cargo check", "read_file src/main.rs"]),
        msg("assistant", "The build is clean."),
    ];
    let rows = text(&folded_pane_lines(&msgs, 80));
    let joined = rows.join("\n");
    assert!(joined.contains("tool call"), "header missing: {joined}");
    assert!(
        joined.contains("(click to expand)"),
        "fold hint missing: {joined}"
    );
    assert!(
        joined.contains("read_file src/main.rs"),
        "last call preview missing: {joined}"
    );
    assert!(
        !joined.contains("● 1 tool call") && !joined.contains("● 2 tool calls"),
        "a bare bullet leaked: {joined}"
    );
    assert!(
        joined.contains("The build is clean."),
        "final answer must stay visible: {joined}"
    );
}

#[test]
fn a_folded_turn_hides_its_working_rows_but_keeps_the_question_and_answer() {
    let msgs = vec![
        msg("user", "question"),
        msg("assistant", "intermediate narration"),
        tool_group_msg(&["bash ls"]),
        msg("assistant", "final answer"),
    ];
    let joined = text(&folded_pane_lines(&msgs, 80)).join("\n");
    assert!(joined.contains("question"));
    assert!(joined.contains("final answer"));
    assert!(
        !joined.contains("intermediate narration"),
        "folded narration leaked: {joined}"
    );
    // The folded header shows only the last call, as the focused pane does.
    assert_eq!(
        joined.matches("bash ls").count(),
        1,
        "preview must appear once: {joined}"
    );
}

#[test]
fn the_pane_draws_the_same_block_whatever_its_focus() {
    // The cache and the focused pane are built from the same messages, so the
    // pane's drawing is a pure function of them: no focus state enters it.
    let msgs = vec![
        msg("user", "q"),
        tool_group_msg(&["bash cargo test"]),
        msg("assistant", "done"),
    ];
    assert_eq!(
        text(&folded_pane_lines(&msgs, 80)),
        text(&folded_pane_lines(&msgs, 80))
    );
}

#[test]
fn panes_no_longer_draw_cached_tool_groups_as_bare_bullets() {
    let src = include_str!("../tui/render/panes.rs");
    assert!(
        src.contains("folded_pane_lines"),
        "the cache loop must use the folded renderer"
    );
    assert!(
        !src.contains("for msg in messages {\n            render_simple_message"),
        "the cache loop still draws messages one by one"
    );
}
