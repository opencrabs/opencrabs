//! #722 phase 2: the background-task manager runs a command detached and, on
//! completion, enqueues a system QueuedUserMessage into the originating session.

use crate::brain::agent::PushOrigin;
use crate::brain::agent::service::MessageEnqueueCallback;
use crate::brain::agent::service::ProgressCallback;
use crate::brain::agent::service::ProgressEvent;
use crate::brain::agent::service::QueuedUserMessage;
use crate::brain::agent::service::background_tasks::{
    BackgroundTaskManager, ClaudeTurnTasks, CmdResult, claude_completion_message,
    claude_needs_survival_delivery, claude_task_label, clear_claude_mirrors_for_session,
    completion_message, format_elapsed, run_detached, short_label, tail_lines,
};
use crate::brain::agent::service::restart_recovery;
use crate::brain::agent::service::session_routes;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[test]
fn tail_keeps_last_n_lines() {
    let text = (1..=100)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let tail = tail_lines(&text, 3);
    assert_eq!(tail, "98\n99\n100");
    // Fewer lines than n -> whole text.
    assert_eq!(tail_lines("a\nb", 10), "a\nb");
}

#[test]
fn completion_message_reflects_success_and_failure() {
    let ok = completion_message(
        "t-ok",
        "cargo test",
        "cargo test --all-features",
        &CmdResult {
            success: true,
            code: 0,
            output: "test result: ok. 5 passed".into(),
        },
        12.0,
    );
    assert!(ok.context_text.contains("exit 0 (success)"));
    assert!(ok.context_text.contains("cargo test --all-features"));
    assert!(ok.context_text.contains("Do not re-run"));
    assert!(ok.display_text.contains("finished"));
    // #15: the typed receipt payload rides along for the echo card.
    let meta = ok.bg_meta.expect("bg completion carries BgTaskMeta");
    assert!(meta.success);
    assert_eq!(meta.label, "cargo test");
    assert_eq!(meta.elapsed_secs, 12.0);
    assert_eq!(meta.tail, "test result: ok. 5 passed");

    let fail = completion_message(
        "t-fail",
        "build",
        "cargo build",
        &CmdResult {
            success: false,
            code: 101,
            output: "error[E0001]".into(),
        },
        3.0,
    );
    assert!(fail.context_text.contains("exit 101 (failure)"));
    assert!(fail.display_text.contains("failed"));
    let meta = fail.bg_meta.expect("failed completion still carries meta");
    assert!(!meta.success);
    assert_eq!(meta.elapsed_secs, 3.0);
}

#[test]
fn format_elapsed_buckets_match_the_spec() {
    assert_eq!(format_elapsed(0.4), "0s");
    assert_eq!(format_elapsed(42.0), "42s");
    assert_eq!(format_elapsed(59.6), "1m 0s"); // rounds up into the minute bucket
    assert_eq!(format_elapsed(185.0), "3m 5s");
    assert_eq!(format_elapsed(3599.4), "59m 59s");
    assert_eq!(format_elapsed(3720.0), "1h 2m");
}

#[test]
fn short_label_takes_the_command_after_cd() {
    assert_eq!(short_label("cd ~/proj && cargo test"), "cargo test");
    assert_eq!(short_label("cargo build"), "cargo build");
}

#[tokio::test]
// The test_guard serializes suites touching the process-global parked-queue
// state; holding it across the polling `.await`s below is the entire point —
// same shape as the session_notify suite (#22).
#[allow(clippy::await_holding_lock)]
async fn spawn_command_enqueues_on_completion() {
    // register_session_route → claim_session touches the process-global
    // parked-queue state, so serialize against other suites that do too.
    let _guard = restart_recovery::test_guard();
    #[allow(clippy::type_complexity)]
    let recorded: Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = recorded.clone();
    let enqueue: MessageEnqueueCallback = Arc::new(move |sid, msg| {
        rec.lock().unwrap().push((sid, msg));
    });

    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    // The manager no longer carries its own route (fork #19 — delivery goes
    // through the one gated route, which resolves the session's registered
    // route), so claim the session the way a channel would: register the
    // recording callback as its route.
    session_routes::register_session_route(sid, enqueue);
    let cwd = std::env::temp_dir();

    mgr.clone().spawn_command(
        sid,
        cwd,
        "echo probe".to_string(),
        "echo BG_DONE_MARKER".to_string(),
        None,
    );

    // Wait (bounded) for the detached command to finish and enqueue.
    let mut waited = 0;
    while recorded.lock().unwrap().is_empty() && waited < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }

    let r = recorded.lock().unwrap();
    assert_eq!(r.len(), 1, "completion should have enqueued exactly once");
    assert_eq!(r[0].0, sid);
    assert!(r[0].1.context_text.contains("BG_DONE_MARKER"));
    assert!(r[0].1.context_text.contains("exit 0 (success)"));
    // Running count drops back to zero after completion.
    assert_eq!(mgr.running_for(sid), 0);
}

/// #1748: `spawn_command_with_hook` hands the outcome to the hook INSTEAD of
/// the generic session delivery; a hook that replaces the process on success
/// would orphan an in-memory enqueue mid-flight. Same lifecycle (timer, status
/// file, DB accounting), different completion route.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn spawn_command_with_hook_replaces_generic_delivery() {
    // register_session_route → claim_session touches the process-global
    // parked-queue state, so serialize against other suites that do too.
    let _guard = restart_recovery::test_guard();
    #[allow(clippy::type_complexity)]
    let recorded: Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = recorded.clone();
    let enqueue: MessageEnqueueCallback = Arc::new(move |sid, msg| {
        rec.lock().unwrap().push((sid, msg));
    });

    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    session_routes::register_session_route(sid, enqueue);

    // The hook records what it learned; the session route must stay silent.
    let hooked: Arc<Mutex<Option<(bool, bool)>>> = Arc::new(Mutex::new(None));
    let hk = hooked.clone();
    let hook: crate::brain::agent::service::background_tasks::CompletionHook = Box::new(
        move |ctx: crate::brain::agent::service::background_tasks::HookContext| {
            Box::pin(async move {
                *hk.lock().unwrap() = Some((ctx.result.success, ctx.elapsed_secs >= 0.0));
            })
        },
    );

    mgr.clone().spawn_command_with_hook(
        sid,
        std::env::temp_dir(),
        "hook probe".to_string(),
        "echo HOOK_DONE_MARKER".to_string(),
        hook,
        None,
    );

    // Wait (bounded) for the detached command to finish and fire the hook.
    let mut waited = 0;
    while hooked.lock().unwrap().is_none() && waited < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }

    let h = hooked.lock().unwrap();
    let (success, elapsed_ok) = h.expect("hook must fire on completion");
    assert!(success, "echo must report success to the hook");
    assert!(elapsed_ok, "hook carries the command runtime");

    // The generic delivery did NOT run: the session route got nothing.
    assert!(
        recorded.lock().unwrap().is_empty(),
        "a hook replaces generic session delivery"
    );
    // Running count drops back to zero after completion.
    assert_eq!(mgr.running_for(sid), 0);
}

// --- #2218: one task, one report ---

/// #2218: a surface that drew the row has already reported the outcome.
/// Delivering it again sent the same completion as a second message, and
/// because an idle session resumes on delivery, that message opened a whole
/// agent turn with tools attached. One `cargo test` produced a row, a
/// "background task finished" line, and a turn.
///
/// This asserts the suppression itself rather than the wording: the session
/// route must stay silent while the finish reaches the callback that owns the
/// row.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_settled_row_suppresses_the_second_report() {
    let _guard = restart_recovery::test_guard();
    #[allow(clippy::type_complexity)]
    let recorded: Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = recorded.clone();
    let enqueue: MessageEnqueueCallback = Arc::new(move |sid, msg| {
        rec.lock().unwrap().push((sid, msg));
    });

    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    session_routes::register_session_route(sid, enqueue);

    // What a surface that draws rows hands the manager: the turn's progress
    // callback, kept alive past the turn that spawned the task.
    #[allow(clippy::type_complexity)]
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    let progress: ProgressCallback = Arc::new(move |_sid, event| {
        ev.lock().unwrap().push(event);
    });

    mgr.clone().spawn_command(
        sid,
        std::env::temp_dir(),
        "row probe".to_string(),
        "echo ROW_DONE_MARKER".to_string(),
        Some(progress),
    );

    let mut waited = 0;
    while events.lock().unwrap().len() < 2 && waited < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }

    let ids = {
        let seen = events.lock().unwrap();
        assert_eq!(
            seen.len(),
            2,
            "exactly a start and one finish reach the surface, got {seen:?}"
        );
        match (&seen[0], &seen[1]) {
            (
                ProgressEvent::DetachedTaskStarted { task_id, .. },
                ProgressEvent::DetachedTaskFinished {
                    task_id: finished, ..
                },
            ) => (task_id.clone(), finished.clone()),
            other => panic!("expected a start then a finish, got {other:?}"),
        }
    };
    assert_eq!(
        ids.0, ids.1,
        "the finish must settle the row the start drew"
    );

    // Same window the other delivery tests allow: a push that should not
    // happen has had ample time to show up here.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        recorded.lock().unwrap().is_empty(),
        "a completion the row already reported must not be delivered again"
    );
    assert_eq!(mgr.running_for(sid), 0);
}

/// #2218 fallback: the delivery is not gone, it is the fallback. A task whose
/// row disappeared before the finish (the surface went away, the process
/// restarted, no callback was ever registered) still has to reach the session,
/// or the outcome is lost with no report at all. The row is dropped through
/// the same seam a surface uses when it settles one.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_finish_with_no_row_still_delivers() {
    let _guard = restart_recovery::test_guard();
    #[allow(clippy::type_complexity)]
    let recorded: Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = recorded.clone();
    let enqueue: MessageEnqueueCallback = Arc::new(move |sid, msg| {
        rec.lock().unwrap().push((sid, msg));
    });

    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    session_routes::register_session_route(sid, enqueue);

    #[allow(clippy::type_complexity)]
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    let progress: ProgressCallback = Arc::new(move |_sid, event| {
        ev.lock().unwrap().push(event);
    });

    // Long enough that the row can be dropped before the command exits.
    mgr.clone().spawn_command(
        sid,
        std::env::temp_dir(),
        "gone probe".to_string(),
        "sleep 1; echo NO_ROW_MARKER".to_string(),
        Some(progress),
    );
    mgr.mirror_finished(sid, "gone probe");

    let mut waited = 0;
    while recorded.lock().unwrap().is_empty() && waited < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }

    let r = recorded.lock().unwrap();
    assert_eq!(
        r.len(),
        1,
        "a finish with no row must deliver exactly once, got {}",
        r.len()
    );
    assert!(r[0].1.context_text.contains("NO_ROW_MARKER"));
    drop(r);

    // No finish reached the surface: the row it would have settled was gone.
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .all(|e| !matches!(e, ProgressEvent::DetachedTaskFinished { .. })),
        "a dropped row must not receive a finish"
    );
    assert_eq!(mgr.running_for(sid), 0);
}

// --- #1776 seam 2: mirrored claude-cli task lifecycle ---

#[test]
fn claude_task_label_carries_source_tag() {
    assert_eq!(claude_task_label("btglunt18"), "claude-cli btglunt18");
}

#[test]
fn mirror_registers_then_removes() {
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    let label = claude_task_label("btglunt18");

    // Before start: nothing running for the session.
    assert_eq!(mgr.running_for(sid), 0);

    mgr.mirror_started(sid, &label);
    assert_eq!(mgr.running_for(sid), 1);
    let rows = mgr.running_tasks(sid);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].label, "claude-cli btglunt18",
        "row carries the source tag"
    );

    // The completed notification removes the shadow row.
    mgr.mirror_finished(sid, &label);
    assert_eq!(mgr.running_for(sid), 0);
    assert!(mgr.running_tasks(sid).is_empty());
}

#[test]
fn mirror_finish_is_noop_for_unknown_task() {
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();

    // A notification for a task we never saw started (mid-session attach):
    // must not panic and must not conjure a row.
    mgr.mirror_finished(sid, &claude_task_label("ghost01"));
    assert_eq!(mgr.running_for(sid), 0);
}

#[test]
fn mirror_finish_removes_oldest_on_duplicate_labels() {
    let mgr = Arc::new(BackgroundTaskManager::new());
    let sid = Uuid::new_v4();
    let label = claude_task_label("btglunt18");

    // Two tasks with the same id-shaped label: the manager cannot tell them
    // apart (same rule as spawned commands with identical labels), so the
    // first finish drops the OLDEST row and the survivor stays visible.
    mgr.mirror_started(sid, &label);
    std::thread::sleep(std::time::Duration::from_millis(5));
    mgr.mirror_started(sid, &label);
    assert_eq!(mgr.running_for(sid), 2);

    mgr.mirror_finished(sid, &label);
    assert_eq!(mgr.running_for(sid), 1, "oldest row removed, survivor kept");

    mgr.mirror_finished(sid, &label);
    assert_eq!(mgr.running_for(sid), 0, "second finish clears the survivor");
}

#[test]
fn survivor_delivery_fires_only_for_tasks_not_started_this_turn() {
    let sid = Uuid::new_v4();
    let other = Uuid::new_v4();
    let mut turn_started = ClaudeTurnTasks::default();

    // Empty marker set: every notification is a post-exit survivor.
    assert!(claude_needs_survival_delivery(
        &turn_started,
        sid,
        "bAAA1111"
    ));

    // Started THIS turn: silent, claude sees the notification natively.
    turn_started.insert((sid, "bAAA1111".to_string()));
    assert!(!claude_needs_survival_delivery(
        &turn_started,
        sid,
        "bAAA1111"
    ));

    // Session isolation: the same task id under another session, or another
    // task under this session, is still a survivor.
    turn_started.insert((other, "bAAA1111".to_string()));
    turn_started.insert((sid, "bBBB2222".to_string()));
    assert!(claude_needs_survival_delivery(
        &turn_started,
        other,
        "bCCC3333"
    ));
    assert!(claude_needs_survival_delivery(
        &turn_started,
        sid,
        "bDDD4444"
    ));
}

#[test]
fn claude_completion_message_carries_status_and_summary_mechanically() {
    let msg = claude_completion_message("bAAA1111", "completed", Some("tests passed"));
    assert!(
        msg.context_text.contains("claude-cli bAAA1111"),
        "label in context"
    );
    assert!(msg.context_text.contains("Status: completed"));
    assert!(msg.context_text.contains("Summary: tests passed"));
    assert!(msg.context_text.contains("Do not re-run the task"));
    assert_eq!(
        msg.display_text,
        "🔧 background task finished: claude-cli bAAA1111"
    );
    assert_eq!(msg.origin, PushOrigin::BackgroundTask, "#1221 echo tag");
    assert!(msg.bg_meta.is_none(), "no CmdResult, no fabricated receipt");
}

#[test]
fn claude_completion_message_failed_wording_and_summary_fallback() {
    let msg = claude_completion_message("bBBB2222", "failed", None);
    assert!(msg.context_text.contains("Status: failed"));
    assert!(msg.context_text.contains("Summary: (no summary provided)"));
    assert_eq!(
        msg.display_text,
        "🔧 background task failed: claude-cli bBBB2222"
    );
}

// --- #1984: mirror rows must not outlive their notification or their ----
// --- process, and the detached wait must never be open-ended --------------

#[test]
fn task_notification_clears_the_row_for_any_status() {
    // The old match arm removed the mirror row only for completed|failed, so
    // a task claude reports with any OTHER terminal status (stopped,
    // cancelled, killed) left the row alive forever, and that stuck row is
    // what kept the typing tick pinging. The arm now keys the clear on the
    // SUBTYPE; only the synthetic result wake stays status-limited. The arm
    // lives inside the 1200-line stream loop (no provider harness exists to
    // drive it), so this is a source pin, the same shape the WhatsApp parity
    // test (#1989) uses, and the clearing itself is behavioral in
    // `process_exit_clears_the_sessions_mirrors_and_markers_only` below.
    let src = include_str!("../brain/agent/service/helpers.rs");
    assert!(
        src.contains("(\"task_notification\", st)"),
        "any task_notification must clear the mirror row, not only the two \
         statuses the old arm enumerated"
    );
    assert!(
        !src.contains("(\"task_notification\", Some(s @ (\"completed\" | \"failed\")))"),
        "the status-filtered clear is the #1984 stuck-row bug, restored"
    );
    assert!(
        src.contains("matches!(st, Some(\"completed\") | Some(\"failed\"))"),
        "the synthetic survival wake must stay limited to result statuses"
    );
}

#[test]
fn process_exit_clears_the_sessions_mirrors_and_markers_only() {
    // The other stuck-row source: a notification that never arrives (task
    // stopped, CLI died). The exit guard drains the marker set, and every
    // marker IS a row this process mirrored in, so rows and markers leave
    // together. Another session's mid-turn rows must survive untouched.
    let mgr = BackgroundTaskManager::new();
    let sid_a = Uuid::new_v4();
    let sid_b = Uuid::new_v4();
    let mut tasks: ClaudeTurnTasks = ClaudeTurnTasks::new();
    for (sid, id) in [(sid_a, "a1"), (sid_a, "a2"), (sid_b, "b1")] {
        tasks.insert((sid, id.to_string()));
        mgr.mirror_started(sid, &claude_task_label(id));
    }
    assert_eq!(mgr.running_for(sid_a), 2);
    clear_claude_mirrors_for_session(&mut tasks, &mgr, sid_a);
    assert_eq!(
        mgr.running_for(sid_a),
        0,
        "a session's mirrors must not outlive its process"
    );
    assert_eq!(mgr.running_for(sid_b), 1, "other sessions are untouched");
    assert!(
        !tasks.iter().any(|(s, _)| *s == sid_a),
        "A's markers go with its rows"
    );
    assert_eq!(tasks.len(), 1, "B's marker stays");
}

/// The wait-what-the-shell-owns behavior of #1984. These are shell job
/// semantics (`&`), so unix.
#[cfg(unix)]
#[tokio::test]
async fn detached_run_does_not_wait_on_survivor_output() {
    // `sleep 10 &` leaves a child holding the write end of the output. The
    // old `.output()` waited for EOF on that pipe, so it waited on a shell
    // that had ALREADY exited, and a long-lived server held the tracker row
    // (and every typing tick counting it) open forever. Waiting the shell
    // only returns the moment `echo done` has run.
    let sid = Uuid::new_v4();
    let started = std::time::Instant::now();
    let result = run_detached(
        "sleep 10 & echo done",
        &std::env::temp_dir(),
        sid,
        std::time::Duration::from_secs(30),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(result.success, "the shell exited 0: {}", result.output);
    assert!(
        result.output.contains("done"),
        "output must be captured: {:?}",
        result.output
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the shell exited at once; waiting {elapsed:?} means the wait is on \
         the survivor's inherited output, not on the shell (#1984)"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn detached_run_stops_waiting_at_its_ceiling() {
    // The other half: a foreground command that never exits (a dev server
    // without `&`) used to hold its row open forever. The ceiling cuts the
    // wait, marks the task failed, and still reports what printed before.
    let sid = Uuid::new_v4();
    let started = std::time::Instant::now();
    let result = run_detached(
        "echo starting; sleep 30",
        &std::env::temp_dir(),
        sid,
        std::time::Duration::from_millis(300),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(!result.success, "a ceiling cut is not a success");
    assert!(
        result.output.contains("ceiling"),
        "the receipt must say why: {:?}",
        result.output
    );
    assert!(
        result.output.contains("starting"),
        "what ran before the cut is still reported"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the ceiling must end the wait, not sleep 30 ({elapsed:?})"
    );
}
