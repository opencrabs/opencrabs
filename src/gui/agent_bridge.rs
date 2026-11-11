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
use opencrabs::brain::tools::suggest_options::SuggestionItem;
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
    ProgressFor(Uuid, ProgressEvent, std::time::Instant),
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
    /// The user stopped this turn and the stop dropped it where it stood
    /// (#2657). No `Done` and no `Failed` follows a dropped future, so this is
    /// the only thing that tells the window the turn is gone. It carries no
    /// text on purpose: whoever asked for the stop already said so, and this
    /// just settles a card that would otherwise claim work forever.
    Cancelled { session: Uuid },
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
    /// A turn the previous process died in the middle of is running again
    /// (#2627). The message travels with it because the card has to show what
    /// is being answered, and the cancel handle because a resumed turn is a
    /// turn: the stop button has to be able to end it.
    TurnResumed {
        session: Uuid,
        /// The message the killed process was answering.
        user_message: String,
        /// The handle that stops it, exactly like one the composer started.
        cancel: tokio_util::sync::CancellationToken,
    },
    /// A text command answered off the UI thread; the line goes to the
    /// notices (#2469).
    CommandReply { session: Option<Uuid>, text: String },
    /// A text command asking for one visible turn to be dispatched on the UI
    /// thread: `/plan <query>`, `/execute` seeding the checklist, or `/clear`
    /// asking the agent to acknowledge the cut. The turn belongs to the
    /// session that ran the command, so it carries that id with it (#2672).
    ///
    /// `display` is the line the user sees when the text is scaffolding the
    /// model needs and the user does not (#2756). `/execute` is the case that
    /// matters: the model reads a full `[SYSTEM: PLAN APPROVED]` prompt, the
    /// window shows the receipt, and history keeps the receipt.
    CommandTurn {
        session: Uuid,
        text: String,
        display: Option<String>,
    },
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

/// The window must exist before the GUI claims the profile (#2784). Claiming
/// preempts (SIGTERMs) the running TUI/daemon, so doing it before the window
/// opened meant a window that failed to open left the user with no surface at
/// all. Bootstrap and window-open race: whichever finishes last releases the
/// claim, exactly once.
static SURFACE_GATE: SurfaceGate<(Config, opencrabs::services::surface::SurfaceDeps)> =
    SurfaceGate::new();

/// Coordination for the deferred surface claim. Generic over the payload so the
/// ordering invariant is unit-testable without booting real channels.
struct SurfaceGate<T> {
    window_up: std::sync::atomic::AtomicBool,
    pending: std::sync::Mutex<Option<T>>,
}

impl<T> SurfaceGate<T> {
    const fn new() -> Self {
        Self {
            window_up: std::sync::atomic::AtomicBool::new(false),
            pending: std::sync::Mutex::new(None),
        }
    }

    /// Bootstrap finished: hold the deps until the window is up.
    fn set_pending(&self, deps: T) {
        *self.pending.lock().unwrap() = Some(deps);
    }

    /// The window opened successfully.
    fn mark_window_up(&self) {
        self.window_up
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Take the deps only once the window is up. The `.take()` under the lock
    /// means exactly one caller ever receives them, whichever side wins the race.
    fn take_if_ready(&self) -> Option<T> {
        if !self.window_up.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        self.pending.lock().unwrap().take()
    }
}

/// Boot the surface (claim the profile, serve channels) once the window is up
/// AND bootstrap has produced the deps. Called from both sides of the race.
fn maybe_boot_surface() {
    // The deps only exist once build() has run on SURFACE_RUNTIME, so if there
    // is something to take, the runtime is already initialised.
    let Some(runtime) = SURFACE_RUNTIME.get() else {
        return;
    };
    let Some((config, deps)) = SURFACE_GATE.take_if_ready() else {
        return;
    };
    runtime.spawn(async move {
        match opencrabs::services::surface::boot_surface(&config, deps).await {
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

/// Called by the window once `cx.open_window` has succeeded (#2784). Releases
/// the surface claim that bootstrap deferred until the window exists, so a
/// failed window never takes the running TUI/daemon down with it.
pub fn window_is_up() {
    SURFACE_GATE.mark_window_up();
    maybe_boot_surface();
}

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
            let _ = sink.send(TurnEvent::CommandReply {
                session: None,
                text: msg.to_string(),
            });
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
    // Claim the profile only once the window exists (#2784). Claiming preempts
    // the running TUI/daemon, so doing it here, before cx.open_window has run,
    // meant a window that failed to open killed the user's running surface and
    // left nothing behind. Stash the deps and let the window-open side release
    // the claim; whichever of the two finishes last boots the surface, once.
    // Still detached: connecting channels can take a few seconds and a slow
    // gateway waits up to 30s, so services come up behind an already usable chat.
    {
        let boot_config = config.clone();
        SURFACE_GATE.set_pending((boot_config, deps));
        maybe_boot_surface();
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
    let recovery_enqueue = enqueue.clone();
    opencrabs::brain::agent::service::register_local_route(enqueue);

    // The same pass the terminal runs at boot (#2626). Detached commands and
    // sub-agents die with the process, but their rows do not: without this the
    // window left every row from the previous run unaccounted for, so nothing
    // was reported into the session that owns it and the row survived to
    // report again on the next boot. On the surface runtime rather than a
    // thread of its own, because `recover` schedules its last-resort flush 30s
    // out and a task belonging to a runtime that is dropped when a thread's
    // `block_on` returns never runs.
    tokio::spawn(async move {
        let reported =
            opencrabs::brain::agent::service::restart_recovery::recover(Some(recovery_enqueue))
                .await;
        // Logged with the count on every boot, zero included: a silent boot
        // and a boot that never ran the pass read the same otherwise.
        crate::logging::info(&format!(
            "restart recovery: {reported} item(s) interrupted by a previous run"
        ));
    });

    let bridge = AgentBridge {
        service: Arc::new(service),
    };

    // Pick up the turns the last process died inside (#2627). Every turn the
    // window runs writes `pending_requests`, but nothing read the table back,
    // so a window closed mid-tool came up idle while the terminal on the same
    // ledger carried on. Detached: the scan is a DB round trip and the resumes
    // are their own threads, so the window is usable while they start.
    spawn_boot_resume(bridge.service.clone(), db.pool().clone(), sink.clone());

    Ok(bridge)
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

/// Race a turn against its stop token (#2657).
///
/// The service reads the token cooperatively: at tool-iteration boundaries,
/// and once a streaming call has returned. Inside one long provider stream
/// neither checkpoint runs, so a stop was invisible until the answer finished
/// (`turn cancelled by Esc`, then `turn done` tens of seconds later). Racing
/// the whole turn is the window's equivalent of the terminal's
/// `handle.abort()`: when the token fires the future is dropped where it
/// stands, which runs the service's own drop guards and keeps the text it
/// already streamed.
///
/// Biased on the token, so a stop that lands in the same poll as the answer
/// wins. The user asked for the turn to be gone; painting an answer over their
/// cancellation is what makes a stop look like it did nothing.
///
/// `None` means the stop fired and the turn was dropped. `Some(output)` is the
/// turn's own result, untouched.
pub async fn race_turn<F>(
    turn: F,
    cancel: &tokio_util::sync::CancellationToken,
) -> Option<F::Output>
where
    F: std::future::Future,
{
    tokio::pin!(turn);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        output = &mut turn => Some(output),
    }
}

/// Cut the trailing unanswered pair before a resend duplicates it (#2697).
///
/// A turn writes its `user` row and an empty `assistant` placeholder at the
/// moment it starts, before the provider is asked. A stop that lands after the
/// first token cannot hand the text back (the `untouched()` hand-back only
/// covers a stop that beat the token), so that pair stays behind with no
/// answer, and sending the same text again appended a second copy of it.
///
/// The pair is cut only when it is provably leftover: the newest row is an
/// `assistant` row whose content is still empty, and the row before it is a
/// `user` row carrying exactly the outgoing text. Anything else ends the walk.
/// A real answer, a turn that ran a tool (tool activity lives on the assistant
/// row, so an empty one answered nothing), and a different question are
/// history, and history is never touched.
async fn drop_unanswered_duplicate(
    messages: &opencrabs::services::MessageService,
    session_id: Uuid,
    outgoing: &str,
) {
    let rows = match messages
        .list_recent_messages_for_session(session_id, 2)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            crate::logging::error(&format!("gui dedup read: {e}"));
            return;
        }
    };
    let Some(placeholder) = rows.last() else {
        return;
    };
    if placeholder.role != "assistant" || !placeholder.content.trim().is_empty() {
        return;
    }
    let Some(question) = rows.get(rows.len().saturating_sub(2)) else {
        return;
    };
    if question.role != "user" {
        return;
    }
    // The ledger keeps the redacted form of the text the turn was given.
    let stored = opencrabs::utils::sanitize::redact_secrets(outgoing);
    if question.content.trim() != stored.trim() {
        return;
    }
    for row in [question, placeholder] {
        if let Err(e) = messages.delete_message(row.id).await {
            crate::logging::error(&format!("gui dedup delete: {e}"));
            return;
        }
    }
    crate::logging::info(&format!(
        "gui dedup: dropped the unanswered pair a resend would have duplicated \
         (session {session_id})"
    ));
}

/// Run one turn on a 4 MB thread: the TUI's `send_message_with_tools_and_mode`
/// call with the queue-completion handshake the TUI does around it.
///
/// The progress forwarder (`install_progress_forwarder`) already streams
/// events to `sink`; this thread only delivers the terminal event.
///
/// `display` is the row the user should read for this turn: the receipt of a
/// command, or a channel's clean rendering of a wrapped message. The model
/// always receives `content`; history gets `display` when one is supplied
/// (#2756). Callers with nothing to hide pass `None` and history keeps the
/// text that was sent.
pub fn spawn_turn(
    service: Arc<AgentService>,
    session_id: Uuid,
    content: String,
    display: Option<String>,
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
                // Before the turn writes its rows: a resend of the text an
                // unanswered turn left behind must not stack a second copy of
                // it in history (#2697).
                // Compare against the row history will actually hold, which is
                // the display line when a turn hides its prompt (#2756).
                let persisted = display.as_deref().unwrap_or(&content);
                let messages = opencrabs::services::MessageService::new(service.context().clone());
                drop_unanswered_duplicate(&messages, session_id, persisted).await;
                let turn = service.send_message_with_tools_and_display(
                    session_id,
                    content,
                    display,
                    None,
                    Some(cancel.clone()),
                    None,
                    None,
                    "tui",
                    None,
                    None,
                );
                // A cooperative token only lands where the service looks for
                // it, and inside one provider stream it never looks (#2657).
                let Some(outcome) = race_turn(turn, &cancel).await else {
                    let _ = sink.send(TurnEvent::Cancelled {
                        session: session_id,
                    });
                    return;
                };
                match outcome {
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

/// Re-run one interrupted turn on a 4 MB thread (#2627).
///
/// `resume_interrupted_turn`, not `send_message_with_tools_and_mode`: a resume
/// is a one-shot recovery and must never re-track itself. If this turn is
/// interrupted too (a stop, another crash) the session has already been
/// cleared from `pending_requests`, so it cannot resume forever (#729).
fn spawn_resume(
    service: Arc<AgentService>,
    session_id: Uuid,
    content: String,
    sink: EventSink,
    cancel: tokio_util::sync::CancellationToken,
    pending: Arc<std::sync::atomic::AtomicUsize>,
) {
    let thread_sink = sink.clone();
    let thread_pending = pending.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-agent-resume".into())
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
                        text: format!("resume runtime: {e}"),
                    });
                    finish_boot_resume(&thread_pending, &sink);
                    return;
                }
            };
            let turn_sink = sink.clone();
            runtime.block_on(async move {
                let turn = service.resume_interrupted_turn(
                    session_id,
                    content,
                    None,
                    Some(cancel.clone()),
                    None,
                    None,
                    "tui",
                    None,
                );
                // A resumed turn is a turn: the same stop applies, and the
                // same cooperative gap (#2657).
                let Some(outcome) = race_turn(turn, &cancel).await else {
                    // It answered nobody, so it is not a delivered resume; the
                    // summary must not claim otherwise.
                    opencrabs::brain::agent::service::boot_report::record_failed();
                    let _ = turn_sink.send(TurnEvent::Cancelled {
                        session: session_id,
                    });
                    return;
                };
                match outcome {
                    Ok(response) => {
                        opencrabs::brain::agent::service::boot_report::record_delivered();
                        let _ = turn_sink.send(TurnEvent::Done {
                            session: session_id,
                            content: response.content,
                            context_tokens: response.context_tokens,
                            tokens_per_second: response.tokens_per_second,
                        });
                    }
                    Err(e) => {
                        opencrabs::brain::agent::service::boot_report::record_failed();
                        let msg = opencrabs::brain::agent::format_user_error(&e);
                        let _ = turn_sink.send(TurnEvent::Failed {
                            session: session_id,
                            text: msg,
                        });
                    }
                }
            });
            finish_boot_resume(&thread_pending, &sink);
        });
    if let Err(e) = spawned {
        // A thread that never started must still resolve the boot line, or the
        // summary is never emitted at all.
        crate::logging::error(&format!("resume thread spawn failed: {e}"));
        finish_boot_resume(&pending, &sink);
    }
}

/// Resolve the boot accounting once the last dispatched resume is done.
///
/// The summary is only worth reading after every resume has answered, because
/// that is when `delivered` and `failed` are final. Emitting it at dispatch
/// time would print a row of zeros and look like nothing came back.
fn finish_boot_resume(pending: &std::sync::atomic::AtomicUsize, sink: &EventSink) {
    use std::sync::atomic::Ordering;
    if pending.fetch_sub(1, Ordering::AcqRel) != 1 {
        return;
    }
    // Reached only when at least one resume was dispatched, so the line always
    // names work that came back and is worth putting on screen.
    let line = opencrabs::brain::agent::service::boot_report::summary_line();
    crate::logging::info(&line);
    let _ = sink.send(TurnEvent::CommandReply {
        session: None,
        text: line,
    });
}

/// The eight characters of a session id a person could match against a search,
/// taken by iterator rather than by slicing a `str` at a byte offset: the site
/// is new code, and the byte-slice ratchet counts any new slice, so taking the
/// characters says out loud what a slice only assumes about the formatter.
fn short_session(id: Uuid) -> String {
    id.simple().to_string().chars().take(8).collect()
}

/// Resume the turns the last process died in the middle of (#2627).
///
/// The window has written `pending_requests` on every turn since it gained
/// tools: `spawn_turn` goes through the same tracked path the terminal uses.
/// Nothing ever read the table back, so closing or killing the window mid-tool
/// lost the work while the TUI picked the same rows up on its next launch.
///
/// This is that read: the ledger is scanned once at boot, cleared before any
/// resume starts (a crash mid-resume must not replay the same session on the
/// next launch), and each distinct session is re-run from the message the row
/// carries. A `system` row is a push turn killed mid-tool: replaying the LLM
/// turn could double-execute the side effect, so it is re-delivered through the
/// wake path instead, exactly as the terminal does (#12).
pub fn spawn_boot_resume(service: Arc<AgentService>, pool: opencrabs::db::Pool, sink: EventSink) {
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-boot-resume".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::logging::error(&format!("boot resume runtime: {e}"));
                    return;
                }
            };
            runtime.block_on(boot_resume(service, pool, sink));
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("boot resume thread spawn: {e}"));
    }
}

/// What a boot scan found, already split into the work it implies.
#[derive(Debug, Default, PartialEq, Eq)]
struct BootPlan {
    /// Every session a surviving row names, duplicates included: the summary
    /// counts rows, because that is what the previous process lost track of.
    interrupted: Vec<Uuid>,
    /// Sessions to re-run from the message their row carries.
    resumes: Vec<(Uuid, String)>,
    /// Sessions whose push has to be re-delivered instead of replayed (#12).
    pushes: Vec<(Uuid, String)>,
}

/// Split the surviving rows into the work boot will do, one entry per session.
///
/// Pure and separate on purpose: this is the whole decision, and it can be
/// checked without a database, a provider or a window. The ledger may hold
/// several rows for one session (a retried turn, a push that landed inside a
/// turn), and replaying one session per row would run the same turn two or
/// three times. Rows arrive oldest first, so the FIRST one for a session wins:
/// it is the turn that was actually running when the process died.
fn boot_plan(requests: &[opencrabs::db::PendingRequest]) -> BootPlan {
    let mut plan = BootPlan::default();
    let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for req in requests {
        let Ok(session_id) = Uuid::parse_str(&req.session_id) else {
            // An unusable id names no session, so it is skipped rather than
            // guessed at.
            continue;
        };
        plan.interrupted.push(session_id);
        if !seen.insert(session_id) {
            continue;
        }
        if req.origin == "system" {
            plan.pushes.push((session_id, req.user_message.clone()));
        } else {
            plan.resumes.push((session_id, req.user_message.clone()));
        }
    }
    plan
}

/// The scan half of [`spawn_boot_resume`], on the caller's runtime.
async fn boot_resume(service: Arc<AgentService>, pool: opencrabs::db::Pool, sink: EventSink) {
    use opencrabs::brain::agent::service::boot_report;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pending_repo = opencrabs::db::PendingRequestRepository::new(pool.clone());
    // A table that cannot take a row means nothing in this run survives the
    // next restart, and an empty scan is then blindness, not health (#1401).
    if let Err(e) = pending_repo.probe().await {
        crate::logging::warn(&format!(
            "restart recovery DISABLED for this run: {e:#}. Interrupted turns will not be resumed after the next restart."
        ));
        boot_report::record_tracking_disabled(format!("{e:#}"));
        return;
    }
    let requests = match pending_repo.get_interrupted().await {
        Ok(requests) => requests,
        Err(e) => {
            crate::logging::error(&format!("boot resume: pending scan failed: {e:#}"));
            return;
        }
    };
    if requests.is_empty() {
        crate::logging::info(&boot_report::summary_line());
        return;
    }
    // Cleared before the first resume runs: these rows are about to be worked
    // off, and a crash in the middle must not queue them again forever (#729).
    if let Err(e) = pending_repo.clear_all().await {
        crate::logging::warn(&format!("boot resume: clearing the ledger failed: {e:#}"));
    }

    let sessions = opencrabs::db::SessionRepository::new(pool);
    // What the scan decided, decided once and testable on its own: the async
    // half below only carries it out.
    let plan = boot_plan(&requests);
    for session_id in &plan.interrupted {
        boot_report::record_interrupted(*session_id);
    }
    let pending = Arc::new(AtomicUsize::new(0));
    for (session_id, message) in plan.pushes {
        // A push turn killed mid-tool: re-deliver the push, never replay the
        // tool call it was inside (#12). The window's local route claims it,
        // so it lands as a completion rather than waiting for a channel.
        opencrabs::brain::agent::service::restart_recovery::deliver_or_park(
            session_id,
            QueuedUserMessage {
                context_text: message,
                display_text: format!(
                    "⚠️ A push-initiated turn was interrupted by a restart, re-delivering it (session {})",
                    short_session(session_id)
                ),
                origin: opencrabs::brain::agent::PushOrigin::Recovery,
                bg_meta: None,
            },
        );
    }
    for (session_id, user_message) in plan.resumes {
        boot_report::record_resumed(session_id);
        // The session's own working directory, restored per session and never
        // through the global, so two resumes in different repos cannot clobber
        // each other (#703, #2007).
        if let Ok(Some(session)) = sessions.find_by_id(session_id).await
            && let Some(dir) = session.working_directory.as_deref()
        {
            let path = PathBuf::from(dir);
            if path.is_dir() {
                service.set_session_only_working_directory(session_id, path);
            }
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        // The card first: the window has to know this conversation is busy
        // before the first chunk arrives, or the turn paints into a window
        // that believes it is idle.
        let _ = sink.send(TurnEvent::TurnResumed {
            session: session_id,
            user_message: user_message.clone(),
            cancel: cancel.clone(),
        });
        pending.fetch_add(1, Ordering::AcqRel);
        // The continuation prompt, not the row's own text: the turn picks up
        // from the context already in the session, and replaying the request
        // would run it twice (#2627, same rule as the terminal at
        // `resume_delivery.rs`).
        spawn_resume(
            service.clone(),
            session_id,
            opencrabs::brain::agent::service::restart_recovery::CONTINUATION_PROMPT.to_string(),
            sink.clone(),
            cancel,
            pending.clone(),
        );
    }
    if pending.load(Ordering::Acquire) == 0 {
        // Only push rows were found (or the ids were unusable): nothing was
        // dispatched, so nothing will ever call the completion.
        let line = boot_report::summary_line();
        crate::logging::info(&line);
        let _ = sink.send(TurnEvent::CommandReply {
            session: None,
            text: line,
        });
    }
}

/// The service-global progress forwarder: every `(session, event)` pair
/// lands in `sink` for the UI to drain. Installed on the builder inside
/// `build` (the builder consumes self, so there is no post-hoc setter).
fn install_progress_forwarder(sink: &EventSink) -> ProgressCallback {
    let sink = sink.clone();
    Arc::new(move |session, event| {
        let _ = sink.send(TurnEvent::ProgressFor(
            session,
            event,
            std::time::Instant::now(),
        ));
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
    let waiting = {
        let mut map = queue();
        let entry = map.entry(session_id).or_default();
        entry.push(msg);
        entry.len()
    };
    // A live repro of a stuck queue reads from the log: every push names its
    // session and the count waiting after it (#2695).
    crate::logging::info(&format!(
        "queue push: session {session_id}, {waiting} waiting"
    ));
}

/// What is waiting for `session_id`, oldest first. The strip shows these.
pub fn queue_snapshot(session_id: Uuid) -> Vec<QueuedUserMessage> {
    queue().get(&session_id).cloned().unwrap_or_default()
}

/// Take everything waiting for `session_id`, so it is sent once. A message a
/// composer has pulled out to edit is no longer in the queue (#2637), so the
/// drain can never send a recall behind the user's back.
pub fn queue_take(session_id: Uuid) -> Vec<QueuedUserMessage> {
    let taken = queue().remove(&session_id).unwrap_or_default();
    // The other half of the flush receipt: what left the queue, and how much
    // of it, so a stuck-queue repro can pair every push with its take (#2695).
    // Empty takes stay silent: the tool-boundary drain asks on every round.
    if !taken.is_empty() {
        crate::logging::info(&format!(
            "queue take: session {session_id}, {} taken",
            taken.len()
        ));
    }
    taken
}

/// The whole queue waiting for the composer's session, taken out in one call,
/// oldest first.
///
/// Up used to walk the queue one message per press, newest first (#2629), and
/// each step took its message so the drain could not send the original while
/// the user edited a copy (#2637). Three queued messages were three separate
/// edits; the owner asked for all of them at once (#2696). One take now yields
/// the entire queue in the order it was written, and what the composer holds
/// stays out of the queue until it is sent.
pub fn queue_recall_take() -> Option<(Uuid, Vec<QueuedUserMessage>)> {
    let session = (*COMPOSER_SESSION.lock().ok()?)?;
    // `queue_take` is the same take-all the Esc flush uses, so a recall and a
    // flush can never disagree about what is still waiting, and the take
    // leaves one receipt in the GUI log (#2695).
    let taken = queue_take(session);
    if taken.is_empty() {
        return None;
    }
    Some((session, taken))
}

/// Put the messages a recall took for editing back where they waited, oldest
/// first. Abandoning a recall returns every original untouched, so nothing that
/// was queued is lost by looking at it (#2637).
pub fn queue_recall_return(session_id: Uuid, msg: QueuedUserMessage) {
    queue_push(session_id, msg);
}

/// Point the composer at `session_id`, so recall and the strip agree with it.
pub fn set_composer_session(session_id: Option<Uuid>) {
    if let Ok(mut s) = COMPOSER_SESSION.lock() {
        *s = session_id;
    }
}

/// The follow-up options `suggest_options` offered, keyed by session (#2708).
///
/// In memory only, and that is correct rather than a gap: the tool's labels
/// never reach the ledger (`suggest_options` answers with "Surfaced N
/// follow-up suggestion(s)." and nothing else), so there is no stored copy to
/// restore. The map lives as long as the process, which is the same lifetime
/// the terminal gives its own follow-up list. Do not add a "restore on reload"
/// path: there is nothing to restore from.
static SUGGESTIONS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<Uuid, Vec<SuggestionItem>>>,
> = std::sync::OnceLock::new();

fn suggestions()
-> std::sync::MutexGuard<'static, std::collections::HashMap<Uuid, Vec<SuggestionItem>>> {
    SUGGESTIONS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Replace what `session_id` is offering. The newest offer wins: the tool runs
/// at most once per turn, so an older strip is stale by definition. An empty
/// list is a clear, which keeps one call able to do both.
pub fn set_options(session_id: Uuid, items: Vec<SuggestionItem>) {
    let count = items.len();
    {
        let mut map = suggestions();
        if items.is_empty() {
            map.remove(&session_id);
        } else {
            map.insert(session_id, items);
        }
    }
    // The GUI log is the only place a dropped offer can be seen, since the
    // buttons are never persisted (#2708).
    crate::logging::info(&format!(
        "suggest options: session {session_id}, {count} offered"
    ));
}

/// What `session_id` is offering, so its strip can draw it.
pub fn options_snapshot(session_id: Uuid) -> Vec<SuggestionItem> {
    suggestions().get(&session_id).cloned().unwrap_or_default()
}

/// Take the offer off `session_id`. A pick, a send, a stop and a session
/// switch all end it: the buttons belonged to the turn that made them.
pub fn options_take(session_id: Uuid) -> Vec<SuggestionItem> {
    suggestions().remove(&session_id).unwrap_or_default()
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
                    let _ = done_sink.send(TurnEvent::TurnReverted {
                        session: session_id,
                    });
                    return;
                }
            };
            runtime.block_on(async move {
                let messages = opencrabs::services::MessageService::new(service.context().clone());
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
                    let _ = done_sink.send(TurnEvent::TurnReverted {
                        session: session_id,
                    });
                    return;
                };
                if question.role == "user"
                    && let Err(e) = messages.delete_message(question.id).await
                {
                    crate::logging::error(&format!("turn undo user row: {e}"));
                }
                let _ = done_sink.send(TurnEvent::TurnReverted {
                    session: session_id,
                });
            });
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("turn undo thread: {e}"));
        let _ = sink.send(TurnEvent::TurnReverted {
            session: session_id,
        });
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
                    let _ = sink.send(TurnEvent::CommandReply {
                        session: Some(session_id),
                        text: format!("command: {e}"),
                    });
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
        return vec![TurnEvent::CommandReply {
            session: Some(session_id),
            text: format!("{name} is not a window command."),
        }];
    };

    match command {
        // The window handles `/cd` itself (#2527): the picker screen for a
        // bare `/cd`, the local applier for a path. It never reaches this
        // runner, so this arm is an unreachable guard, not a reply.
        tc::TextCommand::Cd => vec![],
        tc::TextCommand::Plan => {
            let line = opencrabs::utils::plan_mode::enter_plan_mode(session_id).await;
            let mut out = vec![TurnEvent::CommandReply {
                session: Some(session_id),
                text: line,
            }];
            // `/plan <query>` enters plan mode and uses the trailing text as
            // the planning intent, exactly as the channels do (#579).
            if !arg.is_empty() {
                out.push(TurnEvent::CommandTurn {
                    session: session_id,
                    text: arg.to_string(),
                    display: None,
                });
            }
            out
        }
        tc::TextCommand::ShowPlan => vec![TurnEvent::CommandReply {
            session: Some(session_id),
            text: opencrabs::utils::plan_mode::show_plan(session_id).await,
        }],
        tc::TextCommand::Execute => match opencrabs::utils::plan_mode::try_approve(
            session_id,
            opencrabs::tui::plan::ApprovalSource::User,
        )
        .await
        {
            opencrabs::utils::plan_mode::ApproveOutcome::Refused(text) => {
                vec![TurnEvent::CommandReply {
                    session: Some(session_id),
                    text,
                }]
            }
            opencrabs::utils::plan_mode::ApproveOutcome::SeedTurn { prompt, display } => {
                vec![TurnEvent::CommandTurn {
                    session: session_id,
                    text: prompt,
                    display: Some(display),
                }]
            }
        },
        tc::TextCommand::Discard => {
            let pool = match Database::connect(db_path).await {
                Ok(db) => db.pool().clone(),
                Err(e) => {
                    return vec![TurnEvent::CommandReply {
                        session: Some(session_id),
                        text: format!("/discard could not open the database: {e}"),
                    }];
                }
            };
            let svc = ServiceContext::new(pool);
            vec![TurnEvent::CommandReply {
                session: Some(session_id),
                text: opencrabs::utils::plan_mode::discard(session_id, &svc).await,
            }]
        }
        tc::TextCommand::Clear => match bridge() {
            Some(bridge) => match bridge.service.clear_context(session_id).await {
                Ok(receipt) => vec![TurnEvent::CommandReply {
                    session: Some(session_id),
                    text: opencrabs::tui::clear_notice::cleared(&receipt),
                }],
                Err(e) => vec![TurnEvent::CommandReply {
                    session: Some(session_id),
                    text: opencrabs::tui::clear_notice::failed(&e),
                }],
            },
            None => vec![TurnEvent::CommandReply {
                session: Some(session_id),
                text: "/clear needs the agent running; it is offline right now.".to_string(),
            }],
        },
        tc::TextCommand::Rtk => {
            #[cfg(feature = "rtk")]
            {
                if !opencrabs::rtk::is_rtk_available().await {
                    return vec![TurnEvent::CommandReply {
                        session: Some(session_id),
                        text: opencrabs::rtk::RTK_NOT_INSTALLED_HELP.to_string(),
                    }];
                }
                match tokio::process::Command::new("rtk")
                    .arg("gain")
                    .output()
                    .await
                {
                    Ok(output) => vec![TurnEvent::CommandReply {
                        session: Some(session_id),
                        text: tc::rtk_reply(
                            output.status.success(),
                            &String::from_utf8_lossy(&output.stdout),
                            &String::from_utf8_lossy(&output.stderr),
                        ),
                    }],
                    Err(e) => vec![TurnEvent::CommandReply {
                        session: Some(session_id),
                        text: format!("rtk gain: {e}"),
                    }],
                }
            }
            #[cfg(not(feature = "rtk"))]
            {
                vec![TurnEvent::CommandReply {
                    session: Some(session_id),
                    text: "RTK is not compiled into this build.".to_string(),
                }]
            }
        }
        tc::TextCommand::Approve => {
            if arg.is_empty() {
                let policy = opencrabs::config::Config::load()
                    .map(|cfg| cfg.agent.approval_policy)
                    .unwrap_or_else(|_| "ask".to_string());
                vec![TurnEvent::CommandReply {
                    session: Some(session_id),
                    text: tc::approval_reply(&policy),
                }]
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
                            vec![TurnEvent::CommandReply {
                                session: Some(session_id),
                                text: tc::approval_set(policy),
                            }]
                        }
                        Err(e) => vec![TurnEvent::CommandReply {
                            session: Some(session_id),
                            text: format!("/approve could not write the policy: {e}"),
                        }],
                    },
                    None => vec![TurnEvent::CommandReply {
                        session: Some(session_id),
                        text: tc::approval_unknown(arg),
                    }],
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
            vec![TurnEvent::CommandReply {
                session: Some(session_id),
                text: tc::profiles_reply(&entries, &current),
            }]
        }
        tc::TextCommand::Restart => {
            // Shares the terminal's restart so the session is resumed on the
            // way back up instead of coming back to an empty chat. Only
            // returns when the restart could not be started.
            match opencrabs::brain::SelfUpdater::auto_detect().and_then(|u| u.restart(session_id)) {
                Ok(()) => Vec::new(),
                Err(e) => vec![TurnEvent::CommandReply {
                    session: Some(session_id),
                    text: format!("/restart could not cycle the process: {e}"),
                }],
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
                        Ok(_) => vec![TurnEvent::CommandReply {
                            session: Some(session_id),
                            text: tc::whisper_reply(),
                        }],
                        Err(e) => vec![TurnEvent::CommandReply {
                            session: Some(session_id),
                            text: tc::whisper_failed(&format!(
                                "Failed to launch WhisperCrabs: {e}"
                            )),
                        }],
                    }
                }
                Err(e) => vec![TurnEvent::CommandReply {
                    session: Some(session_id),
                    text: tc::whisper_failed(&e.to_string()),
                }],
            }
        }
        // Handled on the UI thread: both compose a turn rather than reading
        // the engine, so the window dispatches them where `send_message` lives.
        tc::TextCommand::Compact | tc::TextCommand::Goal | tc::TextCommand::Cowork => {
            vec![TurnEvent::CommandReply {
                session: Some(session_id),
                text: format!("{name} is dispatched by the window, not the bridge."),
            }]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::brain::tools::suggest_options::SuggestionStyle;

    /// The surface claim must wait for the window and fire exactly once,
    /// whichever side of the race finishes first (#2784). A claim before the
    /// window is up would preempt the running TUI/daemon, and a failed window
    /// would then leave the user with no surface at all.
    #[test]
    fn surface_claim_waits_for_the_window_and_fires_once() {
        // Bootstrap first: deps ready, window still down -> no claim.
        let gate: SurfaceGate<u32> = SurfaceGate::new();
        gate.set_pending(1);
        assert!(
            gate.take_if_ready().is_none(),
            "must not claim the profile before the window is up"
        );
        // Window opens -> the claim fires exactly once.
        gate.mark_window_up();
        assert_eq!(
            gate.take_if_ready(),
            Some(1),
            "the window releases the claim"
        );
        assert!(gate.take_if_ready().is_none(), "must not claim twice");

        // Window first: window up, deps not built yet -> no claim.
        let gate2: SurfaceGate<u32> = SurfaceGate::new();
        gate2.mark_window_up();
        assert!(
            gate2.take_if_ready().is_none(),
            "no deps yet, nothing to claim"
        );
        gate2.set_pending(2);
        assert_eq!(
            gate2.take_if_ready(),
            Some(2),
            "bootstrap releases the claim"
        );
        assert!(gate2.take_if_ready().is_none(), "must not claim twice");
    }

    /// Up brings the WHOLE queue into the composer in one take, oldest first
    /// (#2696), and the take empties the queue: a recall the user abandons must
    /// not be sent when the turn ends (#2637).
    #[test]
    fn up_recalls_the_whole_queue_at_once() {
        // One test for the queue's recall path: `COMPOSER_SESSION` and `QUEUE`
        // are process-wide, so two tests here would race on them.
        let session = Uuid::from_u128(0x2696_0001);
        let other = Uuid::from_u128(0x2696_0002);

        for text in ["one", "two", "three"] {
            queue_push(session, QueuedUserMessage::plain(text.to_string()));
        }
        // A completion for another session must not enter this queue.
        queue_push(other, QueuedUserMessage::plain("elsewhere".to_string()));

        set_composer_session(Some(session));
        let (from, taken) = queue_recall_take().expect("the queue has messages");
        assert_eq!(from, session, "a recall never reaches another session");
        let recalled: Vec<String> = taken.iter().map(|msg| msg.display_text.clone()).collect();
        assert_eq!(
            recalled,
            vec!["one".to_string(), "two".to_string(), "three".to_string()],
            "one Up yields the whole queue, oldest first"
        );
        assert!(
            queue_recall_take().is_none(),
            "the take emptied the queue, so a second Up has nothing to recall"
        );
        assert!(
            queue_snapshot(session).is_empty(),
            "a message the composer holds is no longer queued, so nothing can send it behind the user"
        );
        assert_eq!(
            queue_snapshot(other).len(),
            1,
            "taking this session's queue leaves another session's alone"
        );

        // Abandoning the recall puts every original back, in the order it was
        // written.
        for msg in taken {
            queue_recall_return(session, msg);
        }
        let left: Vec<String> = queue_snapshot(session)
            .iter()
            .map(|msg| msg.display_text.clone())
            .collect();
        assert_eq!(
            left,
            vec!["one".to_string(), "two".to_string(), "three".to_string()],
            "returning a recall preserves the original order"
        );

        let _ = queue_take(session);
        let _ = queue_take(other);
        set_composer_session(None);
    }

    /// #2708: the follow-up offer is per session, so a pane or a session
    /// switch never shows another conversation's buttons, and a take clears
    /// only the session it names.
    #[test]
    fn suggested_options_stay_with_their_session() {
        let session = Uuid::from_u128(0x2708_0001);
        let other = Uuid::from_u128(0x2708_0002);

        set_options(
            session,
            vec![
                SuggestionItem::new("run the tests", SuggestionStyle::Primary),
                SuggestionItem::new("delete the branch", SuggestionStyle::Danger),
            ],
        );
        set_options(other, vec![SuggestionItem::default_styled("elsewhere")]);

        let offers = options_snapshot(session);
        assert_eq!(offers.len(), 2, "both labels reach their own session");
        assert_eq!(offers[0].label, "run the tests");
        assert_eq!(
            offers[1].style,
            SuggestionStyle::Danger,
            "the style travels with the label, so the row keeps its colour"
        );
        assert_eq!(
            options_snapshot(other).len(),
            1,
            "one session's offer never lands in another's strip"
        );

        let taken = options_take(session);
        assert_eq!(taken.len(), 2, "a pick takes the offer it came from");
        assert!(
            options_snapshot(session).is_empty(),
            "a taken offer is gone, so a second press cannot send a stale label"
        );
        assert_eq!(
            options_snapshot(other).len(),
            1,
            "taking this session's offer leaves another session's alone"
        );

        // An empty set is a clear: that is the shape the stop and send paths
        // use, so one call covers both.
        set_options(other, Vec::new());
        assert!(
            options_snapshot(other).is_empty(),
            "an empty set clears the slot"
        );

        let _ = options_take(session);
        let _ = options_take(other);
    }

    /// Esc flushes the WHOLE queue into one turn (#2695): the drain takes
    /// every waiting message, oldest first, and `join` folds them into a
    /// single message, so nothing stays stuck behind the stop. The window's
    /// `drain_pending` and the pane's `drain_pane_pending` both run this exact
    /// pair, and the tool-boundary callback joins the same way.
    #[test]
    fn esc_flush_takes_all_queued_into_one_joined_turn() {
        // QUEUE is process-wide, but this test owns its session key and never
        // touches COMPOSER_SESSION, so it cannot race the recall walk above.
        let session = Uuid::from_u128(0x2695_0001);
        for text in ["first", "second", "third"] {
            queue_push(session, QueuedUserMessage::plain(text.to_string()));
        }
        let msgs = queue_take(session);
        assert_eq!(msgs.len(), 3, "the flush takes every waiting message");
        let joined = QueuedUserMessage::join(&msgs).expect("a non-empty queue joins");
        assert_eq!(
            joined.display_text, "first\nsecond\nthird",
            "all three ride out in one turn, oldest first"
        );
        assert_eq!(
            joined.context_text, "first\nsecond\nthird",
            "the context half joins the same way for typed messages"
        );
        assert!(
            queue_snapshot(session).is_empty(),
            "nothing stays stuck in the queue after the flush"
        );
        // A second drain finds nothing: the flush never sends a message twice.
        let again = queue_take(session);
        assert!(again.is_empty(), "the queue is empty once flushed");
        assert!(
            QueuedUserMessage::join(&again).is_none(),
            "an empty queue spawns no turn"
        );
    }

    /// Boot picks up exactly the work the killed process was in the middle of,
    /// once per session, and never replays a push as a tool turn (#2627).
    #[test]
    fn the_window_boot_resumes_an_interrupted_turn() {
        fn row(session: Uuid, message: &str, origin: &str) -> opencrabs::db::PendingRequest {
            opencrabs::db::PendingRequest {
                id: Uuid::new_v4().to_string(),
                session_id: session.to_string(),
                user_message: message.to_string(),
                channel: "tui".to_string(),
                channel_chat_id: None,
                channel_thread_id: None,
                channel_message_id: None,
                origin: origin.to_string(),
            }
        }
        let a = Uuid::from_u128(0x2627_0001);
        let b = Uuid::from_u128(0x2627_0002);
        let c = Uuid::from_u128(0x2627_0003);

        let plan = boot_plan(&[
            row(a, "build it", "user"),
            row(a, "build it again", "user"),
            row(b, "why is CI red", "user"),
            row(c, "background job finished", "system"),
        ]);

        assert_eq!(
            plan.interrupted.len(),
            4,
            "every surviving row is interrupted work, duplicates included"
        );
        assert_eq!(
            plan.resumes,
            vec![
                (a, "build it".to_string()),
                (b, "why is CI red".to_string()),
            ],
            "one resume per session, from the row that was running first"
        );
        assert_eq!(
            plan.pushes,
            vec![(c, "background job finished".to_string())],
            "a push row is re-delivered, never replayed as a tool turn (#12)"
        );

        // An unusable id names no session and is skipped, not guessed at.
        let mut broken = row(a, "orphan", "user");
        broken.session_id = "not-a-uuid".to_string();
        assert!(
            boot_plan(&[broken]).resumes.is_empty(),
            "a row whose id is not a UUID names no session"
        );

        let src = include_str!("agent_bridge.rs");
        let scan = src
            .split("async fn boot_resume(")
            .nth(1)
            .expect("the boot scan");
        let scan = scan
            .split("/// The service-global progress forwarder")
            .next()
            .expect("the end of the scan");
        assert!(scan.contains("get_interrupted()"), "boot reads the ledger");
        let clear = scan.find("clear_all()").expect("the ledger is cleared");
        let dispatch = scan.find("spawn_resume(").expect("the resume is spawned");
        assert!(
            clear < dispatch,
            "rows are cleared before any resume runs, or a crash mid-resume replays them (#729)"
        );

        let resume = src
            .split("fn spawn_resume(")
            .nth(1)
            .expect("the resume runner");
        assert!(
            resume.contains("resume_interrupted_turn("),
            "a resume goes through the untracked path"
        );
        assert!(
            !resume.contains("send_message_with_tools_and_mode("),
            "a resume that re-tracked itself would resume forever (#729)"
        );
        assert!(
            src.contains("spawn_boot_resume(bridge.service.clone()"),
            "the window's boot actually runs the scan"
        );
    }

    /// The window runs the recovery pass the terminal runs, or every detached
    /// command from the previous process is left unaccounted for and reports
    /// again on the next boot (#2626).
    #[test]
    fn the_window_boot_runs_the_restart_recovery_pass() {
        // Read the shipped source only. This test spells the calls it looks
        // for, and on the whole file the first match for the bootstrap was
        // this test's own needle, so the slice started at the bottom of the
        // file and every assertion below it read the wrong text (#2673).
        let src = include_str!("agent_bridge.rs");
        let shipped = src
            .split("#[cfg(test)]")
            .next()
            .expect("the shipped source before the tests");
        let boot = shipped
            .split("async fn build(")
            .nth(1)
            .expect("the window's bootstrap");
        let boot = boot
            .split("/// Create one conversation in the shared ledger")
            .next()
            .expect("the end of the bootstrap");

        let route_needle = ["register_local", "_route(enqueue)"].concat();
        let route = boot
            .find(route_needle.as_str())
            .expect("the window registers the destination a parked report flushes to");
        let pass = boot
            .find("restart_recovery::recover(Some(")
            .expect("the boot runs the restart recovery pass");
        assert!(
            route < pass,
            "the local destination exists before the pass can produce a report"
        );
        assert!(
            boot.contains("tokio::spawn("),
            "the pass rides the surface runtime: recover's 30s flush dies with any \
             runtime a boot thread drops when its block_on returns"
        );
        assert!(
            boot.contains("interrupted by a previous run"),
            "the count is logged on every boot, zero included"
        );
    }

    /// Both turn runners race their stop token (#2657).
    ///
    /// The service reads the token where it looks for it, which inside one
    /// provider stream is never, so a stop that is only handed to the service
    /// is invisible until the answer finishes. Both runners have to race it:
    /// a resumed turn is a turn and its stop button has to end it too.
    #[test]
    fn gui_cancel_both_runners_race_the_stop() {
        let src = include_str!("agent_bridge.rs");
        let shipped = src
            .split("#[cfg(test)]")
            .next()
            .expect("the shipped source before the tests");
        let mut raced = 0usize;
        for (runner, end) in [
            ("fn spawn_turn(", "/// Re-run one interrupted turn"),
            ("fn spawn_resume(", "/// Resolve the boot accounting"),
        ] {
            let body = shipped
                .split(runner)
                .nth(1)
                .expect("the runner is in the shipped source");
            let body = body.split(end).next().expect("the runner has an end");
            assert!(
                body.contains("race_turn(turn, &cancel)"),
                "{runner} must race the turn against its stop token, or the stop \
                 only lands when the provider finishes (#2657)"
            );
            assert!(
                body.contains("TurnEvent::Cancelled"),
                "{runner} must settle the window when the stop drops the turn: a \
                 dropped future sends no Done and no Failed"
            );
            raced += 1;
        }
        assert_eq!(raced, 2, "both turn runners were scanned");
    }

    /// A turn nobody stopped comes back exactly as it was.
    #[tokio::test]
    async fn gui_cancel_race_hands_an_untouched_turn_back() {
        let cancel = tokio_util::sync::CancellationToken::new();
        assert_eq!(
            race_turn(std::future::ready(7u8), &cancel).await,
            Some(7),
            "with no stop the race is the turn's own result"
        );
    }

    /// A stop that lands in the same poll as the answer wins (#2657).
    ///
    /// The user asked for the turn to be gone. Painting an answer over their
    /// cancellation is exactly the behaviour that made the stop look dead.
    #[tokio::test]
    async fn gui_cancel_race_prefers_the_stop_when_both_are_ready() {
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            race_turn(std::future::ready(7u8), &cancel).await,
            None,
            "a fired token beats a turn that is already finished"
        );
    }

    /// The dropped future is the whole fix: it must be dropped where it stands.
    #[tokio::test]
    async fn gui_cancel_race_drops_the_turn_where_it_stands() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Dropped(std::sync::Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = std::sync::Arc::new(AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = cancel.clone();
        // The stop lands while the turn sits in a stream that never returns,
        // which is the shape of the bug: one long provider call.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            token.cancel();
        });
        let turn = async move {
            let _guard = guard;
            std::future::pending::<()>().await
        };
        assert!(
            race_turn(turn, &cancel).await.is_none(),
            "the stop settles the race"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "the turn future is dropped at its current await, the window's \
             handle.abort(): without this the provider keeps streaming and the \
             turn reports done minutes after the stop (#2657)"
        );
    }

    // ── #2697: a resend must not duplicate the unanswered pair ──────────────

    /// A ledger with one session, for the resend-dedup tests.
    async fn temp_ledger() -> (opencrabs::services::MessageService, Uuid) {
        let db = Database::connect_in_memory().await.expect("in-memory db");
        db.run_migrations().await.expect("migrations");
        let context = ServiceContext::new(db.pool().clone());
        let sessions = opencrabs::services::SessionService::new(context.clone());
        let session = sessions
            .create_session(Some("dedup".to_string()))
            .await
            .expect("session");
        (
            opencrabs::services::MessageService::new(context),
            session.id,
        )
    }

    /// What a turn does on start: the question row, then an empty placeholder.
    async fn open_turn(messages: &opencrabs::services::MessageService, session: Uuid, text: &str) {
        messages
            .create_message(session, "user".to_string(), text.to_string())
            .await
            .expect("user row");
        messages
            .create_message(session, "assistant".to_string(), String::new())
            .await
            .expect("placeholder");
    }

    /// The resend keeps one copy of the question and gets a fresh placeholder.
    #[tokio::test]
    async fn dedup_leaves_one_copy_of_a_resent_message() {
        let (messages, session) = temp_ledger().await;
        open_turn(&messages, session, "ship it").await;

        drop_unanswered_duplicate(&messages, session, "ship it").await;
        // The turn the resend starts persists exactly what any turn persists.
        open_turn(&messages, session, "ship it").await;

        let rows = messages
            .list_messages_for_session(session)
            .await
            .expect("rows");
        let asked: Vec<_> = rows.iter().filter(|r| r.role == "user").collect();
        assert_eq!(
            asked.len(),
            1,
            "the resend must not duplicate the question: {rows:?}"
        );
        assert_eq!(asked[0].content, "ship it");
        assert_eq!(rows.len(), 2, "one question and one fresh placeholder");
        assert_eq!(rows[1].role, "assistant");
        assert!(rows[1].content.is_empty(), "the placeholder is unanswered");
    }

    /// An answered pair is history: a later identical question leaves it alone.
    #[tokio::test]
    async fn dedup_keeps_an_answered_pair() {
        let (messages, session) = temp_ledger().await;
        messages
            .create_message(session, "user".to_string(), "ship it".to_string())
            .await
            .expect("user row");
        messages
            .create_message(session, "assistant".to_string(), "done".to_string())
            .await
            .expect("answer");

        drop_unanswered_duplicate(&messages, session, "ship it").await;

        let rows = messages
            .list_messages_for_session(session)
            .await
            .expect("rows");
        assert_eq!(rows.len(), 2, "an answered pair is never cut: {rows:?}");
        assert_eq!(rows[1].content, "done");
    }

    /// A different question behind an empty placeholder is not a duplicate.
    #[tokio::test]
    async fn dedup_keeps_a_different_question() {
        let (messages, session) = temp_ledger().await;
        open_turn(&messages, session, "other question").await;

        drop_unanswered_duplicate(&messages, session, "ship it").await;

        let rows = messages
            .list_messages_for_session(session)
            .await
            .expect("rows");
        assert_eq!(rows.len(), 2, "only the exact text is a resend: {rows:?}");
        assert_eq!(rows[0].content, "other question");
    }

    /// Anything after the placeholder ends the walk: only the trailing pair is
    /// ever a candidate, never rows a turn already wrote behind it.
    #[tokio::test]
    async fn dedup_stops_at_anything_after_the_placeholder() {
        let (messages, session) = temp_ledger().await;
        open_turn(&messages, session, "ship it").await;
        messages
            .create_message(session, "system".to_string(), "system note".to_string())
            .await
            .expect("system row");

        drop_unanswered_duplicate(&messages, session, "ship it").await;

        let rows = messages
            .list_messages_for_session(session)
            .await
            .expect("rows");
        assert_eq!(
            rows.len(),
            3,
            "the last row is not the empty placeholder, so nothing is cut: {rows:?}"
        );
    }

    /// The guard is wired into the send path, ahead of the turn's first write.
    #[test]
    fn dedup_runs_before_the_turn_persists() {
        let src = include_str!("agent_bridge.rs");
        let shipped = src
            .split("#[cfg(test)]")
            .next()
            .expect("the shipped source before the tests");
        let body = shipped
            .split("pub fn spawn_turn(")
            .nth(1)
            .expect("spawn_turn is in the shipped source");
        let body = body
            .split("/// Re-run one interrupted turn")
            .next()
            .expect("spawn_turn has an end");
        let guard = body
            .find("drop_unanswered_duplicate(")
            .expect("spawn_turn runs the dedup guard");
        let send = body
            .find("send_message_with_tools_and_display(")
            .expect("spawn_turn sends the turn");
        assert!(
            guard < send,
            "the guard has to run before the turn persists its rows (#2697)"
        );
    }
}
