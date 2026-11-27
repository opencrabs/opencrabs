//! Watch the work a CLI turn left running after it ended (#3219).
//!
//! When a CLI provider's process ends with its turn, anything one of its tools
//! detached keeps running with nobody holding it: no row, no completion. This
//! adopts that work. A short grace after the process exits, it picks up the
//! `systemd-run` units the turn announced and the processes still tagged with
//! the session (see [`crate::brain::detached`]), opens a mirrored row for
//! each, and polls until it ends. The end is delivered to the session the same
//! way a post-exit claude task is (#1776), so the user hears about it without
//! asking.
//!
//! The wait is owned here, in this process, which is what lets it outlive the
//! turn and the CLI process that started the work.

use std::sync::Arc;
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::background_tasks::{BackgroundTaskManager, format_elapsed, output_tail_within};
use super::types::{PushOrigin, QueuedUserMessage};
use crate::brain::detached::{self, ProcId, UnitState};

/// How long after the CLI exits before looking for survivors. A process that
/// was merely winding down with the CLI (an MCP server, a tool's pipe) is gone
/// by then, so it is not mistaken for work the turn left behind.
const GRACE: Duration = Duration::from_secs(10);

/// How often a watched process or unit is checked.
const POLL: Duration = Duration::from_secs(5);

/// Consecutive `systemctl` failures after which a unit stops being watched.
const UNIT_READ_FAILURES: u32 = 3;

/// Output lines quoted in a completion.
const TAIL_LINES: usize = 30;

/// Adopt what the CLI turn of `session_id` left running. Returns at once;
/// the search and every watch run on the current tokio runtime. Outside a
/// runtime (a synchronous test dropping a guard) there is nothing to run on,
/// and nothing is adopted.
pub(crate) fn adopt_after_exit(mgr: Arc<BackgroundTaskManager>, session_id: Uuid) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        tokio::time::sleep(GRACE).await;
        for unit in detached::take_units(session_id) {
            watch_unit(mgr.clone(), session_id, unit);
        }
        let found = tokio::task::spawn_blocking(move || detached::survivors(session_id))
            .await
            .unwrap_or_default();
        for id in found {
            watch_process(mgr.clone(), session_id, id);
        }
    });
}

/// Watch one detached process until it exits.
fn watch_process(mgr: Arc<BackgroundTaskManager>, session_id: Uuid, id: ProcId) {
    let identity = id.identity();
    if !detached::claim(&identity) {
        return;
    }
    let command = detached::cmdline(id.pid, 80);
    let label = if command.is_empty() {
        format!("detached pid {}", id.pid)
    } else {
        format!("detached: {command}")
    };
    let log = detached::stdout_file(id.pid);
    mgr.mirror_started_external(session_id, identity.clone(), label.clone(), log.clone());
    tokio::spawn(async move {
        let watched = Instant::now();
        while detached::alive(id) {
            tokio::time::sleep(POLL).await;
        }
        mgr.mirror_finished(session_id, &identity);
        detached::release(&identity);
        let tail = log
            .as_deref()
            .map(|path| output_tail_within(path, TAIL_LINES, 64 * 1024));
        let msg = completion_message(
            &label,
            "exited. Its exit code is not visible here: the process was not \
             started by this one, so check the output below for a status line",
            tail.as_deref(),
            watched.elapsed().as_secs_f32(),
        );
        deliver(session_id, msg, &label);
    });
}

/// Watch one `systemd-run` unit until it stops.
fn watch_unit(mgr: Arc<BackgroundTaskManager>, session_id: Uuid, unit: String) {
    let identity = format!("unit:{unit}");
    if !detached::claim(&identity) {
        return;
    }
    let label = format!("unit: {unit}");
    mgr.mirror_started_external(session_id, identity.clone(), label.clone(), None);
    tokio::spawn(async move {
        let watched = Instant::now();
        let mut failures = 0;
        let state = loop {
            match unit_state(&unit).await {
                Some(state) if state.ended() => break Some(state),
                Some(_) => failures = 0,
                None => {
                    failures += 1;
                    if failures >= UNIT_READ_FAILURES {
                        break None;
                    }
                }
            }
            tokio::time::sleep(POLL).await;
        };
        mgr.mirror_finished(session_id, &identity);
        detached::release(&identity);
        let outcome = state.map(|s| s.outcome()).unwrap_or_else(|| {
            "unknown: systemctl could not read the unit, so it stopped being watched".to_string()
        });
        let tail = journal_tail(&unit).await;
        let msg = completion_message(
            &label,
            &outcome,
            tail.as_deref(),
            watched.elapsed().as_secs_f32(),
        );
        deliver(session_id, msg, &label);
    });
}

/// The unit's state, or `None` when `systemctl` could not be run.
async fn unit_state(unit: &str) -> Option<UnitState> {
    let out = tokio::process::Command::new("systemctl")
        .args(["--user", "show", unit])
        .args(["-p", "LoadState", "-p", "ActiveState"])
        .args(["-p", "Result", "-p", "ExecMainStatus"])
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| UnitState::parse(&String::from_utf8_lossy(&out.stdout)))
}

/// The unit's last journal lines, when it logged to the journal.
async fn journal_tail(unit: &str) -> Option<String> {
    let lines = TAIL_LINES.to_string();
    let out = tokio::process::Command::new("journalctl")
        .args([
            "--user",
            "-u",
            unit,
            "-n",
            &lines,
            "--no-pager",
            "-o",
            "cat",
        ])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// Deliver a completion to its session, logging when it had nowhere to go.
fn deliver(session_id: Uuid, msg: QueuedUserMessage, label: &str) {
    match super::session_routes::deliver_to_session(session_id, msg, true) {
        super::session_routes::Delivery::NoRoute => tracing::warn!(
            target: "background_task",
            "detached work '{label}' ended for session {session_id} with nowhere to deliver"
        ),
        _ => tracing::info!(
            target: "background_task",
            "delivered the end of detached work '{label}' for session {session_id}"
        ),
    }
}

/// The message that tells the session a piece of detached work has ended.
///
/// Mechanical context only. No `BgTaskMeta`: there is no `CmdResult` for work
/// this process never spawned, and the time given is how long it was watched,
/// which is a floor on its runtime, not the runtime.
pub(crate) fn completion_message(
    label: &str,
    outcome: &str,
    tail: Option<&str>,
    watched_secs: f32,
) -> QueuedUserMessage {
    let output = tail
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("(no output known: it did not write to a log file or the journal)");
    let context = format!(
        "[System: work a turn left running after it ended has finished.\n\
         Task: {label}\n\
         Status: {outcome}\n\
         Watched for: {}\n\
         Output (last lines):\n{output}\n\n\
         Report the result to the user and continue anything that was waiting \
         on it. Do not re-run it.]",
        format_elapsed(watched_secs),
    );
    let display = format!("🔧 background task ended: {label}");
    let mut msg = QueuedUserMessage::system(context, display);
    // #1221: marks this delivery for the Telegram collapsible echo bubble.
    msg.origin = PushOrigin::BackgroundTask;
    msg
}
