//! The environment snapshot that `/clear` appends to its marker, and the
//! turn instructions for the reply that follows it.
//!
//! The snapshot points at state that already persists (plan files and the
//! working directory) and never copies it, so these are pure string checks.

use crate::brain::agent::service::clear_snapshot::{
    EnvironmentSnapshot, PlanState, reply_instructions, snapshot_block,
};
use crate::utils::plan_files::PlanModeState;

fn snapshot(plan_state: PlanState) -> EnvironmentSnapshot {
    EnvironmentSnapshot {
        working_directory: Some("/work/repo".to_string()),
        plan_state,
        plan_md: Some("/plans/s1.md".to_string()),
        plan_json: Some("/plans/s1.json".to_string()),
    }
}

#[test]
fn active_plan_points_at_its_files() {
    let block = snapshot_block(&snapshot(PlanState::Active));
    assert!(block.contains("- Working directory: /work/repo"));
    assert!(block.contains("- Plan: active, checklist running"));
    assert!(block.contains("- Plan document: /plans/s1.md"));
    assert!(block.contains("- Plan state file: /plans/s1.json"));
}

#[test]
fn no_plan_names_no_plan_files() {
    let mut snap = snapshot(PlanState::None);
    snap.plan_md = None;
    snap.plan_json = None;
    let block = snapshot_block(&snap);
    assert!(block.contains("- Plan: none in this session"));
    assert!(!block.contains("Plan document"));
    assert!(!block.contains("Plan state file"));
}

#[test]
fn missing_working_directory_is_reported_not_omitted() {
    let mut snap = snapshot(PlanState::None);
    snap.working_directory = Some("   ".to_string());
    assert!(snapshot_block(&snap).contains("- Working directory: not set"));
}

#[test]
fn editing_plan_without_md_omits_the_document_line() {
    let mut snap = snapshot(PlanState::Editing);
    snap.plan_md = None;
    let block = snapshot_block(&snap);
    assert!(block.contains("- Plan: editing, not started yet"));
    assert!(!block.contains("Plan document"));
    assert!(block.contains("- Plan state file: /plans/s1.json"));
}

#[test]
fn both_editing_substates_map_to_editing() {
    assert_eq!(
        PlanState::from(PlanModeState::PreInitEditing),
        PlanState::Editing
    );
    assert_eq!(
        PlanState::from(PlanModeState::PostInitEditing),
        PlanState::Editing
    );
    assert_eq!(PlanState::from(PlanModeState::NoPlan), PlanState::None);
    assert_eq!(PlanState::from(PlanModeState::Active), PlanState::Active);
}

#[test]
fn reply_always_asks_for_acknowledgement_confidence_and_roast() {
    for state in [PlanState::Active, PlanState::Editing, PlanState::None] {
        let text = reply_instructions("my session", &snapshot(state));
        assert!(text.contains("context window was wiped"), "{state:?}");
        assert!(text.contains("how confident"), "{state:?}");
        assert!(text.contains("ROAST"), "{state:?}");
        assert!(text.contains("session 'my session'"), "{state:?}");
    }
}

#[test]
fn reply_continuation_follows_the_plan_state() {
    let active = reply_instructions("t", &snapshot(PlanState::Active));
    assert!(active.contains("resume its next incomplete task now"));

    let editing = reply_instructions("t", &snapshot(PlanState::Editing));
    assert!(editing.contains("still in editing"));

    let none = reply_instructions("t", &snapshot(PlanState::None));
    assert!(none.contains("ask what the user wants next"));
}
