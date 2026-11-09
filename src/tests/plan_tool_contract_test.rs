//! Contract tests for the plan tool's lifecycle-engine behavior:
//! mode disambiguation (design vs checklist), pre-init upgrade/replace
//! rules, re-init refusal while a plan is live, Active-only checklist
//! operations, `add_tasks` plus the `add_task` alias, import rules,
//! keeping a completed plan live until turn settle (no mid-complete
//! archive), and the removal of auto-approve on first `start`.

use crate::brain::tools::plan_tool::PlanTool;
use crate::brain::tools::{Tool, ToolExecutionContext};
use crate::config::profile::{home_for_profile, with_profile_home_async};
use crate::tui::plan::{ApprovalSource, PlanStatus};
use crate::utils::plan_files::{
    PlanModeState, PreInitOrigin, load_plan, plan_json_path, plan_md_path, plan_mode_state,
    set_pre_init_editing, set_pre_init_editing_with_origin,
};
use serde_json::json;

async fn in_temp_home<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let profile = format!("plan-contract-test-{}", uuid::Uuid::new_v4());
    let out = with_profile_home_async(Some(&profile), f).await;
    let home = home_for_profile(Some(&profile));
    let _ = std::fs::remove_dir_all(&home);
    out
}

async fn run(
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

/// Helper to approve a plan so start/complete operations are allowed.
async fn approve_plan(ctx: &ToolExecutionContext) {
    if let Some(mut plan) = load_plan(ctx.session_id).await {
        plan.approve(ApprovalSource::User);
        crate::utils::plan_files::save_plan(&plan).await.unwrap();
    }
}

#[tokio::test]
async fn design_init_creates_md_and_enters_editing() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, out) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "Refactor auth", "mode": "design" }),
        )
        .await;
        assert!(ok, "design init must succeed, got: {out}");
        let md = plan_md_path(ctx.session_id).await;
        assert!(md.exists(), "design init must create the session .md");
        assert!(
            out.contains(&md.display().to_string()),
            "result must return the absolute .md path, got: {out}"
        );
        assert!(
            out.contains("Do NOT call 'start'"),
            "design init must steer the model away from start, got: {out}"
        );
        assert_eq!(
            plan_mode_state(ctx.session_id).await,
            PlanModeState::PostInitEditing
        );
        let plan = load_plan(ctx.session_id).await.unwrap();
        assert_eq!(plan.status, PlanStatus::Editing);
        assert!(plan.tasks.is_empty());
        assert!(plan.approved_at.is_none());
    })
    .await;
}

#[tokio::test]
async fn omitted_mode_disambiguates_by_tasks() {
    in_temp_home(async {
        let tool = PlanTool;
        // No tasks → design.
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, _) = run(&tool, &ctx, json!({ "operation": "init", "title": "D" })).await;
        assert!(ok);
        assert_eq!(
            plan_mode_state(ctx.session_id).await,
            PlanModeState::PostInitEditing
        );

        // Tasks → checklist, Editing (requires approval before start).
        let ctx2 = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, out) = run(
            &tool,
            &ctx2,
            json!({ "operation": "init", "title": "C", "tasks": [{ "title": "t1", "description": "d1" }] }),
        )
        .await;
        assert!(ok, "checklist init must succeed, got: {out}");
        assert!(
            out.contains("Editing"),
            "checklist init reports Editing, got: {out}"
        );
        assert_eq!(
            plan_mode_state(ctx2.session_id).await,
            PlanModeState::PostInitEditing
        );
    })
    .await;
}

#[tokio::test]
async fn conflicting_mode_and_tasks_are_refused() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, msg) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "X", "mode": "design", "tasks": [{ "title": "t", "description": "d" }] }),
        )
        .await;
        assert!(!ok, "design with tasks must be refused");
        assert!(msg.contains("design"), "got: {msg}");

        let (ok, msg) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "X", "mode": "checklist" }),
        )
        .await;
        assert!(!ok, "checklist without tasks must be refused");
        assert!(msg.contains("checklist"), "got: {msg}");

        // Neither refusal left plan artifacts behind.
        assert_eq!(plan_mode_state(ctx.session_id).await, PlanModeState::NoPlan);
    })
    .await;
}

#[tokio::test]
async fn checklist_init_creates_no_design_md() {
    // #1145: checklist plans carry no design document — the scaffold `.md`
    // existed only to hold the plan in Editing for approval (#573), and the
    // card rendered its hollow `## Implementation steps` section as a phantom
    // prose block. No file, no "Plan document" line in the message.
    // #20: this holds even under tool auto-approve — the plan gate never
    // inherits the tool approval policy, so the plan parks in Editing.
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4()).with_auto_approve(true);
        let (ok, out) = run(
            &tool,
            &ctx,
            json!({
                "operation": "init",
                "title": "Ship fix",
                "tasks": [{ "title": "t1", "description": "d1" }]
            }),
        )
        .await;
        assert!(ok, "checklist init must succeed, got: {out}");
        assert!(
            !plan_md_path(ctx.session_id).await.exists(),
            "checklist init must not create a design .md"
        );
        assert!(
            !out.contains("Plan document"),
            "checklist init must not point at a plan document, got: {out}"
        );
        assert_eq!(
            plan_mode_state(ctx.session_id).await,
            PlanModeState::PostInitEditing
        );
        let plan = load_plan(ctx.session_id).await.unwrap();
        assert_eq!(plan.status, PlanStatus::Editing);
        assert!(
            plan.pending_approval,
            "#20: auto-approve no longer self-activates; the plan waits"
        );
        assert_eq!(plan.tasks.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn checklist_init_editing_creates_no_design_md() {
    // #1145, user-review flavor (no auto-approve): still no `.md`, and the
    // message keeps its card-dedup guidance without a document path.
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, out) = run(
            &tool,
            &ctx,
            json!({
                "operation": "init",
                "title": "Ship fix",
                "tasks": [{ "title": "t1", "description": "d1" }]
            }),
        )
        .await;
        assert!(ok, "checklist init must succeed, got: {out}");
        assert!(
            !plan_md_path(ctx.session_id).await.exists(),
            "checklist init must not create a design .md"
        );
        assert!(
            !out.contains("Plan document"),
            "checklist init must not point at a plan document, got: {out}"
        );
        assert_eq!(
            plan_mode_state(ctx.session_id).await,
            PlanModeState::PostInitEditing
        );
        let plan = load_plan(ctx.session_id).await.unwrap();
        assert_eq!(plan.status, PlanStatus::Editing);
        assert_eq!(plan.tasks.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn design_init_waits_in_editing_under_auto_approve_when_gate_on() {
    // #20: the rush-refusal existed because tool auto-approve used to
    // auto-activate design plans. With `plan_require_approval` on (the
    // default) the plan gate no longer inherits the tool approval policy, so
    // an agent-initiated design in yolo is harmless — it parks in Editing for
    // explicit approval exactly like a checklist. The refusal survives only
    // under the escape hatch (plan_approval_gate_test).
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4()).with_auto_approve(true);
        let (ok, msg) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "Yolo design", "mode": "design" }),
        )
        .await;
        assert!(
            ok,
            "gate-on yolo design init must succeed and wait, got: {msg}"
        );
        assert!(
            plan_md_path(ctx.session_id).await.exists(),
            "design init must create its .md"
        );
        assert_eq!(
            plan_mode_state(ctx.session_id).await,
            PlanModeState::PostInitEditing
        );
        let plan = load_plan(ctx.session_id).await.unwrap();
        assert_eq!(plan.status, PlanStatus::Editing);
        assert!(plan.pending_approval);
        assert!(plan.approved_at.is_none());

        // Keyword soft-nudge origin (user typed plan-shaped words, never the
        // slash): same contract — parks in Editing.
        let ctx_nudge =
            ToolExecutionContext::new(uuid::Uuid::new_v4()).with_auto_approve(true);
        set_pre_init_editing(ctx_nudge.session_id).await.unwrap();
        let (ok, msg) = run(
            &tool,
            &ctx_nudge,
            json!({ "operation": "init", "title": "Nudge design", "mode": "design" }),
        )
        .await;
        assert!(
            ok,
            "nudge-origin yolo design must park in Editing, got: {msg}"
        );
        let nudge_plan = load_plan(ctx_nudge.session_id).await.unwrap();
        assert_eq!(nudge_plan.status, PlanStatus::Editing);

        // Checklist stays allowed under auto-approve (fresh session — the
        // design plan above is live in ctx, reinit there is refused).
        let ctx_checklist = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, _) = run(
            &tool,
            &ctx_checklist,
            json!({ "operation": "init", "title": "Yolo checklist", "tasks": [{ "title": "t", "description": "d" }] }),
        )
        .await;
        assert!(ok, "checklist init must succeed");
    })
    .await;
}

#[tokio::test]
async fn design_init_allowed_under_auto_approve_when_plan_slash_armed() {
    // The user typed /plan themselves: the deliberate brake. Design init is
    // allowed, the plan parks in Editing for review, and /execute resumes
    // the rush (approve -> Active + seed turn).
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4()).with_auto_approve(true);
        set_pre_init_editing_with_origin(ctx.session_id, PreInitOrigin::Slash)
            .await
            .unwrap();

        let (ok, msg) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "Yolo design", "mode": "design" }),
        )
        .await;
        assert!(ok, "slash-armed yolo design must be allowed, got: {msg}");
        assert_eq!(
            plan_mode_state(ctx.session_id).await,
            PlanModeState::PostInitEditing,
            "the design parks in Editing for review"
        );
        let plan = load_plan(ctx.session_id).await.unwrap();
        assert_eq!(plan.status, PlanStatus::Editing);

        // The scaffold .md is not approvable yet.
        assert!(matches!(
            crate::utils::plan_mode::try_approve(ctx.session_id, ApprovalSource::User).await,
            crate::utils::plan_mode::ApproveOutcome::Refused(_)
        ));

        // Agent writes the design prose; /execute then resumes the rush.
        std::fs::write(
            plan_md_path(ctx.session_id).await,
            "# Yolo design\n\n\
             ## Context\n\
             - **Problem:** Yolo cannot review plans.\n\
             - **Target state:** /plan in yolo parks for review.\n\
             - **Intent:** Ship the gate.\n\n\
             ## Implementation steps\n\
             1. Scope the refusal to slash origin.\n\
             2. Amend the ADRs.\n",
        )
        .unwrap();
        match crate::utils::plan_mode::try_approve(ctx.session_id, ApprovalSource::User).await {
            crate::utils::plan_mode::ApproveOutcome::SeedTurn { prompt, .. } => {
                assert!(prompt.contains("PLAN APPROVED"), "got: {prompt}");
                assert!(prompt.contains("add_tasks"), "got: {prompt}");
            }
            other => panic!("expected SeedTurn from /execute, got {other:?}"),
        }
        let plan = load_plan(ctx.session_id).await.unwrap();
        assert_eq!(plan.status, PlanStatus::Active);
        assert!(plan.approved_at.is_some());
    })
    .await;
}

#[tokio::test]
async fn pre_init_upgrades_to_design_and_replaces_for_checklist() {
    in_temp_home(async {
        let tool = PlanTool;

        // Upgrade: pre-init → design init → post-init Editing.
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        set_pre_init_editing(ctx.session_id).await.unwrap();
        let (ok, _) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "Upgraded", "mode": "design" }),
        )
        .await;
        assert!(ok, "design init from pre-init must upgrade the sidecar");
        assert_eq!(
            plan_mode_state(ctx.session_id).await,
            PlanModeState::PostInitEditing
        );
        let plan = load_plan(ctx.session_id).await.unwrap();
        assert!(!plan.pre_init_editing, "the flag is consumed by init");

        // Replace: pre-init → checklist init → PostInitEditing (requires approval).
        let ctx2 = ToolExecutionContext::new(uuid::Uuid::new_v4());
        set_pre_init_editing(ctx2.session_id).await.unwrap();
        let (ok, _) = run(
            &tool,
            &ctx2,
            json!({ "operation": "init", "title": "Changed my mind", "tasks": [{ "title": "t", "description": "d" }] }),
        )
        .await;
        assert!(ok, "checklist init from pre-init must replace the flag");
        assert_eq!(
            plan_mode_state(ctx2.session_id).await,
            PlanModeState::PostInitEditing
        );
    })
    .await;
}

#[tokio::test]
async fn reinit_refused_while_plan_is_live() {
    in_temp_home(async {
        let tool = PlanTool;

        // Post-init Editing blocks init.
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "First" }),
        )
        .await;
        let (ok, msg) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "Second" }),
        )
        .await;
        assert!(!ok, "re-init over post-init Editing must be refused");
        assert!(msg.to_lowercase().contains("discard"), "got: {msg}");

        // Active blocks init too.
        let ctx2 = ToolExecutionContext::new(uuid::Uuid::new_v4());
        run(
            &tool,
            &ctx2,
            json!({ "operation": "init", "title": "Live", "tasks": [{ "title": "t", "description": "d" }] }),
        )
        .await;
        let (ok, msg) = run(
            &tool,
            &ctx2,
            json!({ "operation": "init", "title": "Second" }),
        )
        .await;
        assert!(!ok, "re-init over Active must be refused");
        assert!(msg.to_lowercase().contains("discard"), "got: {msg}");
    })
    .await;
}

#[tokio::test]
async fn checklist_ops_blocked_while_editing() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "Design" }),
        )
        .await;

        for op in [
            json!({ "operation": "start" }),
            json!({ "operation": "complete", "task_order": 1 }),
            json!({ "operation": "add_tasks", "tasks": [{ "title": "t", "description": "d" }] }),
            json!({ "operation": "add_task", "title": "t", "description": "d" }),
        ] {
            let (ok, msg) = run(&tool, &ctx, op.clone()).await;
            assert!(!ok, "{op} must be refused while Editing");
            assert!(
                msg.contains("Editing") || msg.contains("approve"),
                "refusal must explain the Editing block, got: {msg}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn add_tasks_appends_multiple_and_alias_still_works() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "List", "tasks": [{ "title": "one", "description": "d1" }] }),
        )
        .await;
        approve_plan(&ctx).await;

        let (ok, out) = run(
            &tool,
            &ctx,
            json!({ "operation": "add_tasks", "tasks": [{ "title": "two", "description": "d2" }, { "title": "three", "description": "d3" }] }),
        )
        .await;
        assert!(ok, "add_tasks must succeed, got: {out}");
        assert!(out.contains("2 task(s)") && out.contains("3 tasks"), "got: {out}");

        let (ok, out) = run(
            &tool,
            &ctx,
            json!({ "operation": "add_task", "title": "four", "description": "d4" }),
        )
        .await;
        assert!(ok, "add_task alias must keep working, got: {out}");
        assert_eq!(load_plan(ctx.session_id).await.unwrap().tasks.len(), 4);

        let (ok, msg) = run(&tool, &ctx, json!({ "operation": "add_tasks", "tasks": [] })).await;
        assert!(!ok, "empty add_tasks must be refused, got: {msg}");
    })
    .await;
}

#[tokio::test]
async fn first_start_does_not_auto_approve() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "NoAutoApprove", "tasks": [{ "title": "t", "description": "d" }] }),
        )
        .await;
        // Start is blocked until approval.
        let (ok, msg) = run(&tool, &ctx, json!({ "operation": "start" })).await;
        assert!(!ok, "start must be blocked until approval");
        assert!(msg.contains("Editing") || msg.contains("approval"), "got: {msg}");

        // After approval, start works.
        approve_plan(&ctx).await;
        let (ok, _) = run(&tool, &ctx, json!({ "operation": "start" })).await;
        assert!(ok);
        let plan = load_plan(ctx.session_id).await.unwrap();
        assert!(
            plan.approved_at.is_some(),
            "approve() stamps approved_at"
        );
    })
    .await;
}

#[tokio::test]
async fn completing_last_task_keeps_plan_live_until_settle() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        run(
            &tool,
            &ctx,
            json!({ "operation": "init", "title": "Short", "tasks": [{ "title": "only", "description": "d" }] }),
        )
        .await;
        approve_plan(&ctx).await;
        run(&tool, &ctx, json!({ "operation": "start" })).await;
        let (ok, out) = run(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok);
        // ADR 0005 Decision 9: the tool no longer archives mid-complete. The
        // plan stays live with its full all-☑ checklist until the turn settles.
        assert!(
            out.contains("Plan complete"),
            "completion reported, got: {out}"
        );
        assert!(
            plan_json_path(ctx.session_id).await.exists(),
            "live JSON stays until turn settle, not archived mid-complete"
        );
        let plan = load_plan(ctx.session_id).await.expect("plan still live");
        assert!(plan.is_complete(), "every task is resolved");
        assert_eq!(plan_mode_state(ctx.session_id).await, PlanModeState::Active);

        // The surface-agnostic settle hook (run_tool_loop_inner) runs exactly
        // this: a finished plan archives to NoPlan once the turn settles.
        crate::utils::plan_files::archive_plan(ctx.session_id)
            .await
            .unwrap();
        assert!(
            !plan_json_path(ctx.session_id).await.exists(),
            "settle archive removes the live JSON"
        );
        assert_eq!(plan_mode_state(ctx.session_id).await, PlanModeState::NoPlan);
    })
    .await;
}

#[tokio::test]
async fn empty_import_is_refused() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("empty-plan.json");
        std::fs::write(
            &file,
            serde_json::to_string(&json!({
                "title": "Empty",
                "description": "no tasks",
                "tasks": []
            }))
            .unwrap(),
        )
        .unwrap();

        let (ok, msg) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "file_path": file.to_str().unwrap() }),
        )
        .await;
        assert!(!ok, "empty import must be refused");
        assert!(msg.contains("no tasks"), "got: {msg}");
        assert_eq!(plan_mode_state(ctx.session_id).await, PlanModeState::NoPlan);
    })
    .await;
}

#[tokio::test]
async fn import_refused_while_live_but_replaces_pre_init() {
    in_temp_home(async {
        let tool = PlanTool;
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("import-plan.json");
        std::fs::write(
            &file,
            serde_json::to_string(&json!({
                "title": "Imported",
                "description": "structured",
                "tasks": [{ "title": "t1", "description": "d", "task_type": "edit" }]
            }))
            .unwrap(),
        )
        .unwrap();

        // From pre-init: import replaces the flag and waits for approval.
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        set_pre_init_editing(ctx.session_id).await.unwrap();
        let (ok, _) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "file_path": file.to_str().unwrap() }),
        )
        .await;
        assert!(ok, "import from pre-init must replace the flag");
        // #20: the pending_approval marker parks the import in Editing — it
        // no longer rides draft-normalization to Active under the tool policy.
        assert_eq!(
            plan_mode_state(ctx.session_id).await,
            PlanModeState::PostInitEditing
        );

        // From post-init Editing: refused.
        let ctx2 = ToolExecutionContext::new(uuid::Uuid::new_v4());
        run(
            &tool,
            &ctx2,
            json!({ "operation": "init", "title": "Design" }),
        )
        .await;
        let (ok, msg) = run(
            &tool,
            &ctx2,
            json!({ "operation": "init", "file_path": file.to_str().unwrap() }),
        )
        .await;
        assert!(
            !ok,
            "import over post-init Editing must be refused, got: {msg}"
        );
    })
    .await;
}

#[tokio::test]
async fn tasks_require_non_empty_description_on_every_entry_point() {
    in_temp_home(async {
        let tool = PlanTool;

        // init with inline tasks: blank description refused (ToolError).
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let result = tool
            .execute(
                json!({
                    "operation": "init",
                    "title": "Checklist",
                    "mode": "checklist",
                    "tasks": [{ "title": "t1", "description": "  " }]
                }),
                &ctx,
            )
            .await;
        assert!(
            result.is_err(),
            "inline task with blank description must be refused"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Task description cannot be empty")
        );

        // add_tasks on a live checklist: omitted description refused.
        let ctx2 = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, out) = run(
            &tool,
            &ctx2,
            json!({
                "operation": "init",
                "title": "Checklist",
                "mode": "checklist",
                "tasks": [{ "title": "t1", "description": "real work" }]
            }),
        )
        .await;
        assert!(ok, "valid checklist init must succeed, got: {out}");
        approve_plan(&ctx2).await;
        let result = tool
            .execute(
                json!({
                    "operation": "add_tasks",
                    "tasks": [{ "title": "t2" }]
                }),
                &ctx2,
            )
            .await;
        assert!(
            result.is_err(),
            "add_tasks without description must be refused"
        );

        // import: blank task description refused, session stays NoPlan.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("blank-task-desc.json");
        std::fs::write(
            &file,
            serde_json::to_string(&json!({
                "title": "Imported",
                "description": "structured",
                "tasks": [
                    { "title": "t1", "description": "d1", "task_type": "edit" },
                    { "title": "t2", "description": "", "task_type": "edit" }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let ctx3 = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, msg) = run(
            &tool,
            &ctx3,
            json!({ "operation": "init", "file_path": file.to_str().unwrap() }),
        )
        .await;
        assert!(
            !ok,
            "import with blank task description must be refused, got: {msg}"
        );
        assert!(msg.contains("task 2"), "got: {msg}");
        assert_eq!(
            plan_mode_state(ctx3.session_id).await,
            PlanModeState::NoPlan
        );
    })
    .await;
}

#[tokio::test]
async fn import_requires_non_empty_root_title_and_description() {
    in_temp_home(async {
        let tool = PlanTool;
        let dir = tempfile::TempDir::new().unwrap();

        // Missing title entirely (serde defaults it to empty).
        let no_title = dir.path().join("no-title.json");
        std::fs::write(
            &no_title,
            serde_json::to_string(&json!({
                "description": "has description but no title",
                "tasks": [{ "title": "t1", "description": "d", "task_type": "edit" }]
            }))
            .unwrap(),
        )
        .unwrap();
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, msg) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "file_path": no_title.to_str().unwrap() }),
        )
        .await;
        assert!(!ok, "import without root title must be refused, got: {msg}");
        assert!(msg.contains("'title'"), "got: {msg}");
        assert_eq!(plan_mode_state(ctx.session_id).await, PlanModeState::NoPlan);

        // Whitespace-only description present in the JSON.
        let blank_desc = dir.path().join("blank-desc.json");
        std::fs::write(
            &blank_desc,
            serde_json::to_string(&json!({
                "title": "Has title",
                "description": "   ",
                "tasks": [{ "title": "t1", "description": "d", "task_type": "edit" }]
            }))
            .unwrap(),
        )
        .unwrap();
        let ctx2 = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let (ok, msg) = run(
            &tool,
            &ctx2,
            json!({ "operation": "init", "file_path": blank_desc.to_str().unwrap() }),
        )
        .await;
        assert!(
            !ok,
            "import with blank root description must be refused, got: {msg}"
        );
        assert!(msg.contains("'description'"), "got: {msg}");
        assert_eq!(
            plan_mode_state(ctx2.session_id).await,
            PlanModeState::NoPlan
        );
    })
    .await;
}

#[tokio::test]
async fn import_auto_assigns_order_and_defaults_complexity() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("minimal-plan.json");
        // The schema's recommended minimal shape: omit order (auto-assigned)
        // and complexity (defaults to 3). A provided out-of-range complexity
        // must clamp to the 1-5 scale.
        std::fs::write(
            &file,
            serde_json::to_string(&json!({
                "title": "Minimal",
                "description": "no order, no complexity",
                "tasks": [
                    { "title": "t1", "description": "d1", "task_type": "research" },
                    { "title": "t2", "description": "d2", "task_type": "edit" },
                    { "title": "t3", "description": "d3", "task_type": "test", "complexity": 9 }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let (ok, out) = run(
            &tool,
            &ctx,
            json!({ "operation": "init", "file_path": file.to_str().unwrap() }),
        )
        .await;
        assert!(ok, "minimal import must succeed, got: {out}");

        let plan = load_plan(ctx.session_id).await.unwrap();
        let orders: Vec<usize> = plan.tasks.iter().map(|t| t.order).collect();
        assert_eq!(
            orders,
            vec![1, 2, 3],
            "order must be auto-assigned 1-based from array position, got: {orders:?}"
        );
        assert_eq!(
            plan.tasks[0].complexity, 3,
            "omitted complexity must default to 3"
        );
        assert_eq!(
            plan.tasks[1].complexity, 3,
            "omitted complexity must default to 3"
        );
        assert_eq!(
            plan.tasks[2].complexity, 5,
            "out-of-range complexity must clamp to the 1-5 scale"
        );
    })
    .await;
}

#[tokio::test]
async fn midlist_insertion_insert_after_and_insert_before() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());

        // Initialize 3 tasks: t1, t2, t3
        run(
            &tool,
            &ctx,
            json!({
                "operation": "init",
                "title": "Checklist",
                "mode": "checklist",
                "tasks": [
                    { "title": "t1", "description": "d1" },
                    { "title": "t2", "description": "d2" },
                    { "title": "t3", "description": "d3" }
                ]
            }),
        )
        .await;
        approve_plan(&ctx).await;

        // Insert after task 1
        let (ok, out) = run(
            &tool,
            &ctx,
            json!({
                "operation": "add_task",
                "title": "t1.5",
                "description": "d1.5",
                "insert_after": 1
            }),
        )
        .await;
        assert!(ok, "insert_after 1 must succeed: {out}");

        let plan = load_plan(ctx.session_id).await.unwrap();
        let titles: Vec<&str> = plan.tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["t1", "t1.5", "t2", "t3"]);
        let orders: Vec<usize> = plan.tasks.iter().map(|t| t.order).collect();
        assert_eq!(orders, vec![1, 2, 3, 4]);

        // Insert before 'final'
        let (ok, out) = run(
            &tool,
            &ctx,
            json!({
                "operation": "add_tasks",
                "tasks": [
                    { "title": "t2.5", "description": "d2.5" }
                ],
                "insert_before": "final"
            }),
        )
        .await;
        assert!(ok, "insert_before 'final' must succeed: {out}");

        let plan = load_plan(ctx.session_id).await.unwrap();
        let titles: Vec<&str> = plan.tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["t1", "t1.5", "t2", "t2.5", "t3"]);
        let orders: Vec<usize> = plan.tasks.iter().map(|t| t.order).collect();
        assert_eq!(orders, vec![1, 2, 3, 4, 5]);

        // Start and complete task 1 & 2
        run(&tool, &ctx, json!({ "operation": "start" })).await;
        run(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        run(&tool, &ctx, json!({ "operation": "start" })).await;
        run(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 2, "action": "success" }),
        )
        .await;

        // Inserting before task 1 (completed) must fail
        let (ok, err) = run(
            &tool,
            &ctx,
            json!({
                "operation": "add_task",
                "title": "invalid",
                "description": "d",
                "insert_before": 1
            }),
        )
        .await;
        assert!(!ok, "inserting before completed task 1 must fail");
        assert!(err.contains("already completed/skipped"), "got: {err}");

        // Specifying both insert_before and insert_after must fail
        let (ok, err) = run(
            &tool,
            &ctx,
            json!({
                "operation": "add_task",
                "title": "invalid",
                "description": "d",
                "insert_before": 3,
                "insert_after": 2
            }),
        )
        .await;
        assert!(!ok, "specifying both must fail");
        assert!(err.contains("both"), "got: {err}");
    })
    .await;
}

#[tokio::test]
async fn show_plan_windowing_and_full_flag() {
    in_temp_home(async {
        let tool = PlanTool;
        let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());

        // Create 15 tasks
        let mut tasks = Vec::new();
        for i in 1..=15 {
            tasks.push(json!({ "title": format!("task {i}"), "description": format!("desc {i}") }));
        }

        run(
            &tool,
            &ctx,
            json!({
                "operation": "init",
                "title": "Long Plan",
                "mode": "checklist",
                "tasks": tasks
            }),
        )
        .await;
        approve_plan(&ctx).await;

        // Default show_plan applies accordion windowing
        let (ok, out) = run(&tool, &ctx, json!({ "operation": "show_plan" })).await;
        assert!(ok);
        assert!(
            out.contains("collapsed"),
            "output must be windowed/collapsed: {out}"
        );

        // show_plan with full: true renders everything
        let (ok, out_full) = run(
            &tool,
            &ctx,
            json!({ "operation": "show_plan", "full": true }),
        )
        .await;
        assert!(ok);
        assert!(
            !out_full.contains("collapsed"),
            "full output must not contain collapsed summary: {out_full}"
        );
        for i in 1..=15 {
            assert!(out_full.contains(&format!("task {i}")));
        }
    })
    .await;
}
