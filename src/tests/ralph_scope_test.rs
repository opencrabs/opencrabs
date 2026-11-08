//! End-scoped verification gate (#2694).
//!
//! The gate used to run on EVERY task completion: a multi-task plan paid for a
//! full per-type command suite (cargo test / clippy on a Rust project) once per
//! task, hours of it, while the default advisory policy could not even reject a
//! completion. The gate exists to validate the finished plan, so by default it
//! now runs once, on the task whose completion finishes the plan.
//!
//! These tests drive the real `complete` handler and watch a marker file the
//! configured command leaves behind: the marker exists if and only if the gate
//! actually ran. That is a behaviour receipt, not a log scrape. Fixtures live in
//! unique tempdirs under a throwaway profile home, so nothing reads or writes
//! the user's real ralph_loop.toml.

use crate::brain::tools::plan_tool::PlanTool;
use crate::brain::tools::{Tool, ToolExecutionContext};
use crate::config::profile::{home_for_profile, with_profile_home_async};
use crate::tui::plan::{ApprovalSource, TaskStatus};
use crate::utils::plan_files::{load_plan, save_plan};
use serde_json::json;
use std::fs;
use uuid::Uuid;

async fn in_temp_home<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let profile = format!("ralph-scope-test-{}", Uuid::new_v4());
    let out = with_profile_home_async(Some(&profile), f).await;
    let home = home_for_profile(Some(&profile));
    let _ = std::fs::remove_dir_all(&home);
    out
}

async fn run_tool(
    tool: &PlanTool,
    ctx: &ToolExecutionContext,
    input: serde_json::Value,
) -> (bool, String) {
    let r = tool.execute(input, ctx).await.unwrap();
    let text = if r.success {
        r.output
    } else {
        r.error.unwrap_or_default()
    };
    (r.success, text)
}

/// A project `ralph_loop.toml` with the gate enabled and a marker-leaving
/// command for the `edit` type, so the per-type path runs something observable
/// instead of the cargo fallback. `scope = None` omits the key entirely, which
/// must resolve to the `final` default.
fn scope_dir(name: &str, scope: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("scope2694_{name}_"))
        .tempdir()
        .expect("tempdir");
    let scope_line = scope
        .map(|s| format!("scope = \"{s}\"\n"))
        .unwrap_or_default();
    fs::write(
        dir.path().join("ralph_loop.toml"),
        format!(
            "[verification]\nenabled = true\n{scope_line}\n\
             [[verification.task_type_commands]]\n\
             task_type = \"edit\"\ncommands = [\"touch marker.txt\"]\n"
        ),
    )
    .expect("write config");
    dir
}

/// Init an approved checklist plan bound to the policy tempdir, so the gate
/// config comes from there and never from the machine-wide safety copy.
async fn approved_ctx(
    dir: &tempfile::TempDir,
    tool: &PlanTool,
    tasks: serde_json::Value,
) -> ToolExecutionContext {
    let mut ctx = ToolExecutionContext::new(Uuid::new_v4());
    ctx.working_directory = dir.path().to_path_buf();
    let (ok, out) = run_tool(
        tool,
        &ctx,
        json!({
            "operation": "init",
            "title": "scope plan",
            "mode": "checklist",
            "tasks": tasks
        }),
    )
    .await;
    assert!(ok, "init must succeed: {out}");
    if let Some(mut plan) = load_plan(ctx.session_id).await {
        plan.approve(ApprovalSource::User);
        save_plan(&plan).await.unwrap();
    }
    ctx
}

fn two_tasks() -> serde_json::Value {
    json!([
        { "title": "one", "description": "d", "task_type": "edit" },
        { "title": "two", "description": "d", "task_type": "edit" }
    ])
}

/// The core deferral receipt: on a two-task plan the gate must NOT run for task
/// 1 (not final) and MUST run for task 2 (finishes the plan). The marker file is
/// the witness. Task 1 still completes, logged Uncertain, never a silent pass.
async fn assert_two_task_defers(scope: Option<&str>, name: &str) {
    let dir = scope_dir(name, scope);
    let tool = PlanTool;
    let ctx = approved_ctx(&dir, &tool, two_tasks()).await;
    let marker = dir.path().join("marker.txt");

    run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
    let (ok, out) = run_tool(
        &tool,
        &ctx,
        json!({ "operation": "complete", "task_order": 1, "action": "success" }),
    )
    .await;
    assert!(ok, "a deferred completion still completes the task: {out}");
    assert!(
        !marker.exists(),
        "the gate must not run on a non-final task under scope=final"
    );
    assert!(
        out.contains("UNCERTAIN") && out.contains("deferred"),
        "the deferral must be reported, not hidden: {out}"
    );
    let plan = load_plan(ctx.session_id).await.unwrap();
    assert_eq!(
        plan.get_task_by_order(1).unwrap().status,
        TaskStatus::Completed,
        "deferral skips the commands, not the completion"
    );

    run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
    let (ok, out) = run_tool(
        &tool,
        &ctx,
        json!({ "operation": "complete", "task_order": 2, "action": "success" }),
    )
    .await;
    assert!(ok, "the final completion must succeed: {out}");
    assert!(
        marker.exists(),
        "the gate must run once, on the task that finishes the plan"
    );
}

#[tokio::test]
async fn scope_default_defers_verification_to_the_final_task() {
    in_temp_home(async {
        assert_two_task_defers(None, "default").await;
    })
    .await;
}

#[tokio::test]
async fn scope_final_explicit_defers_verification_to_the_final_task() {
    in_temp_home(async {
        assert_two_task_defers(Some("final"), "final").await;
    })
    .await;
}

/// `scope = "each"` is the opt-in old behaviour: the gate runs on the very first
/// task, marker and all.
#[tokio::test]
async fn scope_each_runs_the_gate_on_every_task() {
    in_temp_home(async {
        let dir = scope_dir("each", Some("each"));
        let tool = PlanTool;
        let ctx = approved_ctx(&dir, &tool, two_tasks()).await;
        let marker = dir.path().join("marker.txt");

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok, "each completes task 1: {out}");
        assert!(
            marker.exists(),
            "scope=each must run the gate on the first task, the old behaviour"
        );
    })
    .await;
}

/// A single-task plan's only task is trivially final, so even under the default
/// the gate runs immediately. This pins the is_final_task base case end to end.
#[tokio::test]
async fn scope_final_single_task_plan_runs_the_gate_immediately() {
    in_temp_home(async {
        let dir = scope_dir("single", None);
        let tool = PlanTool;
        let ctx = approved_ctx(
            &dir,
            &tool,
            json!([{ "title": "only", "description": "d", "task_type": "edit" }]),
        )
        .await;
        let marker = dir.path().join("marker.txt");

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok, "the single task completes: {out}");
        assert!(
            marker.exists(),
            "a single-task plan's only task is final, so the gate runs at once"
        );
    })
    .await;
}
