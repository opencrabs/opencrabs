//! Live tool block for WhatsApp resume turns (#2023).
//!
//! The network side of `flow_message.rs` needs a live client, but everything
//! the user reads is decided by the pure fold and render in `flow_render.rs`.

use crate::brain::agent::ProgressEvent;
use crate::channels::whatsapp::flow_render::{MAX_VISIBLE_STEPS, Phase, Step, fold, render};
use serde_json::json;
use std::time::Duration;

fn started(name: &str) -> ProgressEvent {
    ProgressEvent::ToolStarted {
        tool_name: name.to_string(),
        tool_input: json!({}),
        tool_call_id: String::new(),
    }
}

fn completed(name: &str, success: bool) -> ProgressEvent {
    ProgressEvent::ToolCompleted {
        tool_name: name.to_string(),
        tool_input: json!({}),
        success,
        summary: String::new(),
        tool_call_id: String::new(),
    }
}

fn narration(text: &str) -> ProgressEvent {
    ProgressEvent::IntermediateText {
        text: text.to_string(),
        reasoning: None,
    }
}

#[test]
fn completion_closes_the_running_step_it_belongs_to() {
    let mut steps = Vec::new();
    fold(&mut steps, &started("bash"));
    fold(&mut steps, &completed("bash", true));

    assert_eq!(steps.len(), 1, "a start and its completion are one step");
    assert!(matches!(
        &steps[0],
        Step::Tool {
            status: Some(true),
            ..
        }
    ));
}

#[test]
fn failed_completion_marks_the_step_failed() {
    let mut steps = Vec::new();
    fold(&mut steps, &started("bash"));
    fold(&mut steps, &completed("bash", false));

    assert!(matches!(
        &steps[0],
        Step::Tool {
            status: Some(false),
            ..
        }
    ));
}

#[test]
fn completion_closes_the_most_recent_open_call_of_that_name() {
    let mut steps = Vec::new();
    fold(&mut steps, &started("bash"));
    fold(&mut steps, &started("bash"));
    fold(&mut steps, &completed("bash", true));

    assert_eq!(steps.len(), 2);
    assert!(matches!(&steps[0], Step::Tool { status: None, .. }));
    assert!(matches!(
        &steps[1],
        Step::Tool {
            status: Some(true),
            ..
        }
    ));
}

#[test]
fn completion_without_a_start_is_still_recorded() {
    let mut steps = Vec::new();
    let changed = fold(&mut steps, &completed("read_file", false));

    assert!(changed);
    assert_eq!(
        steps.len(),
        1,
        "a result with no matching start is not lost"
    );
    assert!(matches!(
        &steps[0],
        Step::Tool {
            status: Some(false),
            ..
        }
    ));
}

#[test]
fn blank_narration_changes_nothing() {
    let mut steps = Vec::new();
    let changed = fold(&mut steps, &narration("   \n  "));

    assert!(!changed);
    assert!(steps.is_empty());
}

#[test]
fn narration_is_shown_on_one_line() {
    let mut steps = Vec::new();
    fold(&mut steps, &narration("first line\n\nsecond   line"));
    let block = render(&steps, Phase::Running, Duration::from_secs(3));

    assert!(
        block.contains("💬 first line second line"),
        "narration folds to one line: {block}"
    );
}

#[test]
fn narration_is_scrubbed_before_it_reaches_the_chat() {
    let mut steps = Vec::new();
    fold(
        &mut steps,
        &narration("calling with Authorization: Bearer sk-abc123 now"),
    );
    let block = render(&steps, Phase::Running, Duration::from_secs(1));

    assert!(!block.contains("sk-abc123"), "secret leaked: {block}");
}

#[test]
fn running_header_shows_the_call_count_and_clock() {
    let mut steps = Vec::new();
    fold(&mut steps, &started("bash"));
    fold(&mut steps, &started("read_file"));
    let block = render(&steps, Phase::Running, Duration::from_secs(45));

    assert!(
        block.starts_with("⚙️ *Working* · 2 tool calls · ⏱ 45s"),
        "header wrong: {block}"
    );
}

#[test]
fn a_single_call_is_singular() {
    let mut steps = Vec::new();
    fold(&mut steps, &started("bash"));
    let block = render(&steps, Phase::Running, Duration::from_secs(1));

    assert!(block.starts_with("⚙️ *Working* · 1 tool call ·"), "{block}");
}

#[test]
fn settled_headers_name_the_outcome() {
    let finished = render(&[], Phase::Finished, Duration::from_secs(72));
    let failed = render(&[], Phase::Failed, Duration::from_secs(72));

    assert!(finished.starts_with("✅ *Finished* · 0 tool calls · ⏱ 1m 12s"));
    assert!(failed.starts_with("❌ *Failed* · 0 tool calls · ⏱ 1m 12s"));
}

#[test]
fn steps_show_running_done_and_failed_icons() {
    let mut steps = Vec::new();
    fold(&mut steps, &started("bash"));
    fold(&mut steps, &started("read_file"));
    fold(&mut steps, &completed("read_file", false));
    let block = render(&steps, Phase::Running, Duration::from_secs(2));

    assert!(block.contains("⚙️ bash"), "running icon missing: {block}");
    assert!(
        block.contains("❌ read_file"),
        "failed icon missing: {block}"
    );
}

#[test]
fn long_turns_fold_older_steps_into_one_line() {
    let mut steps = Vec::new();
    for _ in 0..(MAX_VISIBLE_STEPS + 3) {
        fold(&mut steps, &started("bash"));
    }
    let block = render(&steps, Phase::Running, Duration::from_secs(9));
    let lines: Vec<&str> = block.lines().collect();

    assert_eq!(
        lines.len(),
        MAX_VISIBLE_STEPS + 2,
        "header, fold line, then the visible steps"
    );
    assert_eq!(lines[1], "… 3 earlier steps");
}

#[test]
fn long_labels_are_cut_by_characters_not_bytes() {
    let mut steps = Vec::new();
    let long: String = "é".repeat(300);
    fold(&mut steps, &narration(&long));
    let block = render(&steps, Phase::Running, Duration::from_secs(1));
    let step_line = block.lines().nth(1).expect("one step line");

    assert!(step_line.ends_with('…'), "a cut is marked: {step_line}");
    assert!(
        step_line.chars().count() <= 165,
        "label respects the ceiling: {}",
        step_line.chars().count()
    );
}

#[test]
fn clock_formats_seconds_then_minutes() {
    let under = render(&[], Phase::Running, Duration::from_secs(59));
    let over = render(&[], Phase::Running, Duration::from_secs(125));

    assert!(under.ends_with("⏱ 59s"), "{under}");
    assert!(over.ends_with("⏱ 2m 5s"), "{over}");
}

#[test]
fn tool_arguments_are_scrubbed_before_they_reach_the_chat() {
    let mut steps = Vec::new();
    fold(
        &mut steps,
        &ProgressEvent::ToolStarted {
            tool_name: "bash".to_string(),
            tool_input: json!({
                "command": "curl -H \"Authorization: Bearer sk-abc123\" https://api.example.com"
            }),
            tool_call_id: String::new(),
        },
    );
    let block = render(&steps, Phase::Running, Duration::from_secs(1));

    assert!(!block.contains("sk-abc123"), "secret leaked: {block}");
}

// ---- Detached background task rows (#2145, #2150) -------------------------

fn task_started(task_id: &str, label: &str) -> ProgressEvent {
    ProgressEvent::DetachedTaskStarted {
        task_id: task_id.to_string(),
        label: label.to_string(),
    }
}

fn task_finished(task_id: &str, success: bool) -> ProgressEvent {
    ProgressEvent::DetachedTaskFinished {
        task_id: task_id.to_string(),
        label: String::new(),
        success,
        elapsed_secs: 1.5,
    }
}

#[test]
fn a_detached_task_folds_into_one_row_that_settles() {
    let mut steps = Vec::new();
    fold(&mut steps, &task_started("t1", "cargo test"));

    assert_eq!(steps.len(), 1, "a detached task is one row");
    let running = render(&steps, Phase::Running, Duration::from_secs(1));
    assert!(running.contains("⚙️"), "{running}");

    fold(&mut steps, &task_finished("t1", true));

    assert_eq!(
        steps.len(),
        1,
        "the finish settles the same row, not a new one"
    );
    let done = render(&steps, Phase::Running, Duration::from_secs(1));
    assert!(done.contains("✅"), "{done}");
}

#[test]
fn a_failed_detached_task_shows_the_failed_icon() {
    let mut steps = Vec::new();
    fold(&mut steps, &task_started("t1", "cargo test"));
    fold(&mut steps, &task_finished("t1", false));

    let block = render(&steps, Phase::Running, Duration::from_secs(1));
    assert!(block.contains("❌"), "{block}");
}

#[test]
fn a_finish_settles_its_own_row_when_two_tasks_share_a_label() {
    let mut steps = Vec::new();
    fold(&mut steps, &task_started("t1", "cargo test"));
    fold(&mut steps, &task_started("t2", "cargo test"));

    // The first task finishing must not settle the second one, which is still
    // running: matching on the label would hit whichever row came first.
    fold(&mut steps, &task_finished("t1", true));

    assert!(matches!(
        &steps[0],
        Step::Tool {
            status: Some(true),
            task_id: Some(id),
            ..
        } if id == "t1"
    ));
    assert!(matches!(
        &steps[1],
        Step::Tool {
            status: None,
            task_id: Some(id),
            ..
        } if id == "t2"
    ));
}

#[test]
fn a_finish_for_a_row_this_block_never_saw_changes_nothing() {
    let mut steps = Vec::new();
    fold(&mut steps, &task_started("t1", "cargo test"));

    let changed = fold(&mut steps, &task_finished("t9", true));

    assert!(!changed, "an unknown task id must not settle another row");
    assert!(matches!(&steps[0], Step::Tool { status: None, .. }));
}

#[test]
fn a_detached_row_names_itself_in_the_block() {
    let mut steps = Vec::new();
    fold(&mut steps, &task_started("t1", "cargo test"));

    let block = render(&steps, Phase::Running, Duration::from_secs(1));

    assert!(block.contains("cargo test"), "{block}");
}

// ---- The 15-minute edit window (#1408, #2150) -----------------------------

#[test]
fn the_edit_window_is_open_inside_fifteen_minutes() {
    use crate::channels::whatsapp::flow_render::within_edit_window;

    assert!(within_edit_window(Duration::from_secs(0)));
    assert!(within_edit_window(Duration::from_secs(14 * 60)));
    assert!(within_edit_window(Duration::from_secs(15 * 60 - 1)));
}

#[test]
fn the_edit_window_closes_at_fifteen_minutes() {
    use crate::channels::whatsapp::flow_render::within_edit_window;

    assert!(!within_edit_window(Duration::from_secs(15 * 60)));
    assert!(!within_edit_window(Duration::from_secs(16 * 60)));
    assert!(!within_edit_window(Duration::from_secs(60 * 60)));
}
