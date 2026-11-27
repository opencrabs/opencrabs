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
    /// The full command as handed to the shell, when this process knows it.
    ///
    /// The label is cut for display (#3204), so a surface that wants to show
    /// what is actually running needs the uncut text; the detail modal prints
    /// it in full. `None` for mirrored external tasks, whose command belongs
    /// to the spawning CLI and was never handed to us.
    pub command: Option<String>,
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
    /// Where the task came from, which decides how a surface tags it and
    /// where its live output can be found.
    pub source: TaskSource,
    /// The identity a mirrored row is removed by, when it differs from the
    /// label a surface prints. A claude-cli task is keyed on `claude-cli <id>`
    /// but shows the description the CLI gave it; `None` means the label is
    /// the identity, which is every spawned command.
    pub key: Option<String>,
    /// The file the task's merged output is being written to while it runs,
    /// when this process knows it. See [`RunningTask::output_path`].
    pub output: Option<PathBuf>,
    /// The signal that stops the command, for a row this process spawned.
    /// `None` for a mirrored task: its process belongs to someone else, and
    /// a row that merely vanished would claim a stop that never happened.
    pub cancel: Option<std::sync::Arc<tokio::sync::Notify>>,
}

/// What asking to cancel one running task came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The stop was signalled. The row leaves the roster when the command's
    /// own completion path runs, the same way a finished task leaves it.
    Signalled,
    /// The task is running but is not this process's to stop (a mirrored
    /// claude-cli task).
    NotOurs,
    /// No such task is running in that session.
    NotFound,
}

impl CancelOutcome {
    /// Whether the stop was actually signalled. A surface outside this crate
    /// cannot name the variants, and this is the one thing it needs to know.
    pub fn signalled(self) -> bool {
        self == CancelOutcome::Signalled
    }
}

/// Where a running row came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskSource {
    /// A command this process spawned detached.
    Detached,
    /// A claude-cli background task, mirrored in by its CLI task id.
    Claude { id: String },
    /// Any other mirrored external task.
    Mirror,
}

impl TaskSource {
    /// The short tag a surface prints beside the row.
    pub fn tag(&self) -> &'static str {
        match self {
            TaskSource::Detached => "detached",
            TaskSource::Claude { .. } => "claude",
            TaskSource::Mirror => "task",
        }
    }
}

impl RunningTask {
    /// The file holding this task's output so far, if one can be found.
    ///
    /// A spawned command knows its own log. A claude-cli task writes to a
    /// path inside the CLI's temp tree that no event reports while the task
    /// runs, so it is looked up by task id on demand. `None` is an ordinary
    /// answer: the file may not exist yet, and the layout is the CLI's own
    /// detail rather than a contract.
    pub fn output_path(&self) -> Option<PathBuf> {
        if let Some(path) = self.output.as_ref() {
            return Some(path.clone());
        }
        match &self.source {
            TaskSource::Claude { id } => claude_task_output(&std::env::temp_dir(), id),
            _ => None,
        }
    }
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

    /// Where task `task_id` of `session_id` is writing its output, if it is
    /// still running and a file can be found (see [`RunningTask::output_path`]).
    pub fn output_path_for(&self, session_id: Uuid, task_id: Uuid) -> Option<PathBuf> {
        self.running_tasks(session_id)
            .into_iter()
            .find(|t| t.task_id == task_id)?
            .output_path()
    }

    /// Ask task `task_id` of `session_id` to stop.
    ///
    /// Only a command this process spawned can be stopped: its shell and the
    /// work the shell started are killed, and the task then finishes through
    /// the ordinary completion path with a receipt that says it was cancelled,
    /// so the agent learns of it the same way it learns of any other exit.
    pub fn cancel(&self, session_id: Uuid, task_id: Uuid) -> CancelOutcome {
        let Some(task) = self
            .running_tasks(session_id)
            .into_iter()
            .find(|t| t.task_id == task_id)
        else {
            return CancelOutcome::NotFound;
        };
        match task.cancel {
            Some(signal) => {
                // `notify_one` keeps a permit when the waiter is not parked
                // yet, so a cancel that lands before the command is spawned
                // still stops it.
                signal.notify_one();
                CancelOutcome::Signalled
            }
            None => CancelOutcome::NotOurs,
        }
    }

    /// The scrubbed tail of a task's output file (see [`output_tail`]). A
    /// method so a surface outside this crate can read what it was handed.
    pub fn output_tail(&self, path: &std::path::Path, max_lines: usize) -> String {
        output_tail(path, max_lines)
    }

    /// The scrubbed output a task's full view scrolls through (see
    /// [`output_history`]).
    pub fn output_history(&self, path: &std::path::Path) -> String {
        output_history(path)
    }

    fn mark_started(
        &self,
        session_id: Uuid,
        task_id: Uuid,
        label: &str,
        progress: Option<ProgressCallback>,
    ) {
        self.push_row(
            session_id,
            RunningTask {
                task_id,
                label: label.to_string(),
                command: None,
                started: std::time::Instant::now(),
                progress,
                source: TaskSource::Mirror,
                key: None,
                output: None,
                cancel: None,
            },
        );
    }

    fn push_row(&self, session_id: Uuid, row: RunningTask) {
        if let Ok(mut m) = self.running.lock() {
            m.entry(session_id).or_default().push(row);
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
            if let Some(pos) = tasks
                .iter()
                .position(|t| t.key.as_deref().unwrap_or(&t.label) == label)
            {
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

    /// Mirror a claude-cli background task under the name the CLI gave it.
    ///
    /// The row is still removed by `claude-cli <id>` ([`claude_task_label`]),
    /// so [`Self::mirror_finished`] is unchanged, but a surface prints the
    /// task's own description instead of an opaque id. A missing or blank
    /// description falls back to the keyed label.
    pub fn mirror_started_claude(&self, session_id: Uuid, id: &str, description: Option<&str>) {
        let key = claude_task_label(id);
        let label = description
            .and_then(|d| d.lines().next())
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| key.clone());
        self.push_row(
            session_id,
            RunningTask {
                task_id: Uuid::new_v4(),
                label,
                command: None,
                started: std::time::Instant::now(),
                progress: None,
                source: TaskSource::Claude { id: id.to_string() },
                key: Some(key),
                output: None,
                cancel: None,
            },
        );
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
        // The log path is chosen here rather than inside the run so the row
        // can carry it: a surface reads the same file while the command runs.
        let log_path = detached_log_path(session_id);
        let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
        self.push_row(
            session_id,
            RunningTask {
                task_id,
                label: label.clone(),
                command: Some(command.clone()),
                started: std::time::Instant::now(),
                progress: progress.clone(),
                source: TaskSource::Detached,
                key: None,
                output: Some(log_path.clone()),
                cancel: Some(cancel.clone()),
            },
        );
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
        let waiter = async move {
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
            let result = run_detached_to(
                &command,
                &cwd,
                session_id,
                DETACH_CEILING,
                log_path,
                Some(cancel),
            )
            .await;
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
            // The callback is read first because it lives on the row, but it
            // fires AFTER the row is gone: a surface that answers the finish
            // by reading the running roster must not still find the task it is
            // being told about. Firing first left a window where the badge
            // outlived the completion, and it was wide enough for a test to
            // catch it every run (#2676).
            let finish_cb = this.progress_for(session_id, task_id);
            this.mark_finished_by_id(session_id, task_id);
            if let Some(cb) = finish_cb {
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
        };
        spawn_waiter(waiter);
    }
}

/// The runtime a detached command's waiter lives on, created once for the
/// process.
///
/// A detached command outlives the turn that started it by definition, so its
/// waiter cannot live on the caller's runtime (#2676). The window runs every
/// turn on a `current_thread` runtime it drops the moment the turn returns
/// (`gui::agent_bridge::spawn_turn`), and the waiter went with it: `child.wait`
/// was dropped mid-await, `kill_on_drop` killed the shell, the command's own
/// children kept running orphaned, and the finalize never ran. The row,
/// the status file and the DB record then read `Running` forever, which is the
/// hourglass that never goes away.
///
/// One worker is enough: the waiter spends its life awaiting the child and the
/// DB, never blocking on CPU. `enable_all` is required for the process driver,
/// the timer that bounds the wait, and the DB's IO.
static TASK_RUNTIME: std::sync::OnceLock<Option<tokio::runtime::Runtime>> =
    std::sync::OnceLock::new();

/// Spawn a detached command's waiter on the process-lifetime runtime.
///
/// Falls back to the caller's runtime only if the dedicated one cannot be
/// built, which is a startup-level failure worth an error line: the fallback
/// still runs the command, it just inherits the old "lives as long as the
/// spawning turn" limitation.
fn spawn_waiter<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let runtime = TASK_RUNTIME.get_or_init(|| {
        match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("opencrabs-detached")
            .enable_all()
            .build()
        {
            Ok(rt) => Some(rt),
            Err(e) => {
                tracing::error!(
                    target: "background_task",
                    "Could not build the detached task runtime: {e}. Falling back to the \
                     spawning runtime, where a completion can die with its turn."
                );
                None
            }
        }
    });
    match runtime {
        Some(rt) => {
            rt.spawn(future);
        }
        None => {
            tokio::spawn(future);
        }
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
#[cfg(test)]
pub(crate) async fn run_detached(
    command: &str,
    cwd: &std::path::Path,
    session_id: Uuid,
    ceiling: std::time::Duration,
) -> CmdResult {
    run_detached_to(
        command,
        cwd,
        session_id,
        ceiling,
        detached_log_path(session_id),
        None,
    )
    .await
}

/// How a cancelled command's receipt opens, so the agent reads the stop as
/// the user's decision rather than as a failure to investigate.
pub(crate) const CANCELLED_NOTE: &str =
    "[cancelled by the user from the task list; the command was stopped]";

/// A fresh temp log path for one detached command of `session_id`.
fn detached_log_path(session_id: Uuid) -> PathBuf {
    std::env::temp_dir().join(format!(
        "opencrabs-bg-{}-{}-{}.log",
        session_id.as_simple(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    ))
}

/// [`run_detached`] writing to a log path the caller chose, so the caller can
/// publish that path before the command starts.
pub(crate) async fn run_detached_to(
    command: &str,
    cwd: &std::path::Path,
    session_id: Uuid,
    ceiling: std::time::Duration,
    log_path: PathBuf,
    cancel: Option<std::sync::Arc<tokio::sync::Notify>>,
) -> CmdResult {
    use crate::utils::shell::PushShellCommand;
    use tokio::process::Command;
    let (shell, shell_arg) = crate::utils::shell::shell_pair();

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

    // A cancel ends the wait the same way the ceiling does. With no signal
    // to wait on, that arm never resolves.
    let cancelled = async {
        match cancel.as_ref() {
            Some(signal) => signal.notified().await,
            None => std::future::pending().await,
        }
    };
    let waited = tokio::select! {
        waited = tokio::time::timeout(ceiling, child.wait()) => Some(waited),
        _ = cancelled => None,
    };
    let Some(waited) = waited else {
        // The user asked for this one to stop. The shell's own children go
        // first, while the shell is still their parent and can be asked for
        // them: killing only the shell would leave the build it started
        // running, and the row would claim a stop that never happened.
        if let Some(pid) = child.id() {
            crate::utils::shell::kill_process_tree(pid);
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
        let output = format!("{CANCELLED_NOTE}\n{}", read_log_tail(&log_path));
        let _ = std::fs::remove_file(&log_path);
        return CmdResult {
            success: false,
            code: -1,
            output,
        };
    };
    let outcome = match waited {
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

/// The last `max_lines` lines of a running task's output, ready to draw.
///
/// Reads at most the final 16 KiB, drops terminal escapes and carriage-return
/// overwrites, and passes the text through the same secret scrub the log
/// writers use: a command's raw output can print a token, and a surface must
/// not be the one place that shows it. A missing or unreadable file is an
/// empty string, never an error, because the file belongs to a process that
/// may not have written yet or may already have cleaned up.
pub fn output_tail(path: &std::path::Path, max_lines: usize) -> String {
    output_tail_within(path, max_lines, 16 * 1024)
}

/// How many lines of a task's output the full view keeps.
pub const HISTORY_LINES: usize = 2000;

/// How many bytes off the end of the file the full view reads. A build log
/// can run to tens of megabytes, and a surface that re-reads it while the
/// task runs must not pay for all of it.
pub const HISTORY_BYTES: u64 = 512 * 1024;

/// The output a task's full view scrolls through: the last
/// [`HISTORY_LINES`] lines within the final [`HISTORY_BYTES`], cleaned and
/// scrubbed exactly as [`output_tail`] cleans its handful.
pub fn output_history(path: &std::path::Path) -> String {
    output_tail_within(path, HISTORY_LINES, HISTORY_BYTES)
}

/// [`output_tail`] reading at most `max_bytes` off the end of the file.
pub fn output_tail_within(path: &std::path::Path, max_lines: usize, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let clipped = size > max_bytes;
    if clipped && file.seek(SeekFrom::End(-(max_bytes as i64))).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = file.take(max_bytes).read_to_end(&mut buf);
    let raw = String::from_utf8_lossy(&buf);
    let plain = super::tool_loop::strip_ansi_output(&raw);
    let mut lines: Vec<&str> = plain
        .lines()
        // A progress bar redraws one line with `\r`; only the last draw is
        // what a terminal would be showing.
        .map(|l| l.rsplit('\r').find(|part| !part.is_empty()).unwrap_or(""))
        .map(str::trim_end)
        .collect();
    if clipped && !lines.is_empty() {
        // The seek landed mid-line; drop the fragment.
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let from = lines.len().saturating_sub(max_lines);
    let text = lines[from..].join("\n");
    crate::logging::redact::scrub(&text).into_owned()
}

/// Find a claude-cli background task's output file under `temp`.
///
/// The CLI writes it to `<temp>/claude-<uid>/<cwd slug>/<session>/tasks/<id>.output`
/// and reports that path in no event while the task runs, so it is found by
/// walking the three levels above `tasks/`. The newest match wins, in case a
/// task id was ever reused across sessions. Ids that are not a plain token are
/// refused so a hostile id cannot steer the lookup out of the tree.
///
/// The temp directory is shared with every other user on the machine, so
/// each level has to be ours and real: a directory or file owned by someone
/// else, or reached through a symlink, is skipped, and the match must be a
/// regular file. Otherwise anyone could plant a `claude-x` tree whose
/// "output" is a link to a file only we can read, or a pipe that never ends.
pub fn claude_task_output(temp: &std::path::Path, id: &str) -> Option<PathBuf> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let file = format!("{id}.output");
    let dirs = |p: &std::path::Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| ours_and_real(p).is_some_and(|m| m.is_dir()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for root in dirs(temp) {
        let is_claude = root
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("claude-"));
        if !is_claude {
            continue;
        }
        for cwd in dirs(&root) {
            for session in dirs(&cwd) {
                let tasks = session.join("tasks");
                if !ours_and_real(&tasks).is_some_and(|m| m.is_dir()) {
                    continue;
                }
                let candidate = tasks.join(&file);
                let Some(meta) = ours_and_real(&candidate).filter(|m| m.is_file()) else {
                    continue;
                };
                let at = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(seen, _)| at > *seen) {
                    best = Some((at, candidate));
                }
            }
        }
    }
    best.map(|(_, path)| path)
}

/// The metadata of `path` itself when it is not a symlink and, on unix,
/// belongs to the user running this process. `None` for anything else,
/// including a path that cannot be read.
fn ours_and_real(path: &std::path::Path) -> Option<std::fs::Metadata> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        if meta.uid() != uid {
            return None;
        }
    }
    Some(meta)
}

/// The last real statement of a compound command, if it has one.
///
/// A detached launch usually carries preparation before the work (`export
/// PATH=…; rm -f old.log; ./rebuild.sh`) and often a farewell after it
/// (`…; echo EXIT=$?`). The strip has one line, and the only part of it that
/// says what is RUNNING is the payload statement: the first cut spent the
/// whole field on `export` and never reached the script (#3207).
///
/// Preparation to skip: variable assignments (`export FOO=bar`, `FOO=bar`),
/// `set` flag lines, and bare directory changes anywhere before the payload.
/// Farewells to skip: a trailing `echo`, `exit`, `true`, `false` or `:`,
/// matched on the first word so `exit $code` and `echo "EXIT=$code" >> log`
/// count as the farewells they are. A single-statement command returns
/// unchanged, and a command that is nothing but preparation keeps it, because
/// something must render.
fn payload_statement(segments: &[String]) -> Option<&str> {
    let is_prep = |s: &str| {
        let bare = s.trim_start();
        bare == "set"
            || bare.starts_with("set ")
            || bare.starts_with("export ")
            || (!bare.is_empty()
                && !bare.contains(' ')
                && bare.contains('=')
                && !bare.starts_with('-'))
    };
    let is_farewell = |s: &str| {
        // Match the first WORD, not the whole segment: a farewell carries
        // arguments in practice (`exit $code`, `echo "EXIT=$code" >> log`),
        // and an exact-match test let both through as if they were the
        // payload, which labelled a gate run `exit $code` (#3207).
        matches!(
            s.split_whitespace().next().unwrap_or_default(),
            "echo" | "exit" | "true" | "false" | ":"
        )
    };
    let mid: Vec<&str> = segments
        .iter()
        .map(String::as_str)
        .filter(|s| !is_prep(s) && !is_farewell(s))
        .collect();
    if mid.is_empty() {
        return None;
    }
    Some(mid[mid.len() - 1])
}

/// A short human label for a command (first meaningful token sequence), for the
/// "running in the background" acknowledgement and the completion tag.
///
/// A compound command labels its PAYLOAD statement, the one that actually
/// runs, rather than its preparation (#3207): `export PATH=…; rm -f x.log;
/// ./rebuild.sh` spent the whole field on `export` and never named the
/// script. Single-statement commands are unchanged. Long path arguments are
/// folded to `anchor/…/file` before the cut (#3204), and the cut stays as the
/// last resort for a payload with no long path to fold.
pub(crate) fn short_label(command: &str) -> String {
    let after_cd = crate::utils::command_label::command_label(command);
    let folded = match payload_statement(&after_cd.split("; ").collect::<Vec<_>>()) {
        Some(payload) => payload.to_string(),
        None => after_cd,
    };
    let folded = crate::utils::command_label::shrink_paths(&folded);
    let label: String = folded.chars().take(60).collect();
    if folded.chars().count() > 60 {
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
