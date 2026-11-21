//! #3004: what a session has already announced has to outlive the process.
//!
//! The failure this pins is a launch, not a tick. The old record was a set on
//! the app, so it died with the window: every relaunch after a clean exit, a
//! panic or a machine restart compared the plan file against an empty set and
//! flashed every task that had settled weeks ago. The durable ledger keys the
//! record by session and writes it on the settle itself.
//!
//! Every test drives an explicit file under a temp dir, so nothing here can
//! read or write the live OpenCrabs home. That is not decoration:
//! `atomic_write` refuses the live home under `cfg(test)` precisely so a
//! leaking test fails loudly instead of rewriting the owner's real record.

use uuid::Uuid;

use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use crate::utils::settle_ledger::SettleLedger;

/// An Active plan owning one task per entry in `statuses`.
fn plan_with(session: Uuid, statuses: &[TaskStatus]) -> PlanDocument {
    let mut plan = PlanDocument::new(session, "durable settle".to_string());
    plan.status = PlanStatus::Active;
    for (i, status) in statuses.iter().enumerate() {
        let mut task = PlanTask::new(
            i + 1,
            format!("task {}", i + 1),
            String::new(),
            TaskType::Other("test".to_string()),
        );
        task.status = status.clone();
        plan.add_task(task);
    }
    plan
}

/// Every task id in a plan, in the order they were added.
fn ids(plan: &PlanDocument) -> Vec<Uuid> {
    plan.tasks.iter().map(|task| task.id).collect()
}

#[test]
fn a_settle_survives_a_new_ledger_over_the_same_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("notify_seen.json");
    let session = Uuid::new_v4();
    let plan = plan_with(session, &[TaskStatus::Completed]);

    // First process: the settle is fresh, so it flashes and is written.
    let mut first = SettleLedger::at(path.clone());
    let fresh = first.record(session, Some(&plan));
    assert_eq!(fresh, ids(&plan), "the first process flashes the settle");
    assert_eq!(
        first.announced(session),
        ids(&plan),
        "the settle reached the file, not just the process"
    );

    // Second process: same plan, same file, no flash. This is the relaunch
    // the old in-memory set could not survive.
    let mut second = SettleLedger::at(path);
    assert!(
        second.record(session, Some(&plan)).is_empty(),
        "a relaunch must not replay a settle the last process already announced"
    );
}

#[test]
fn a_task_that_settled_while_the_app_was_closed_still_flashes_once() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("notify_seen.json");
    let session = Uuid::new_v4();

    // The plan is open, nothing has settled.
    let mut ledger = SettleLedger::at(path.clone());
    let mut plan = plan_with(session, &[TaskStatus::Pending]);
    assert!(ledger.record(session, Some(&plan)).is_empty());
    drop(ledger);

    // The process stops. The turn finishes in the meantime, so the same task
    // in the same plan is Completed by the time the window opens again.
    plan.tasks[0].status = TaskStatus::Completed;
    let mut relaunched = SettleLedger::at(path);
    assert_eq!(
        relaunched.record(session, Some(&plan)).len(),
        1,
        "a settle the user never saw still announces, once"
    );
    assert!(
        relaunched.record(session, Some(&plan)).is_empty(),
        "and only once, because the record is now on disk"
    );
}

#[test]
fn one_session_never_answers_for_another() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("notify_seen.json");
    let announcing = Uuid::new_v4();
    let quiet = Uuid::new_v4();

    let mut ledger = SettleLedger::at(path.clone());
    let first = plan_with(announcing, &[TaskStatus::Completed]);
    assert_eq!(ledger.record(announcing, Some(&first)).len(), 1);

    // A different conversation with its own settled task still announces. The
    // ids are distinct plans, so the union set cannot hide this one either.
    let second = plan_with(quiet, &[TaskStatus::Completed]);
    assert_eq!(
        ledger.record(quiet, Some(&second)).len(),
        1,
        "another session's record must not silence this one"
    );

    // And the durable side keeps them apart: each session reads back only its
    // own task, so a restart cannot hand one conversation's history to another.
    let reader = SettleLedger::at(path);
    assert_eq!(reader.announced(announcing), ids(&first));
    assert_eq!(reader.announced(quiet), ids(&second));
}

#[test]
fn a_plan_with_no_session_is_deduped_in_memory_and_never_written() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("notify_seen.json");
    let plan = plan_with(Uuid::nil(), &[TaskStatus::Completed]);

    let mut ledger = SettleLedger::at(path.clone());
    assert_eq!(
        ledger.record(Uuid::nil(), Some(&plan)).len(),
        1,
        "a pre-init plan still flashes on the tick that sees it"
    );
    assert!(
        ledger.record(Uuid::nil(), Some(&plan)).is_empty(),
        "and stays quiet afterwards"
    );
    assert!(
        !path.exists(),
        "there is no session to key a durable record on, so no file is written"
    );
}

#[test]
fn a_corrupt_record_reads_empty_and_is_rewritten_clean() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("notify_seen.json");
    std::fs::write(&path, "{\"sessions\": {\"not-a-uuid\": [}]").expect("write the broken file");

    let session = Uuid::new_v4();
    let plan = plan_with(session, &[TaskStatus::Completed]);
    let mut ledger = SettleLedger::at(path.clone());
    assert_eq!(
        ledger.record(session, Some(&plan)).len(),
        1,
        "an unreadable record costs one replay, never silence"
    );

    let text = std::fs::read_to_string(&path).expect("the record was rewritten");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("the rewrite is valid JSON");
    assert!(
        parsed.get("sessions").is_some(),
        "the rewrite carries the documented shape: {text}"
    );
    assert_eq!(
        SettleLedger::at(path).announced(session),
        ids(&plan),
        "the replay replaced the broken file with a readable record"
    );
}

#[test]
fn the_record_is_merged_with_the_file_not_written_over_it() {
    // The terminal and the window can run at once, and each only knows what
    // IT announced. Writing a snapshot read at startup would erase the other
    // surface's entries; the write re-reads the file and merges instead.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("notify_seen.json");
    let terminal = Uuid::new_v4();
    let window = Uuid::new_v4();

    let mut terminal_ledger = SettleLedger::at(path.clone());
    let mut window_ledger = SettleLedger::at(path.clone());
    // Both opened before either wrote, which is the racing case.
    let terminal_plan = plan_with(terminal, &[TaskStatus::Completed]);
    let window_plan = plan_with(window, &[TaskStatus::Completed]);

    terminal_ledger.record(terminal, Some(&terminal_plan));
    window_ledger.record(window, Some(&window_plan));

    let reader = SettleLedger::at(path);
    assert_eq!(
        reader.announced(terminal),
        ids(&terminal_plan),
        "the second surface's write kept the first surface's record"
    );
    assert_eq!(reader.announced(window), ids(&window_plan));
}
