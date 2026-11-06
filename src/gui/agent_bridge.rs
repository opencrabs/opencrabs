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
use opencrabs::brain::agent::service::MessageEnqueueCallback;
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
    /// Output of a `!cmd` the user ran in the window (#2437).
    BangOutput(String),
    /// ctrl+k cleared this session's messages; the transcript empties (#2430).
    MessagesCleared(Uuid),
    /// ctrl+k could not clear the session: the reason, for the notices.
    ClearFailed(String),
    /// A newer release exists; carries its version (#2406).
    UpdateAvailable(String),
    /// `/evolve` finished: the text to show in the chat notices (#2406).
    EvolveDone(String),
    /// Progress straight from the agent service, with the session it came
    /// from. The id matters: sub-agents run their own sessions and their
    /// tool rows and token counts must never be painted into the
    /// conversation the user is reading.
    ProgressFor(Uuid, ProgressEvent),
    /// The turn finished; carries the final response content and the
    /// context size the provider reported for it (the footer's ctx budget).
    /// The session travels with it: a split pane runs its own turn, and the
    /// window has to know which leaf to settle (#2614).
    Done {
        session: Uuid,
        content: String,
        /// Prompt tokens the model saw on the final call. Same number the
        /// TUI reads from `response.context_tokens`.
        context_tokens: u32,
        /// The service's own output rate for this turn: provider-reported
        /// tokens over summed active streaming windows, which is the number
        /// the Telegram card prints. `None` when the provider or the CLI
        /// path could not measure it.
        tokens_per_second: Option<f64>,
    },
    /// The turn failed; carries the user-facing message the channels use
    /// and the session it belonged to, for the same reason as `Done` (#2614).
    Failed { session: Uuid, text: String },
    /// A detached background task finished: the resumed-turn payload, not a UI
    /// notice. The row it settles is drawn and closed by the progress events;
    /// this is what tells the agent the command it backgrounded is done.
    ///
    /// `context_text` is what the model reads (the command, its status and its
    /// output); `display_text` is the one-line receipt the user sees.
    BackgroundTaskDone {
        session_id: Uuid,
        context_text: String,
        display_text: String,
    },
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
    /// A model switch finished; carries the session that asked and the line
    /// the terminal prints for it.
    ModelSwitched { session: Uuid, line: String },
    /// A model switch could not be attempted; carries the session that asked
    /// and the reason.
    ModelSwitchFailed { session: Uuid, error: String },
    /// A stopped turn's rows are gone: reload the conversation without the
    /// user bubble that was handed back to the composer (#2635).
    TurnReverted { session: Uuid },
    /// A text command answered off the UI thread; the line goes to the
    /// notices (#2469).
    CommandReply(String),
    /// A text command asking for one visible turn to be dispatched on the UI
    /// thread: `/plan <query>`, `/execute` seeding the checklist, or `/clear`
    /// asking the agent to acknowledge the cut.
    CommandTurn(String),
    /// Personalized brain files were written from the onboarding notes; carries
    /// the line naming how many landed.
    BrainReady(String),
    /// Brain generation could not finish; carries the reason.
    BrainFailed(String),
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

/// How often the window asks GitHub for a newer release: twice a day (#2406).
const UPDATE_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);

/// Check for a newer release now and then twice a day, on its own thread. A
/// release found comes back as `UpdateAvailable` (#2406).
pub fn spawn_update_checks(sink: EventSink) {
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-update-check".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::logging::error(&format!("update check runtime: {e}"));
                    return;
                }
            };
            runtime.block_on(async move {
                loop {
                    if let Some(latest) = opencrabs::brain::tools::evolve::check_for_update().await
                        && let Err(e) = sink.send(TurnEvent::UpdateAvailable(latest))
                    {
                        crate::logging::error(&format!("update notice not delivered: {e}"));
                        return;
                    }
                    tokio::time::sleep(UPDATE_CHECK_EVERY).await;
                }
            });
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("update check thread: {e}"));
    }
}

/// Delete one session's messages from the ledger, as the TUI's ctrl+k does. The
/// session row stays, so the conversation is kept and only its history clears (#2430).
pub fn spawn_clear_messages(db_path: PathBuf, session_id: Uuid, sink: EventSink) {
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-clear-messages".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::logging::error(&format!("clear runtime: {e}"));
                    let _ = sink.send(TurnEvent::ClearFailed(format!("clear: {e}")));
                    return;
                }
            };
            let event = runtime.block_on(async move {
                let db = match Database::connect(&db_path).await {
                    Ok(db) => db,
                    Err(e) => return TurnEvent::ClearFailed(format!("clear: {e}")),
                };
                let service = opencrabs::services::MessageService::new(ServiceContext::new(
                    db.pool().clone(),
                ));
                match service.delete_messages_for_session(session_id).await {
                    Ok(()) => TurnEvent::MessagesCleared(session_id),
                    Err(e) => TurnEvent::ClearFailed(format!("clear: {e}")),
                }
            });
            if let Err(e) = sink.send(event) {
                crate::logging::error(&format!("clear result not delivered: {e}"));
            }
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("clear thread: {e}"));
    }
}

/// Run `/evolve` on its own thread, as the TUI does: download the latest
/// release and report the outcome. The text comes back as `EvolveDone` (#2406).
pub fn spawn_evolve(sink: EventSink, session_id: Uuid) {
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-evolve".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::logging::error(&format!("evolve runtime: {e}"));
                    let _ = sink.send(TurnEvent::EvolveDone(format!("Evolve error: {e}")));
                    return;
                }
            };
            let text = runtime.block_on(async move {
                let tool = opencrabs::brain::tools::evolve::EvolveTool::new(None);
                let ctx = opencrabs::brain::tools::ToolExecutionContext::new(session_id);
                match opencrabs::brain::tools::Tool::execute(&tool, serde_json::json!({}), &ctx)
                    .await
                {
                    Ok(result) if result.success => {
                        if result.output.is_empty() {
                            "Evolve finished.".to_string()
                        } else {
                            result.output
                        }
                    }
                    Ok(result) => format!(
                        "Evolve failed: {}",
                        result.error.unwrap_or_else(|| "unknown error".to_string())
                    ),
                    Err(e) => format!("Evolve error: {e}"),
                }
            });
            if let Err(e) = sink.send(TurnEvent::EvolveDone(text)) {
                crate::logging::error(&format!("evolve result not delivered: {e}"));
            }
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("evolve thread: {e}"));
    }
}

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
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
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
            // Kept for the life of the process, not just for the boot: the
            // channel agents and the cron scheduler live on this runtime, and a
            // dropped runtime would stop both while the window still ran
            // (#2452, #2607). Two workers: one poll, one for whatever a channel
            // or a job is doing.
            let runtime = SURFACE_RUNTIME.get_or_init(|| runtime);
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

/// The runtime the window's background services run on, kept alive for the
/// whole process. The bridge's own calls still open a short-lived runtime each,
/// which is cheap and cannot collide with this one; the channel agents and the
/// cron scheduler need a runtime that outlives the boot thread.
static SURFACE_RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

/// Channels, cron and hot reload, held for the process so their locks stay
/// claimed (#2452, #2607).
static SURFACE_STACK: std::sync::OnceLock<opencrabs::services::surface::SurfaceServices> =
    std::sync::OnceLock::new();

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

    // Background-task resume producer (#2322): this setter is the ONE place a
    // `BackgroundTaskManager` is created, and the window never called it. With
    // no manager a long command cannot detach at all, `tasks_list` is blind,
    // and no detached row can be drawn or settled.
    let enqueue: MessageEnqueueCallback = install_enqueue_forwarder(&sink);

    // The services half (#2452, #2607). Whichever surface is running owns the
    // profile, so the window claims it here and serves the same things the TUI
    // serves: channels, scheduled jobs, hot reload, memory upkeep. Best effort
    // on purpose: if another instance holds the profile and will not stop, the
    // window still chats, it just does not serve.
    let notice_sink: opencrabs::services::surface::NoticeSink = {
        let sink = sink.clone();
        Arc::new(move |msg: &str| {
            crate::logging::info(&format!("surface: {msg}"));
            let _ = sink.send(TurnEvent::CommandReply(msg.to_string()));
        })
    };
    let provider_hook: Arc<dyn Fn(Arc<dyn opencrabs::brain::provider::Provider>) + Send + Sync> =
        Arc::new(|new_provider| {
            if let Some(bridge) = bridge() {
                bridge.service.swap_provider(new_provider);
                crate::logging::info("config reload: window agent swapped provider");
            }
        });
    let deps = opencrabs::services::surface::SurfaceDeps {
        pool: db.pool().clone(),
        provider: provider.clone(),
        tool_registry: tool_registry.clone(),
        subagent_manager: subagent_manager.clone(),
        service_context: context.clone(),
        system_brain: system_brain.clone(),
        working_directory: working_directory.clone(),
        brain_path,
        runtime_info: Some(runtime_info),
        shared_session_id: Arc::new(tokio::sync::Mutex::new(None)),
        notices: notice_sink,
        session_events: None,
        progress: Some(forwarder.clone()),
        provider_reloaded: Some(provider_hook),
        // Interactive: opening the window is a human asking to use this profile
        // now, the same claim the TUI makes.
        headless: false,
    };
    // Detached on purpose: connecting the channels can take a few seconds and a
    // slow gateway waits up to 30s, and the window must not sit on a boot
    // screen for that. Services come up behind an already usable chat.
    {
        let boot_config = config.clone();
        tokio::spawn(async move {
            match opencrabs::services::surface::boot_surface(&boot_config, deps).await {
                Ok(services) => {
                    crate::logging::info(&format!(
                        "surface services ready: cron={} a2a={} preempted={}",
                        services.cron_spawned,
                        services.a2a_ready,
                        services.preempted.len()
                    ));
                    let _ = SURFACE_STACK.set(services);
                }
                Err(e) => {
                    // The window keeps working; only the background half is off.
                    crate::logging::error(&format!("surface services failed: {e:#}"));
                }
            }
        });
    }

    let service = AgentService::new(provider, context.clone(), &config)
        .await
        .with_system_brain(system_brain)
        .with_working_directory(working_directory)
        .with_auto_approve_tools(auto_approve)
        .with_tool_registry(tool_registry)
        .with_subagent_manager(subagent_manager)
        .with_progress_callback(Some(forwarder))
        .with_message_enqueue_callback(Some(enqueue.clone()))
        .with_message_queue_callback(Some(queue_drain_callback()));

    // Delivery route for a completion on a session no channel owns: a
    // sub-agent reached from a tool carries no callback of its own and
    // resolves this instead (#1036). The window is its own process, so this
    // static does not collide with a running TUI.
    opencrabs::brain::agent::service::register_local_route(enqueue);

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

/// One row of the ctrl+g files browser: the tracked path plus its size. A
/// projection of `db::models::File` so the window never reaches into the
/// DB model crate-wide.
pub struct SessionFile {
    pub path: PathBuf,
    pub size: Option<i64>,
}

/// The open conversation's tracked files, the fetch behind ctrl+g. Same shape
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
                    let _ = sink.send(TurnEvent::Failed {
                        session: session_id,
                        text: format!("turn runtime: {e}"),
                    });
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
                            session: session_id,
                            content: response.content,
                            context_tokens: response.context_tokens,
                            tokens_per_second: response.tokens_per_second,
                        });
                    }
                    Err(e) => {
                        let msg = opencrabs::brain::agent::format_user_error(&e);
                        let _ = sink.send(TurnEvent::Failed {
                            session: session_id,
                            text: msg,
                        });
                    }
                }
            });
        });
    if let Err(e) = spawned {
        let _ = sink.send(TurnEvent::Failed {
            session: session_id,
            text: format!("turn thread spawn: {e}"),
        });
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

/// The window's completion sink: a finished background task becomes a
/// [`TurnEvent::BackgroundTaskDone`], so the open conversation resumes with the
/// result instead of going quiet while the work it started still runs.
///
/// A failed send is logged, never dropped: a silently discarded completion is
/// exactly what made this class of loss invisible on the TUI (#1206).
fn install_enqueue_forwarder(sink: &EventSink) -> MessageEnqueueCallback {
    let sink = sink.clone();
    Arc::new(move |session_id, msg| {
        if let Err(e) = sink.send(TurnEvent::BackgroundTaskDone {
            session_id,
            context_text: msg.context_text,
            display_text: msg.display_text,
        }) {
            tracing::error!(
                target: "background_task",
                "Could not hand session {session_id}'s completion to the GUI: {e}"
            );
        }
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

/// Messages the user typed while a turn ran, keyed by session. The agent takes
/// them between tool calls, the way the terminal does, so a queued message
/// reaches the running turn instead of waiting for it to end (#2441).
static QUEUE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<Uuid, Vec<QueuedUserMessage>>>,
> = std::sync::OnceLock::new();

/// The session the composer is writing into, so Up can recall its last queued
/// message without the editor knowing the session (#2441).
static COMPOSER_SESSION: std::sync::Mutex<Option<Uuid>> = std::sync::Mutex::new(None);

fn queue() -> std::sync::MutexGuard<'static, std::collections::HashMap<Uuid, Vec<QueuedUserMessage>>>
{
    QUEUE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Queue a message the user typed while `session_id` is busy.
pub fn queue_push(session_id: Uuid, msg: QueuedUserMessage) {
    queue().entry(session_id).or_default().push(msg);
}

/// What is waiting for `session_id`, oldest first. The strip shows these.
pub fn queue_snapshot(session_id: Uuid) -> Vec<QueuedUserMessage> {
    queue().get(&session_id).cloned().unwrap_or_default()
}

/// Take everything waiting for `session_id`, so it is sent once. A recall hold
/// names a slot in this queue, so it goes with it (#2629).
pub fn queue_take(session_id: Uuid) -> Vec<QueuedUserMessage> {
    queue_recall_hold(None);
    queue().remove(&session_id).unwrap_or_default()
}

/// Which slot `depth` names in a queue of `len`, counting back from the
/// newest. `None` once Up walks past the head (#2629).
fn recall_index(len: usize, depth: usize) -> Option<usize> {
    len.checked_sub(depth + 1)
}

/// The queued message `depth` places back from the newest, left where it is.
///
/// Recall used to pop the tail, so only the newest message was ever editable
/// and the rest were unreachable until the turn ended (#2629). It reads
/// instead: the queue keeps every message, and the edit lands back in its own
/// slot when it is sent again.
pub fn queue_recall_at(depth: usize) -> Option<QueuedUserMessage> {
    let session = (*COMPOSER_SESSION.lock().ok()?)?;
    let snapshot = queue_snapshot(session);
    let index = recall_index(snapshot.len(), depth)?;
    snapshot.get(index).cloned()
}

/// Which queued message the composer is holding, by the text it went in with,
/// so the send path can put an edit back where it came from instead of
/// appending a second copy (#2629).
///
/// A position would be wrong: a background report landing while the user edits
/// pushes every slot one down, and the send would replace a message they never
/// touched.
static RECALL_HELD: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Remember (or forget) the queued message the focused composer went back to.
pub fn queue_recall_hold(text: Option<String>) {
    if let Ok(mut held) = RECALL_HELD.lock() {
        *held = text;
    }
}

/// The text the focused composer recalled, if it is editing a queued message.
pub fn queue_recall_held() -> Option<String> {
    RECALL_HELD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Send a recalled message: `Some(edited)` replaces the queued one in its own
/// slot, `None` drops it because the turn it was queued behind is over and
/// this text is what is running now.
///
/// The slot is found by text, and a message that left the queue while the user
/// was editing is queued again as an edited copy: their words are never
/// silently dropped (#2629).
pub fn queue_recall_commit(session_id: Uuid, original: &str, edited: Option<QueuedUserMessage>) {
    let mut replaced = false;
    {
        let mut map = queue();
        if let Some(entry) = map.get_mut(&session_id)
            && let Some(index) = entry.iter().rposition(|m| m.display_text == original)
        {
            match edited.clone() {
                Some(msg) => entry[index] = msg,
                None => {
                    entry.remove(index);
                }
            }
            replaced = true;
            if entry.is_empty() {
                map.remove(&session_id);
            }
        }
    }
    if !replaced && let Some(msg) = edited {
        queue_push(session_id, msg);
    }
    queue_recall_hold(None);
}

/// Point the composer at `session_id`, so recall and the strip agree with it.
pub fn set_composer_session(session_id: Option<Uuid>) {
    if let Ok(mut s) = COMPOSER_SESSION.lock() {
        *s = session_id;
    }
}

/// The agent's between-tool-calls hook: everything queued for the session,
/// joined into one message. The agent calls it after each tool round.
fn queue_drain_callback() -> opencrabs::brain::agent::service::MessageQueueCallback {
    Arc::new(|session_id| Box::pin(async move { QueuedUserMessage::join(&queue_take(session_id)) }))
}

/// A message queued while a turn runs: the TUI's exact type, joined and
/// submitted when the current turn completes.
pub type PendingMessage = QueuedUserMessage;

/// Switch the model behind the live service on a big-stack worker.
///
/// `direct_model_switch` is the terminal's own path: it writes the config,
/// recreates the provider and updates the session row, so the window cannot
/// drift from what the terminal would have done with the same line. It can do
/// network work (a handshake per candidate), which is why it never runs on the
/// UI thread and never on a gpui worker (tiny stacks, #2299).
pub fn spawn_switch_model(
    service: Arc<AgentService>,
    session_id: Uuid,
    pair: String,
    sink: EventSink,
) {
    let error_sink = sink.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-model-switch".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = sink.send(TurnEvent::ModelSwitchFailed {
                        session: session_id,
                        error: format!("switch runtime: {e}"),
                    });
                    return;
                }
            };
            let line = runtime.block_on(opencrabs::channels::commands::direct_model_switch(
                &service, session_id, &pair,
            ));
            let _ = sink.send(TurnEvent::ModelSwitched {
                session: session_id,
                line,
            });
        });
    if let Err(e) = spawned {
        let _ = error_sink.send(TurnEvent::ModelSwitchFailed {
            session: session_id,
            error: format!("switch thread: {e}"),
        });
    }
}

/// Delete the rows a turn wrote before it was stopped (#2635).
///
/// A turn persists its `user` row and an empty `assistant` placeholder at the
/// moment it starts, before the provider is even asked. A stop that beats the
/// first token hands that text back to the composer, so leaving those rows
/// would redraw the bubble the user just took back on the next reload. The
/// trailing pair is cut only when it is the pair the turn opened: a finished
/// turn, or anything a tool wrote, stops the walk.
pub fn spawn_undo_last_turn(service: Arc<AgentService>, session_id: Uuid, sink: EventSink) {
    let done_sink = sink.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-turn-undo".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::logging::error(&format!("turn undo runtime: {e}"));
                    let _ = done_sink.send(TurnEvent::TurnReverted { session: session_id });
                    return;
                }
            };
            runtime.block_on(async move {
                let messages =
                    opencrabs::services::MessageService::new(service.context().clone());
                let rows = match messages.list_messages_for_session(session_id).await {
                    Ok(rows) => rows,
                    Err(e) => {
                        crate::logging::error(&format!("turn undo read: {e}"));
                        return;
                    }
                };
                // Newest first: the placeholder, then the question it echoed.
                let Some(placeholder) = rows.last() else {
                    return;
                };
                if placeholder.role != "assistant" || !placeholder.content.trim().is_empty() {
                    // The turn had already answered, or wrote nothing yet. An
                    // answer that landed between the stop and this thread is
                    // the user's: only the empty placeholder is ever cut.
                    return;
                }
                if let Err(e) = messages.delete_message(placeholder.id).await {
                    crate::logging::error(&format!("turn undo assistant row: {e}"));
                    return;
                }
                let Some(question) = rows.get(rows.len().saturating_sub(2)) else {
                    let _ = done_sink.send(TurnEvent::TurnReverted { session: session_id });
                    return;
                };
                if question.role == "user"
                    && let Err(e) = messages.delete_message(question.id).await
                {
                    crate::logging::error(&format!("turn undo user row: {e}"));
                }
                let _ = done_sink.send(TurnEvent::TurnReverted { session: session_id });
            });
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("turn undo thread: {e}"));
        let _ = sink.send(TurnEvent::TurnReverted { session: session_id });
    }
}

/// Generate the personalized brain files from the onboarding notes.
///
/// A direct port of the terminal's `generate_brain_files_background`
/// (`tui/app/dialogs.rs`): one completion through the provider the just-saved
/// config selected, parsed into the five brain files and written into the
/// workspace. The terminal runs it in the background after setup so nobody
/// waits on it; the window does the same and reports the outcome as a notice.
pub fn spawn_brain_generation(
    service: Arc<AgentService>,
    prompt: String,
    workspace: PathBuf,
    sink: EventSink,
) {
    let error_sink = sink.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-brain-gen".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = sink.send(TurnEvent::BrainFailed(format!("runtime: {e}")));
                    return;
                }
            };
            let provider = service.provider().clone();
            let model = service.provider_model().to_string();
            let request = opencrabs::brain::provider::LLMRequest::new(
                model,
                vec![opencrabs::brain::provider::Message::user(prompt)],
            )
            .with_max_tokens(65536);
            let result = runtime.block_on(async move {
                match tokio::time::timeout(Duration::from_secs(120), provider.complete(request))
                    .await
                {
                    Ok(Ok(response)) => {
                        let text: String = response
                            .content
                            .iter()
                            .filter_map(|block| match block {
                                opencrabs::brain::provider::ContentBlock::Text { text } => {
                                    Some(text.as_str())
                                }
                                _ => None,
                            })
                            .collect();
                        let parsed = opencrabs::tui::onboarding::parse_brain_sections(&text);
                        // SOUL and USER are the two a setup cannot do without;
                        // without them the reply was prose, not sections.
                        if parsed[0].is_none() || parsed[1].is_none() {
                            return Err("the answer had no brain sections".to_string());
                        }
                        let files = [
                            ("SOUL.md", &parsed[0]),
                            ("USER.md", &parsed[1]),
                            ("AGENTS.md", &parsed[2]),
                            ("TOOLS.md", &parsed[3]),
                            ("MEMORY.md", &parsed[4]),
                        ];
                        let mut written = 0;
                        for (name, content) in &files {
                            if let Some(content) = content {
                                if let Err(e) = std::fs::write(workspace.join(name), content) {
                                    return Err(format!("writing {name}: {e}"));
                                }
                                written += 1;
                            }
                        }
                        Ok(written)
                    }
                    Ok(Err(e)) => Err(format!("generation failed: {e}")),
                    Err(_) => Err("generation timed out after 120s".to_string()),
                }
            });
            let _ = match result {
                Ok(written) => sink.send(TurnEvent::BrainReady(format!(
                    "{written} brain files personalized"
                ))),
                Err(e) => sink.send(TurnEvent::BrainFailed(e)),
            };
        });
    if let Err(e) = spawned {
        let _ = error_sink.send(TurnEvent::BrainFailed(format!("thread: {e}")));
    }
}

/// Run one of the window's text commands off the UI thread (#2469).
///
/// The work is the engine's own: plan mode is `utils::plan_mode`, clear is
/// `AgentService::clear_context` (the call the channels make, which persists
/// the marker the loader cuts at), restart is `SelfUpdater`, and the approval
/// policy is the value `Config::write_key` already persists for the terminal.
/// Nothing here re-implements a command; it only decides which engine call
/// answers the line and what the window should paint.
pub fn spawn_text_command(
    db_path: PathBuf,
    session_id: Uuid,
    name: String,
    arg: String,
    sink: EventSink,
) {
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-text-command".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::logging::error(&format!("command runtime: {e}"));
                    let _ = sink.send(TurnEvent::CommandReply(format!("command: {e}")));
                    return;
                }
            };
            let events = runtime.block_on(run_text_command(&db_path, session_id, &name, &arg));
            for event in events {
                if let Err(e) = sink.send(event) {
                    crate::logging::error(&format!("command result not delivered: {e}"));
                    break;
                }
            }
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("command thread: {e}"));
    }
}

/// The engine calls behind one text command, in order.
async fn run_text_command(
    db_path: &std::path::Path,
    session_id: Uuid,
    name: &str,
    arg: &str,
) -> Vec<TurnEvent> {
    use super::text_commands as tc;

    let Some(command) = tc::classify(name) else {
        return vec![TurnEvent::CommandReply(format!(
            "{name} is not a window command."
        ))];
    };

    match command {
        // The window handles `/cd` itself (#2527): the picker screen for a
        // bare `/cd`, the local applier for a path. It never reaches this
        // runner, so this arm is an unreachable guard, not a reply.
        tc::TextCommand::Cd => vec![],
        tc::TextCommand::Plan => {
            let line = opencrabs::utils::plan_mode::enter_plan_mode(session_id).await;
            let mut out = vec![TurnEvent::CommandReply(line)];
            // `/plan <query>` enters plan mode and uses the trailing text as
            // the planning intent, exactly as the channels do (#579).
            if !arg.is_empty() {
                out.push(TurnEvent::CommandTurn(arg.to_string()));
            }
            out
        }
        tc::TextCommand::ShowPlan => vec![TurnEvent::CommandReply(
            opencrabs::utils::plan_mode::show_plan(session_id).await,
        )],
        tc::TextCommand::Execute => match opencrabs::utils::plan_mode::try_approve(
            session_id,
            opencrabs::tui::plan::ApprovalSource::User,
        )
        .await
        {
            opencrabs::utils::plan_mode::ApproveOutcome::Refused(text) => {
                vec![TurnEvent::CommandReply(text)]
            }
            opencrabs::utils::plan_mode::ApproveOutcome::SeedTurn { prompt } => {
                vec![TurnEvent::CommandTurn(prompt)]
            }
        },
        tc::TextCommand::Discard => {
            let pool = match Database::connect(db_path).await {
                Ok(db) => db.pool().clone(),
                Err(e) => {
                    return vec![TurnEvent::CommandReply(format!(
                        "/discard could not open the database: {e}"
                    ))];
                }
            };
            let svc = ServiceContext::new(pool);
            vec![TurnEvent::CommandReply(
                opencrabs::utils::plan_mode::discard(session_id, &svc).await,
            )]
        }
        tc::TextCommand::Clear => match bridge() {
            Some(bridge) => match bridge.service.clear_context(session_id).await {
                Ok(receipt) => vec![TurnEvent::CommandReply(
                    opencrabs::tui::clear_notice::cleared(&receipt),
                )],
                Err(e) => vec![TurnEvent::CommandReply(
                    opencrabs::tui::clear_notice::failed(&e),
                )],
            },
            None => vec![TurnEvent::CommandReply(
                "/clear needs the agent running; it is offline right now.".to_string(),
            )],
        },
        tc::TextCommand::Rtk => {
            #[cfg(feature = "rtk")]
            {
                if !opencrabs::rtk::is_rtk_available().await {
                    return vec![TurnEvent::CommandReply(
                        opencrabs::rtk::RTK_NOT_INSTALLED_HELP.to_string(),
                    )];
                }
                match tokio::process::Command::new("rtk")
                    .arg("gain")
                    .output()
                    .await
                {
                    Ok(output) => vec![TurnEvent::CommandReply(tc::rtk_reply(
                        output.status.success(),
                        &String::from_utf8_lossy(&output.stdout),
                        &String::from_utf8_lossy(&output.stderr),
                    ))],
                    Err(e) => vec![TurnEvent::CommandReply(format!("rtk gain: {e}"))],
                }
            }
            #[cfg(not(feature = "rtk"))]
            {
                vec![TurnEvent::CommandReply(
                    "RTK is not compiled into this build.".to_string(),
                )]
            }
        }
        tc::TextCommand::Approve => {
            if arg.is_empty() {
                let policy = opencrabs::config::Config::load()
                    .map(|cfg| cfg.agent.approval_policy)
                    .unwrap_or_else(|_| "ask".to_string());
                vec![TurnEvent::CommandReply(tc::approval_reply(&policy))]
            } else {
                match tc::approval_target(arg) {
                    Some(policy) => match opencrabs::config::Config::write_key(
                        "agent",
                        "approval_policy",
                        policy,
                    ) {
                        Ok(_) => {
                            // Both halves: the file and the live service. A
                            // written key alone would leave the running window
                            // gating tools by the policy it started with, which
                            // is the staleness the terminal had to rebuild for.
                            if let Some(b) = bridge() {
                                b.service.set_auto_approve_tools(
                                    opencrabs::utils::approval::policy_auto_approves(policy),
                                );
                            }
                            vec![TurnEvent::CommandReply(tc::approval_set(policy))]
                        }
                        Err(e) => vec![TurnEvent::CommandReply(format!(
                            "/approve could not write the policy: {e}"
                        ))],
                    },
                    None => vec![TurnEvent::CommandReply(tc::approval_unknown(arg))],
                }
            }
        }
        // `Cd` has no arm here any more (#2527): the window intercepts it in
        // `run_text_command` (the picker screen for a bare `/cd`, the local
        // applier for `/cd <path>`), so a directory change never travels the
        // turn pipeline and never lights the thinking spinner.
        tc::TextCommand::Profiles => {
            let current = opencrabs::config::profile::current_profile_name();
            let entries: Vec<(String, Option<String>)> =
                opencrabs::config::profile::list_profiles()
                    .map(|rows| {
                        rows.into_iter()
                            .map(|row| (row.name, row.description))
                            .collect()
                    })
                    .unwrap_or_default();
            vec![TurnEvent::CommandReply(tc::profiles_reply(
                &entries, &current,
            ))]
        }
        tc::TextCommand::Restart => {
            // Shares the terminal's restart so the session is resumed on the
            // way back up instead of coming back to an empty chat. Only
            // returns when the restart could not be started.
            match opencrabs::brain::SelfUpdater::auto_detect().and_then(|u| u.restart(session_id)) {
                Ok(()) => Vec::new(),
                Err(e) => vec![TurnEvent::CommandReply(format!(
                    "/restart could not cycle the process: {e}"
                ))],
            }
        }
        tc::TextCommand::Whisper => {
            // The terminal's own installer and its own binary, launched the
            // same way: `/whisper` must not grow a second downloader here.
            match opencrabs::tui::app::ensure_whispercrabs().await {
                Ok(binary) => {
                    match std::process::Command::new(&binary)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        Ok(_) => vec![TurnEvent::CommandReply(tc::whisper_reply())],
                        Err(e) => vec![TurnEvent::CommandReply(tc::whisper_failed(&format!(
                            "Failed to launch WhisperCrabs: {e}"
                        )))],
                    }
                }
                Err(e) => vec![TurnEvent::CommandReply(tc::whisper_failed(&e.to_string()))],
            }
        }
        // Handled on the UI thread: both compose a turn rather than reading
        // the engine, so the window dispatches them where `send_message` lives.
        tc::TextCommand::Compact | tc::TextCommand::Goal | tc::TextCommand::Cowork => {
            vec![TurnEvent::CommandReply(format!(
                "{name} is dispatched by the window, not the bridge."
            ))]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Up walks the whole queue, newest first (#2629). Recall used to pop the
    /// tail, which made the older messages unreachable for the whole turn.
    #[test]
    fn up_recalls_the_queue_one_message_at_a_time() {
        // One test for the queue's recall path: `COMPOSER_SESSION` and `QUEUE`
        // are process-wide, so two tests here would race on them.
        let session = Uuid::from_u128(0x2629_0001);
        let other = Uuid::from_u128(0x2629_0002);

        for text in ["one", "two", "three"] {
            queue_push(session, QueuedUserMessage::plain(text.to_string()));
        }
        // A completion for another session must not enter this queue.
        queue_push(other, QueuedUserMessage::plain("elsewhere".to_string()));

        set_composer_session(Some(session));
        let walked: Vec<String> = (0..)
            .map_while(|depth| queue_recall_at(depth).map(|msg| msg.display_text))
            .collect();
        assert_eq!(
            walked,
            vec!["three".to_string(), "two".to_string(), "one".to_string()],
            "Up walks from the newest to the oldest"
        );
        assert_eq!(
            queue_snapshot(session).len(),
            3,
            "walking the queue leaves every message in it, so an edit is not a removal"
        );

        // Sending the edited middle message replaces it where it stands.
        queue_recall_hold(Some("two".to_string()));
        queue_recall_commit(
            session,
            "two",
            Some(QueuedUserMessage::plain("two edited".to_string())),
        );
        let left: Vec<String> = queue_snapshot(session)
            .iter()
            .map(|msg| msg.display_text.clone())
            .collect();
        assert_eq!(
            left,
            vec![
                "one".to_string(),
                "two edited".to_string(),
                "three".to_string()
            ],
            "an edit lands in its own slot instead of appending a copy"
        );
        assert!(queue_recall_held().is_none(), "the hold ends with the send");

        // A message that left the queue while it was being edited is queued
        // again rather than dropped.
        queue_recall_commit(
            session,
            "gone",
            Some(QueuedUserMessage::plain("rescued".to_string())),
        );
        assert_eq!(
            queue_snapshot(session)
                .last()
                .map(|m| m.display_text.clone()),
            Some("rescued".to_string()),
            "typed words are never silently dropped"
        );

        let _ = queue_take(session);
        let _ = queue_take(other);
        set_composer_session(None);
    }
}
