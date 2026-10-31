//! The GUI's bridge into the live agent core (`brain::agent`).
//!
//! Phase-1 scope, mirroring the TUI's send path: bootstrap one
//! [`AgentService`] on a dedicated 4 MB thread (the same stack rule every
//! DB/provider path here follows), route `EditorEvent::Sent` through
//! `send_message_with_tools_and_mode` on the selected session, and fan the
//! service's [`ProgressEvent`]s back to the UI through a channel the window
//! polls on its executor.
//!
//! Progress callback registration is service-global
//! (`with_progress_callback`), so the callback only forwards
//! `(session_id, event)` pairs into the channel; which pane cares is the
//! window's business.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use opencrabs::brain::agent::AgentService;
use opencrabs::brain::agent::{ProgressCallback, ProgressEvent, QueuedUserMessage};
use opencrabs::brain::prompt_builder::{BrainLoader, RuntimeInfo};
use opencrabs::brain::tools::ToolRegistry;
use opencrabs::config::Config;
use opencrabs::db::Database;
use opencrabs::services::ServiceContext;
use std::path::PathBuf;
use uuid::Uuid;

/// A turn outcome the worker thread hands back to the UI thread.
pub enum TurnEvent {
    /// Progress straight from the agent service, with the session it came
    /// from. The id matters: sub-agents run their own sessions and their
    /// tool rows and token counts must never be painted into the
    /// conversation the user is reading.
    ProgressFor(Uuid, ProgressEvent),
    /// The turn finished; carries the final response content and the
    /// context size the provider reported for it (the footer's ctx budget).
    Done {
        content: String,
        /// Prompt tokens the model saw on the final call. Same number the
        /// TUI reads from `response.context_tokens`.
        context_tokens: u32,
    },
    /// The turn failed; carries the user-facing message the channels use.
    Failed(String),
    /// Bootstrap finished: the service is live. It owns no session: every
    /// turn runs on the conversation the window has open, so the agent and
    /// the pane can never disagree about which session is being written.
    Ready,
    /// A new conversation was created for the window to open.
    SessionCreated(Uuid),
    /// Creating a conversation failed; the pane says why.
    SessionCreateFailed(String),
    /// A conversation was deleted (the session row is soft-deleted, exactly
    /// like the TUI's `SessionService::delete_session`: archived, not
    /// dropped, so usage history stays intact).
    SessionDeleted(Uuid),
    /// Deleting failed; the pane says why.
    SessionDeleteFailed(String),
    /// The resolved plan-file path for a conversation. `None` only when the
    /// lookup machinery itself died; a session without a project binding
    /// still resolves to the profile's `session/` fallback, matching what
    /// `plan_json_path` writes.
    PlanPath {
        session: Uuid,
        path: Option<std::path::PathBuf>,
    },
    /// Bootstrap itself died; carries the error text.
    BootstrapFailed(String),
}

/// Everything the UI needs to drive turns. Built by [`bootstrap`], delivered
/// through the event channel; the UI holds it in an `Option` until Ready.
///
/// There is deliberately no session on the bridge. The old field made every
/// turn land in a throwaway "GPUI" session while the pane showed another
/// one, which is why a request typed into an open conversation came back to
/// an empty transcript: the answer was in a different session.
pub struct AgentBridge {
    pub service: Arc<AgentService>,
}

/// Sink every event lands in. Cloned into the progress callback and the
/// worker threads; drained by the window's poll tick.
pub type EventSink = std::sync::mpsc::Sender<TurnEvent>;

/// Build the full agent stack the TUI uses, on a 4 MB thread.
///
/// Steps mirror `cli/ui.rs`: connect the shared DB (runs migrations, so a
/// first GUI launch on a fresh profile works), create the configured
/// provider, register the same core tool set, build the system brain from
/// the workspace brain files, and attach the global progress forwarder.
///
/// `config` is read once on the main thread before the window opens; the
/// tiny-stack rule forbids re-running `Config::load` on workers.
pub fn spawn_bootstrap(config: Config, sink: EventSink) {
    // #2299: pair this with the "agent ready" line so one run shows whether
    // bootstrap started at all, and what it did next.
    crate::logging::info("bootstrap start");
    let thread_sink = sink.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-agent-boot".into())
        .spawn(move || {
            let sink = thread_sink;
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::logging::error(&format!("bootstrap tokio runtime failed: {e}"));
                    let _ = sink.send(TurnEvent::BootstrapFailed(format!("tokio runtime: {e}")));
                    return;
                }
            };
            runtime.block_on(async move {
                match build(config, sink.clone()).await {
                    Ok(bridge) => {
                        let _ = sink.send(TurnEvent::Ready);
                        // Leak-free ownership: the service must outlive this
                        // thread, so hand it to a static holder the UI reads.
                        SERVICE_VEHICLE.set(bridge);
                    }
                    Err(e) => {
                        // #2299: the window used to show this and record nothing,
                        // so a failed start left no trail on disk at all.
                        crate::logging::error(&format!("bootstrap failed: {e}"));
                        let _ = sink.send(TurnEvent::BootstrapFailed(e));
                    }
                }
            });
        });

    if let Err(e) = spawned {
        crate::logging::error(&format!("bootstrap thread spawn failed: {e}"));
        let _ = sink.send(TurnEvent::BootstrapFailed(format!(
            "bootstrap thread spawn: {e}"
        )));
    }
}

static SERVICE_VEHICLE: OnceBridge = OnceBridge::new();

/// One-shot holder so the service (and its tokio-internal pieces) survive
/// the bootstrap thread. `OnceLock` semantics, no unsafe.
struct OnceBridge(std::sync::OnceLock<AgentBridge>);

impl OnceBridge {
    const fn new() -> Self {
        Self(std::sync::OnceLock::new())
    }
    fn set(&self, b: AgentBridge) {
        let _ = self.0.set(b);
    }
    /// The live bridge, once bootstrap completed.
    pub fn get(&self) -> Option<&AgentBridge> {
        self.0.get()
    }
}

/// Access the live bridge after `TurnEvent::Ready`.
pub fn bridge() -> Option<&'static AgentBridge> {
    SERVICE_VEHICLE.get()
}

async fn build(config: Config, sink: EventSink) -> Result<AgentBridge, String> {
    let db = Database::connect(&config.database.path)
        .await
        .map_err(|e| {
            crate::logging::error(&format!("bootstrap db connect failed: {e:#}"));
            format!("db connect: {e}")
        })?;
    // The on-screen line stays short; the log gets the whole chain. Before
    // #2299 "Failed to run database migrations" reached the user with its
    // cause already discarded, which is exactly why #2297 had no trail.
    db.run_migrations().await.map_err(|e| {
        crate::logging::error(&format!("bootstrap db migrations failed: {e:#}"));
        format!("db migrations: {e}")
    })?;
    let context = ServiceContext::new(db.pool().clone());

    let provider = opencrabs::brain::provider::create_provider(&config)
        .await
        .map_err(|e| {
            crate::logging::error(&format!("bootstrap provider failed: {e:#}"));
            format!("provider: {e}")
        })?;

    // Same core tool set the TUI registers, via the lib's public wrapper.
    let tool_registry = Arc::new(ToolRegistry::new());
    let subagent_manager = opencrabs::cli::tool_setup::register_core_agent_tools_embedded(
        &tool_registry,
        &db,
        &config,
    );

    // System brain from the workspace brain files, same as the TUI.
    let brain_path = BrainLoader::resolve_path();
    let brain_loader = BrainLoader::new(brain_path.clone());
    let working_directory = opencrabs::utils::cwd::launch_cwd();
    let runtime_info = RuntimeInfo {
        model: Some(provider.default_model().to_string()),
        provider: Some(provider.name().to_string()),
        working_directory: Some(opencrabs::brain::tools::error::collapse_home(
            &working_directory,
        )),
    };
    let system_brain = brain_loader.build_core_brain(Some(&runtime_info));

    let auto_approve = config.agent.approval_policy.as_str() == "auto-always";
    let forwarder: ProgressCallback = install_progress_forwarder(&sink);
    let service = AgentService::new(provider, context.clone(), &config)
        .await
        .with_system_brain(system_brain)
        .with_working_directory(working_directory)
        .with_auto_approve_tools(auto_approve)
        .with_tool_registry(tool_registry)
        .with_subagent_manager(subagent_manager)
        .with_progress_callback(Some(forwarder));

    Ok(AgentBridge {
        service: Arc::new(service),
    })
}

/// Create one conversation in the shared ledger and report its id back.
///
/// Mirrors the TUI's `create_new_session`: the row is titled "New Chat",
/// inherits the running provider and model (a blank session later answers
/// with something other than the model the window shows), and records the
/// launch directory so a resume lands where the user was working.
///
/// Runs on a 4 MB thread like every other DB path here (gpui's background
/// workers have tiny stacks), and only connects: migrations belong to the
/// daemon and to the agent bootstrap, never to a UI action.
pub fn spawn_create_session(
    db_path: std::path::PathBuf,
    sink: EventSink,
    provider: Option<String>,
    model: Option<String>,
) {
    let thread_sink = sink.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-session-create".into())
        .spawn(move || {
            let sink = thread_sink;
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = sink.send(TurnEvent::SessionCreateFailed(format!(
                        "session runtime: {e}"
                    )));
                    return;
                }
            };
            let result = runtime.block_on(async move {
                let db = Database::connect(&db_path)
                    .await
                    .map_err(|e| format!("db connect: {e}"))?;
                let service = opencrabs::services::SessionService::new(ServiceContext::new(
                    db.pool().clone(),
                ));
                let working_directory = Some(
                    opencrabs::utils::cwd::launch_cwd()
                        .to_string_lossy()
                        .to_string(),
                );
                service
                    .create_session_with_provider(
                        Some("New Chat".to_string()),
                        provider,
                        model,
                        working_directory,
                    )
                    .await
                    .map(|s| s.id)
                    .map_err(|e| format!("create session: {e}"))
            });
            match result {
                Ok(id) => {
                    let _ = sink.send(TurnEvent::SessionCreated(id));
                }
                Err(e) => {
                    let _ = sink.send(TurnEvent::SessionCreateFailed(e));
                }
            }
        });
    if let Err(e) = spawned {
        let _ = sink.send(TurnEvent::SessionCreateFailed(format!(
            "session thread spawn: {e}"
        )));
    }
}

/// Delete one conversation through the shared `SessionService`, on the same
/// 4 MB thread every DB path here uses.
///
/// `delete_session` is a SOFT delete: the row is archived, never dropped, so
/// the usage ledger keeps its history. The TUI's `d` key on the sessions list
/// calls exactly this.
/// Register the files a message carried against the session that carried
/// them, exactly what the TUI does before sending (`FileService::
/// get_or_create_file` in `tui/app/input.rs`). The message text already holds
/// the `<<IMG:>>` / `<<VID:>>` markers, so this is the bookkeeping half: the
/// file lands in the session's artifacts and the DB remembers it.
///
/// Fire and forget by design: a failure here must never hold up or fail the
/// turn, which is why it logs and returns. Runs on its own 4 MB thread for
/// the same reason every other DB path does.
pub fn spawn_track_files(db_path: PathBuf, session_id: Uuid, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-file-track".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::logging::error(&format!("file track runtime: {e}"));
                    return;
                }
            };
            runtime.block_on(async move {
                let db = match Database::connect(&db_path).await {
                    Ok(db) => db,
                    Err(e) => {
                        crate::logging::error(&format!("file track db connect: {e}"));
                        return;
                    }
                };
                let service =
                    opencrabs::services::FileService::new(ServiceContext::new(db.pool().clone()));
                for path in paths {
                    // The name in the log is the file's own: a failure has to
                    // say which attachment did not get tracked.
                    let name = path.display().to_string();
                    if let Err(e) = service.get_or_create_file(session_id, path, None).await {
                        crate::logging::error(&format!("file track {name}: {e}"));
                    }
                }
            });
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("file track thread spawn: {e}"));
    }
}

/// One row of the Cmd+G files browser: the tracked path plus its size. A
/// projection of `db::models::File` so the window never reaches into the
/// DB model crate-wide.
pub struct SessionFile {
    pub path: PathBuf,
    pub size: Option<i64>,
}

/// The open conversation's tracked files, the fetch behind Cmd+G. Same shape
/// as the window's `load_view`: an own 4 MB thread with an own current-thread
/// runtime, because gpui's workers are dispatch-queue threads with tiny
/// stacks and sqlx needs a Tokio reactor.
pub async fn load_files(db_path: PathBuf, session_id: Uuid) -> Result<Vec<SessionFile>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-file-list".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("tokio runtime failed: {e}")));
                    return;
                }
            };
            let result = runtime.block_on(async move {
                let db = Database::connect(&db_path)
                    .await
                    .map_err(|e| format!("db connect: {e:#}"))?;
                let service =
                    opencrabs::services::FileService::new(ServiceContext::new(db.pool().clone()));
                let files = service
                    .list_files_for_session(session_id)
                    .await
                    .map_err(|e| format!("{e:#}"))?;
                Ok(files
                    .into_iter()
                    .map(|f| SessionFile {
                        path: f.path,
                        size: f.size,
                    })
                    .collect())
            });
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("file list thread dropped its result".into()),
        },
        Err(e) => Err(format!("file list thread failed to spawn: {e}")),
    }
}

pub fn spawn_delete_session(db_path: std::path::PathBuf, session_id: Uuid, sink: EventSink) {
    let thread_sink = sink.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-session-delete".into())
        .spawn(move || {
            let sink = thread_sink;
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = sink.send(TurnEvent::SessionDeleteFailed(format!(
                        "session runtime: {e}"
                    )));
                    return;
                }
            };
            let result = runtime.block_on(async move {
                let db = Database::connect(&db_path)
                    .await
                    .map_err(|e| format!("db connect: {e}"))?;
                let service = opencrabs::services::SessionService::new(ServiceContext::new(
                    db.pool().clone(),
                ));
                service
                    .delete_session(session_id)
                    .await
                    .map_err(|e| format!("delete session: {e}"))
            });
            match result {
                Ok(()) => {
                    let _ = sink.send(TurnEvent::SessionDeleted(session_id));
                }
                Err(e) => {
                    let _ = sink.send(TurnEvent::SessionDeleteFailed(e));
                }
            }
        });
    if let Err(e) = spawned {
        let _ = sink.send(TurnEvent::SessionDeleteFailed(format!(
            "session delete thread spawn: {e}"
        )));
    }
}

/// Resolve a conversation's plan-file path off the UI thread. The lookup
/// reads the DB through the pool, and deadpool demands a Tokio reactor: the
/// gpui executor has none, so the first render that ran this inline killed
/// the window six seconds after launch (gate smoke, 2026-10-31). Same shape
/// as every other bridge call: 4 MB thread, own current-thread runtime,
/// result back over the event channel.
pub fn spawn_plan_path(db_path: std::path::PathBuf, session_id: Uuid, sink: EventSink) {
    let thread_sink = sink.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-plan-path".into())
        .spawn(move || {
            let sink = thread_sink;
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = sink.send(TurnEvent::PlanPath {
                        session: session_id,
                        path: None,
                    });
                    crate::logging::error(&format!("plan path runtime: {e}"));
                    return;
                }
            };
            // Reproduce `plan_json_path`'s resolution: the project dir when
            // the session is bound, the profile `session/` fallback when it
            // is not (or the DB is unreachable). Never the global pool.
            let dir = runtime
                .block_on(async move {
                    match Database::connect(&db_path).await {
                        Ok(db) => {
                            opencrabs::utils::plan_files::project_session_dir(session_id, db.pool())
                                .await
                        }
                        Err(e) => {
                            crate::logging::error(&format!("plan path db: {e}"));
                            None
                        }
                    }
                })
                .unwrap_or_else(|| opencrabs::config::opencrabs_home().join("session"));
            let path = dir.join(format!(".opencrabs_plan_{session_id}.json"));
            let _ = sink.send(TurnEvent::PlanPath {
                session: session_id,
                path: Some(path),
            });
        });
    if spawned.is_err() {
        let _ = sink.send(TurnEvent::PlanPath {
            session: session_id,
            path: None,
        });
    }
}

/// Run one turn on a 4 MB thread: the TUI's `send_message_with_tools_and_mode`
/// call with the queue-completion handshake the TUI does around it.
///
/// The progress forwarder (`install_progress_forwarder`) already streams
/// events to `sink`; this thread only delivers the terminal event.
pub fn spawn_turn(
    service: Arc<AgentService>,
    session_id: Uuid,
    content: String,
    sink: EventSink,
    cancel: tokio_util::sync::CancellationToken,
) {
    let thread_sink = sink.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-agent-turn".into())
        .spawn(move || {
            let sink = thread_sink;
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = sink.send(TurnEvent::Failed(format!("turn runtime: {e}")));
                    return;
                }
            };
            runtime.block_on(async move {
                match service
                    .send_message_with_tools_and_mode(session_id, content, None, Some(cancel))
                    .await
                {
                    Ok(response) => {
                        let _ = sink.send(TurnEvent::Done {
                            content: response.content,
                            context_tokens: response.context_tokens,
                        });
                    }
                    Err(e) => {
                        let msg = opencrabs::brain::agent::format_user_error(&e);
                        let _ = sink.send(TurnEvent::Failed(msg));
                    }
                }
            });
        });
    if let Err(e) = spawned {
        let _ = sink.send(TurnEvent::Failed(format!("turn thread spawn: {e}")));
    }
}

/// The service-global progress forwarder: every `(session, event)` pair
/// lands in `sink` for the UI to drain. Installed on the builder inside
/// `build` (the builder consumes self, so there is no post-hoc setter).
fn install_progress_forwarder(sink: &EventSink) -> ProgressCallback {
    let sink = sink.clone();
    Arc::new(move |session, event| {
        let _ = sink.send(TurnEvent::ProgressFor(session, event));
    })
}

/// Drain helper for the UI tick: take every event currently queued.
pub fn drain(rx: &Receiver<TurnEvent>) -> Vec<TurnEvent> {
    let mut out = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(ev) => out.push(ev),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    out
}

/// The poll cadence for the UI tick. 50 ms is below human perception for
/// streaming text and cheap when idle.
pub const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A message queued while a turn runs: the TUI's exact type, joined and
/// submitted when the current turn completes.
pub type PendingMessage = QueuedUserMessage;
