//! #2733: a plan task that settles in a session the user is NOT looking at
//! must chime when it settles, not when that pane is finally focused.
//!
//! The defect was a stale cache, not a missing signal. Only the focused pane
//! reloaded its plan document, so a background pane compared against the
//! checklist it held at the moment it was opened: `newly_resolved` saw
//! nothing, nothing chimed, and the settle rang minutes later on the click
//! that loaded the file again.
//!
//! The tests drive the two seams the tick uses: the per-pane mtime reload and
//! the settled-task counter (`fresh_settled_tasks`, which is exactly the
//! number the chime responds to). They stop short of `announce_settled_tasks`
//! because that one raises an OS banner, and a test must never pop a
//! notification on the machine running it.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use uuid::Uuid;

use crate::tui::pane::{PaneId, PaneManager, SplitDirection};
use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use crate::utils::desktop_notify::fresh_settled_tasks;

/// The tick's comparison, pinned in source: it has to refresh every pane from
/// its own file before counting, and it must never resolve the reload through
/// the focused pane (that resolution IS the bug).
const STATE_SRC: &str = include_str!("../tui/app/state.rs");

/// An Active plan holding one task, with a caller-chosen id so the same task
/// can be moved from pending to settled between writes.
fn plan_with_task(session: Uuid, task_id: Uuid, status: TaskStatus) -> PlanDocument {
    let mut plan = PlanDocument::new(session, "background settle".to_string());
    plan.status = PlanStatus::Active;
    let mut task = PlanTask::new(
        1,
        "write the thing".to_string(),
        String::new(),
        TaskType::Other("test".to_string()),
    );
    task.id = task_id;
    task.status = status;
    plan.add_task(task);
    plan
}

/// Write a plan the way the plan tool does: the JSON file, nothing else.
fn write_plan(path: &std::path::Path, plan: &PlanDocument) {
    std::fs::write(path, serde_json::to_string(plan).expect("plan json")).expect("write plan");
}

/// Push the file's mtime forward, so the reload gate sees a change even if
/// two writes land inside one filesystem timestamp tick.
fn bump_mtime(path: &std::path::Path, secs: u64) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open plan file");
    file.set_modified(SystemTime::now() + Duration::from_secs(secs))
        .expect("stamp the plan file");
}

/// The reported case: the pane that settles is not the one holding the
/// keyboard, and its file changed while it was in the background.
#[test]
fn a_background_pane_counts_its_settle_without_focus() {
    let dir = tempfile::tempdir().expect("temp dir");
    let watched_session = Uuid::new_v4();
    let background_session = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let path = dir
        .path()
        .join(format!(".opencrabs_plan_{background_session}.json"));

    // Bind the settling pane first, then split so it LOSES focus: that is
    // exactly the layout the issue describes.
    let pending = plan_with_task(background_session, task_id, TaskStatus::Pending);
    write_plan(&path, &pending);
    let mut panes = PaneManager::new();
    panes.set_pane_plan(PaneId::ROOT, Some(path.clone()), Some(pending));
    panes.split(SplitDirection::Horizontal);
    let focused = panes
        .focused_pane()
        .map(|pane| pane.id)
        .expect("focused pane");
    panes
        .get_mut(focused)
        .expect("focused pane exists")
        .session_id = Some(watched_session);
    assert_ne!(
        focused,
        PaneId::ROOT,
        "the settling pane has to be the background one"
    );

    // Nothing has settled yet, so the counter stays at zero.
    panes.refresh_plans_changed_on_disk();
    let mut seen = HashSet::new();
    {
        let docs = [panes
            .get(PaneId::ROOT)
            .and_then(|pane| pane.plan_document.as_ref())];
        assert_eq!(
            fresh_settled_tasks(&mut seen, &docs).len(),
            0,
            "a pending task is not a settle"
        );
    }

    // The task settles in the file while the OTHER pane has the keyboard.
    let settled = plan_with_task(background_session, task_id, TaskStatus::Completed);
    write_plan(&path, &settled);
    bump_mtime(&path, 2);
    assert_eq!(
        panes.refresh_plans_changed_on_disk(),
        1,
        "the background pane must re-read its own plan file"
    );

    let docs = [panes
        .get(PaneId::ROOT)
        .and_then(|pane| pane.plan_document.as_ref())];
    let fresh = fresh_settled_tasks(&mut seen, &docs);
    assert_eq!(
        fresh.len(),
        1,
        "the chime counter has to reach 1 for a settle in an unfocused pane"
    );
    assert_eq!(fresh[0].0, task_id, "the settle is the task that finished");
    assert!(fresh[0].2, "a completed task rings as a success");
}

/// The reload is gated on the mtime, and it has to be: `load_plan_from_path`
/// retires legacy terminal plans from disk, so polling it every tick would
/// mutate plan files for no reason.
#[test]
fn an_untouched_plan_file_is_not_re_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    let session = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let path = dir.path().join(format!(".opencrabs_plan_{session}.json"));
    let pending = plan_with_task(session, task_id, TaskStatus::Pending);
    write_plan(&path, &pending);

    let mut panes = PaneManager::new();
    panes.set_pane_plan(PaneId::ROOT, Some(path.clone()), Some(pending));
    assert_eq!(
        panes.refresh_plans_changed_on_disk(),
        0,
        "the document was just read from this exact file"
    );

    let settled = plan_with_task(session, task_id, TaskStatus::Completed);
    write_plan(&path, &settled);
    bump_mtime(&path, 2);
    assert_eq!(
        panes.refresh_plans_changed_on_disk(),
        1,
        "a rewritten plan is one reload"
    );
    assert_eq!(
        panes.refresh_plans_changed_on_disk(),
        0,
        "and the same file is not read twice"
    );
}

/// A pane with no plan file costs nothing on the tick.
#[test]
fn panes_without_a_plan_path_are_skipped() {
    let mut panes = PaneManager::new();
    assert_eq!(panes.refresh_plans_changed_on_disk(), 0);
}

/// The tick's own body: refresh every pane, then count. Reading the reload
/// path out of `focused_plan_path()` is the regression this pins.
#[test]
fn the_tick_refreshes_every_pane_before_counting() {
    let body = STATE_SRC
        .split("pub(crate) async fn settle_plan_tasks(&mut self) -> usize {")
        .nth(1)
        .and_then(|tail| tail.split("\n    /// The chime itself").next())
        .expect("settle_plan_tasks exists in the tick");
    assert!(
        body.contains("refresh_plans_changed_on_disk()"),
        "the tick must reload every pane from its own file first: {body}"
    );
    assert!(
        body.contains("bind_missing_plan_paths().await"),
        "a restored split has panes with a session and no plan path: {body}"
    );
    assert!(
        !body.contains("focused_plan_path()"),
        "resolving the reload through the focused pane is the bug (#2733)"
    );
}
