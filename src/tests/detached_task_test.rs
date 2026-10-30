//! #2145: a detached task's start and finish reach the surface as progress
//! events carrying ONE stable id, so the finish updates the row the start
//! already drew instead of opening a second message and a second block.
//!
//! Before this, a detach emitted nothing at all: `spawn_inner` registered the
//! task and returned, the turn ended, and the outcome arrived later as a
//! standalone push. Every surface therefore had no row to update and no id to
//! address one with, which is what these tests pin.

use crate::brain::agent::ProgressCallback;
use crate::brain::agent::ProgressEvent;
use crate::brain::agent::service::MessageEnqueueCallback;
use crate::brain::agent::service::QueuedUserMessage;
use crate::brain::agent::service::background_tasks::BackgroundTaskManager;
use crate::brain::agent::service::restart_recovery;
use crate::brain::agent::service::session_routes;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// The events one task emitted, in order, each tagged with its session.
type EventLog = Arc<Mutex<Vec<(Uuid, ProgressEvent)>>>;

/// Collect every event a task emits, in order.
fn recorder() -> (EventLog, ProgressCallback) {
    let events: EventLog = Arc::new(Mutex::new(Vec::new()));
    let rec = events.clone();
    let cb: ProgressCallback = Arc::new(move |sid, ev| {
        rec.lock().unwrap().push((sid, ev));
    });
    (events, cb)
}

/// A route that records what the completion push delivers, so the fallback
/// path (no callback) can be asserted without a live channel.
fn route(recorded: Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>>) -> MessageEnqueueCallback {
    Arc::new(move |sid, msg| {
        recorded.lock().unwrap().push((sid, msg));
    })
}

/// Pull `(task_id, label)` out of the start event.
fn started(events: &[(Uuid, ProgressEvent)]) -> Option<(String, String)> {
    events.iter().find_map(|(_, ev)| match ev {
        ProgressEvent::DetachedTaskStarted { task_id, label } => {
            Some((task_id.clone(), label.clone()))
        }
        _ => None,
    })
}

/// Pull `(task_id, label, success, elapsed)` out of the finish event.
fn finished(events: &[(Uuid, ProgressEvent)]) -> Option<(String, String, bool, f32)> {
    events.iter().find_map(|(_, ev)| match ev {
        ProgressEvent::DetachedTaskFinished {
            task_id,
            label,
            success,
            elapsed_secs,
        } => Some((task_id.clone(), label.clone(), *success, *elapsed_secs)),
        _ => None,
    })
}

/// Criterion 1: the start and the finish report the SAME task id.
///
/// This is the whole mechanism. If the finish minted its own id, a surface
/// could not tell which row to update and would have to post a new message,
/// which is exactly the defect #2145 describes.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn start_and_finish_carry_the_same_task_id() {
    let _guard = restart_recovery::test_guard();
    let recorded: Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>> = Arc::new(Mutex::new(Vec::new()));
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    session_routes::register_session_route(sid, route(recorded));

    let (events, cb) = recorder();
    mgr.clone().spawn_command(
        sid,
        std::env::temp_dir(),
        "echo probe".to_string(),
        "echo DETACH_ID_MARKER".to_string(),
        Some(cb),
    );

    let mut waited = 0;
    while finished(&events.lock().unwrap()).is_none() && waited < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }

    let events = events.lock().unwrap();
    let (start_id, start_label) = started(&events).expect("a start event must be emitted");
    let (finish_id, finish_label, success, elapsed) =
        finished(&events).expect("a finish event must be emitted");

    assert_eq!(
        start_id, finish_id,
        "the finish must carry the id the start published, otherwise a \
         surface cannot find the row it already drew"
    );
    assert_eq!(start_label, "echo probe");
    assert_eq!(finish_label, "echo probe");
    assert!(success, "the marker command exits 0");
    assert!(elapsed >= 0.0);
    // One task, one identity: no second id was minted anywhere.
    assert_eq!(events.len(), 2, "exactly one start and one finish");
}

/// Criterion 2: the finish allocates NO new identity, because the id it
/// carries already addresses the live row and the receipt payload.
///
/// A surface needs exactly one thing to update in place: the id of the row
/// it drew. This asserts that id is the same in all three places it appears
/// (the start event, the running row, the completion receipt), so there is
/// no second address to invent a second message for.
///
/// Since #2218 the receipt push is the FALLBACK, sent only when the row is
/// gone: a row that is still there is settled by the finish event and gets no
/// second report. The row is dropped here on purpose, through the same seam a
/// surface uses, so the receipt path runs and the id it carries can be
/// compared with the one the start published.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn the_finish_addresses_the_row_the_start_already_created() {
    let _guard = restart_recovery::test_guard();
    let recorded: Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>> = Arc::new(Mutex::new(Vec::new()));
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    session_routes::register_session_route(sid, route(recorded.clone()));

    // Snapshot the live row ids at the moment the start event fires. The row
    // is registered before the callback runs, so this is deterministic, not a
    // race: it is the same ordering a surface sees.
    let row_ids: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
    let ids = row_ids.clone();
    let probe = mgr.clone();
    let cb: ProgressCallback = Arc::new(move |_, ev| {
        if matches!(ev, ProgressEvent::DetachedTaskStarted { .. }) {
            let mut got = probe
                .running_tasks(sid)
                .into_iter()
                .map(|t| t.task_id)
                .collect::<Vec<_>>();
            got.sort();
            *ids.lock().unwrap() = got;
        }
    });

    mgr.clone().spawn_command(
        sid,
        std::env::temp_dir(),
        "echo probe".to_string(),
        "sleep 1; echo ROW_ID_MARKER".to_string(),
        Some(cb),
    );
    // The row goes away before the command exits, so the fallback delivery is
    // the one that carries the receipt (#2218).
    mgr.mirror_finished(sid, "echo probe");

    let mut waited = 0;
    while recorded.lock().unwrap().is_empty() && waited < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }

    let meta = recorded
        .lock()
        .unwrap()
        .first()
        .and_then(|(_, m)| m.bg_meta.clone())
        .expect("the completion push carries its receipt payload");

    let row_ids = row_ids.lock().unwrap();
    assert_eq!(row_ids.len(), 1, "exactly one row exists while it runs");
    assert_eq!(
        row_ids[0].to_string(),
        meta.task_id,
        "the receipt must name the row it belongs to, so a surface updates \
         that row instead of posting the outcome as a new message"
    );
    assert!(
        !meta.task_id.is_empty(),
        "an empty id would force every surface back to the fallback path"
    );
}

/// The fallback still works: a task spawned with no callback (a surface that
/// has none, or an older caller) emits nothing, and its result is still
/// delivered as a push. Losing the result to gain a row would be a regression.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_task_without_a_callback_still_delivers_its_result() {
    let _guard = restart_recovery::test_guard();
    let recorded: Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>> = Arc::new(Mutex::new(Vec::new()));
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    session_routes::register_session_route(sid, route(recorded.clone()));

    mgr.clone().spawn_command(
        sid,
        std::env::temp_dir(),
        "echo probe".to_string(),
        "echo NO_CALLBACK_MARKER".to_string(),
        None,
    );

    let mut waited = 0;
    while recorded.lock().unwrap().is_empty() && waited < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }

    let r = recorded.lock().unwrap();
    assert_eq!(r.len(), 1, "the push must still arrive");
    assert!(r[0].1.context_text.contains("NO_CALLBACK_MARKER"));
    let meta = r[0].1.bg_meta.clone().expect("receipt payload present");
    assert_eq!(
        meta.task_id.len(),
        36,
        "even without a callback the receipt names a real task id"
    );
    assert_eq!(mgr.running_for(sid), 0, "the row is cleared on finish");
}

/// A mirrored external task (claude-cli) gets a row identity but emits no
/// events: the CLI owns the task, so there is nothing to update in place.
#[test]
fn mirrored_tasks_get_a_row_identity_but_emit_nothing() {
    let mgr = BackgroundTaskManager::new();
    let sid = Uuid::new_v4();
    mgr.mirror_started(sid, "cargo test");
    let rows = mgr.running_tasks(sid);
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].task_id, Uuid::nil());
    assert!(
        rows[0].progress.is_none(),
        "a mirrored row has no callback to emit through"
    );
    mgr.mirror_finished(sid, "cargo test");
    assert!(mgr.running_tasks(sid).is_empty());
}

/// A finish removes exactly its own row, not the oldest same-label row.
///
/// Two identical commands can run at once; the label-based removal cannot
/// tell them apart, which is why the finish addresses by id (#2145).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_finish_removes_its_own_row_not_a_same_label_sibling() {
    let _guard = restart_recovery::test_guard();
    let recorded: Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>> = Arc::new(Mutex::new(Vec::new()));
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    session_routes::register_session_route(sid, route(recorded.clone()));

    let (events, cb) = recorder();
    // One quick task, then a slower one under the SAME label.
    mgr.clone().spawn_command(
        sid,
        std::env::temp_dir(),
        "same label".to_string(),
        "true".to_string(),
        Some(cb.clone()),
    );
    mgr.clone().spawn_command(
        sid,
        std::env::temp_dir(),
        "same label".to_string(),
        "sleep 1".to_string(),
        Some(cb),
    );

    // The first finish must leave the second row standing.
    let mut waited = 0;
    while finished(&events.lock().unwrap()).is_none() && waited < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }
    assert_eq!(
        mgr.running_for(sid),
        1,
        "the sibling row must survive the first finish"
    );

    let mut waited = 0;
    while mgr.running_for(sid) > 0 && waited < 100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }
    assert_eq!(mgr.running_for(sid), 0, "both rows clear when both finish");
}
