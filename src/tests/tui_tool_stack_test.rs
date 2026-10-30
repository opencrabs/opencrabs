//! Tests for tool call group stacking in TUI rendering.
//!
//! When 3+ consecutive tool_group messages appear (possibly with thinking-only
//! assistant messages between them), they should be rendered as a single
//! collapsed summary instead of N separate bullet blocks.

use crate::tui::app::{DisplayMessage, ToolCallEntry, ToolCallGroup};
use uuid::Uuid;

/// Helper: create a tool_group DisplayMessage with N tool calls
fn make_tool_group_msg(num_calls: usize) -> DisplayMessage {
    let calls: Vec<ToolCallEntry> = (0..num_calls)
        .map(|i| ToolCallEntry {
            task_id: None,
            description: format!("Tool call {}", i),
            success: true,
            details: None,
            completed: true,
            tool_input: serde_json::Value::Null,
        })
        .collect();

    DisplayMessage {
        id: Uuid::new_v4(),
        role: "tool_group".to_string(),
        content: format!("{} tool calls", num_calls),
        timestamp: chrono::Utc::now(),
        token_count: None,
        cost: None,
        approval: None,
        approve_menu: None,
        details: None,
        expanded: false,
        expanded_full: false,
        tool_group: Some(ToolCallGroup {
            calls,
            expanded: false,
        }),
        duration_secs: None,
    }
}

/// Helper: create a thinking-only assistant message (no visible text)
fn make_thinking_only_msg() -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: "assistant".to_string(),
        content: String::new(), // Empty content
        timestamp: chrono::Utc::now(),
        token_count: None,
        cost: None,
        approval: None,
        approve_menu: None,
        details: Some("Thinking about the problem...".to_string()),
        expanded: false,
        expanded_full: false,
        tool_group: None,
        duration_secs: None,
    }
}

/// Helper: create a regular assistant message with visible text
fn make_assistant_msg(text: &str) -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: "assistant".to_string(),
        content: text.to_string(),
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

/// Count consecutive tool_group messages starting from idx, skipping
/// thinking-only assistants.
///
/// This used to be a hand-maintained COPY of the loop inside `render_chat`,
/// so it could drift from the real renderer without any test failing. It now
/// delegates to the extracted `scan_tool_stack` the renderer itself calls
/// (#743 step 1), so these tests exercise production logic.
fn count_consecutive_groups(messages: &[DisplayMessage], start_idx: usize) -> (usize, usize) {
    let stack = crate::tui::render::chat::scan_tool_stack(messages, start_idx);
    (stack.count, stack.total_calls)
}

#[test]
fn three_consecutive_groups_are_stacked() {
    let messages = vec![
        make_tool_group_msg(2),
        make_tool_group_msg(3),
        make_tool_group_msg(1),
    ];

    let (count, total_calls) = count_consecutive_groups(&messages, 0);
    assert_eq!(count, 3, "Should detect 3 consecutive tool groups");
    assert_eq!(total_calls, 6, "Should count 6 total tool calls");
}

#[test]
fn two_consecutive_groups_not_stacked() {
    let messages = vec![make_tool_group_msg(2), make_tool_group_msg(3)];

    let (count, _total_calls) = count_consecutive_groups(&messages, 0);
    assert_eq!(count, 2, "Should detect 2 consecutive tool groups");
    // Stacking threshold is 3, so 2 should NOT be stacked (handled in render logic)
}

#[test]
fn groups_with_thinking_between_are_stacked() {
    let messages = vec![
        make_tool_group_msg(2),
        make_thinking_only_msg(),
        make_tool_group_msg(3),
        make_thinking_only_msg(),
        make_tool_group_msg(1),
    ];

    let (count, total_calls) = count_consecutive_groups(&messages, 0);
    assert_eq!(
        count, 3,
        "Should detect 3 tool groups across thinking-only messages"
    );
    assert_eq!(total_calls, 6, "Should count 6 total tool calls");
}

#[test]
fn groups_broken_by_visible_text_not_stacked() {
    let messages = vec![
        make_tool_group_msg(2),
        make_tool_group_msg(3),
        make_assistant_msg("Here's the result"), // Visible text breaks the chain
        make_tool_group_msg(1),
    ];

    let (count, total_calls) = count_consecutive_groups(&messages, 0);
    assert_eq!(
        count, 2,
        "Should only count 2 groups before the visible text"
    );
    assert_eq!(
        total_calls, 5,
        "Should count 5 tool calls in first 2 groups"
    );

    // Check the group after the break
    let (count2, total_calls2) = count_consecutive_groups(&messages, 3);
    assert_eq!(count2, 1, "Should detect 1 group after the break");
    assert_eq!(total_calls2, 1, "Should count 1 tool call");
}

#[test]
fn single_group_not_stacked() {
    let messages = vec![make_tool_group_msg(5)];

    let (count, total_calls) = count_consecutive_groups(&messages, 0);
    assert_eq!(count, 1, "Should detect 1 tool group");
    assert_eq!(total_calls, 5, "Should count 5 tool calls");
}

#[test]
fn empty_messages_not_counted() {
    let messages: Vec<DisplayMessage> = vec![];

    let (count, total_calls) = count_consecutive_groups(&messages, 0);
    assert_eq!(count, 0, "Should detect 0 tool groups in empty list");
    assert_eq!(total_calls, 0, "Should count 0 tool calls");
}

// ── run end / skip_count (#743 step 1) ──────────────────────────
//
// The renderer derives `skip_count = end - msg_idx - 1` to avoid re-rendering
// messages already consumed by the stack. The old hand-copied helper returned
// only (count, total_calls), so the END index — the part that decides whether
// the renderer skips too few (duplicate rows) or too many (dropped rows) — had
// no coverage at all.

use crate::tui::render::chat::scan_tool_stack;

#[test]
fn run_end_covers_trailing_thinking_between_groups() {
    // group, thinking, group  →  the whole run is consumed.
    let messages = vec![
        make_tool_group_msg(1),
        make_thinking_only_msg(),
        make_tool_group_msg(1),
    ];
    let stack = scan_tool_stack(&messages, 0);
    assert_eq!(stack.count, 2, "two groups");
    assert_eq!(
        stack.end, 3,
        "run must consume the interleaved thinking too"
    );
}

#[test]
fn run_end_stops_before_visible_text() {
    // group, assistant-with-text  →  the text message must NOT be consumed,
    // or the renderer would skip it and the reply would vanish from the chat.
    let messages = vec![
        make_tool_group_msg(1),
        make_assistant_msg("here is the answer"),
    ];
    let stack = scan_tool_stack(&messages, 0);
    assert_eq!(stack.count, 1);
    assert_eq!(stack.end, 1, "must stop before the visible assistant text");
}

#[test]
fn run_end_is_start_plus_one_for_a_lone_group() {
    let messages = vec![make_tool_group_msg(3)];
    let stack = scan_tool_stack(&messages, 0);
    assert_eq!((stack.count, stack.total_calls, stack.end), (1, 3, 1));
}

#[test]
fn scan_from_a_mid_list_offset_only_walks_forward() {
    // Starting mid-list must not re-count earlier groups.
    let messages = vec![
        make_tool_group_msg(2),
        make_assistant_msg("text breaks the run"),
        make_tool_group_msg(1),
        make_tool_group_msg(1),
    ];
    let stack = scan_tool_stack(&messages, 2);
    assert_eq!(stack.count, 2, "only the groups at/after the offset");
    assert_eq!(stack.total_calls, 2);
    assert_eq!(stack.end, 4);
}
