//! #2123: `sleep` is refused while a detached task runs for the session.
//!
//! A detached task resumes the session on its own completion, so waiting on a
//! counter only burns time that belongs to other work. The gate is scoped to
//! the session that owns a running task: with nothing detached, `sleep` is
//! ordinary shell and runs as before.

use crate::brain::agent::service::background_tasks::BackgroundTaskManager;
use crate::brain::tools::Tool;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::bash::{BashTool, sleep_refusal, sleeping_segment};
use std::sync::Arc;
use uuid::Uuid;

#[test]
fn plain_sleep_is_matched() {
    assert!(sleeping_segment("sleep 90").is_some());
    assert!(sleeping_segment("sleep 0.5").is_some());
}

#[test]
fn sleep_inside_a_chain_is_matched() {
    assert!(sleeping_segment("date && sleep 45").is_some());
    assert!(sleeping_segment("echo hi; sleep 1").is_some());
    assert!(sleeping_segment("false || sleep 2").is_some());
}

#[test]
fn sleep_behind_a_wrapper_is_matched() {
    assert!(sleeping_segment("sudo sleep 1").is_some());
    assert!(sleeping_segment("nohup sleep 1").is_some());
    assert!(sleeping_segment("timeout 30 sleep 5").is_some());
    assert!(sleeping_segment("timeout -s KILL 30 sleep 5").is_some());
    assert!(sleeping_segment("FOO=bar sleep 1").is_some());
}

#[test]
fn sleep_in_a_loop_body_is_matched() {
    assert!(sleeping_segment("for i in 1 2 3; do sleep 1; done").is_some());
}

#[test]
fn the_word_sleep_inside_arguments_is_not_matched() {
    // Searching for this very rule must stay possible.
    assert!(sleeping_segment(r#"grep -rn "sleep" logs/"#).is_none());
    assert!(sleeping_segment("rg 'sleep 90' src/").is_none());
    assert!(sleeping_segment("echo sleep").is_none());
    assert!(sleeping_segment("git log --grep=sleep").is_none());
}

#[test]
fn no_running_task_means_no_refusal() {
    assert!(sleep_refusal("sleep 90", &[]).is_none());
}

#[test]
fn a_running_task_refuses_sleep() {
    let running = ["cargo test".to_string()];
    let msg = sleep_refusal("sleep 90", &running).expect("a running task must refuse the sleep");
    assert!(msg.contains("sleep 90"), "message names the command: {msg}");
    assert!(
        msg.contains("`cargo test` is still running"),
        "message names the task label: {msg}"
    );
    assert!(
        msg.contains("reports back on its own"),
        "message explains why waiting is pointless: {msg}"
    );
}

#[test]
fn several_running_tasks_are_all_named() {
    let running = ["cargo test".to_string(), "cargo build".to_string()];
    let msg = sleep_refusal("sleep 90", &running).expect("running tasks must refuse the sleep");
    assert!(
        msg.contains("2 detached tasks are still running"),
        "message counts them: {msg}"
    );
    assert!(
        msg.contains("cargo test, cargo build"),
        "message names every label: {msg}"
    );
}

#[test]
fn a_running_task_does_not_refuse_other_commands() {
    let running = ["cargo test".to_string()];
    assert!(sleep_refusal("cargo test", &running).is_none());
    assert!(sleep_refusal("grep -rn 'sleep' logs/", &running).is_none());
}

#[tokio::test]
async fn execute_refuses_sleep_while_a_task_runs() {
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    mgr.mirror_started(sid, "cargo test");

    let mut context = ToolExecutionContext::new(sid).with_auto_approve(true);
    context.background_manager = Some(mgr);

    let result = BashTool
        .execute(serde_json::json!({ "command": "sleep 30" }), &context)
        .await
        .unwrap();

    assert!(!result.success, "the sleep must be refused");
    // `ToolResult::error` carries the message in `error`, and that is the field
    // `build_tool_result_content` hands the model. Same shape the interactive
    // and recent-failure refusals already use.
    let msg = result.error.unwrap_or_default();
    assert!(
        msg.contains("`cargo test` is still running"),
        "error names the task that is running: {msg}"
    );
    assert!(
        msg.contains("reports back on its own"),
        "error explains the refusal: {msg}"
    );
}

#[tokio::test]
async fn execute_allows_sleep_when_nothing_is_detached() {
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();

    let mut context = ToolExecutionContext::new(sid).with_auto_approve(true);
    context.background_manager = Some(mgr);

    // `sleep 0` returns immediately, so this proves the gate opens without
    // costing the test any wall time.
    let result = BashTool
        .execute(serde_json::json!({ "command": "sleep 0" }), &context)
        .await
        .unwrap();

    assert!(
        result.success,
        "sleep runs when nothing is detached: {}",
        result.output
    );
}

#[tokio::test]
async fn another_sessions_task_does_not_refuse_this_sessions_sleep() {
    let mgr = Arc::new(BackgroundTaskManager::new());
    let busy = Uuid::new_v4();
    let idle = Uuid::new_v4();
    mgr.mirror_started(busy, "cargo test");

    let mut context = ToolExecutionContext::new(idle).with_auto_approve(true);
    context.background_manager = Some(mgr);

    let result = BashTool
        .execute(serde_json::json!({ "command": "sleep 0" }), &context)
        .await
        .unwrap();

    assert!(
        result.success,
        "the gate is per session, not global: {}",
        result.output
    );
}

#[tokio::test]
async fn sleep_runs_again_once_the_task_finished() {
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    mgr.mirror_started(sid, "cargo test");

    let mut context = ToolExecutionContext::new(sid).with_auto_approve(true);
    context.background_manager = Some(mgr.clone());

    let refused = BashTool
        .execute(serde_json::json!({ "command": "sleep 30" }), &context)
        .await
        .unwrap();
    assert!(
        !refused.success,
        "the sleep must be refused while the task runs"
    );

    mgr.mirror_finished(sid, "cargo test");
    assert_eq!(mgr.running_for(sid), 0, "the registry must be clear again");

    let allowed = BashTool
        .execute(serde_json::json!({ "command": "sleep 0" }), &context)
        .await
        .unwrap();
    assert!(
        allowed.success,
        "sleep is ordinary shell once nothing is detached: {}",
        allowed.output
    );
}
