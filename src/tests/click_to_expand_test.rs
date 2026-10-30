//! Tests for click-to-expand on the TUI chat.
//!
//! A left-click on a message row resolves an action from the message under the
//! cursor: a tool-call group toggles that group's `expanded` flag, a message
//! with reasoning `details` toggles its `expanded` flag, and any plain text row
//! falls through to highlight-select. This mirrors Ctrl+O but scoped to the one
//! block clicked instead of flipping every block at once.
//!
//! The full TUI App is too heavy to construct in unit tests (needs DB,
//! provider, services), so we test the pure `DisplayMessage::click_action`
//! classifier that the click handler dispatches on.

use crate::tui::app::{ClickAction, DisplayMessage, ToolCallEntry, ToolCallGroup};
use uuid::Uuid;

fn base_msg(role: &str) -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: role.to_string(),
        content: String::new(),
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

fn with_tool_group(expanded: bool) -> DisplayMessage {
    let mut msg = base_msg("tool_group");
    msg.tool_group = Some(ToolCallGroup {
        calls: vec![ToolCallEntry {
            task_id: None,
            description: "read a file".to_string(),
            success: true,
            details: None,
            completed: true,
            tool_input: serde_json::Value::Null,
        }],
        expanded,
    });
    msg
}

#[test]
fn details_cycle_is_collapsed_capped_full_collapsed() {
    // #727: reasoning cycles through three states on each click / ctrl+o.
    let mut msg = base_msg("assistant");
    msg.details = Some("a\nb\nc".to_string());
    assert!(!msg.expanded && !msg.expanded_full, "starts collapsed");

    msg.cycle_details_expand();
    assert!(msg.expanded && !msg.expanded_full, "1st -> capped");

    msg.cycle_details_expand();
    assert!(msg.expanded && msg.expanded_full, "2nd -> full");

    msg.cycle_details_expand();
    assert!(
        !msg.expanded && !msg.expanded_full,
        "3rd -> collapsed (cycle wraps)"
    );
}

#[test]
fn tool_group_row_toggles_that_group() {
    let msg = with_tool_group(false);
    assert_eq!(msg.click_action(), ClickAction::ToggleToolGroup);
}

#[test]
fn reasoning_details_row_toggles_details() {
    let mut msg = base_msg("assistant");
    msg.details = Some("Thinking about the problem...".to_string());
    assert_eq!(msg.click_action(), ClickAction::ToggleDetails);
}

#[test]
fn plain_text_row_falls_through_to_select() {
    let mut msg = base_msg("assistant");
    msg.content = "just some visible text".to_string();
    assert_eq!(msg.click_action(), ClickAction::Select);
}

#[test]
fn user_message_row_falls_through_to_select() {
    let mut msg = base_msg("user");
    msg.content = "hello".to_string();
    assert_eq!(msg.click_action(), ClickAction::Select);
}

#[test]
fn tool_group_takes_precedence_over_details() {
    // A message carrying both resolves to the tool-group toggle first.
    let mut msg = with_tool_group(true);
    msg.details = Some("some reasoning".to_string());
    assert_eq!(msg.click_action(), ClickAction::ToggleToolGroup);
}

#[test]
fn toggle_flips_only_the_clicked_group() {
    // Simulate the handler applying the action: two independent groups, only
    // the clicked one flips.
    let mut a = with_tool_group(false);
    let b = with_tool_group(false);

    // Click resolves to ToggleToolGroup on `a`; apply it.
    if a.click_action() == ClickAction::ToggleToolGroup
        && let Some(ref mut g) = a.tool_group
    {
        g.expanded = !g.expanded;
    }

    assert!(
        a.tool_group.as_ref().unwrap().expanded,
        "clicked group expands"
    );
    assert!(
        !b.tool_group.as_ref().unwrap().expanded,
        "neighbor group is untouched"
    );
    // The neighbor still classifies as a togglable group; it just wasn't clicked.
    assert_eq!(b.click_action(), ClickAction::ToggleToolGroup);
}
