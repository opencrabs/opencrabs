//! Background task manager (#722).
//!
//! Runs a genuinely long command detached (so it doesn't churn the bash 600s
//! cap) and, on completion, enqueues a synthetic `QueuedUserMessage` into the
//! originating session via the surface enqueue callback. The tool loop drains
//! that at the next iteration boundary — injected mid-turn if the agent is still
//! working, or starting a fresh turn if it went idle.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use uuid::Uuid;

use super::types::{BgTaskMeta, ProgressCallback, ProgressEvent, PushOrigin, QueuedUserMessage};

/// Result of a finished background command.
#[derive(Debug, Clone)]
pub struct CmdResult {
    pub success: bool,
    pub code: i32,
    pub output: String,
}

/// Everything a completion hook learns about a finished command: the result
/// plus the wall-clock runtime the generic receipt carries (#15).
pub struct HookContext {
    pub result: CmdResult,
    pub elapsed_secs: f32,
}

/// Post-completion action run INSTEAD of the generic session delivery
/// (#1748). The detached rebuild uses it to exec-restart into the fresh
/// binary: an in-memory enqueue would be orphaned the moment exec()
/// replaces the process, so the hook delivers its own outcome text through
/// the routes it needs (session route, channel targets).
pub type CompletionHook = Box<
    dyn FnOnce(HookContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send,
>;

/// One in-flight background command.
#[derive(Clone)]
pub struct RunningTask {
    /// Stable id for this task (#2145).
    ///
    /// Minted once at spawn and carried by both progress events, so a finish
    /// finds the row its start created. Mirrored external tasks (a claude-cli
    /// background task) get an id too; nothing emits events for them, but the
    /// field stays non-optional so consumers never have to special-case it.
    pub task_id: Uuid,
    /// Short label for the command, e.g. `cargo test`.
    pub label: String,
    /// When it was spawned, for the elapsed time a surface displays.
    pub started: std::time::Instant,
    /// The turn's progress callback, kept alive past the turn that spawned
    /// the task (#2145).
    ///
    /// The turn ends while the command is still running, and with it the only
    /// route its events had. Holding the `Arc` here is what lets the finish
    /// reach the surface that drew the row. `None` for mirrored external
    /// tasks, which emit no events.
    pub progress: Option<ProgressCallback>,
}

impl std::fmt::Debug for RunningTask {
    /// Manual because the callback is a closure: there is no meaningful
    /// representation of it, and printing the label plus age is what a
    /// debugging reader actually wants (#2145).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningTask")
            .field("task_id", &self.task_id)
            .field("label", &self.label)
            .field("elapsed", &self.started.elapsed())
            .field("has_progress", &self.progress.is_some())
            .finish()
    }
}

/// Manages background commands and resumes their sessions on completion.
pub struct BackgroundTaskManager {
    /// In-flight background tasks per session.
    ///
    /// Holds the label and start time, not just a count, because a surface has
    /// to be able to say WHAT is running and for how long. A detached task
    /// takes the turn idle, so without this the TUI has nothing at all to draw
    /// while a long build runs and the wait looks like a hang (#762).
    running: Mutex<HashMap<Uuid, Vec<RunningTask>>>,
}

use super::work_status::{CommandExit, WorkStatus};

impl BackgroundTaskManager {
    pub fn new() -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
        }
    }

    /// How many background tasks are currently running for `session_id`.
    pub fn running_for(&self, session_id: Uuid) -> usize {
        self.running
            .lock()
            .map(|m| m.get(&session_id).map(Vec::len).unwrap_or(0))
            .unwrap_or(0)
    }

    /// What is running for `session_id`, oldest first, for surfaces that show
    /// progress. Returns owned data so the caller never holds the lock.
    pub fn running_tasks(&self, session_id: Uuid) -> Vec<RunningTask> {
        self.running
            .lock()
            .map(|m| m.get(&session_id).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    fn mark_started(
        &self,
        session_id: Uuid,
        task_id: Uuid,
        label: &str,
        progress: Option<ProgressCallback>,
    ) {
        if let Ok(mut m) = self.running.lock() {
            m.entry(session_id).or_default().push(RunningTask {
                task_id,
                label: label.to_string(),
                started: std::time::Instant::now(),
                progress,
            });
        }
    }

    /// The callback a task's finish must emit through (#2145), cloned out so
    /// the caller never holds the lock while it fires.
    fn progress_for(&self, session_id: Uuid, task_id: Uuid) -> Option<ProgressCallback> {
        self.running
            .lock()
            .ok()?
            .get(&session_id)?
            .iter()
            .find(|t| t.task_id == task_id)
            .and_then(|t| t.progress.clone())
    }

    fn mark_finished(&self, session_id: Uuid, label: &str) {
        if let Ok(mut m) = self.running.lock()
            && let Some(tasks) = m.get_mut(&session_id)
        {
            // Remove the OLDEST entry with this label: two `cargo test` runs are
            // indistinguishable here, and dropping the oldest keeps the elapsed
            // time shown for the survivor honest.
            if let Some(pos) = tasks.iter().position(|t| t.label == label) {
                tasks.remove(pos);
            }
            if tasks.is_empty() {
                m.remove(&session_id);
            }
        }
    }

    /// Remove the row for exactly `task_id` (#2145).
    ///
    /// The label-based [`Self::mark_finished`] cannot tell two runs of the same
    /// command apart, which is fine for a count but wrong once a finish has to
    /// address one specific row.
    fn mark_finished_by_id(&self, session_id: Uuid, task_id: Uuid) {
        if let Ok(mut m) = self.running.lock()
            && let Some(tasks) = m.get_mut(&session_id)
        {
            tasks.retain(|t| t.task_id != task_id);
            if tasks.is_empty() {
                m.remove(&session_id);
            }
        }
    }

    /// Mirror an EXTERNAL task's lifecycle into the tracker (#1776 seam 2):
    /// a claude-cli background task never runs as our spawned process, but
    /// surfaces still need to show it as in-flight work for the session —
    /// a backgrounded CLI task takes the turn idle and, without a row here,
    /// the wait looks like a hang (#762 is the same disease, spawned flavor).
    /// `label` is the mirror's identity: [`Self::mirror_finished`] removes
    /// the oldest row carrying it, exactly like [`Self::mark_finished`].
    ///
    /// No progress callback: the CLI owns the task and emits no events, so a
    /// mirrored row is never updated in place (#2145).
    pub fn mirror_started(&self, session_id: Uuid, label: &str) {
        self.mark_started(session_id, Uuid::new_v4(), label, None);
    }

    /// Remove a mirrored external task row (see [`Self::mirror_started`]).
    /// No-op when nothing matches: a `task_notification` may name a task we
    /// never saw started (mid-session attach, log replay) — that is not an
    /// error, there is just nothing to clean up.
    pub fn mirror_finished(&self, session_id: Uuid, label: &str) {
        self.mark_finished(session_id, label);
    }

    /// Spawn `command` (via `sh -c`) in `cwd`, detached; on completion enqueue a
    /// system message into `session_id` summarizing the result. Returns
    /// immediately — the caller's turn is free to end.
    ///
    /// `progress` is the CALLING TURN's callback (#2145). It is stored on the
    /// running row so the finish can still reach the surface after the turn
    /// that spawned the task has ended, and so the start lands inside that
    /// turn's own tool block instead of a new one.
    pub fn spawn_command(
        self: std::sync::Arc<Self>,
        session_id: Uuid,
        cwd: PathBuf,
        label: String,
        command: String,
        progress: Option<ProgressCallback>,
    ) {
        self.spawn_inner(session_id, cwd, label, command, None, progress);
    }

    /// Spawn like [`Self::spawn_command`], but hand the outcome to `hook`
    /// instead of the generic session delivery (#1748). Identical lifecycle:
    /// timer, status file, DB accounting; only the completion route differs.
    pub fn spawn_command_with_hook(
        self: std::sync::Arc<Self>,
        session_id: Uuid,
        cwd: PathBuf,
        label: String,
        command: String,
        hook: CompletionHook,
        progress: Option<ProgressCallback>,
    ) {
        self.spawn_inner(session_id, cwd, label, command, Some(hook), progress);
    }

    fn spawn_inner(
        self: std::sync::Arc<Self>,
        session_id: Uuid,
        cwd: PathBuf,
        label: String,
        command: String,
        hook: Option<CompletionHook>,
        progress: Option<ProgressCallback>,
    ) {
        let task_id = Uuid::new_v4();
        self.mark_started(session_id, task_id, &label, progress.clone());
        // #2145: emit the start NOW, while the caller's turn is still live, so
        // the surface draws the row in the block that holds the `bash` call
        // that detached this task. Emitted before the tokio spawn because the
        // spawn returns immediately and the turn may end before it runs.
        if let Some(cb) = progress.as_ref() {
            cb(
                session_id,
                ProgressEvent::DetachedTaskStarted {
                    task_id: task_id.to_string(),
                    label: label.clone(),
                },
            );
        }
        let this = std::sync::Arc::clone(&self);
        // Gap 2 (#1160): mid-run visibility. The status file exists from
        // spawn with label/command/session, so tasks_list consumers can see
        // what a detached command IS before it finishes. Best-effort: never
        // fatal to the command itself.
        if let Err(e) = WorkStatus::new_command(
            &task_id.to_string(),
            &session_id.to_string(),
            &label,
            &command,
        ) {
            tracing::warn!(
                target: "background_task",
                "Could not write detached status for {task_id}: {e}"
            );
        }
        tokio::spawn(async move {
            // Log the START as well as the finish. Only completions were
            // logged, so a task that never finished left no trace of having
            // begun, and reconstructing which commands got detached meant
            // inferring it from the completions that did arrive.
            tracing::info!(
                target: "background_task",
                "Background task '{label}' started for session {session_id} \
                 (id={task_id}, cwd={})",
                cwd.display()
            );
            // Persist BEFORE running: a restart mid-command must find a row to
            // report as interrupted, otherwise the session waits forever on a
            // resume that can no longer come (#763).
            if let Some(repo) = task_repo() {
                let cwd_str = cwd.to_string_lossy().to_string();
                if let Err(e) = repo
                    .record(task_id, session_id, &label, &command, &cwd_str)
                    .await
                {
                    // Not fatal: the command still runs and still resumes the
                    // session in this process. Only restart accounting is lost.
                    tracing::error!(
                        target: "background_task",
                        "Failed to persist background task '{label}': {e:#}"
                    );
                }
            }
            let started = std::time::Instant::now();
            let result = run_detached(&command, &cwd, session_id, DETACH_CEILING).await;
            // Capture ONCE: the log line, the status file and the receipt
            // payload (#15) must all report the same runtime.
            let elapsed_secs = started.elapsed().as_secs_f32();
            // Exit code and elapsed time, not just a boolean: how long a task
            // actually took is the only way to tell a correct detach from a
            // wasteful one, and it was nowhere in the log.
            tracing::info!(
                target: "background_task",
                "Background task '{label}' for session {session_id} finished \
                 (success={}, exit={}, elapsed={:.1}s)",
                result.success,
                result.code,
                elapsed_secs
            );
            // Gap 2 (#1160): rewrite the status file with exit info, so any
            // reader between process-exit and session-resume sees the
            // terminal state instead of a forever-running spawn record.
            if let Err(e) = WorkStatus::finish_command(
                &task_id.to_string(),
                &session_id.to_string(),
                &label,
                &command,
                CommandExit {
                    success: result.success,
                    code: result.code,
                    elapsed_secs,
                    output_bytes: result.output.len(),
                },
            ) {
                tracing::warn!(
                    target: "background_task",
                    "Could not write detached status for {task_id}: {e}"
                );
            }
            if let Some(repo) = task_repo()
                && let Err(e) = repo.clear(task_id).await
            {
                // A stale row makes the NEXT startup report a phantom
                // interruption, so this must be visible even though the
                // command itself succeeded.
                tracing::error!(
                    target: "background_task",
                    "Failed to clear background task '{label}' after completion: {e:#}"
                );
            }
            // Clear the indicator BEFORE delivering, not after. The task is
            // over the moment the process exits, but mark_finished sat behind
            // the enqueue callback, so the "running" badge outlived the work by
            // however long delivery took — on a killed task the user saw the
            // agent confirm it had stopped while the input border still showed
            // it running.
            //
            // Only touches the in-memory map, so moving it earlier cannot
            // affect what gets delivered.
            //
            // #2145: emit the finish through the callback stored on the row,
            // read BEFORE the row is dropped. This is what updates the row the
            // start created instead of announcing the outcome as a new
            // message.
            //
            // #2266: the row update is cosmetic, the receipt is functional.
            // #2218 made the delivery below a fallback that is skipped
            // whenever a row callback exists, but every interactive surface
            // registers one for as long as the turn's row lives, so the
            // receipt became dead code in practice: the agent never learned
            // the task finished, and the user became the polling layer. The
            // row settles here AND the completion is delivered below, both
            // every time. The turn that reports the outcome is the contract,
            // not a duplicate.
            if let Some(cb) = this.progress_for(session_id, task_id) {
                cb(
                    session_id,
                    ProgressEvent::DetachedTaskFinished {
                        task_id: task_id.to_string(),
                        label: label.clone(),
                        success: result.success,
                        elapsed_secs,
                    },
                );
            }
            this.mark_finished_by_id(session_id, task_id);
            if let Some(hook) = hook {
                // A hook replaces the generic delivery entirely (#1748): the
                // rebuild exec-replaces the process on success, so an
                // in-memory enqueue would be orphaned mid-flight. The hook
                // delivers its own outcome text through the routes it needs.
                hook(HookContext {
                    result,
                    elapsed_secs,
                })
                .await;
                return;
            }
            // Deliver through the ONE gated route (fork #19): the same
            // `deliver_to_session` that sub-agent completions and the
            // session_notify tool use, so channel-ownership, mid-turn and
            // redirect decisions live in exactly one place instead of being
            // re-derived per surface. Resolves the owner by SESSION, never by
            // whichever service executed the command — a channel session
            // driven from the TUI runs on the TUI's service, and the old
            // direct-resolve would answer into the TUI and leave the channel
            // that asked for the work waiting on a reply that never comes
            // (#940). interrupt=true: a completion is the origin's own
            // awaited work, exactly like a sub-agent's; it must reach it even
            // mid-turn (fork #13).
            let msg = completion_message(
                &task_id.to_string(),
                &label,
                &command,
                &result,
                elapsed_secs,
            );
            let outcome = super::session_routes::deliver_to_session(session_id, msg, true);
            match outcome {
                super::session_routes::Delivery::Redirected { to } => {
                    tracing::info!(
                        target: "background_task",
                        "Background task '{label}' completion for session {session_id} was \
                         redirected to session {to}, which now owns its channel"
                    );
                }
                super::session_routes::Delivery::Parked => {
                    tracing::info!(
                        target: "background_task",
                        "Background task '{label}' completion for session {session_id} is \
                         parked until its channel claims the session"
                    );
                }
                super::session_routes::Delivery::NoRoute => {
                    tracing::warn!(
                        target: "background_task",
                        "Background task '{label}' completion for session {session_id} had \
                         nowhere to go; the session will not hear about it"
                    );
                }
                super::session_routes::Delivery::RefusedInFlight { .. } => {
                    // Unreachable by construction: interrupt=true is passed
                    // above, so the fork #13 gate cannot refuse. Kept explicit
                    // so a future change to the flag cannot drop the outcome
                    // silently (port seam: upstream's match has no catch-all).
                    tracing::warn!(
                        target: "background_task",
                        "Background task '{label}' completion for session {session_id} was \
                         refused by the mid-turn gate despite interrupt=true"
                    );
                }
                super::session_routes::Delivery::Delivered => {}
            }
        });
    }
}

/// The background-task repository, when a pool exists.
///
/// Resolved per call through the global pool rather than threaded through the
/// manager, because `spawn_command` is reached from the bash tool which has no
/// pool in its context. `None` before the DB is initialized (early startup,
/// tests), which simply means restart accounting is skipped.
pub(super) fn task_repo() -> Option<crate::db::BackgroundTaskRepository> {
    crate::db::global_pool().map(|p| crate::db::BackgroundTaskRepository::new(p.clone()))
}

/// The hard ceiling on waiting for a detached command's shell (#1984).
///
/// The old wait was unbudgeted: a foregrounded never-exiting command (a dev
/// server without `&`, a watch loop) held its tracker row open forever, and
/// every typing tick counting that row pinged into eternity. The ceiling
/// matches the flow-ticker's 30 minutes: past that no one is still watching
/// this indicator, and an orphan row must not outlive the session's attention.
pub(crate) const DETACH_CEILING: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Run `command` through the platform shell (`cmd /C` on Windows, `sh -c`
/// elsewhere) in `cwd`, capturing merged stdout+stderr.
///
/// #1984 rewrote the capture. The old `.output()` waited for the pipe's EOF,
/// and a command that leaves children behind (`nohup x &`, a server backgrounded
/// in the same shell) hands that pipe to a child that outlives the shell, so
/// EOF never arrived, the wait never ended, `mark_finished` never ran, and the
/// row stayed live forever, exactly the stuck typing indicator this issue
/// reports. Output now goes to a temp file and the wait observes the SHELL
/// only, bounded by `ceiling`: a survivor may keep appending to the file, and
/// that is fine, because nobody waits on it any more.
pub(crate) async fn run_detached(
    command: &str,
    cwd: &std::path::Path,
    session_id: Uuid,
    ceiling: std::time::Duration,
) -> CmdResult {
    use crate::utils::shell::PushShellCommand;
    use tokio::process::Command;
    let (shell, shell_arg) = crate::utils::shell::shell_pair();

    let log_path = std::env::temp_dir().join(format!(
        "opencrabs-bg-{}-{}-{}.log",
        session_id.as_simple(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    ));
    let launch_fail = |note: String| {
        // Distinct from a command that ran and failed: nothing executed at
        // all, so the exit code below is not one the command produced.
        tracing::error!(
            target: "background_task",
            "Background command could not be launched in {}: {note}",
            cwd.display()
        );
        CmdResult {
            success: false,
            code: -1,
            output: format!("failed to launch: {note}"),
        }
    };
    let log = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(e) => return launch_fail(e.to_string()),
    };
    // The clone shares the file descriptor, so stdout and stderr interleave
    // at one offset the same way the merged pipe used to.
    let stderr_log = match log.try_clone() {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&log_path);
            return launch_fail(e.to_string());
        }
    };
    let mut cmd = Command::new(shell);
    cmd.push_shell_command(shell_arg, command)
        .current_dir(cwd)
        .env("OPENCRABS_SESSION_ID", session_id.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(stderr_log)
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&log_path);
            return launch_fail(e.to_string());
        }
    };

    let outcome = match tokio::time::timeout(ceiling, child.wait()).await {
        Ok(Ok(status)) => CmdResult {
            success: status.success(),
            code: status.code().unwrap_or(-1),
            output: read_log_tail(&log_path),
        },
        Ok(Err(e)) => {
            tracing::error!(
                target: "background_task",
                "Background command could not be waited on in {}: {e}",
                cwd.display()
            );
            CmdResult {
                success: false,
                code: -1,
                output: format!("failed to run: {e}"),
            }
        }
        Err(_) => {
            // Ceiling reached. Kill the shell now rather than on drop: the
            // tracker row closes with this result, so nothing waits on the
            // dead task any longer. Detached grandchildren (the `nohup` kind
            // this is for) keep running with their file fd, unbothered.
            let _ = child.start_kill();
            let _ = child.wait().await;
            CmdResult {
                success: false,
                code: -1,
                output: format!(
                    "[detached ceiling reached after {}s; the command was \
                     stopped and may have been still running]\n{}",
                    ceiling.as_secs(),
                    read_log_tail(&log_path),
                ),
            }
        }
    };
    // Best-effort cleanup. On Windows a survivor still holding the file makes
    // the remove fail, which leaks one temp log rather than losing the receipt.
    let _ = std::fs::remove_file(&log_path);
    outcome
}

/// The last 64 KiB of a detached command's log, starting on a whole line.
/// Bounded because a chatty survivor can append for the entire ceiling; the
/// completion receipt only ever shows the tail anyway.
fn read_log_tail(path: &std::path::Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const MAX_BYTES: u64 = 64 * 1024;
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    if size > MAX_BYTES && file.seek(SeekFrom::End(-(MAX_BYTES as i64))).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if size > MAX_BYTES
        && let Some(nl) = text.find('\n')
    {
        // The seek landed mid-line; drop the fragment.
        text = text[nl + 1..].to_string();
    }
    text
}

/// A short human label for a command (first meaningful token sequence), for the
/// "running in the background" acknowledgement and the completion tag.
pub(crate) fn short_label(command: &str) -> String {
    let after_cd = crate::utils::command_label::command_label(command);
    let label: String = after_cd.chars().take(60).collect();
    if after_cd.chars().count() > 60 {
        format!("{label}…")
    } else {
        label
    }
}

/// The mirror row's label for a claude-cli background task (#1776 seam 2):
/// the source tag plus the CLI's own task id, so a surface can tell a
/// mirrored CLI task from a spawned command at a glance and the removal
/// side can key on the identical string.
pub(crate) fn claude_task_label(task_id: &str) -> String {
    format!("claude-cli {task_id}")
}

/// Per-turn marker set for claude-cli background tasks (#1776 seam 3):
/// `(session_id, task_id)` pairs started during the CURRENT turn. Cleared at
/// `run_tool_loop_inner` entry and when the CLI stream ends (#1984), so a
/// notification for a task absent from the set is a post-exit survivor and
/// must be delivered synthetically, while a task present stays silent
/// (claude sees those natively mid-turn).
pub(crate) type ClaudeTurnTasks = std::collections::HashSet<(Uuid, String)>;

/// Drop every mirrored row a claude process still owned when it exits
/// (#1984). The marker set is exactly the rows this process mirrored in, so
/// clearing them on process exit means one lost notification can no longer
/// hold a typing tick open forever. A task whose completion genuinely arrives
/// in a LATER turn is unaffected: the synthetic survival delivery keys on the
/// notification itself, and a row that is already gone makes the mirror-side
/// cleanup a harmless no-op (see [`BackgroundTaskManager::mirror_finished`]).
/// Other sessions' rows and markers are untouched.
pub(crate) fn clear_claude_mirrors_for_session(
    tasks: &mut ClaudeTurnTasks,
    mgr: &BackgroundTaskManager,
    session_id: Uuid,
) {
    tasks.retain(|(sid, id)| {
        if *sid != session_id {
            return true;
        }
        mgr.mirror_finished(*sid, &claude_task_label(id));
        false
    });
}

/// The seam-3 discriminator (#1776): a notification whose task was NOT
/// started during this turn is a post-exit survivor — the turn (and its
/// claude process) that spawned the task already ended, so nobody but this
/// synthetic delivery will carry the result to the user.
pub(crate) fn claude_needs_survival_delivery(
    turn_started: &ClaudeTurnTasks,
    session_id: Uuid,
    task_id: &str,
) -> bool {
    !turn_started.contains(&(session_id, task_id.to_string()))
}

/// The synthetic completion message for a post-exit claude task
/// notification (#1776 seam 3). Mechanical context only: the summary text is
/// the CLI's own, never model-judged. No `BgTaskMeta`: there is no
/// `CmdResult` for a task we never spawned, and a fabricated duration would
/// lie on the receipt card — the echo falls back to the display line.
pub(crate) fn claude_completion_message(
    task_id: &str,
    status: &str,
    summary: Option<&str>,
) -> QueuedUserMessage {
    let label = claude_task_label(task_id);
    let context = format!(
        "[System: a background claude task reported completion after the turn \
         that started it already ended.\n\
         Task: {label}\n\
         Status: {status}\n\
         Summary: {}\n\n\
         Deliver this result to the user and continue anything that was \
         waiting on it. Do not re-run the task.]",
        summary.unwrap_or("(no summary provided)"),
    );
    let display = format!(
        "🔧 background task {}: {label}",
        if status == "completed" {
            "finished"
        } else {
            "failed"
        }
    );
    let mut msg = QueuedUserMessage::system(context, display);
    // #1221: marks this delivery for the Telegram collapsible echo bubble.
    msg.origin = PushOrigin::BackgroundTask;
    msg
}

/// Keep only the last `n` lines of `text`.
pub(crate) fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Build the resume message from a finished background command (#722). Pure so
/// the framing is unit-testable without spawning anything. `elapsed_secs` is
/// the detached command's wall-clock runtime; it rides along in the typed
/// `BgTaskMeta` payload (#15) so the receipt card renders a duration without
/// parsing the context text.
pub(crate) fn completion_message(
    task_id: &str,
    label: &str,
    command: &str,
    result: &CmdResult,
    elapsed_secs: f32,
) -> QueuedUserMessage {
    let status = if result.success {
        "exit 0 (success)".to_string()
    } else {
        format!("exit {} (failure)", result.code)
    };
    let tail = tail_lines(&result.output, 50);
    let context = format!(
        "[System: the background task you started has finished.\n\
         Task: {label}\n\
         Command: {command}\n\
         Status: {status}\n\
         Output (last 50 lines):\n{tail}\n\n\
         Report the result to the user and continue anything that was waiting on it. \
         Do not re-run the command — this IS its result.]"
    );
    let display = format!(
        "🔧 background task {}: {label}",
        if result.success { "finished" } else { "failed" }
    );
    let mut msg = QueuedUserMessage::system(context, display);
    // #1221: marks this delivery for the Telegram collapsible echo bubble.
    msg.origin = PushOrigin::BackgroundTask;
    // #15: typed receipt payload — the echo renders the card from this,
    // never from the `[System: ...]` context text.
    msg.bg_meta = Some(BgTaskMeta {
        // #2145: the receipt names the task it belongs to, so a surface that
        // already drew a running row updates THAT row instead of posting the
        // outcome as a second message.
        task_id: task_id.to_string(),
        success: result.success,
        label: label.to_string(),
        elapsed_secs,
        tail,
    });
    msg
}

/// Human duration for the receipt card (#15): `42s`, `3m 5s`, `1h 12m`.
/// Rounds to whole seconds; sub-second tasks show `0s`.
pub(crate) fn format_elapsed(secs: f32) -> String {
    let total = secs.max(0.0).round() as u64;
    if total < 60 {
        format!("{total}s")
    } else if total < 3600 {
        format!("{}m {}s", total / 60, total % 60)
    } else {
        format!("{}h {}m", total / 3600, (total % 3600) / 60)
    }
}
