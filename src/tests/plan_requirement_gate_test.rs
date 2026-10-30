//! Mechanical plan requirement (`brain::tools::plan_gate::check_plan_requirement`).
//!
//! Prose in the brain files asked the agent to use the plan tool for
//! research, audits and multi-file fixes. It was skipped often enough that
//! the requirement is now mechanical: past a configured number of tool calls
//! in one turn, a session with no plan is refused until it creates one.
//!
//! These tests pin the decision table, including every escape hatch, because
//! each one is a way the gate could deadlock the agent or strand a run:
//!
//! - `plan` and `tool_search` always pass (the unblock path)
//! - sub-agent and cron contexts are exempt
//! - `0` disables the gate
//! - any existing plan state satisfies it
//!
//! A structural pin then asserts the registry actually calls the gate, since
//! a correct decision table that nothing invokes enforces nothing.

use crate::brain::tools::plan_gate::check_plan_requirement;
use crate::config::profile::{home_for_profile, with_profile_home_async};
use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskType};
use crate::utils::plan_files::{create_design_md, save_plan};
use uuid::Uuid;

const THRESHOLD: usize = 12;

async fn in_temp_home<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let profile = format!("plan-requirement-test-{}", Uuid::new_v4());
    let out = with_profile_home_async(Some(&profile), f).await;
    let home = home_for_profile(Some(&profile));
    let _ = std::fs::remove_dir_all(&home);
    out
}

async fn make_active(sid: Uuid) {
    let mut plan = PlanDocument::new(sid, "Exec".to_string());
    let mut task = PlanTask::new(1, "t1".to_string(), "d".to_string(), TaskType::Edit);
    task.start();
    plan.add_task(task);
    plan.status = PlanStatus::Active;
    save_plan(&plan).await.unwrap();
}

#[tokio::test]
async fn below_the_threshold_allows_without_a_plan() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        let decision = check_plan_requirement(sid, "read_file", 3, THRESHOLD, false).await;
        assert!(
            decision.is_allowed(),
            "3 calls is still one step; the gate must not fire"
        );
    })
    .await;
}

#[tokio::test]
async fn exactly_at_the_threshold_still_allows() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        // The threshold is inclusive: the Nth call runs, the N+1th is refused.
        let decision = check_plan_requirement(sid, "bash", THRESHOLD, THRESHOLD, false).await;
        assert!(
            decision.is_allowed(),
            "call {THRESHOLD} is the last free one"
        );
    })
    .await;
}

#[tokio::test]
async fn over_the_threshold_without_a_plan_denies_and_names_the_count() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        let decision = check_plan_requirement(sid, "bash", THRESHOLD + 1, THRESHOLD, false).await;
        let reason = match decision {
            crate::brain::tools::plan_gate::GateDecision::Deny(reason) => reason,
            other => panic!("expected a deny, got {other:?}"),
        };
        assert!(
            reason.contains(&(THRESHOLD + 1).to_string()),
            "the denial must name the turn's call count: {reason}"
        );
        assert!(
            reason.contains("plan") && reason.contains("init"),
            "the denial must name the call that unblocks it: {reason}"
        );
    })
    .await;
}

#[tokio::test]
async fn the_plan_tool_always_passes_at_any_count() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        let decision = check_plan_requirement(sid, "plan", 500, THRESHOLD, false).await;
        assert!(
            decision.is_allowed(),
            "gating the unblock path would deadlock the gate"
        );
    })
    .await;
}

#[tokio::test]
async fn tool_search_always_passes_at_any_count() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        // With lazy tools on, the agent may need to discover `plan` first.
        let decision = check_plan_requirement(sid, "tool_search", 500, THRESHOLD, false).await;
        assert!(decision.is_allowed());
    })
    .await;
}

#[tokio::test]
async fn an_exempt_context_never_denies() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        // Sub-agent sessions and cron runs execute work someone else planned.
        let decision = check_plan_requirement(sid, "bash", 500, THRESHOLD, true).await;
        assert!(
            decision.is_allowed(),
            "an exempt surface must never be refused"
        );
    })
    .await;
}

#[tokio::test]
async fn a_threshold_of_zero_disables_the_gate() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        let decision = check_plan_requirement(sid, "bash", 500, 0, false).await;
        assert!(decision.is_allowed(), "0 is the documented off switch");
    })
    .await;
}

#[tokio::test]
async fn an_active_plan_allows_work() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        make_active(sid).await;
        let decision = check_plan_requirement(sid, "bash", THRESHOLD + 20, THRESHOLD, false).await;
        assert!(
            decision.is_allowed(),
            "a live checklist IS the requirement being met"
        );
    })
    .await;
}

#[tokio::test]
async fn a_design_being_drafted_allows_work() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        // A design-track plan (Editing) counts: the agent is doing exactly
        // what the gate asks, even though the checklist is not seeded yet.
        let plan = PlanDocument::new(sid, "Design".to_string());
        save_plan(&plan).await.unwrap();
        create_design_md(sid, "Design").await.unwrap();
        let decision =
            check_plan_requirement(sid, "read_file", THRESHOLD + 1, THRESHOLD, false).await;
        assert!(decision.is_allowed());
    })
    .await;
}

/// The decision table is only worth anything if the registry consults it.
///
/// Pinned structurally because `ToolRegistry::execute` needs a live registry,
/// provider and session to exercise end to end; what can silently break is the
/// CALL disappearing during a refactor, which this catches.
#[test]
fn the_registry_consults_the_plan_requirement() {
    let registry = include_str!("../brain/tools/registry.rs");
    assert!(
        registry.contains("check_plan_requirement("),
        "registry.rs must call check_plan_requirement; a gate nothing invokes enforces nothing"
    );
    assert!(
        registry.contains("context.turn_tool_calls"),
        "the registry must pass the turn's call count, not a constant"
    );
    assert!(
        registry.contains("self.plan_gate_after_tool_calls"),
        "the registry must pass the configured threshold"
    );
}

/// The count has to come from the turn, and it has to be stamped per call.
#[test]
fn the_tool_loop_stamps_the_turn_call_count() {
    let tool_loop = include_str!("../brain/agent/service/tool_loop.rs");
    assert!(
        tool_loop.contains("tool_context.turn_tool_calls = turn_tool_input.len();"),
        "tool_loop.rs must stamp the running turn count before dispatch"
    );
    assert!(
        tool_loop.contains("tool_context.plan_gate_exempt = self.plan_gate_exempt;"),
        "tool_loop.rs must carry the exemption into the execution context"
    );
}
