//! Channel Factory
//!
//! Shared factory for creating channel agent services at runtime.
//! Used by both static startup (ui.rs) and dynamic connection (whatsapp_connect tool).

use crate::brain::agent::AgentService;
use crate::brain::provider::Provider;
use crate::brain::tools::ToolRegistry;
use crate::config::{Config, VoiceConfig};
use crate::services::ServiceContext;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use uuid::Uuid;

/// Factory for creating channel-specific AgentService instances.
///
/// Holds all shared state needed to spin up channel agents (Telegram, WhatsApp, etc.)
/// both at startup and dynamically at runtime via tools.
///
/// The `tool_registry` is set lazily via [`set_tool_registry`] to break the circular
/// dependency between tool registration and factory creation.
pub struct ChannelFactory {
    /// The primary provider every agent this factory builds starts on.
    ///
    /// Behind a lock since #1700: it used to be a bare `Arc` captured at
    /// process start, so an agent built after a key rotation or a `[agent]
    /// provider` edit still got the pre-rotation instance, and there was no
    /// way to reach the agents already built. `reload_providers` is the writer.
    provider: std::sync::RwLock<Arc<dyn Provider>>,
    /// Every agent service this factory produced, as weak refs (#1700).
    ///
    /// The channel manager keeps `JoinHandle`s, not services, so without this
    /// registry nothing outside a channel task can reach a running channel
    /// agent. Weak on purpose: a stopped channel must not be kept alive by the
    /// registry, and a dead entry is pruned on the next reload.
    live_agents: std::sync::Mutex<Vec<std::sync::Weak<AgentService>>>,
    service_context: ServiceContext,
    shared_brain: String,
    tool_registry: OnceLock<Arc<ToolRegistry>>,
    working_directory: PathBuf,
    brain_path: PathBuf,
    shared_session_id: Arc<Mutex<Option<Uuid>>>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    session_updated_tx:
        OnceLock<tokio::sync::mpsc::UnboundedSender<crate::brain::agent::ChannelSessionEvent>>,
    /// Runtime info (model/provider/working dir) used to faithfully rebuild
    /// the channel system brain when a brain file changes (#213). Set once at
    /// startup via [`set_runtime_info`]; absent on the cron daemon path, where
    /// the brain is built with no runtime info anyway.
    runtime_info: OnceLock<crate::brain::prompt_builder::RuntimeInfo>,
    /// Sub-agent manager for compaction persistence (#936). Set once at startup
    /// via [`set_subagent_manager`]; wired into every agent service the factory
    /// creates so compaction can inject running sub-agent IDs into the summary.
    subagent_manager: OnceLock<Arc<crate::brain::tools::subagent::SubAgentManager>>,
    /// Headless factory (#129): agents built by the daemon's cron scheduler
    /// have no live user surface. Stamped into every agent service so
    /// interactive-only tools hard-error (belt-and-braces backstop behind the
    /// registry-level exclusion). Default `false` (interactive channel factory).
    headless: std::sync::atomic::AtomicBool,
    /// Plan-gate exemption (#2161): every agent this factory builds skips the
    /// mechanical plan requirement. Cron runs are the task already: there is no
    /// user in the loop to approve a plan, so a job that crosses the tool-call
    /// threshold mid-run would strand itself asking for one. Default `false`
    /// (interactive channel factory), where the gate applies.
    plan_gate_exempt: std::sync::atomic::AtomicBool,
    /// Channel-manager handle (#148): wired into every agent service so the
    /// tool loop can derive the ambient `origin_target` from the session
    /// ownership maps. Set once at startup via [`set_channel_manager`];
    /// absent on cron/daemon paths, where "here" resolution must refuse.
    channel_manager: OnceLock<Arc<crate::channels::ChannelManager>>,
}

impl ChannelFactory {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn Provider>,
        service_context: ServiceContext,
        shared_brain: String,
        working_directory: PathBuf,
        brain_path: PathBuf,
        shared_session_id: Arc<Mutex<Option<Uuid>>>,
        config_rx: tokio::sync::watch::Receiver<Config>,
    ) -> Self {
        Self {
            provider: std::sync::RwLock::new(provider),
            live_agents: std::sync::Mutex::new(Vec::new()),
            service_context,
            shared_brain,
            tool_registry: OnceLock::new(),
            working_directory,
            brain_path,
            shared_session_id,
            config_rx,
            session_updated_tx: OnceLock::new(),
            runtime_info: OnceLock::new(),
            subagent_manager: OnceLock::new(),
            headless: std::sync::atomic::AtomicBool::new(false),
            plan_gate_exempt: std::sync::atomic::AtomicBool::new(false),
            channel_manager: OnceLock::new(),
        }
    }

    /// Mark this factory headless (#129): every agent service it builds gets
    /// `with_headless(true)` so interactive-only tools hard-error. Used by the
    /// daemon's cron-scheduler factory; the interactive channel factory never
    /// calls this.
    pub fn set_headless(&self, headless: bool) {
        self.headless
            .store(headless, std::sync::atomic::Ordering::Relaxed);
    }

    /// Exempt every agent this factory builds from the mechanical plan gate
    /// (#2161). Set by [`configure_cron_factory`] on the daemon's cron factory.
    pub fn set_plan_gate_exempt(&self, exempt: bool) {
        self.plan_gate_exempt
            .store(exempt, std::sync::atomic::Ordering::Relaxed);
    }

    /// Wire in the runtime info so channel agents rebuild their system brain
    /// faithfully (model/provider/working-dir lines preserved) when a brain
    /// file changes at runtime (#213). Call once at startup.
    pub fn set_runtime_info(&self, info: crate::brain::prompt_builder::RuntimeInfo) {
        let _ = self.runtime_info.set(info);
    }

    /// Wire in the TUI session-updated sender so channel agents trigger live TUI refresh.
    pub fn set_session_updated_tx(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::brain::agent::ChannelSessionEvent>,
    ) {
        let _ = self.session_updated_tx.set(tx);
    }

    /// Set the tool registry (call once, after Arc<ToolRegistry> is created).
    pub fn set_tool_registry(&self, registry: Arc<ToolRegistry>) {
        let _ = self.tool_registry.set(registry);
    }

    /// Set the sub-agent manager for compaction persistence (#936).
    pub fn set_subagent_manager(
        &self,
        manager: Arc<crate::brain::tools::subagent::SubAgentManager>,
    ) {
        let _ = self.subagent_manager.set(manager);
    }

    /// Set the channel-manager handle for ambient origin derivation (#148).
    /// The manager already holds the channel-state Arcs, so this is the
    /// single wiring point; TUI boot sets it after constructing the manager.
    pub fn set_channel_manager(&self, manager: Arc<crate::channels::ChannelManager>) {
        let _ = self.channel_manager.set(manager);
    }

    /// The live channel-manager handle, when one was set at boot (#148).
    /// Cron/daemon paths construct factories without one — `None` there,
    /// and "here"/channel-URL resolution refuses rather than guesses.
    pub fn channel_manager(&self) -> Option<Arc<crate::channels::ChannelManager>> {
        self.channel_manager.get().cloned()
    }

    /// Create a new AgentService configured for channel use.
    ///
    /// Channels that implement their own approval flow (WhatsApp, Telegram, Discord, Slack)
    /// pass an `override_approval_callback` per call — they must NOT have `auto_approve_tools`
    /// set, otherwise the override is ignored. A2A and headless tools that have no interactive
    /// user can set their own auto-approval via session context.
    pub async fn create_agent_service(&self) -> Arc<AgentService> {
        self.create_agent_service_with_queue(None).await
    }

    /// Like [`create_agent_service`](Self::create_agent_service) but wires a
    /// per-session message-queue callback so the tool loop can inject a queued
    /// message between rounds. Telegram uses this to surface a mid-turn reaction
    /// into the running loop (#302 Stage 2); channels without one pass `None`.
    pub async fn create_agent_service_with_queue(
        &self,
        message_queue_callback: Option<crate::brain::agent::MessageQueueCallback>,
    ) -> Arc<AgentService> {
        self.create_agent_service_full(message_queue_callback, None)
            .await
    }

    /// Like [`create_agent_service_with_queue`](Self::create_agent_service_with_queue)
    /// but also wires the background-task enqueue producer (#722) so a channel can
    /// resume a session when a detached long command finishes.
    pub async fn create_agent_service_full(
        &self,
        message_queue_callback: Option<crate::brain::agent::MessageQueueCallback>,
        message_enqueue_callback: Option<crate::brain::agent::service::MessageEnqueueCallback>,
    ) -> Arc<AgentService> {
        let config = self.config_rx.borrow().clone();
        let mut builder = AgentService::new(
            self.current_provider(),
            self.service_context.clone(),
            &config,
        )
        .await
        .with_system_brain(self.shared_brain.clone())
        // Live brain rebuild (#213): channel agents pick up brain-file
        // edits on their next turn. Channels use the core brain; the
        // lazy-tools suffix mirrors the startup assembly. Seeded from
        // `shared_brain` so nothing is re-read until a file changes.
        .with_brain_rebuild(
            crate::brain::prompt_builder::BrainLoader::new(self.brain_path.clone()),
            self.runtime_info.get().cloned(),
            true,
            config.agent.lazy_tools,
        )
        .with_working_directory(self.working_directory.clone())
        .with_brain_path(self.brain_path.clone());

        if let Some(registry) = self.tool_registry.get() {
            builder = builder.with_tool_registry(registry.clone());
        }

        if self.headless.load(std::sync::atomic::Ordering::Relaxed) {
            builder = builder.with_headless(true);
        }

        if self
            .plan_gate_exempt
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            builder = builder.with_plan_gate_exempt(true);
        }

        if let Some(tx) = self.session_updated_tx.get() {
            builder = builder.with_session_updated_tx(tx.clone());
        }

        if let Some(mgr) = self.subagent_manager.get() {
            builder = builder.with_subagent_manager(mgr.clone());
        }

        if let Some(mgr) = self.channel_manager.get() {
            builder = builder.with_channel_manager(mgr.clone());
        }

        if message_queue_callback.is_some() {
            builder = builder.with_message_queue_callback(message_queue_callback);
        }

        if message_enqueue_callback.is_some() {
            builder = builder.with_message_enqueue_callback(message_enqueue_callback);
        }

        let agent = Arc::new(builder);
        // #1700: register this instance so a config reload can reach it. Every
        // agent the factory builds passes through this one function
        // (`create_agent_service` and `_with_queue` delegate here), so this is
        // the only registration point. Dead weaks are pruned on the way in
        // rather than left to accumulate until the next reload.
        if let Ok(mut live) = self.live_agents.lock() {
            live.retain(|w| w.strong_count() > 0);
            live.push(Arc::downgrade(&agent));
        }
        agent
    }

    /// The provider new agents should be built on: whatever the last reload
    /// installed, not the instance captured at process start (#1700).
    pub fn current_provider(&self) -> Arc<dyn Provider> {
        self.provider
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Number of agents this factory has built that are still alive.
    /// Test seam for the #1700 registry: an unregistered agent is invisible to
    /// a reload, which is exactly the bug being fixed.
    pub fn live_agent_count(&self) -> usize {
        self.live_agents
            .lock()
            .map(|live| live.iter().filter(|w| w.strong_count() > 0).count())
            .unwrap_or(0)
    }

    /// Push a reloaded config into every agent this factory built (#1700).
    ///
    /// The chain half of #1249 never reached channel agents: `AgentService`
    /// builds its `[providers.fallback]` chain in `new()` and
    /// `reload_fallback_providers` had exactly one production caller, the TUI's
    /// `app.agent_service()` in `src/cli/ui.rs`. A provider deleted from the
    /// chain kept serving channel sessions until restart, and the factory's own
    /// `provider` field was a bare `Arc`, so even an agent built afterwards
    /// inherited the pre-rotation instance.
    ///
    /// `primary` is the provider the caller already rebuilt, or `None` when that
    /// rebuild failed. The chain reload runs either way: the two are
    /// independent, and a stale chain is the state being fixed (#1249 rule).
    ///
    /// There is deliberately no per-channel hook. Channels read `config_rx`
    /// with `borrow()` on each message and nothing in the repo consumes
    /// `.changed()`, so no channel-side "config changed" arm exists to hang
    /// this on; a central registry is the only shape that reaches them.
    ///
    /// The class this belongs to is pinned by
    /// `src/tests/config_freeze_class_test.rs`, which asserts both freeze shapes
    /// rather than only one: no spawned task holding a `Config` (#1696) and no
    /// struct holding a bare provider `Arc` (#1700). A census on `Config` alone
    /// passes while A2A, WhatsApp/Trello and the secondary-profile cron factory
    /// stay frozen, because those reach their provider through this factory and
    /// not through a `Config`. Keep that test in step when changing this
    /// function, and see it for why #1249 could close with channels and RSI
    /// still broken.
    pub async fn reload_providers(&self, config: &Config, primary: Option<Arc<dyn Provider>>) {
        if let Some(new_primary) = primary.as_ref() {
            match self.provider.write() {
                Ok(mut slot) => *slot = new_primary.clone(),
                Err(e) => tracing::warn!(
                    "ConfigWatcher: channel provider slot NOT updated, lock poisoned: {e} — \
                     agents built from now on keep the previous provider"
                ),
            }
        }

        let live: Vec<Arc<AgentService>> = match self.live_agents.lock() {
            Ok(mut registry) => {
                let upgraded: Vec<Arc<AgentService>> = registry
                    .iter()
                    .filter_map(std::sync::Weak::upgrade)
                    .collect();
                // Prune in the same critical section: a channel that stopped
                // between two reloads must not linger as a dead entry.
                registry.retain(|w| w.strong_count() > 0);
                upgraded
            }
            Err(e) => {
                tracing::warn!(
                    "ConfigWatcher: channel agent registry lock poisoned: {e} — \
                     no channel agent reloaded"
                );
                return;
            }
        };

        // Counted before iteration: the `for` loop below moves `live`.
        let live_count = live.len();

        for agent in live {
            if let Some(new_primary) = primary.as_ref() {
                agent.swap_provider(new_primary.clone());
            }
            agent.reload_fallback_providers(config).await;
        }

        tracing::info!(
            "ConfigWatcher: {} channel agent(s) reloaded (primary {})",
            live_count,
            match primary.as_ref() {
                Some(p) => format!("swapped to '{}'", p.name()),
                None => "rebuild failed, chain-only reload".to_string(),
            }
        );
    }

    pub fn shared_session_id(&self) -> Arc<Mutex<Option<Uuid>>> {
        self.shared_session_id.clone()
    }

    /// The shared tool registry, if it has been wired in yet. Used by the cron
    /// scheduler to give a foreign-profile agent the same tools (the registry is
    /// profile-agnostic dispatch; per-call config drives profile behaviour).
    pub fn tool_registry(&self) -> Option<Arc<ToolRegistry>> {
        self.tool_registry.get().cloned()
    }

    pub fn service_context(&self) -> ServiceContext {
        self.service_context.clone()
    }

    /// Get a clone of the config watch receiver for channels to subscribe to.
    pub fn config_rx(&self) -> tokio::sync::watch::Receiver<Config> {
        self.config_rx.clone()
    }

    /// Read the current voice config from the watch channel (always latest).
    pub fn voice_config(&self) -> VoiceConfig {
        self.config_rx.borrow().voice_config()
    }

    /// Read the current OpenAI TTS key from the watch channel (always latest).
    pub fn openai_tts_key(&self) -> Option<String> {
        let cfg = self.config_rx.borrow();
        cfg.providers
            .tts
            .as_ref()
            .and_then(|t| t.openai.as_ref())
            .and_then(|p| p.api_key.clone())
    }
}

/// Configure a factory to build agents for cron execution (#2161).
///
/// A scheduled job runs with no user in the loop: it is already the task
/// someone planned, and there is nobody to approve a plan card. Both flags are
/// set here rather than at each call site, because the daemon's factory is
/// exactly the surface where forgetting one strands a job mid-run: the
/// active-profile branch in `resolve_job_agent` reuses this factory, and before
/// #2161 it inherited `headless` but carried no plan exemption at all, so a job
/// that crossed the tool-call threshold stranded itself asking for a plan.
pub fn configure_cron_factory(factory: &ChannelFactory) {
    factory.set_headless(true);
    factory.set_plan_gate_exempt(true);
}
