//! Tests for the shared plan-mode command state machine
//! (`utils::plan_mode`): the Approve validator (golden fixture), the two
//! idle approve paths (first approve, empty-tasks seed retry), the
//! deterministic refusals, `/plan`, `/discard`, `/show-plan`, and the
//! seed window used by the gate and the Building checklist… chrome.

use crate::config::profile::{home_for_profile, with_profile_home_async};
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tui::plan::{ApprovalSource, PlanDocument, PlanStatus, PlanTask, TaskType};
use crate::utils::plan_files::{
    self, PlanModeState, create_design_md, plan_md_path, plan_mode_state, save_plan,
    set_pre_init_editing,
};
use crate::utils::plan_mode::{
    ApproveOutcome, CHECKLIST_APPROVED_RECEIPT, DESIGN_APPROVED_RECEIPT, discard, enter_plan_mode,
    in_seed_window, seed_prompt, show_plan, try_approve, validate_for_approve,
};
use uuid::Uuid;

/// The golden session-plan fixture from the plan-mode UX ADR.
const GOLDEN_MD: &str = "# Add session plan Approve gate\n\n\
## Context\n\
- **Problem:** Plan mode has no structured design doc before execution.\n\
- **Target state:** Users Approve a `.md` plan, then checklist runs automatically.\n\
- **Intent:** Ship Track A for TUI and Telegram in v1.\n\n\
## Implementation steps\n\
1. Add Approve validator — implement Rust scan in Phase 6 harness.\n\
2. Wire seed turn — synthetic user message, add_tasks, start.\n\
   - Done when: empty tasks triggers idle retry via /execute (forbidden while turn running).\n";

#[test]
fn seed_prompt_carries_the_done_when_contract() {
    let p = seed_prompt(std::path::Path::new("/tmp/plan.md"));
    assert!(p.contains("Map 'Done when:' bullets to acceptance_criteria when present"));
    assert!(
        p.contains("derive one checkable criterion"),
        "steps without a Done when bullet must not silently seed empty criteria"
    );
    assert!(p.contains("runnable command and their expected outcome"));
}

async fn in_temp_home<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let profile = format!("plan-mode-test-{}", Uuid::new_v4());
    let out = with_profile_home_async(Some(&profile), f).await;
    let home = home_for_profile(Some(&profile));
    let _ = std::fs::remove_dir_all(&home);
    out
}

async fn make_post_init(sid: Uuid, md_body: &str) {
    let plan = PlanDocument::new(sid, "Golden".to_string());
    save_plan(&plan).await.unwrap();
    create_design_md(sid, "Golden").await.unwrap();
    std::fs::write(plan_md_path(sid).await, md_body).unwrap();
}

#[test]
fn validator_accepts_golden_fixture_and_rejects_gaps() {
    assert!(validate_for_approve(GOLDEN_MD).is_ok());
    assert!(validate_for_approve("").is_err(), "empty .md must refuse");
    assert!(
        validate_for_approve("# T\n\n## Context\n- **Problem:** x\n").is_err(),
        "missing labels and steps must refuse"
    );
    // Whitespace-only after a label counts as blank.
    let blank_intent = GOLDEN_MD.replace(
        "- **Intent:** Ship Track A for TUI and Telegram in v1.",
        "- **Intent:**   ",
    );
    assert!(validate_for_approve(&blank_intent).is_err());
    // Strictness locked: any non-empty text passes, even "TBD".
    let tbd = GOLDEN_MD.replace(
        "- **Intent:** Ship Track A for TUI and Telegram in v1.",
        "- **Intent:** TBD",
    );
    assert!(validate_for_approve(&tbd).is_ok());
}

#[tokio::test]
async fn first_approve_transitions_and_returns_seed_turn() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        make_post_init(sid, GOLDEN_MD).await;

        match try_approve(sid, ApprovalSource::User).await {
            ApproveOutcome::SeedTurn { prompt, display } => {
                assert!(prompt.contains("PLAN APPROVED"));
                // #2756: the model reads the prompt, the user reads the
                // receipt. The approval turn must carry the receipt so the
                // bubble and the stored row never show the scaffolding.
                // Zero tasks + a design `.md`: this is the DESIGN branch, so
                // the receipt promises the seed, not a running checklist.
                assert_eq!(display, DESIGN_APPROVED_RECEIPT);
                assert!(prompt.contains("add_tasks"));
                assert!(prompt.contains(&plan_md_path(sid).await.display().to_string()));
                assert!(prompt.contains("Do NOT edit project files"));
            }
            other => panic!("expected SeedTurn, got {other:?}"),
        }
        let plan = plan_files::load_plan(sid).await.unwrap();
        assert_eq!(plan.status, PlanStatus::Active);
        assert!(
            plan.approved_at.is_some(),
            "user Approve stamps approved_at"
        );
        assert!(
            in_seed_window(sid).await,
            "post-approve, pre-seed = seed window"
        );
    })
    .await;
}

#[tokio::test]
async fn checklist_approve_transitions_without_md_validation() {
    // #573 audit-plan shape: init mode="checklist" authored tasks during
    // Editing, held there by an empty placeholder scaffold `.md`. Approve must
    // succeed on the checklist — the tasks are the deliverable — instead of
    // refusing because the design template is unfilled.
    in_temp_home(async {
        let sid = Uuid::new_v4();
        let mut plan = PlanDocument::new(sid, "Audit".to_string());
        for i in 1..=3 {
            plan.add_task(PlanTask::new(
                i,
                format!("task {i}"),
                "d".to_string(),
                TaskType::Edit,
            ));
        }
        plan.status = PlanStatus::Editing;
        // No `.md` at all: since #1145 checklist init no longer writes the
        // placeholder scaffold — the approval-queue marker is the durable
        // `pending_approval` flag, and approve must still succeed on the
        // checklist, the tasks being the deliverable (#573).
        plan.pending_approval = true;
        save_plan(&plan).await.unwrap();

        assert_eq!(plan_mode_state(sid).await, PlanModeState::PostInitEditing);

        match try_approve(sid, ApprovalSource::User).await {
            ApproveOutcome::SeedTurn { prompt, display } => {
                assert!(prompt.contains("PLAN APPROVED"));
                // #2756: the model reads the prompt, the user reads the
                // receipt. The approval turn must carry the receipt so the
                // bubble and the stored row never show the scaffolding.
                assert_eq!(display, CHECKLIST_APPROVED_RECEIPT);
                assert!(
                    prompt.contains("already defined"),
                    "checklist approval dispatches the start prompt, not the .md seed: {prompt}"
                );
            }
            other => panic!("checklist approve should transition, got {other:?}"),
        }
        let plan = plan_files::load_plan(sid).await.unwrap();
        assert_eq!(plan.status, PlanStatus::Active);
        assert!(plan.approved_at.is_some());
        assert_eq!(plan.tasks.len(), 3, "tasks are preserved, not reseeded");
    })
    .await;
}

#[tokio::test]
async fn approve_refuses_invalid_template_without_transition() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        make_post_init(sid, "just prose, no template").await;

        match try_approve(sid, ApprovalSource::User).await {
            ApproveOutcome::Refused(msg) => {
                assert!(
                    msg.contains("Not ready") || msg.contains("not ready"),
                    "got: {msg}"
                );
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert_eq!(plan_mode_state(sid).await, PlanModeState::PostInitEditing);
        assert!(
            plan_files::load_plan(sid)
                .await
                .unwrap()
                .approved_at
                .is_none()
        );
    })
    .await;
}

#[tokio::test]
async fn approve_refuses_no_plan_pre_init_and_running_checklist() {
    in_temp_home(async {
        // NoPlan.
        let sid = Uuid::new_v4();
        assert!(matches!(
            try_approve(sid, ApprovalSource::User).await,
            ApproveOutcome::Refused(_)
        ));

        // Pre-init.
        set_pre_init_editing(sid).await.unwrap();
        assert!(matches!(
            try_approve(sid, ApprovalSource::User).await,
            ApproveOutcome::Refused(_)
        ));
        plan_files::discard_plan(sid).await;

        // Active with a running checklist: /execute is not applicable.
        let mut plan = PlanDocument::new(sid, "Run".to_string());
        let mut t = PlanTask::new(1, "t".to_string(), "d".to_string(), TaskType::Edit);
        t.start();
        plan.add_task(t);
        plan.status = PlanStatus::Active;
        save_plan(&plan).await.unwrap();
        match try_approve(sid, ApprovalSource::User).await {
            ApproveOutcome::Refused(msg) => assert!(msg.contains("already Active"), "got: {msg}"),
            other => panic!("expected Refused, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn empty_tasks_seed_retry_redispatches_without_second_approve() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        make_post_init(sid, GOLDEN_MD).await;

        // First approve.
        let first_stamp = match try_approve(sid, ApprovalSource::User).await {
            ApproveOutcome::SeedTurn { .. } => plan_files::load_plan(sid)
                .await
                .unwrap()
                .approved_at
                .unwrap(),
            other => panic!("expected SeedTurn, got {other:?}"),
        };

        // Seed failed (tasks still empty): idle retry re-dispatches the
        // seed turn with no status transition and no re-stamp.
        match try_approve(sid, ApprovalSource::User).await {
            ApproveOutcome::SeedTurn { prompt, display } => {
                assert!(prompt.contains("add_tasks"));
                // #2756: the design-track retry carries the design receipt.
                assert_eq!(display, DESIGN_APPROVED_RECEIPT);
            }
            other => panic!("expected retry SeedTurn, got {other:?}"),
        }
        let plan = plan_files::load_plan(sid).await.unwrap();
        assert_eq!(plan.status, PlanStatus::Active);
        assert_eq!(plan.approved_at.unwrap(), first_stamp, "no second approve");
    })
    .await;
}

#[tokio::test]
async fn plan_command_sets_durable_pre_init_and_discard_clears() {
    in_temp_home(async {
        let db = Database::connect_in_memory().await.unwrap();
        db.run_migrations().await.unwrap();
        let ctx = ServiceContext::new(db.pool().clone());
        let sid = Uuid::new_v4();
        let reply = enter_plan_mode(sid).await;
        assert!(reply.contains("Plan mode on"), "got: {reply}");
        assert_eq!(plan_mode_state(sid).await, PlanModeState::PreInitEditing);

        // /plan over a live design plan refuses.
        plan_files::discard_plan(sid).await;
        make_post_init(sid, GOLDEN_MD).await;
        let reply = enter_plan_mode(sid).await;
        assert!(reply.contains("already"), "got: {reply}");

        // /discard deletes both artifacts.
        let reply = discard(sid, &ctx).await;
        assert!(reply.contains("discarded"), "got: {reply}");
        assert_eq!(plan_mode_state(sid).await, PlanModeState::NoPlan);
        assert!(!plan_md_path(sid).await.exists(), ".md must be deleted");

        // Pre-init discard clears the flag.
        set_pre_init_editing(sid).await.unwrap();
        let reply = discard(sid, &ctx).await;
        assert!(reply.contains("pre-init"), "got: {reply}");
        assert_eq!(plan_mode_state(sid).await, PlanModeState::NoPlan);
    })
    .await;
}

#[tokio::test]
async fn discard_clears_the_plan_goal() {
    in_temp_home(async {
        let db = Database::connect_in_memory().await.unwrap();
        db.run_migrations().await.unwrap();
        let ctx = ServiceContext::new(db.pool().clone());
        let sid = Uuid::new_v4();

        // Post-init Editing plan with a goal set, as a started task with
        // acceptance criteria would leave behind.
        make_post_init(sid, GOLDEN_MD).await;
        crate::brain::goal::GoalManager::new(ctx.clone())
            .set_goal(
                sid,
                "Task 1: t1. Done when: x".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(
            crate::brain::goal::GoalManager::new(ctx.clone())
                .get_goal(sid)
                .await
                .unwrap()
                .is_some(),
            "goal must be set before discard"
        );

        let reply = discard(sid, &ctx).await;
        assert!(reply.contains("discarded"), "got: {reply}");
        assert_eq!(plan_mode_state(sid).await, PlanModeState::NoPlan);
        assert!(
            crate::brain::goal::GoalManager::new(ctx.clone())
                .get_goal(sid)
                .await
                .unwrap()
                .is_none(),
            "discard must clear the goal so no stale chrome lingers"
        );
    })
    .await;
}

#[tokio::test]
async fn show_plan_reports_each_state() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        assert!(show_plan(sid).await.contains("No active plan"));

        set_pre_init_editing(sid).await.unwrap();
        assert!(show_plan(sid).await.contains("pre-init"));
        plan_files::discard_plan(sid).await;

        make_post_init(sid, GOLDEN_MD).await;
        let s = show_plan(sid).await;
        assert!(
            s.contains("Editing") && s.contains("Ready to approve"),
            "got: {s}"
        );
        // #540: the design prose itself is surfaced, not just the path, so a
        // chat reviewer with no file access can read the document under review.
        assert!(
            s.contains("Implementation steps")
                && s.contains("Plan mode has no structured design doc"),
            "prose body not surfaced: {s}"
        );

        // Approved but seed not finished: retry guidance.
        assert!(matches!(
            try_approve(sid, ApprovalSource::User).await,
            ApproveOutcome::SeedTurn { .. }
        ));
        let s = show_plan(sid).await;
        assert!(
            s.contains("seed did not finish") || s.contains("empty"),
            "got: {s}"
        );
    })
    .await;
}

#[tokio::test]
async fn show_plan_truncates_long_prose() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        // A valid-but-huge design doc: the golden fixture padded well past the
        // prose cap. #540 must surface the head without blowing Telegram's
        // 4096-char message limit.
        let big = format!(
            "{GOLDEN_MD}{}",
            "\nfiller line to pad the body.".repeat(300)
        );
        make_post_init(sid, &big).await;
        let s = show_plan(sid).await;
        assert!(s.len() < 3600, "prose not truncated: {} chars", s.len());
        assert!(
            s.contains("Add session plan Approve gate"),
            "document head missing: {s}"
        );
    })
    .await;
}

#[tokio::test]
async fn seed_window_closes_when_a_task_starts() {
    in_temp_home(async {
        let sid = Uuid::new_v4();
        make_post_init(sid, GOLDEN_MD).await;
        assert!(matches!(
            try_approve(sid, ApprovalSource::User).await,
            ApproveOutcome::SeedTurn { .. }
        ));
        assert!(in_seed_window(sid).await);

        // add_tasks alone (all Pending) keeps the window open.
        let mut plan = plan_files::load_plan(sid).await.unwrap();
        plan.add_task(PlanTask::new(
            1,
            "t1".to_string(),
            "d".to_string(),
            TaskType::Edit,
        ));
        save_plan(&plan).await.unwrap();
        assert!(
            in_seed_window(sid).await,
            "partial seed stays in the window"
        );

        // start closes it.
        let mut plan = plan_files::load_plan(sid).await.unwrap();
        plan.tasks[0].start();
        save_plan(&plan).await.unwrap();
        assert!(!in_seed_window(sid).await);
    })
    .await;
}

#[tokio::test]
async fn editing_reminder_and_plan_state_block_follow_state() {
    use crate::brain::agent::service::{format_editing_reminder, plan_state_block};
    in_temp_home(async {
        let sid = Uuid::new_v4();
        // NoPlan: no summary block.
        assert!(plan_state_block(sid).await.is_none());

        // Pre-init: reminder and block teach init mode='design', never start.
        set_pre_init_editing(sid).await.unwrap();
        let block = plan_state_block(sid).await.unwrap();
        assert!(block.contains("Pre-init") && block.contains("mode='design'"));
        let reminder = format_editing_reminder(None);
        assert!(reminder.contains("plan init") && !reminder.contains("operation=\"start\""));
        plan_files::discard_plan(sid).await;

        // Post-init: both carry the absolute .md path and forbid start.
        make_post_init(sid, GOLDEN_MD).await;
        let md = plan_md_path(sid).await.display().to_string();
        let block = plan_state_block(sid).await.unwrap();
        assert!(block.contains(&md) && block.contains("No `plan start`"));
        let reminder = format_editing_reminder(Some(plan_md_path(sid).await));
        assert!(reminder.contains(&md));
        assert!(reminder.contains("No pasting plan in chat"));

        // Active: block names the checklist and says plan start.
        assert!(matches!(
            try_approve(sid, ApprovalSource::User).await,
            ApproveOutcome::SeedTurn { .. }
        ));
        let block = plan_state_block(sid).await.unwrap();
        assert!(block.contains("Active checklist") && block.contains("plan start"));
    })
    .await;
}

#[tokio::test]
async fn plan_recovery_derives_from_state() {
    use crate::brain::agent::service::compaction_prompts::PlanRecovery;
    in_temp_home(async {
        let sid = Uuid::new_v4();
        assert_eq!(PlanRecovery::for_session(sid).await, PlanRecovery::NoPlan);
        set_pre_init_editing(sid).await.unwrap();
        assert_eq!(PlanRecovery::for_session(sid).await, PlanRecovery::Editing);
        plan_files::discard_plan(sid).await;
        make_post_init(sid, GOLDEN_MD).await;
        assert_eq!(PlanRecovery::for_session(sid).await, PlanRecovery::Editing);
        assert!(matches!(
            try_approve(sid, ApprovalSource::User).await,
            ApproveOutcome::SeedTurn { .. }
        ));
        assert_eq!(PlanRecovery::for_session(sid).await, PlanRecovery::Active);
    })
    .await;
}
