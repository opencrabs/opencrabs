//! Tests for per-task spec checks (#1904).
//!
//! Acceptance criteria were prose nobody ran: `verify_task_completion`
//! resolved its commands from the task *type*, so a criterion like
//! "cargo test --lib foo reports 4 passed" never executed. A spec check is
//! the runnable half of that criterion, extracted once at `start`, frozen
//! with a sha256, and run at `complete`.
//!
//! Extraction runs first here because it carries the risk: a wrong command
//! locked at start is executed at complete, which would fail a task that was
//! done right. Every refusal below is deliberate.

use crate::brain::tools::plan_tool::{PlanTool, lock_spec_check_if_absent, run_locked_spec_check};
use crate::brain::tools::{Tool, ToolExecutionContext};
use crate::config::profile::{home_for_profile, with_profile_home_async};
use crate::tui::plan::{
    ApprovalSource, PlanDocument, PlanTask, SpecCheck, SpecCheckSource, TaskStatus, TaskType,
    extract_check_commands, is_acceptable_check_command, is_verifiable, spec_check_hash,
};
use crate::utils::plan_files::{load_plan, save_plan};
use serde_json::json;
use std::fs;
use uuid::Uuid;

fn criteria(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ── extraction: the two shapes that lock a command ───────────────────

#[test]
fn plan_spec_check_extracts_a_backticked_command() {
    let commands = extract_check_commands(&criteria(&["`cargo fmt --all -- --check` exits 0"]));
    assert_eq!(commands, vec!["cargo fmt --all -- --check".to_string()]);
}

#[test]
fn plan_spec_check_extracts_a_runner_up_to_its_expectation_stem() {
    let commands = extract_check_commands(&criteria(&[
        "cargo test --lib pricing reports 4 passed, 0 failed",
    ]));
    assert_eq!(commands, vec!["cargo test --lib pricing".to_string()]);
}

#[test]
fn plan_spec_check_skips_a_backticked_noun_and_takes_the_real_command() {
    let commands = extract_check_commands(&criteria(&[
        "the `slugify` helper is wired in and `cargo test --lib slugify` reports 0 failed",
    ]));
    assert_eq!(commands, vec!["cargo test --lib slugify".to_string()]);
}

#[test]
fn plan_spec_check_keeps_a_runner_whose_name_ends_in_a_digit() {
    let commands = extract_check_commands(&criteria(&[
        "python3 -m unittest discover -s tests -v reports 12 passed",
    ]));
    assert_eq!(
        commands,
        vec!["python3 -m unittest discover -s tests -v".to_string()]
    );
}

#[test]
fn plan_spec_check_deduplicates_a_command_named_twice() {
    let commands = extract_check_commands(&criteria(&[
        "cargo test --lib pricing reports 0 failed",
        "cargo test --lib pricing exits 0",
    ]));
    assert_eq!(commands, vec!["cargo test --lib pricing".to_string()]);
}

#[test]
fn plan_spec_check_keeps_criteria_order() {
    let commands = extract_check_commands(&criteria(&[
        "cargo fmt --all -- --check exits 0",
        "cargo test --lib pricing reports 0 failed",
    ]));
    assert_eq!(
        commands,
        vec![
            "cargo fmt --all -- --check".to_string(),
            "cargo test --lib pricing".to_string()
        ]
    );
}

// ── extraction: every refusal below is on purpose ────────────────────

#[test]
fn plan_spec_check_locks_nothing_from_a_prose_criterion() {
    let commands = extract_check_commands(&criteria(&["the documentation reads well"]));
    assert!(
        commands.is_empty(),
        "prose is not a check, got {commands:?}"
    );
}

#[test]
fn plan_spec_check_locks_nothing_when_the_tail_is_prose() {
    // A runner appears, but what follows it is a sentence, not a command
    // line. Slicing at the trailing stem would lock "cargo test ... then
    // check the output is", which is not runnable at all.
    let commands = extract_check_commands(&criteria(&[
        "run cargo test --all-features then check the output is clean",
    ]));
    assert!(commands.is_empty(), "prose tail, got {commands:?}");
}

#[test]
fn plan_spec_check_locks_nothing_from_a_bare_runner() {
    // `cargo` alone prints help and exits 0, which the gate would read as a
    // pass on any task.
    let commands = extract_check_commands(&criteria(&["`cargo` exits 0"]));
    assert!(commands.is_empty(), "bare runner, got {commands:?}");
}

#[test]
fn plan_spec_check_refuses_a_compound_command() {
    let commands = extract_check_commands(&criteria(&["`cargo test && git push` exits 0"]));
    assert!(commands.is_empty(), "compound command, got {commands:?}");
}

#[test]
fn plan_spec_check_refuses_a_piped_command() {
    let commands = extract_check_commands(&criteria(&[
        "cargo test --lib pricing | tail -5 shows 0 failed",
    ]));
    assert!(commands.is_empty(), "piped command, got {commands:?}");
}

#[test]
fn plan_spec_check_does_not_read_a_runner_out_of_a_longer_word() {
    // "digit" ends in "git": the pattern is matched at a word boundary so a
    // noun cannot drag a command out of the sentence.
    assert!(command_start("digit total shows 4 entries").is_none());
    assert!(command_start("the github page shows 0 open issues").is_none());
}

#[test]
fn plan_spec_check_ignores_a_criterion_with_no_expectation() {
    assert!(!is_verifiable("cargo build --release"));
    assert!(extract_check_commands(&criteria(&["cargo build --release"])).is_empty());
}

// ── the lock itself ─────────────────────────────────────────────────

#[test]
fn plan_spec_check_hash_separates_command_boundaries() {
    let joined_left = vec!["cargo test --lib a b".to_string(), "c".to_string()];
    let joined_right = vec!["cargo test --lib a".to_string(), "b c".to_string()];
    assert_ne!(
        spec_check_hash(&joined_left),
        spec_check_hash(&joined_right),
        "where the boundary falls must change the hash"
    );
}

#[test]
fn plan_spec_check_lock_matches_only_the_commands_it_froze() {
    let locked = SpecCheck::lock(vec!["cargo test --lib pricing".to_string()]);
    assert!(locked.matches(&["cargo test --lib pricing".to_string()]));
    assert!(!locked.matches(&["cargo test --lib pricing --".to_string()]));
    assert!(!locked.matches(&[]));
}

#[test]
fn plan_spec_check_with_an_empty_hash_never_matches() {
    // A hand-edited plan file could carry the struct with no hash. An empty
    // hash must not read as "locked and unchanged".
    let forged = SpecCheck {
        commands: vec!["cargo test".to_string()],
        sha256: String::new(),
        source: SpecCheckSource::Criteria,
        confirmed: false,
    };
    assert!(!forged.matches(&["cargo test".to_string()]));
}

#[test]
fn plan_spec_check_round_trips_on_the_task() {
    let mut task = PlanTask::new(1, "t".to_string(), "d".to_string(), TaskType::Test);
    task.spec_check = Some(SpecCheck::lock(vec![
        "cargo test --lib pricing".to_string(),
    ]));
    let json = serde_json::to_string(&task).unwrap();
    let back: PlanTask = serde_json::from_str(&json).unwrap();
    let restored = back.spec_check.expect("spec check survived serde");
    assert_eq!(
        restored.commands,
        vec!["cargo test --lib pricing".to_string()]
    );
    assert_eq!(restored.sha256, spec_check_hash(&restored.commands));
}

#[test]
fn plan_spec_check_is_absent_from_json_when_not_locked() {
    let task = PlanTask::new(1, "t".to_string(), "d".to_string(), TaskType::Test);
    let json = serde_json::to_string(&task).unwrap();
    assert!(
        !json.contains("spec_check"),
        "an unlocked task must not carry the key: {json}"
    );
    let back: PlanTask = serde_json::from_str(&json).unwrap();
    assert!(back.spec_check.is_none());
}

// ── provenance: who may block a completion (#2063) ───────────────────
//
// A criteria-derived check is what the plan approver read, so it may reject.
// A generated one is a model's guess, so it advises until a human confirms
// it. These pin that split, and pin that confirming does not void the lock.

#[test]
fn plan_spec_check_defaults_to_criteria_for_a_pre_2063_lock() {
    // A plan written before #2063 carries neither key. Reading it must give
    // the behaviour it already had, not an error and not a downgrade.
    let legacy: SpecCheck =
        serde_json::from_str(r#"{"commands":["ls ."],"sha256":"deadbeef"}"#).unwrap();
    assert_eq!(legacy.source, SpecCheckSource::Criteria);
    assert!(!legacy.confirmed);
    assert!(legacy.may_block(), "a criteria check kept its teeth");
}

#[test]
fn plan_spec_check_criteria_check_may_block_without_confirmation() {
    let locked = SpecCheck::lock(vec!["ls .".to_string()]);
    assert_eq!(locked.source, SpecCheckSource::Criteria);
    assert!(!locked.confirmed, "criteria checks need no confirmation");
    assert!(locked.may_block());
}

#[test]
fn plan_spec_check_generated_check_is_advisory_until_confirmed() {
    let mut generated = SpecCheck::lock_generated(vec!["ls .".to_string()]);
    assert_eq!(generated.source, SpecCheckSource::Generated);
    assert!(!generated.confirmed);
    assert!(
        !generated.may_block(),
        "a model-written check must never reject a completion on its own"
    );

    generated.confirmed = true;
    assert!(
        generated.may_block(),
        "confirmation is what grants it teeth"
    );
}

#[test]
fn plan_spec_check_confirming_a_generated_check_does_not_move_its_hash() {
    let commands = vec!["cargo test --lib pricing".to_string()];
    let mut generated = SpecCheck::lock_generated(commands.clone());
    let before = generated.sha256.clone();

    generated.confirmed = true;

    assert_eq!(generated.sha256, before, "confirmation must not rehash");
    assert!(
        generated.matches(&commands),
        "a confirmation that voids the lock it just approved is worse than no lock"
    );
}

#[test]
fn plan_spec_check_generated_and_criteria_hash_the_same_commands_alike() {
    // Provenance is not part of the hash. Same commands, same lock, whichever
    // path wrote them, so the run-at-complete comparison stays a comparison of
    // commands and nothing else.
    let commands = vec!["ls .".to_string()];
    assert_eq!(
        SpecCheck::lock(commands.clone()).sha256,
        SpecCheck::lock_generated(commands).sha256
    );
}

#[test]
fn plan_spec_check_generated_commands_pass_the_same_refusal_as_criteria_ones() {
    // The gate runs the text, so the rule has to hold whoever wrote it.
    assert!(is_acceptable_check_command("cargo test --lib pricing"));
    assert!(is_acceptable_check_command("ls ."));
    assert!(is_acceptable_check_command("pytest"));
    assert!(!is_acceptable_check_command("cargo test && git push"));
    assert!(!is_acceptable_check_command("cargo test | tail -5"));
    assert!(!is_acceptable_check_command("cargo"), "bare runner");
    assert!(!is_acceptable_check_command("the documentation reads well"));
    assert!(!is_acceptable_check_command(""));
}

/// Test-local wrapper: the module keeps `command_start_index` private, so
/// this exercises it through the public extractor instead.
fn command_start(criterion: &str) -> Option<String> {
    extract_check_commands(&criteria(&[criterion]))
        .into_iter()
        .next()
}

// ── locking at start, running at complete ────────────────────────────
//
// The lock is the whole point of the feature: a check re-derived at
// completion time would be a check the task can rewrite for itself.

/// A project `ralph_loop.toml`, so the policy comes from this tempdir and never
/// from the machine-wide safety copy (a test that reads the user's real config
/// passes or fails with their setup).
fn policy_dir(name: &str, spec_checks: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("spec1904_{name}_"))
        .tempdir()
        .expect("tempdir");
    fs::write(
        dir.path().join("ralph_loop.toml"),
        format!("[verification]\nenabled = true\nspec_checks = \"{spec_checks}\"\n"),
    )
    .expect("write config");
    dir
}

fn plan_with_one_task(criteria_items: Vec<String>) -> PlanDocument {
    let mut plan = PlanDocument::new(Uuid::new_v4(), "spec check plan".to_string());
    let mut task = PlanTask::new(1, "t".to_string(), "d".to_string(), TaskType::Edit);
    task.acceptance_criteria = criteria_items;
    plan.add_task(task);
    plan
}

/// `ls .` is the pass case on purpose: it exists on every machine this runs
/// on, exits 0, and prints no `test result:` line, so it cannot trip the
/// vacuous-pass guard by accident.
fn passing_criteria() -> Vec<String> {
    criteria(&["`ls .` exits 0"])
}

/// `git log --oneline` exits non-zero in a directory that is not a repository,
/// which is what a fresh tempdir is. No compilation, no fixtures.
fn failing_criteria() -> Vec<String> {
    criteria(&["`git log --oneline` exits 0"])
}

#[test]
fn plan_spec_check_locks_once_and_ignores_a_later_rewrite() {
    let dir = policy_dir("lock_once", "advisory");
    let mut plan = plan_with_one_task(passing_criteria());

    let note = lock_spec_check_if_absent(&mut plan, 1, dir.path()).expect("criteria are runnable");
    assert!(
        note.contains("ls ."),
        "the note must name the check: {note}"
    );

    let locked = plan
        .get_task_by_order(1)
        .unwrap()
        .spec_check
        .clone()
        .expect("locked");
    assert_eq!(locked.commands, vec!["ls .".to_string()]);

    // A second call, which is what the post-compaction recovery `plan start`
    // is, and what a retry after a failure is, must not re-derive the check.
    plan.get_task_by_order_mut(1).unwrap().acceptance_criteria = failing_criteria();
    assert!(lock_spec_check_if_absent(&mut plan, 1, dir.path()).is_none());
    assert_eq!(
        plan.get_task_by_order(1).unwrap().spec_check.as_ref(),
        Some(&locked),
        "the lock must survive both the rewrite and the second start"
    );
}

#[test]
fn plan_spec_check_locks_nothing_under_off_or_without_a_command() {
    let off = policy_dir("lock_off", "off");
    let mut plan = plan_with_one_task(passing_criteria());
    assert!(lock_spec_check_if_absent(&mut plan, 1, off.path()).is_none());
    assert!(plan.get_task_by_order(1).unwrap().spec_check.is_none());

    let on = policy_dir("lock_prose", "advisory");
    let mut prose = plan_with_one_task(criteria(&["the parser rejects a bad slug"]));
    assert!(lock_spec_check_if_absent(&mut prose, 1, on.path()).is_none());
    assert!(
        prose.get_task_by_order(1).unwrap().spec_check.is_none(),
        "prose that names no runner locks nothing and behaves as before"
    );
}

#[test]
fn plan_spec_check_a_pass_is_proof() {
    let dir = policy_dir("pass", "advisory");
    let mut plan = plan_with_one_task(passing_criteria());
    lock_spec_check_if_absent(&mut plan, 1, dir.path());

    let result = run_locked_spec_check(&plan, 1, dir.path());
    assert!(result.passed, "`ls .` exits 0");
    assert!(result.note.is_none());
    assert!(!result.rejected);
    assert_eq!(result.commands, vec!["ls .".to_string()]);
}

#[test]
fn plan_spec_check_a_failure_is_refused_only_under_block() {
    let advisory = policy_dir("fail_advisory", "advisory");
    let mut plan = plan_with_one_task(failing_criteria());
    lock_spec_check_if_absent(&mut plan, 1, advisory.path());

    let result = run_locked_spec_check(&plan, 1, advisory.path());
    assert!(!result.passed, "a non-zero exit is not a pass");
    assert!(!result.rejected, "advisory accepts the completion");
    assert!(
        result.note.expect("must explain").contains("failed"),
        "the note must say the check failed"
    );

    let blocking = policy_dir("fail_block", "block");
    let mut blocked = plan_with_one_task(failing_criteria());
    lock_spec_check_if_absent(&mut blocked, 1, blocking.path());

    let result = run_locked_spec_check(&blocked, 1, blocking.path());
    assert!(result.rejected, "block refuses the completion");
    assert!(!result.passed);
}

#[test]
fn plan_spec_check_a_rewritten_criterion_is_refused_and_not_executed() {
    let dir = policy_dir("stale", "block");
    let marker = dir.path().join("ran.txt");
    let mut plan = plan_with_one_task(criteria(&["`git log --oneline` exits 0"]));

    // Lock a command that would leave a trace, then rewrite the criterion.
    let task = plan.get_task_by_order_mut(1).unwrap();
    task.spec_check = Some(SpecCheck::lock(vec![format!("touch {}", marker.display())]));
    task.acceptance_criteria = passing_criteria();

    let result = run_locked_spec_check(&plan, 1, dir.path());
    assert!(result.rejected, "a lock that no longer matches is refused");
    assert!(!result.passed);
    assert!(
        !marker.exists(),
        "a stale lock must not execute: its result would belong to a criterion \
         that no longer exists"
    );
    assert!(
        result
            .note
            .expect("must explain")
            .contains("no longer matches"),
        "the note must name the real cause"
    );
}

#[test]
fn plan_spec_check_an_unlocked_task_runs_nothing() {
    let dir = policy_dir("none", "block");
    let plan = plan_with_one_task(passing_criteria());

    let result = run_locked_spec_check(&plan, 1, dir.path());
    assert!(!result.passed && !result.rejected);
    assert!(result.note.is_none());
    assert!(result.commands.is_empty());
}

// ── the tool path: `start` locks, `complete` judges ──────────────────
//
// The helpers above pin the pieces. These pin the wiring: a fail-open bug
// here would log a passing belief for a check that never ran, which is the
// failure the whole feature exists to prevent.

async fn in_temp_home<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let profile = format!("spec-check-test-{}", Uuid::new_v4());
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

/// A plan with one task carrying `criterion`, bound to a context whose working
/// directory is the policy tempdir, so the policy comes from there and never
/// from the machine-wide ralph_loop.toml.
async fn tool_with_one_task(
    dir: &tempfile::TempDir,
    tool: &PlanTool,
    criterion: &str,
) -> ToolExecutionContext {
    let mut ctx = ToolExecutionContext::new(Uuid::new_v4());
    ctx.working_directory = dir.path().to_path_buf();
    let (ok, out) = run_tool(
        tool,
        &ctx,
        json!({
            "operation": "init",
            "title": "spec check plan",
            "mode": "checklist",
            "tasks": [{
                "title": "t",
                "description": "d",
                "task_type": "edit",
                "acceptance_criteria": [criterion]
            }]
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

#[tokio::test]
async fn plan_spec_check_tool_locks_at_start_and_reports_the_pass() {
    in_temp_home(async {
        let dir = policy_dir("tool_pass", "advisory");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`ls .` exits 0").await;

        let (ok, out) = run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        assert!(ok, "start must succeed: {out}");
        assert!(
            out.contains("Locked spec check"),
            "start must echo the check it locked: {out}"
        );

        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok, "a passing check must not block the completion: {out}");
        assert!(
            out.contains("Locked spec check passed"),
            "the pass must be reported: {out}"
        );
        assert!(
            !out.contains("UNCERTAIN"),
            "a check that ran and exited 0 is proof, not silence: {out}"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_block_refuses_a_failing_check() {
    in_temp_home(async {
        let dir = policy_dir("tool_block", "block");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`git log --oneline` exits 0").await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(!ok, "block must refuse the completion: {out}");
        assert!(out.contains("spec check REJECTED"), "got: {out}");

        let plan = load_plan(ctx.session_id).await.unwrap();
        assert_ne!(
            plan.get_task_by_order(1).unwrap().status,
            TaskStatus::Completed,
            "a refused completion must not mark the task done"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_advisory_downgrades_a_failing_check() {
    in_temp_home(async {
        let dir = policy_dir("tool_advisory", "advisory");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`git log --oneline` exits 0").await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok, "advisory accepts the completion: {out}");
        assert!(
            out.contains("UNCERTAIN") && out.contains("locked spec check failed"),
            "the downgrade must name the check that failed: {out}"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_never_runs_a_criterion_rewritten_after_start() {
    in_temp_home(async {
        let dir = policy_dir("tool_stale", "block");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`ls .` exits 0").await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;

        // The hole the lock exists to close: the criterion is relaxed after
        // the work started, so the task can complete against wording nobody
        // approved.
        let mut plan = load_plan(ctx.session_id).await.unwrap();
        plan.get_task_by_order_mut(1).unwrap().acceptance_criteria =
            criteria(&["`git log --oneline` exits 0"]);
        save_plan(&plan).await.unwrap();

        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(!ok, "a rewritten criterion must be refused: {out}");
        assert!(
            out.contains("no longer matches"),
            "the refusal must name the cause: {out}"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_advisory_downgrades_a_criterion_rewritten_after_start() {
    in_temp_home(async {
        let dir = policy_dir("tool_stale_advisory", "advisory");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`ls .` exits 0").await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;

        let mut plan = load_plan(ctx.session_id).await.unwrap();
        plan.get_task_by_order_mut(1).unwrap().acceptance_criteria =
            criteria(&["`git log --oneline` exits 0"]);
        save_plan(&plan).await.unwrap();

        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok, "advisory accepts the completion: {out}");
        assert!(
            out.contains("no longer matches") && out.contains("UNCERTAIN"),
            "the downgrade must name the criterion that moved: {out}"
        );
    })
    .await;
}

#[test]
fn plan_spec_check_a_generated_lock_is_not_stale_against_prose_criteria() {
    // The bug this pins: a generated lock was never derived from the criteria,
    // so re-deriving them yields nothing and a naive comparison would fail
    // every time, reading as "the criterion moved" when it never did.
    let dir = policy_dir("gen_not_stale", "advisory");
    let mut plan = plan_with_one_task(criteria(&["the loader reads a config without panicking"]));
    plan.get_task_by_order_mut(1).unwrap().spec_check =
        Some(SpecCheck::lock_generated(vec!["ls .".to_string()]));

    let result = run_locked_spec_check(&plan, 1, dir.path());
    assert!(
        result.passed,
        "a generated check must actually run, not read as stale"
    );
    assert!(result.note.is_none());
    assert!(!result.rejected);
}

#[test]
fn plan_spec_check_a_failing_generated_check_does_not_read_as_stale() {
    let dir = policy_dir("gen_not_stale_fail", "block");
    let mut plan = plan_with_one_task(criteria(&["the loader reads a config without panicking"]));
    plan.get_task_by_order_mut(1).unwrap().spec_check = Some(SpecCheck::lock_generated(vec![
        "git log --oneline".to_string(),
    ]));

    let result = run_locked_spec_check(&plan, 1, dir.path());
    assert!(!result.passed);
    assert!(
        !result.rejected,
        "an unconfirmed generated check cannot reject"
    );
    let note = result.note.expect("must explain");
    assert!(
        note.contains("failed"),
        "the note must name the real cause: {note}"
    );
    assert!(
        !note.contains("no longer matches"),
        "a generated check is not stale: {note}"
    );
    assert!(
        note.contains("not been confirmed"),
        "the note must name the provenance reason: {note}"
    );
}

// ── generation: the model writes a check (#2063) ─────────────────────
//
// The model is asked only when the criteria name no runnable command, and
// what it writes is held to the same rule a criteria-derived command is.
// Every refusal below is the difference between a useful suggestion and a
// gate that rejects work that was done correctly.

/// Provider stub: counts every `complete` and replays a canned reply, or
/// errors. Mirrors `decide_cached_test`'s stub so a suite never reaches the
/// network.
struct SpecStub {
    calls: std::sync::atomic::AtomicUsize,
    reply: Option<String>,
}

impl SpecStub {
    fn answering(reply: &str) -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            reply: Some(reply.to_string()),
        }
    }

    fn failing() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            reply: None,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::brain::provider::Provider for SpecStub {
    async fn complete(
        &self,
        _request: crate::brain::provider::LLMRequest,
    ) -> crate::brain::provider::Result<crate::brain::provider::LLMResponse> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let Some(reply) = &self.reply else {
            return Err(crate::brain::provider::ProviderError::ApiError {
                status: 503,
                message: "stub provider is down".to_string(),
                error_type: None,
            });
        };
        Ok(crate::brain::provider::LLMResponse {
            id: "spec-stub".to_string(),
            model: "spec-stub-model".to_string(),
            content: vec![crate::brain::provider::ContentBlock::Text {
                text: reply.clone(),
            }],
            stop_reason: Some(crate::brain::provider::StopReason::EndTurn),
            usage: crate::brain::provider::TokenUsage {
                input_tokens: 5,
                output_tokens: 10,
                ..Default::default()
            },
            streaming_active_secs: None,
            tool_text_leak: false,
        })
    }

    async fn stream(
        &self,
        _request: crate::brain::provider::LLMRequest,
    ) -> crate::brain::provider::Result<crate::brain::provider::ProviderStream> {
        Err(crate::brain::provider::ProviderError::ApiError {
            status: 501,
            message: "stub does not stream".to_string(),
            error_type: None,
        })
    }

    fn name(&self) -> &str {
        "spec-stub"
    }

    fn default_model(&self) -> &str {
        "spec-stub-model"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["spec-stub-model".to_string()]
    }

    fn context_window(&self, _model: &str) -> Option<u32> {
        Some(8192)
    }

    fn calculate_cost(&self, _model: &str, _input: u32, _output: u32) -> f64 {
        0.0
    }
}

/// A prose task: criteria that name no runner, which is the only case that
/// asks the model for anything.
fn prose_task() -> PlanTask {
    let mut task = PlanTask::new(
        1,
        "Refactor the loader".to_string(),
        "d".to_string(),
        TaskType::Edit,
    );
    task.acceptance_criteria = criteria(&["the loader reads a config without panicking"]);
    task
}

async fn generate_with(stub: std::sync::Arc<SpecStub>) -> Vec<String> {
    let ctx = ToolExecutionContext::new(Uuid::new_v4()).with_provider_override(stub);
    crate::brain::tools::plan_tool::generate_spec_check_commands(&ctx, &prose_task()).await
}

#[test]
fn plan_spec_check_parses_a_plain_line_of_commands() {
    let commands = crate::brain::tools::plan_tool::parse_generated_commands(
        "cargo test --lib pricing\ncargo clippy --all-features -- -D warnings",
    );
    assert_eq!(
        commands,
        vec![
            "cargo test --lib pricing".to_string(),
            "cargo clippy --all-features -- -D warnings".to_string()
        ]
    );
}

#[test]
fn plan_spec_check_parses_through_bullets_numbers_and_fences() {
    // The answer is text, so the model formats it. Refusing a good command
    // for its bullet would waste the call.
    let commands = crate::brain::tools::plan_tool::parse_generated_commands(
        "```\n- `cargo test --lib pricing`\n1. cargo fmt --all -- --check\n```",
    );
    assert_eq!(
        commands,
        vec![
            "cargo test --lib pricing".to_string(),
            "cargo fmt --all -- --check".to_string()
        ]
    );
}

#[test]
fn plan_spec_check_parses_none_as_no_command() {
    assert!(crate::brain::tools::plan_tool::parse_generated_commands("NONE").is_empty());
    assert!(crate::brain::tools::plan_tool::parse_generated_commands("none").is_empty());
    assert!(
        crate::brain::tools::plan_tool::parse_generated_commands("  None\n\n").is_empty(),
        "an explicit none locks nothing"
    );
}

#[test]
fn plan_spec_check_parses_prose_as_no_command() {
    let commands = crate::brain::tools::plan_tool::parse_generated_commands(
        "I could not find a runnable command for this task, since the criterion is \
         about documentation quality.",
    );
    assert!(
        commands.is_empty(),
        "prose must not become a locked command, got {commands:?}"
    );
}

#[test]
fn plan_spec_check_parses_a_compound_reply_as_no_command() {
    // The same refusal the criteria path applies, applied to model text.
    let commands = crate::brain::tools::plan_tool::parse_generated_commands(
        "cargo test --lib pricing && git push\ncargo test --lib pricing | tail -5",
    );
    assert!(
        commands.is_empty(),
        "a compound line is refused whoever wrote it, got {commands:?}"
    );
}

#[test]
fn plan_spec_check_parses_a_bare_runner_as_no_command() {
    let commands = crate::brain::tools::plan_tool::parse_generated_commands("cargo\nls");
    assert!(
        commands.is_empty(),
        "a bare runner prints help and exits 0, got {commands:?}"
    );
}

#[test]
fn plan_spec_check_parses_duplicates_once() {
    let commands = crate::brain::tools::plan_tool::parse_generated_commands(
        "cargo test --lib pricing\ncargo test --lib pricing",
    );
    assert_eq!(commands, vec!["cargo test --lib pricing".to_string()]);
}

#[test]
fn plan_spec_check_prompt_carries_the_task_and_nothing_else() {
    let mut task = prose_task();
    task.acceptance_criteria = criteria(&["the loader reads a config without panicking"]);
    let prompt = crate::brain::tools::plan_tool::build_spec_check_prompt(&task);

    assert!(prompt.contains("Refactor the loader"), "title must be sent");
    assert!(
        prompt.contains("the loader reads a config without panicking"),
        "criteria must be sent"
    );
    // The way out has to be stated, or the model invents a command for prose.
    assert!(
        prompt.contains("NONE"),
        "the prompt must allow a none answer: {prompt}"
    );
    // The input is the task text, not the repository. A prompt that carried
    // the diff would be asking the model to grade work it can already see.
    assert!(
        !prompt.contains("diff") && !prompt.contains("git status"),
        "the prompt must not carry the work: {prompt}"
    );
}

#[tokio::test]
async fn plan_spec_check_generates_a_command_from_a_stub_provider() {
    let stub = std::sync::Arc::new(SpecStub::answering("cargo test --lib pricing"));
    let commands = generate_with(std::sync::Arc::clone(&stub)).await;

    assert_eq!(commands, vec!["cargo test --lib pricing".to_string()]);
    assert_eq!(stub.calls(), 1, "one call per task, not a conversation");
}

#[tokio::test]
async fn plan_spec_check_generation_returns_nothing_when_the_stub_says_none() {
    let stub = std::sync::Arc::new(SpecStub::answering("NONE"));
    assert!(generate_with(stub).await.is_empty());
}

#[tokio::test]
async fn plan_spec_check_generation_survives_a_provider_error() {
    // A failed model call is not a failed start. It locks nothing and the
    // task behaves exactly as it did before generation existed.
    let stub = std::sync::Arc::new(SpecStub::failing());
    let commands = generate_with(std::sync::Arc::clone(&stub)).await;

    assert!(commands.is_empty(), "an error yields no check");
    assert_eq!(stub.calls(), 1, "the failure was a real call, not a skip");
}

#[tokio::test]
async fn plan_spec_check_generation_refuses_a_stub_compound_reply() {
    let stub = std::sync::Arc::new(SpecStub::answering("cargo test && git push"));
    assert!(
        generate_with(stub).await.is_empty(),
        "the refusal holds through the whole generation path"
    );
}

// ── generation through the tool: `start` locks, confirm grants teeth ──

/// Like `tool_with_one_task`, but with a stub provider on the context so the
/// generation path never reaches the network.
async fn tool_with_stub(
    dir: &tempfile::TempDir,
    tool: &PlanTool,
    criterion: &str,
    stub: std::sync::Arc<SpecStub>,
) -> ToolExecutionContext {
    let mut ctx = tool_with_one_task(dir, tool, criterion).await;
    ctx.provider_override = Some(stub);
    ctx
}

#[tokio::test]
async fn plan_spec_check_tool_generates_a_check_when_the_criteria_are_prose() {
    in_temp_home(async {
        let dir = policy_dir("tool_gen", "advisory");
        let tool = PlanTool;
        let stub = std::sync::Arc::new(SpecStub::answering("`ls .`"));
        let ctx = tool_with_stub(
            &dir,
            &tool,
            "the loader reads a config without panicking",
            std::sync::Arc::clone(&stub),
        )
        .await;

        let (ok, out) = run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        assert!(ok, "start must succeed: {out}");
        assert_eq!(stub.calls(), 1, "one model call for a prose task");
        assert!(
            out.contains("MODEL-WRITTEN spec check"),
            "the start result must say the check was written by a model: {out}"
        );
        assert!(
            out.contains("advisory only until you confirm"),
            "the result must state what an unconfirmed check may do: {out}"
        );

        let plan = load_plan(ctx.session_id).await.unwrap();
        let lock = plan
            .get_task_by_order(1)
            .unwrap()
            .spec_check
            .clone()
            .expect("generated check was locked");
        assert_eq!(lock.source, SpecCheckSource::Generated);
        assert!(!lock.confirmed, "generation does not confirm itself");
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_makes_no_model_call_when_the_criteria_name_a_command() {
    in_temp_home(async {
        let dir = policy_dir("tool_gen_skip", "advisory");
        let tool = PlanTool;
        let stub = std::sync::Arc::new(SpecStub::answering("`git log --oneline`"));
        let ctx = tool_with_stub(&dir, &tool, "`ls .` exits 0", std::sync::Arc::clone(&stub)).await;

        let (ok, out) = run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        assert!(ok, "start must succeed: {out}");
        assert_eq!(
            stub.calls(),
            0,
            "criteria that already name a command must cost nothing"
        );
        assert!(
            !out.contains("MODEL-WRITTEN"),
            "a criteria check must not be labelled generated: {out}"
        );

        let plan = load_plan(ctx.session_id).await.unwrap();
        assert_eq!(
            plan.get_task_by_order(1)
                .unwrap()
                .spec_check
                .clone()
                .unwrap()
                .source,
            SpecCheckSource::Criteria
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_recovery_start_does_not_regenerate_the_check() {
    in_temp_home(async {
        let dir = policy_dir("tool_gen_recover", "advisory");
        let tool = PlanTool;
        let stub = std::sync::Arc::new(SpecStub::answering("`ls .`"));
        let ctx = tool_with_stub(
            &dir,
            &tool,
            "the loader reads a config without panicking",
            std::sync::Arc::clone(&stub),
        )
        .await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        let first = load_plan(ctx.session_id)
            .await
            .unwrap()
            .get_task_by_order(1)
            .unwrap()
            .spec_check
            .clone()
            .expect("locked");

        // The post-compaction recovery call: a bare `start` while InProgress.
        let (ok, out) = run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        assert!(ok, "recovery start must succeed: {out}");
        assert_eq!(
            stub.calls(),
            1,
            "a recovery start must not regenerate the check and void its hash"
        );
        assert_eq!(
            load_plan(ctx.session_id)
                .await
                .unwrap()
                .get_task_by_order(1)
                .unwrap()
                .spec_check
                .clone(),
            Some(first),
            "the lock must survive the recovery call unchanged"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_generated_check_never_blocks_unconfirmed() {
    in_temp_home(async {
        // `block` policy is the strongest setting there is, and it still must
        // not let a model-written check reject a completion nobody confirmed.
        let dir = policy_dir("tool_gen_block", "block");
        let tool = PlanTool;
        let stub = std::sync::Arc::new(SpecStub::answering("`git log --oneline`"));
        let ctx = tool_with_stub(
            &dir,
            &tool,
            "the loader reads a config without panicking",
            stub,
        )
        .await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;

        assert!(
            ok,
            "an unconfirmed generated check must not reject, even under block: {out}"
        );
        assert!(
            out.contains("UNCERTAIN") && out.contains("locked spec check failed"),
            "it still records the failure as Uncertain: {out}"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_confirming_grants_the_generated_check_its_teeth() {
    in_temp_home(async {
        let dir = policy_dir("tool_gen_confirm", "block");
        let tool = PlanTool;
        let stub = std::sync::Arc::new(SpecStub::answering("`git log --oneline`"));
        let ctx = tool_with_stub(
            &dir,
            &tool,
            "the loader reads a config without panicking",
            stub,
        )
        .await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;

        // Confirmation is explicit, and it is the only thing that changes the
        // check's power.
        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "start", "confirm_spec_check": true }),
        )
        .await;
        assert!(ok, "confirming must succeed: {out}");
        assert!(out.contains("Confirmed"), "the result must say so: {out}");

        let lock = load_plan(ctx.session_id)
            .await
            .unwrap()
            .get_task_by_order(1)
            .unwrap()
            .spec_check
            .clone()
            .unwrap();
        assert!(lock.confirmed, "the lock records the confirmation");
        assert!(
            lock.matches(&lock.commands.clone()),
            "confirmation must not void the lock it just approved"
        );

        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(!ok, "a confirmed failing check now rejects: {out}");
        assert!(out.contains("spec check REJECTED"), "got: {out}");
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_locks_nothing_when_generation_is_disabled() {
    in_temp_home(async {
        let dir = tempfile::Builder::new()
            .prefix("spec2063_off_")
            .tempdir()
            .expect("tempdir");
        fs::write(
            dir.path().join("ralph_loop.toml"),
            "[verification]\nenabled = true\nspec_checks = \"advisory\"\nspec_check_generation = false\n",
        )
        .expect("write config");
        let tool = PlanTool;
        let stub = std::sync::Arc::new(SpecStub::answering("`ls .`"));
        let ctx = tool_with_stub(
            &dir,
            &tool,
            "the loader reads a config without panicking",
            std::sync::Arc::clone(&stub),
        )
        .await;

        let (ok, out) = run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        assert!(ok, "start must succeed: {out}");
        assert_eq!(stub.calls(), 0, "the key is off, so no call is made");
        assert!(
            load_plan(ctx.session_id)
                .await
                .unwrap()
                .get_task_by_order(1)
                .unwrap()
                .spec_check
                .is_none(),
            "nothing is locked"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_survives_a_provider_error_at_start() {
    in_temp_home(async {
        let dir = policy_dir("tool_gen_err", "block");
        let tool = PlanTool;
        let ctx = tool_with_stub(
            &dir,
            &tool,
            "the loader reads a config without panicking",
            std::sync::Arc::new(SpecStub::failing()),
        )
        .await;

        let (ok, out) = run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        assert!(ok, "a failed model call is not a failed start: {out}");
        assert!(
            load_plan(ctx.session_id)
                .await
                .unwrap()
                .get_task_by_order(1)
                .unwrap()
                .spec_check
                .is_none(),
            "nothing is locked, so the task behaves as it did before"
        );

        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok, "no check means no check to fail: {out}");
    })
    .await;
}
