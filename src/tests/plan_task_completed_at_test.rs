//! A finished plan task records WHEN it finished (#2574).
//!
//! `TaskStatus::Completed` alone says nothing about age, so a checklist could
//! not tell the row just closed from one closed an hour ago: every finished
//! task had to read the same. These tests pin the stamp on the lifecycle
//! edges that write it, and pin the backward-compatible read for plans saved
//! before the field existed.

use chrono::{Duration, Utc};

use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use crate::tui::render::plan_widget::settled_task_role;
use crate::tui::render::theme::Role;
use crate::utils::completion_sound::newly_resolved;

fn task() -> PlanTask {
    PlanTask::new(
        1,
        "Land the footer".to_string(),
        "Move the status bar inside the pane".to_string(),
        TaskType::Edit,
    )
}

#[test]
fn completing_a_task_stamps_the_time() {
    let mut task = task();
    assert!(task.completed_at.is_none(), "a pending task has no stamp");

    let before = Utc::now();
    task.complete(Some("done".to_string()));
    let stamped = task.completed_at.expect("complete stamps the time");

    assert_eq!(task.status, TaskStatus::Completed);
    assert!(
        stamped >= before - Duration::seconds(1) && stamped <= Utc::now(),
        "the stamp is this completion, not a carried value: {stamped}"
    );
}

#[test]
fn skipping_a_task_stamps_the_time() {
    let mut task = task();
    task.skip(Some("nothing to do".to_string()));
    assert_eq!(task.status, TaskStatus::Skipped);
    assert!(
        task.completed_at.is_some(),
        "a skipped task is settled work and needs an age too"
    );
}

#[test]
fn restarting_a_finished_task_clears_the_stamp() {
    let mut task = task();
    task.complete(None);
    assert!(task.completed_at.is_some());

    // A retry makes the task live again; a stale stamp would render it as
    // finished work while it is running.
    task.start();
    assert_eq!(task.status, TaskStatus::InProgress);
    assert!(task.completed_at.is_none(), "restart clears the stamp");
}

#[test]
fn a_plan_saved_before_this_field_still_loads() {
    // The exact shape written by an earlier build: no `completed_at` key.
    let legacy = r#"{
        "id": "9a7d4b1c-6f2e-4a5b-8c3d-1e2f3a4b5c6d",
        "order": 1,
        "title": "Old task",
        "description": "Written before the stamp existed",
        "task_type": "edit",
        "dependencies": [],
        "complexity": 3,
        "acceptance_criteria": [],
        "status": "Completed",
        "notes": null,
        "retry_count": 0
    }"#;

    let task: PlanTask = serde_json::from_str(legacy).expect("legacy task loads");
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(
        task.completed_at.is_none(),
        "absent means unknown age, never a fabricated timestamp"
    );
}

#[test]
fn the_stamp_round_trips_through_json() {
    let mut task = task();
    task.complete(None);
    let json = serde_json::to_string(&task).expect("serializes");
    assert!(
        json.contains("completed_at"),
        "the stamp is persisted: {json}"
    );

    let back: PlanTask = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(back.completed_at, task.completed_at);
}
// ── the dim ramp ────────────────────────────────────────────────

#[test]
fn a_task_just_settled_reads_bright_and_fades_from_there() {
    let now = Utc::now();
    let ago = |secs: i64| Some(now - Duration::seconds(secs));

    assert_eq!(
        settled_task_role(ago(1), now),
        Role::TealCalm,
        "a task closed a moment ago is the one the eye should catch"
    );
    assert_eq!(
        settled_task_role(ago(5), now),
        Role::TealMuted,
        "then it fades to the settled colour"
    );
    assert_eq!(
        settled_task_role(ago(20), now),
        Role::GrayMuted,
        "and keeps fading as it ages"
    );
    assert_eq!(
        settled_task_role(ago(120), now),
        Role::GrayDim,
        "an old completion sinks into the background"
    );
}

#[test]
fn an_unknown_age_never_reads_as_just_finished() {
    let now = Utc::now();
    assert_eq!(
        settled_task_role(None, now),
        Role::TealMuted,
        "a plan written before the stamp existed keeps the old settled colour"
    );
    assert_eq!(
        settled_task_role(Some(now + Duration::seconds(30)), now),
        Role::TealMuted,
        "a stamp in the future is unreadable age, not a fresh completion"
    );
}

// ── the per-task chime ─────────────────────────────────────────

fn plan_with(statuses: &[TaskStatus]) -> PlanDocument {
    let mut plan = PlanDocument::new(uuid::Uuid::new_v4(), "chime".to_string());
    plan.status = PlanStatus::Active;
    for (i, status) in statuses.iter().enumerate() {
        let mut task = PlanTask::new(i + 1, format!("task {i}"), String::new(), TaskType::Edit);
        task.status = status.clone();
        plan.add_task(task);
    }
    plan
}

#[test]
fn each_settled_task_chimes_exactly_once() {
    let mut seen = std::collections::HashSet::new();
    let plan = plan_with(&[TaskStatus::Completed, TaskStatus::InProgress]);
    let plans = [Some(&plan)];

    assert_eq!(
        newly_resolved(&mut seen, &plans),
        1,
        "the first read chimes"
    );
    assert_eq!(
        newly_resolved(&mut seen, &plans),
        0,
        "re-reading the same plan document must not chime again"
    );
}

#[test]
fn the_next_task_to_settle_chimes_on_its_own() {
    let mut seen = std::collections::HashSet::new();
    let mut plan = plan_with(&[TaskStatus::Completed, TaskStatus::InProgress]);
    assert_eq!(newly_resolved(&mut seen, &[Some(&plan)]), 1);

    // Same document, the second task settles: only that one is news.
    plan.tasks[1].complete(None);
    assert_eq!(
        newly_resolved(&mut seen, &[Some(&plan)]),
        1,
        "the already-announced task stays quiet, the new one chimes"
    );
}

#[test]
fn no_plan_and_empty_plans_stay_silent() {
    let mut seen = std::collections::HashSet::new();
    assert_eq!(newly_resolved(&mut seen, &[None]), 0);
    let empty = plan_with(&[]);
    assert_eq!(newly_resolved(&mut seen, &[Some(&empty)]), 0);
}
