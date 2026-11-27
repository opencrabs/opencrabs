//! Background tasks: snapshot, detail, tail and stop.
//!
//! The window reads these while it paints: the rail's spinners and running
//! lines, the strip above the composer, the open task's tail and its full
//! view. Like [`super::Facts`], each operation is answered by a synchronous
//! `answer` over the manager's in-memory roster, so a local surface can ask
//! it inside a frame, and `Op::run` is that same `answer` for the remote
//! door. Nothing here waits on anything.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::brain::agent::service::background_tasks::{
    BackgroundTaskManager, CancelOutcome, RunningTask, TaskSource,
};

use super::{BackendError, Ctx, Op, Scope};

/// The crab's task manager. `NotReady` before boot; `None` when the crab
/// runs without one, which every reader takes as "nothing is running".
fn manager(ctx: &Ctx) -> Result<Option<Arc<BackgroundTaskManager>>, BackendError> {
    Ok(ctx.service()?.background_manager())
}

/// The output file this crab found for each task it was asked about.
///
/// A caller never chooses the file: the crab resolves it from the task's own
/// row, so a reader holding `a2a:read` cannot name an arbitrary file. The
/// path is remembered once found, because a finished task leaves the roster
/// and its view still reads the log it wrote, and a claude-cli task's file
/// only appears a moment after the task does. One entry per task looked at.
fn known_paths() -> &'static Mutex<HashMap<(Uuid, Uuid), PathBuf>> {
    static PATHS: OnceLock<Mutex<HashMap<(Uuid, Uuid), PathBuf>>> = OnceLock::new();
    PATHS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The output file of `task` in `session`: the one already found, else the
/// one the roster reports now when `look_up` allows it, remembered.
fn output_path(
    mgr: &BackgroundTaskManager,
    session: Uuid,
    task: Uuid,
    look_up: bool,
) -> Option<PathBuf> {
    let known = known_paths()
        .lock()
        .ok()
        .and_then(|paths| paths.get(&(session, task)).cloned());
    if known.is_some() || !look_up {
        return known;
    }
    let found = mgr.output_path_for(session, task)?;
    if let Ok(mut paths) = known_paths().lock() {
        paths.insert((session, task), found.clone());
    }
    Some(found)
}

/// Where a running task came from, which decides the tag a surface prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// A command this crab spawned detached.
    Detached,
    /// A claude-cli background task, mirrored in.
    Claude,
    /// Any other mirrored task.
    Mirror,
}

impl TaskKind {
    /// The short tag beside the row, the same words as `TaskSource::tag`.
    pub fn tag(self) -> &'static str {
        match self {
            TaskKind::Detached => "detached",
            TaskKind::Claude => "claude",
            TaskKind::Mirror => "task",
        }
    }
}

impl From<&TaskSource> for TaskKind {
    fn from(source: &TaskSource) -> Self {
        match source {
            TaskSource::Detached => TaskKind::Detached,
            TaskSource::Claude { .. } => TaskKind::Claude,
            TaskSource::Mirror => TaskKind::Mirror,
        }
    }
}

/// One running task as a surface draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRow {
    /// The task's stable id.
    pub task_id: Uuid,
    /// The command or description the row prints.
    pub label: String,
    /// Where it came from.
    pub kind: TaskKind,
    /// Seconds since it started, read when the snapshot was taken.
    pub secs: u64,
    /// Whether this crab spawned it and can therefore stop it.
    pub ours: bool,
}

impl From<RunningTask> for TaskRow {
    fn from(t: RunningTask) -> Self {
        TaskRow {
            task_id: t.task_id,
            kind: TaskKind::from(&t.source),
            secs: t.started.elapsed().as_secs(),
            ours: t.cancel.is_some(),
            label: t.label,
        }
    }
}

/// One session's running tasks, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTasks {
    /// The session.
    pub session: Uuid,
    /// Its running tasks, never empty.
    pub tasks: Vec<TaskRow>,
}

/// What is running in each of `sessions`. A session with nothing running is
/// left out, so an empty answer means the whole set is idle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TasksSnapshot {
    /// The sessions to look at, in the order the answer keeps.
    pub sessions: Vec<Uuid>,
}

impl TasksSnapshot {
    /// The snapshot, now.
    pub fn answer(&self, ctx: &Ctx) -> Result<Vec<SessionTasks>, BackendError> {
        let Some(mgr) = manager(ctx)? else {
            return Ok(Vec::new());
        };
        Ok(self
            .sessions
            .iter()
            .filter_map(|&session| {
                let tasks: Vec<TaskRow> = mgr
                    .running_tasks(session)
                    .into_iter()
                    .map(TaskRow::from)
                    .collect();
                (!tasks.is_empty()).then_some(SessionTasks { session, tasks })
            })
            .collect())
    }
}

impl Op for TasksSnapshot {
    const NAME: &'static str = "tasks.snapshot";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<SessionTasks>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        self.answer(ctx)
    }
}

/// The last lines of one task's output, for the open row of the task list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTail {
    /// The session the task runs in.
    pub session: Uuid,
    /// The task.
    pub task: Uuid,
    /// The file the caller last got back for this task, if any. Only a
    /// check: the crab answers from its own record and never opens a path
    /// it did not resolve for this task, so a hint that differs is dropped.
    pub path: Option<PathBuf>,
    /// How many lines to keep.
    pub lines: usize,
}

/// What a [`TaskTail`] read found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TailRead {
    /// The output file, once it is known.
    pub path: Option<PathBuf>,
    /// The scrubbed tail, when there is a file to read.
    pub text: Option<String>,
}

impl TaskTail {
    /// The tail, now. `None` when the crab has no task manager.
    pub fn answer(&self, ctx: &Ctx) -> Result<Option<TailRead>, BackendError> {
        let Some(mgr) = manager(ctx)? else {
            return Ok(None);
        };
        let path = output_path(&mgr, self.session, self.task, true);
        if self.path.is_some() && self.path != path {
            tracing::debug!(
                "tasks.tail: dropped a path hint for {} that this crab did not resolve",
                self.task
            );
        }
        let text = path.as_deref().map(|p| mgr.output_tail(p, self.lines));
        Ok(Some(TailRead { path, text }))
    }
}

impl Op for TaskTail {
    const NAME: &'static str = "tasks.tail";
    const SCOPE: Scope = Scope::Read;
    type Reply = Option<TailRead>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        self.answer(ctx)
    }
}

/// A fresh look at one task for its full view: whether it still runs, where
/// its output is, and the output itself when the file moved since `stamp`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetail {
    /// The session the task runs in.
    pub session: Uuid,
    /// The task.
    pub task: Uuid,
    /// The file's `(len, mtime)` when the view last read it.
    pub stamp: Option<(u64, SystemTime)>,
}

/// What a [`TaskDetail`] look found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailRead {
    /// Whether the task is still in the roster.
    pub running: bool,
    /// The output file, once the crab has found it.
    pub path: Option<PathBuf>,
    /// The file's `(len, mtime)` now, `None` when it cannot be read.
    pub stamp: Option<(u64, SystemTime)>,
    /// The scrubbed output, `None` when there is no file or it has not
    /// changed since the stamp asked with.
    pub history: Option<String>,
}

impl TaskDetail {
    /// The look, now. `None` when the crab has no task manager.
    pub fn answer(&self, ctx: &Ctx) -> Result<Option<DetailRead>, BackendError> {
        let Some(mgr) = manager(ctx)? else {
            return Ok(None);
        };
        let running = mgr
            .running_tasks(self.session)
            .iter()
            .any(|t| t.task_id == self.task);
        // Looked up only while the task runs; a finished one keeps the file
        // found before it ended.
        let path = output_path(&mgr, self.session, self.task, running);
        let Some(file) = path.clone() else {
            return Ok(Some(DetailRead {
                running,
                path,
                stamp: None,
                history: None,
            }));
        };
        // A file that has not changed is not read again: the read scrubs up
        // to half a megabyte, which is not worth doing once a second for a
        // task that is thinking.
        let stamp = std::fs::metadata(&file)
            .ok()
            .map(|m| (m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
        let history = if stamp.is_some() && stamp == self.stamp {
            None
        } else {
            Some(mgr.output_history(&file))
        };
        Ok(Some(DetailRead {
            running,
            path,
            stamp,
            history,
        }))
    }
}

impl Op for TaskDetail {
    const NAME: &'static str = "tasks.detail";
    const SCOPE: Scope = Scope::Read;
    type Reply = Option<DetailRead>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        self.answer(ctx)
    }
}

/// What asking to stop a task came to, as the manager answered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopOutcome {
    /// The stop was signalled; the row leaves when the command exits.
    Signalled,
    /// The task runs but is not this crab's to stop.
    NotOurs,
    /// No such task runs in that session.
    NotFound,
}

impl StopOutcome {
    /// Whether the stop was actually signalled.
    pub fn signalled(self) -> bool {
        self == StopOutcome::Signalled
    }
}

impl From<CancelOutcome> for StopOutcome {
    fn from(outcome: CancelOutcome) -> Self {
        match outcome {
            CancelOutcome::Signalled => StopOutcome::Signalled,
            CancelOutcome::NotOurs => StopOutcome::NotOurs,
            CancelOutcome::NotFound => StopOutcome::NotFound,
        }
    }
}

/// Ask one running task to stop. The task then ends through its own exit
/// path, the same way a finished task leaves the roster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStop {
    /// The session the task runs in.
    pub session: Uuid,
    /// The task.
    pub task: Uuid,
}

impl TaskStop {
    /// Signal the stop, now. `None` when the crab has no task manager.
    pub fn answer(&self, ctx: &Ctx) -> Result<Option<StopOutcome>, BackendError> {
        Ok(manager(ctx)?.map(|mgr| StopOutcome::from(mgr.cancel(self.session, self.task))))
    }
}

impl Op for TaskStop {
    const NAME: &'static str = "tasks.stop";
    const SCOPE: Scope = Scope::Send;
    type Reply = Option<StopOutcome>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        self.answer(ctx)
    }
}

ops! { TasksSnapshot, TaskTail, TaskDetail, TaskStop }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    async fn booted_ctx() -> Ctx {
        let db = crate::db::Database::connect_in_memory()
            .await
            .expect("in-memory ledger");
        db.run_migrations().await.expect("migrate");
        let context = crate::services::ServiceContext::new(db.pool().clone());
        let provider: Arc<dyn crate::brain::provider::Provider> =
            Arc::new(crate::tests::agent_service_mocks::MockProvider);
        let service = crate::brain::agent::AgentService::new_for_test(provider, context).await;
        let ctx = Ctx::new(PathBuf::from("unused.db"));
        ctx.set_service(Arc::new(service));
        ctx
    }

    #[test]
    fn every_door_is_not_ready_before_boot() {
        let ctx = Ctx::new(PathBuf::from("unused.db"));
        let (session, task) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            TasksSnapshot {
                sessions: vec![session]
            }
            .answer(&ctx),
            Err(BackendError::NotReady)
        );
        assert_eq!(
            TaskTail {
                session,
                task,
                path: None,
                lines: 5
            }
            .answer(&ctx),
            Err(BackendError::NotReady)
        );
        assert_eq!(
            TaskDetail {
                session,
                task,
                stamp: None
            }
            .answer(&ctx),
            Err(BackendError::NotReady)
        );
        assert_eq!(
            TaskStop { session, task }.answer(&ctx),
            Err(BackendError::NotReady)
        );
    }

    /// A crab with no task manager runs nothing: the snapshot is empty and
    /// the per-task doors answer `None`, which is what the window read as
    /// "no manager" before.
    #[tokio::test]
    async fn without_a_manager_nothing_runs() {
        let ctx = booted_ctx().await;
        assert!(ctx.service().expect("booted").background_manager().is_none());
        let (session, task) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            TasksSnapshot {
                sessions: vec![session]
            }
            .answer(&ctx),
            Ok(Vec::new())
        );
        assert_eq!(
            TaskTail {
                session,
                task,
                path: None,
                lines: 5
            }
            .answer(&ctx),
            Ok(None)
        );
        assert_eq!(
            TaskDetail {
                session,
                task,
                stamp: None
            }
            .answer(&ctx),
            Ok(None)
        );
        assert_eq!(TaskStop { session, task }.answer(&ctx), Ok(None));
    }

    /// The doors read the manager's own roster: a mirrored task shows in the
    /// snapshot of its session only, is not ours to stop, and a task the
    /// roster does not hold is not found.
    #[tokio::test]
    async fn the_doors_read_the_managers_roster() {
        let db = crate::db::Database::connect_in_memory()
            .await
            .expect("in-memory ledger");
        db.run_migrations().await.expect("migrate");
        let context = crate::services::ServiceContext::new(db.pool().clone());
        let provider: Arc<dyn crate::brain::provider::Provider> =
            Arc::new(crate::tests::agent_service_mocks::MockProvider);
        let mgr = Arc::new(BackgroundTaskManager::new());
        let service = crate::brain::agent::AgentService::new_for_test(provider, context)
            .await
            .with_existing_background_manager(Some(mgr.clone()), None);
        let ctx = Ctx::new(PathBuf::from("unused.db"));
        ctx.set_service(Arc::new(service));

        let (busy, idle) = (Uuid::new_v4(), Uuid::new_v4());
        mgr.mirror_started(busy, "probe");
        let task = mgr.running_tasks(busy)[0].task_id;

        let snapshot = TasksSnapshot {
            sessions: vec![idle, busy],
        }
        .answer(&ctx)
        .expect("snapshot");
        assert_eq!(snapshot.len(), 1, "the idle session is left out");
        assert_eq!(snapshot[0].session, busy);
        let row = &snapshot[0].tasks[0];
        assert_eq!(
            (row.task_id, row.label.as_str(), row.kind, row.ours),
            (task, "probe", TaskKind::Mirror, false)
        );

        let detail = TaskDetail {
            session: busy,
            task,
            stamp: None,
        }
        .answer(&ctx)
        .expect("detail")
        .expect("a manager");
        assert!(detail.running);
        assert_eq!(detail.path, None, "a mirror has no output file");
        assert_eq!(detail.history, None);

        assert_eq!(
            TaskStop {
                session: busy,
                task
            }
            .answer(&ctx),
            Ok(Some(StopOutcome::NotOurs))
        );
        assert_eq!(
            TaskStop {
                session: busy,
                task: Uuid::new_v4()
            }
            .answer(&ctx),
            Ok(Some(StopOutcome::NotFound))
        );
    }

    /// A caller cannot make the crab read a file by naming it: a path hint
    /// the crab did not resolve for that task is dropped, and a task with no
    /// output file answers no text.
    #[tokio::test]
    async fn a_tail_never_opens_a_path_the_caller_named() {
        let db = crate::db::Database::connect_in_memory()
            .await
            .expect("in-memory ledger");
        db.run_migrations().await.expect("migrate");
        let context = crate::services::ServiceContext::new(db.pool().clone());
        let provider: Arc<dyn crate::brain::provider::Provider> =
            Arc::new(crate::tests::agent_service_mocks::MockProvider);
        let mgr = Arc::new(BackgroundTaskManager::new());
        let service = crate::brain::agent::AgentService::new_for_test(provider, context)
            .await
            .with_existing_background_manager(Some(mgr.clone()), None);
        let ctx = Ctx::new(PathBuf::from("unused.db"));
        ctx.set_service(Arc::new(service));

        let dir = tempfile::tempdir().expect("tempdir");
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, "do not read me\n").expect("write");
        let session = Uuid::new_v4();
        mgr.mirror_started(session, "probe");
        let task = mgr.running_tasks(session)[0].task_id;

        let read = TaskTail {
            session,
            task,
            path: Some(secret),
            lines: 5,
        }
        .answer(&ctx)
        .expect("tail")
        .expect("a manager");
        assert_eq!(read, TailRead { path: None, text: None });
    }

    #[test]
    fn kinds_tag_as_the_source_does() {
        for source in [
            TaskSource::Detached,
            TaskSource::Claude { id: "x".into() },
            TaskSource::Mirror,
        ] {
            assert_eq!(TaskKind::from(&source).tag(), source.tag());
        }
    }

    #[test]
    fn outcomes_keep_the_managers_verdict() {
        for outcome in [
            CancelOutcome::Signalled,
            CancelOutcome::NotOurs,
            CancelOutcome::NotFound,
        ] {
            let ours = StopOutcome::from(outcome);
            assert_eq!(ours.signalled(), outcome.signalled());
            // The window logs `{outcome:?}`: the words must not change.
            assert_eq!(format!("{ours:?}"), format!("{outcome:?}"));
        }
    }

    #[test]
    fn answers_survive_the_wire() {
        let read = DetailRead {
            running: true,
            // `/fixtures/`: a `/tmp/` literal trips the windows-footgun scan.
            path: Some(PathBuf::from("/fixtures/task.log")),
            stamp: Some((42, SystemTime::UNIX_EPOCH)),
            history: Some("line\n".into()),
        };
        let json = serde_json::to_string(&read).expect("encode");
        let back: DetailRead = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, read, "{json}");

        let rows = vec![SessionTasks {
            session: Uuid::nil(),
            tasks: vec![TaskRow {
                task_id: Uuid::nil(),
                label: "cargo test".into(),
                kind: TaskKind::Claude,
                secs: 32,
                ours: false,
            }],
        }];
        let json = serde_json::to_string(&rows).expect("encode");
        let back: Vec<SessionTasks> = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, rows, "{json}");
    }

    #[tokio::test]
    async fn tasks_snapshot_has_parity() {
        let gateway = Gateway::start().await;
        parity!(
            gateway,
            TasksSnapshot {
                sessions: vec![Uuid::new_v4()],
            }
        );
        forbidden!(
            gateway,
            TasksSnapshot {
                sessions: vec![Uuid::new_v4()],
            }
        );
    }

    #[tokio::test]
    async fn task_tail_has_parity() {
        let gateway = Gateway::start().await;
        let op = TaskTail {
            session: Uuid::new_v4(),
            task: Uuid::new_v4(),
            path: None,
            lines: 5,
        };
        parity!(gateway, TaskTail { ..op.clone() });
        forbidden!(gateway, op);
    }

    #[tokio::test]
    async fn task_detail_has_parity() {
        let gateway = Gateway::start().await;
        let op = TaskDetail {
            session: Uuid::new_v4(),
            task: Uuid::new_v4(),
            stamp: None,
        };
        parity!(gateway, TaskDetail { ..op.clone() });
        forbidden!(gateway, op);
    }

    #[tokio::test]
    async fn task_stop_has_parity() {
        let gateway = Gateway::start().await;
        let op = TaskStop {
            session: Uuid::new_v4(),
            task: Uuid::new_v4(),
        };
        parity!(gateway, TaskStop { ..op.clone() });
        forbidden!(gateway, op);
    }
}
