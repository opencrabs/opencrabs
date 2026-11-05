//! The half of the app that keeps working while nobody is looking.
//!
//! A surface is the TUI, the daemon, or the GUI. Whichever one is running owns
//! the profile: it claims the instance lock, serves the channels, runs the cron
//! scheduler and keeps the background maintenance alive. The surface specific
//! parts (a terminal screen, a window, no screen at all) are the caller's job.
//!
//! This module exists because the GUI booted only a chat agent while the TUI and
//! the daemon booted everything else (#2452, #2607). The split is: the caller
//! builds the graph (database, provider, tool registry, brain, working
//! directory) and hands it in, this module wires everything that outlives a
//! single turn onto it.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, mpsc, watch};

use crate::brain::agent::ProgressCallback;
use crate::brain::provider::Provider;
use crate::brain::tools::registry::ToolRegistry;
use crate::brain::tools::subagent::SubAgentManager;
use crate::channels::{ChannelFactory, ChannelManager};
use crate::config::Config;
use crate::config::profile::{InstanceLock, PreemptedInstance, SchedulerLock};
use crate::db::Pool;
use crate::services::ServiceContext;

/// How a surface tells the human about something it did on its own (a
/// preempted instance, a config reload that failed to apply).
///
/// The TUI pushes it into its event channel, the daemon writes it to the log,
/// the GUI drops it into its own notification strip. The module never decides.
pub type NoticeSink = Arc<dyn Fn(&str) + Send + Sync>;

/// What the caller already built. Nothing here is constructed twice.
pub struct SurfaceDeps {
    pub pool: Pool,
    pub provider: Arc<dyn Provider>,
    pub tool_registry: Arc<ToolRegistry>,
    pub subagent_manager: Arc<SubAgentManager>,
    pub service_context: ServiceContext,
    pub system_brain: String,
    pub working_directory: PathBuf,
    pub brain_path: PathBuf,
    pub runtime_info: Option<crate::brain::prompt_builder::RuntimeInfo>,
    /// The session a channel message lands in when it has no binding yet.
    pub shared_session_id: Arc<Mutex<Option<uuid::Uuid>>>,
    pub notices: NoticeSink,
    /// Channel turn lifecycle, so a live surface can show a session thinking.
    /// A surface with no session list (the daemon) passes `None`.
    pub session_events: Option<mpsc::UnboundedSender<crate::brain::agent::ChannelSessionEvent>>,
    /// Progress stream for the connect tools that walk a user through pairing.
    pub progress: Option<ProgressCallback>,
    /// Called with the rebuilt primary provider after a hot reload, so the
    /// surface's own chat agent can swap to it the way the TUI does (#1700).
    #[allow(clippy::type_complexity)]
    pub provider_reloaded: Option<Arc<dyn Fn(Arc<dyn Provider>) + Send + Sync>>,
    /// `true` refuses to start when another instance owns the profile, `false`
    /// preempts it. The TUI and the GUI are interactive and preempt, the daemon
    /// refuses, exactly as `[agent]` documented before this moved here.
    pub headless: bool,
}

/// Everything a running surface now owns. Holding this keeps the locks.
pub struct SurfaceServices {
    pub channel_factory: Arc<ChannelFactory>,
    pub channel_manager: Arc<ChannelManager>,
    /// `true` when this surface is the one the cron scheduler belongs to.
    pub cron_spawned: bool,
    /// `true` when the configured A2A gateway bound its listener.
    pub a2a_ready: bool,
    pub preempted: Vec<PreemptedInstance>,
    _instance_lock: Option<InstanceLock>,
    _scheduler_lock: Option<SchedulerLock>,
    _config_tx: watch::Sender<Config>,
}

/// Claim this profile, or refuse to boot (#1072).
///
/// Branches on `headless` because BOOT.md documents that the TUI takes
/// priority: opening it shuts down a running daemon for the profile. A blanket
/// exit would mean the TUI refuses to start whenever systemd has the daemon up,
/// which is the normal state on a daemonised box, so the interactive path
/// preempts first and only gives up if the lock survives that.
///
/// The daemon path never preempts, or two daemons would take turns killing each
/// other. The GUI is interactive for the same reason the TUI is: opening the
/// window is a human asking to use this profile now.
pub async fn claim_profile(
    headless: bool,
) -> Result<(Option<InstanceLock>, Vec<PreemptedInstance>)> {
    use crate::config::profile::{InstanceGuard, acquire_instance_lock, active_profile};

    let profile = active_profile().unwrap_or("default").to_string();
    let claim = {
        let p = profile.clone();
        tokio::task::spawn_blocking(move || acquire_instance_lock(&p)).await?
    };

    let held_by = match claim {
        InstanceGuard::Acquired(lock) => return Ok((Some(lock), Vec::new())),
        InstanceGuard::Unavailable => return Ok((None, Vec::new())),
        InstanceGuard::Held { pid } => pid,
    };

    if headless {
        let who = held_by
            .map(|p| format!("PID {p}"))
            .unwrap_or_else(|| "another process".to_string());
        tracing::error!(
            "Profile '{profile}' is already running ({who}); refusing to start a second instance"
        );
        anyhow::bail!(
            "OpenCrabs is already running for profile '{profile}' ({who}). Stop it first, or start this one with a different profile: opencrabs -p <name>"
        );
    }

    // Interactive: the running daemon loses. Preemption also releases the
    // channel token locks, which is the "I had to reconnect Telegram" bug it
    // was written for; taking the instance lock here means it now also stops
    // the duplicate reindex that used to run alongside the daemon's.
    let preempted =
        tokio::task::spawn_blocking(crate::config::profile::preempt_other_profile_instances)
            .await
            .unwrap_or_default();
    if !preempted.is_empty() {
        tracing::info!(
            "Surface priority: preempted {} background instance(s) of profile '{profile}'",
            preempted.len()
        );
    }

    // One retry. The kernel releases the flock when the preempted process dies,
    // which is not instant, so a single immediate retry would race the SIGTERM.
    for attempt in 0..20u32 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let p = profile.clone();
        match tokio::task::spawn_blocking(move || acquire_instance_lock(&p)).await? {
            InstanceGuard::Acquired(lock) => {
                tracing::info!("Instance lock acquired after preemption ({attempt} retries)");
                return Ok((Some(lock), preempted));
            }
            InstanceGuard::Unavailable => return Ok((None, preempted)),
            InstanceGuard::Held { .. } => continue,
        }
    }

    let who = held_by
        .map(|p| format!("PID {p}"))
        .unwrap_or_else(|| "another process".to_string());
    tracing::error!("Profile '{profile}' still held by {who} two seconds after preemption");
    anyhow::bail!(
        "OpenCrabs is already running for profile '{profile}' ({who}) and did not stop. Stop it manually, or start this one with a different profile: opencrabs -p <name>"
    );
}

/// The line a surface tells its human when it took channels away from another
/// instance. One per preempted instance.
pub fn preemption_notice(inst: &PreemptedInstance) -> String {
    let chans = if inst.channels.is_empty() {
        "channels".to_string()
    } else {
        inst.channels.join(", ")
    };
    if inst.stopped {
        format!(
            "Shut down a background instance (PID {}) that was holding {} so this surface can own them. It stays down until you start the daemon again.",
            inst.pid, chans
        )
    } else {
        format!(
            "A background instance (PID {}) holding {} did not stop (likely running as another user), so it may still contend for the connection. Stop it manually so this surface can take over.",
            inst.pid, chans
        )
    }
}

/// Boot every service a surface owns, on top of a graph the caller built.
pub async fn boot_surface(config: &Config, deps: SurfaceDeps) -> Result<SurfaceServices> {
    let SurfaceDeps {
        pool,
        provider,
        tool_registry,
        subagent_manager,
        service_context,
        system_brain,
        working_directory,
        brain_path,
        runtime_info,
        shared_session_id,
        notices,
        session_events,
        progress,
        provider_reloaded,
        headless,
    } = deps;

    let (instance_lock, preempted) = claim_profile(headless).await?;
    for inst in &preempted {
        let line = preemption_notice(inst);
        tracing::info!("{line}");
        notices(&line);
    }

    // Single source of truth for every hot reloadable value: channel agents
    // read the latest config per message instead of holding a snapshot.
    let (config_tx, config_rx) = watch::channel(config.clone());

    let channel_factory = Arc::new(ChannelFactory::new(
        provider,
        service_context.clone(),
        system_brain,
        working_directory,
        brain_path,
        shared_session_id,
        config_rx,
    ));
    if let Some(info) = runtime_info {
        channel_factory.set_runtime_info(info);
    }

    // Proactive messaging state. Every channel gets one, and every store it
    // needs to survive a restart is attached here.
    #[cfg(feature = "telegram")]
    let telegram_state = Arc::new(crate::channels::telegram::TelegramState::new());
    #[cfg(feature = "telegram")]
    {
        telegram_state
            .set_plan_card_store(crate::db::repository::PlanCardRepository::new(pool.clone()))
            .await;
        telegram_state
            .set_channel_message_store(crate::db::ChannelMessageRepository::new(pool.clone()))
            .await;
        telegram_state
            .set_followup_store(crate::db::repository::PendingFollowupRepository::new(
                pool.clone(),
            ))
            .await;
        telegram_state
            .set_binding_store(crate::db::SessionBindingRepository::new(pool.clone()))
            .await;
    }
    #[cfg(feature = "whatsapp")]
    let whatsapp_state = Arc::new(crate::channels::whatsapp::WhatsAppState::new());
    #[cfg(feature = "discord")]
    let discord_state = Arc::new(crate::channels::discord::DiscordState::new());
    #[cfg(feature = "slack")]
    let slack_state = Arc::new(crate::channels::slack::SlackState::new());
    #[cfg(feature = "trello")]
    let trello_state = Arc::new(crate::channels::trello::TrelloState::new());

    // The connect and send tools are what make a surface able to reach a
    // channel proactively. The GUI never registered any of them, so a GUI turn
    // could not send a file to Telegram even with a live bot.
    #[cfg(feature = "telegram")]
    {
        tool_registry.register(Arc::new(
            crate::brain::tools::telegram_connect::TelegramConnectTool::new(
                channel_factory.clone(),
                telegram_state.clone(),
            ),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::telegram_send::TelegramSendTool::new(telegram_state.clone()),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::session_search::SessionSearchTool::with_telegram(
                pool.clone(),
                telegram_state.clone(),
            ),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::cowork_connect::CoworkConnectTool::new(telegram_state.clone()),
        ));
    }
    #[cfg(feature = "whatsapp")]
    {
        tool_registry.register(Arc::new(
            crate::brain::tools::whatsapp_connect::WhatsAppConnectTool::new(
                progress.clone(),
                whatsapp_state.clone(),
            ),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::whatsapp_send::WhatsAppSendTool::new(
                whatsapp_state.clone(),
                channel_factory.config_rx(),
            ),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::whatsapp_history::WhatsAppHistoryTool::new(
                crate::db::ChannelMessageRepository::new(pool.clone()),
            ),
        ));
    }
    #[cfg(feature = "discord")]
    {
        tool_registry.register(Arc::new(
            crate::brain::tools::discord_connect::DiscordConnectTool::new(
                channel_factory.clone(),
                discord_state.clone(),
            ),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::discord_send::DiscordSendTool::new(discord_state.clone()),
        ));
    }
    #[cfg(feature = "slack")]
    {
        tool_registry.register(Arc::new(
            crate::brain::tools::slack_connect::SlackConnectTool::new(
                channel_factory.clone(),
                slack_state.clone(),
            ),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::slack_send::SlackSendTool::new(slack_state.clone()),
        ));
    }
    #[cfg(feature = "trello")]
    {
        tool_registry.register(Arc::new(
            crate::brain::tools::trello_connect::TrelloConnectTool::new(
                channel_factory.clone(),
                trello_state.clone(),
            ),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::trello_send::TrelloSendTool::new(trello_state.clone()),
        ));
    }

    crate::cli::tool_setup::register_runtime_tools(&tool_registry, config);
    channel_factory.set_tool_registry(tool_registry.clone());
    channel_factory.set_subagent_manager(subagent_manager);
    if let Some(tx) = session_events {
        channel_factory.set_session_updated_tx(tx);
    }

    let channel_manager = Arc::new(ChannelManager::new(
        channel_factory.clone(),
        pool.clone(),
        #[cfg(feature = "telegram")]
        telegram_state.clone(),
        #[cfg(feature = "whatsapp")]
        whatsapp_state.clone(),
        #[cfg(feature = "discord")]
        discord_state.clone(),
        #[cfg(feature = "slack")]
        slack_state.clone(),
        #[cfg(feature = "trello")]
        trello_state.clone(),
    ));
    channel_factory.set_channel_manager(channel_manager.clone());
    channel_manager.reconcile(config).await;

    // Hot reload: a key rotation, an allowlist edit or a channel toggle reaches
    // the channel agents without a restart. The surface's own chat agent gets
    // the rebuilt provider through `provider_reloaded`.
    {
        use crate::utils::config_watcher::{self, ReloadCallback};

        let mut callbacks: Vec<ReloadCallback> = Vec::new();

        {
            let registry = tool_registry.clone();
            callbacks.push(Arc::new(move |cfg: Config| {
                crate::cli::ui::register_config_dependent_tools(&registry, &cfg);
                tracing::info!("ConfigWatcher: re-registered config-dependent tools");
            }));
        }

        {
            let factory = Arc::clone(&channel_factory);
            let tx = config_tx.clone();
            let hook = provider_reloaded.clone();
            callbacks.push(Arc::new(move |cfg: Config| {
                let _ = tx.send(cfg.clone());
                let factory = Arc::clone(&factory);
                let hook = hook.clone();
                tokio::spawn(async move {
                    // Rebuilt once and handed to both surfaces (#1700): two
                    // calls would pay two constructions and could disagree.
                    let primary = match crate::brain::provider::create_provider(&cfg).await {
                        Ok(p) => {
                            tracing::info!("ConfigWatcher: LLM provider reloaded from new keys");
                            Some(p)
                        }
                        Err(e) => {
                            tracing::warn!(
                                "ConfigWatcher: provider rebuild failed, keeping current: {e}"
                            );
                            None
                        }
                    };
                    if let (Some(hook), Some(p)) = (hook.as_ref(), primary.as_ref()) {
                        hook(p.clone());
                    }
                    // #1249: the chain half reloads even when the primary
                    // rebuild failed. A stale chain is the state being fixed.
                    factory.reload_providers(&cfg, primary).await;
                });
            }));
        }

        callbacks.push(Arc::new(move |cfg: Config| {
            crate::logging::apply_debug_logs_from_config(cfg.agent.debug_logs);
        }));

        {
            let mgr = channel_manager.clone();
            callbacks.push(Arc::new(move |cfg: Config| {
                let mgr = mgr.clone();
                tokio::spawn(async move { mgr.reconcile(&cfg).await });
            }));
        }

        // force_default push (#466): when the active default provider's section
        // opts in, a reload broadcasts its pair to every non-archived session.
        {
            let svc_ctx = ServiceContext::new(pool.clone());
            callbacks.push(Arc::new(move |cfg: Config| {
                let session_svc = crate::services::SessionService::new(svc_ctx.clone());
                tokio::spawn(async move {
                    match crate::services::force_default::apply_force_default(&cfg, &session_svc)
                        .await
                    {
                        Ok(0) => {}
                        Ok(n) => tracing::info!("force_default: {n} session(s) switched"),
                        Err(e) => tracing::error!("force_default push failed: {e:#}"),
                    }
                });
            }));
        }

        // Telegram command refresh: re-register the bot menu when commands.toml
        // changes, so a new slash command appears without a restart.
        #[cfg(feature = "telegram")]
        {
            let tg_state = telegram_state.clone();
            callbacks.push(Arc::new(move |_cfg: Config| {
                let state = tg_state.clone();
                tokio::spawn(async move {
                    if let Some(bot) = state.bot().await {
                        crate::channels::telegram::register_bot_commands(&bot).await;
                        tracing::info!("ConfigWatcher: refreshed Telegram bot commands");
                    }
                });
            }));
        }

        let notice_sink = notices.clone();
        let notify: config_watcher::ReloadNotify = Arc::new(move |msg: String| {
            tracing::warn!("ConfigWatcher: {msg}");
            notice_sink(&msg);
        });

        config_watcher::spawn(callbacks, Some(notify));
    }

    let a2a_ready = spawn_a2a_gateway(config, &channel_factory, &service_context).await;
    let (cron_spawned, scheduler_lock) =
        spawn_cron_scheduler(config, &channel_factory, &service_context, &pool, a2a_ready);
    spawn_background_maintenance(&pool, headless);

    Ok(SurfaceServices {
        channel_factory,
        channel_manager,
        cron_spawned,
        a2a_ready,
        preempted,
        _instance_lock: instance_lock,
        _scheduler_lock: scheduler_lock,
        _config_tx: config_tx,
    })
}

/// A2A gateway, when the config asks for one. Returns whether it bound its
/// listener, which decides whether the cron scheduler may start (#196).
async fn spawn_a2a_gateway(
    config: &Config,
    channel_factory: &Arc<ChannelFactory>,
    service_context: &ServiceContext,
) -> bool {
    if !config.a2a.enabled {
        return false;
    }
    let a2a_agent = channel_factory.create_agent_service().await;
    let a2a_ctx = service_context.clone();
    let a2a_config = config.a2a.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Err(e) =
            crate::a2a::server::start_server(&a2a_config, a2a_agent, a2a_ctx, Some(ready_tx)).await
        {
            tracing::error!("A2A gateway error: {}", e);
        }
    });
    // #196: block the cron scheduler until the listener is bound, so a job due
    // on its first tick finds the gateway listening.
    match tokio::time::timeout(std::time::Duration::from_secs(30), ready_rx).await {
        Ok(Ok(Ok(()))) => true,
        Ok(Ok(Err(e))) => {
            tracing::error!("A2A gateway refused to start: {e}");
            false
        }
        Ok(Err(_)) => {
            tracing::error!("A2A gateway task exited without signalling readiness");
            false
        }
        Err(_) => {
            tracing::error!("Timed out waiting for A2A gateway readiness after 30s (#196)");
            false
        }
    }
}

/// Cron scheduler, one per profile machine wide (#444).
///
/// The lock decides who runs it, so a TUI, a daemon and a GUI never double fire
/// the same job. A start that will not spawn a scheduler takes no lock either
/// (#2096): a process holding the lock while polling nothing blocks everyone.
fn spawn_cron_scheduler(
    config: &Config,
    channel_factory: &Arc<ChannelFactory>,
    service_context: &ServiceContext,
    pool: &Pool,
    a2a_ready: bool,
) -> (bool, Option<SchedulerLock>) {
    let active_profile = crate::config::profile::active_profile()
        .unwrap_or("default")
        .to_string();
    let blocked = crate::cli::ui::cron_blocked_by_a2a(config.a2a.enabled, a2a_ready);
    let lock = crate::cli::ui::scheduler_lock_for_start(blocked, None, || {
        crate::config::profile::acquire_scheduler_lock(&active_profile)
    });

    if blocked {
        tracing::error!("Cron scheduler not spawned: A2A gateway did not become ready (#196)");
        return (false, None);
    }
    if lock.is_none() {
        tracing::info!(
            "Cron scheduler for '{active_profile}' already running elsewhere, not spawning a second"
        );
        return (false, None);
    }

    let cron_repo = crate::db::CronJobRepository::new(pool.clone());
    let cron_run_repo = crate::db::CronJobRunRepository::new(pool.clone());
    crate::cron::CronScheduler::new(
        cron_repo,
        cron_run_repo,
        channel_factory.clone(),
        service_context.clone(),
    )
    .spawn();
    tracing::info!("Cron scheduler spawned for '{active_profile}'");
    (true, lock)
}

/// Maintenance that has to run whether or not a human is looking: the memory
/// index, the embedding model, the external sweeps, the RSI engine and the
/// session to project backfill.
fn spawn_background_maintenance(pool: &Pool, headless: bool) {
    // Delay startup so a resumed task or a connecting channel does not race the
    // llama-cpp GGML engine, which segfaults under contention.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        match crate::memory::get_store() {
            Ok(store) => {
                match crate::memory::reindex(store).await {
                    Ok(n) => tracing::info!("Startup memory reindex: {n} files"),
                    Err(e) => tracing::warn!("Startup memory reindex failed: {e}"),
                }
                crate::memory::backfill_sweep::spawn(store);
            }
            Err(e) => tracing::warn!("Memory store init failed at startup: {e}"),
        }
        match tokio::task::spawn_blocking(crate::memory::get_engine).await {
            Ok(Ok(_)) => tracing::info!("Embedding engine warmed up"),
            Ok(Err(e)) => tracing::info!("Embedding engine init skipped: {e}"),
            Err(e) => tracing::warn!("Embedding engine warmup failed: {e}"),
        }
        crate::memory::freshness::spawn_external_sweep();
    });

    // The first bash command should not block on the RTK binary resolution.
    crate::rtk::warm_up();

    // RSI digest and periodic analysis. This surface has no notification strip
    // of its own here, so the notifications land in the log, which is where an
    // operator looks for them.
    let (rsi_tx, mut rsi_rx) = mpsc::unbounded_channel();
    crate::brain::rsi::spawn_rsi_engine(pool.clone(), rsi_tx, headless);
    tokio::spawn(async move {
        while let Some(notification) = rsi_rx.recv().await {
            let text = crate::brain::rsi::format_rsi_notification(&notification);
            tracing::info!("RSI: {text}");
        }
    });

    // Sessions that predate their project, or were never linked at all (#1445).
    let svc_ctx = ServiceContext::new(pool.clone());
    tokio::spawn(async move {
        let project_svc = crate::services::ProjectService::new(svc_ctx);
        if let Err(e) = project_svc.backfill_unassigned_sessions().await {
            tracing::warn!(error = %e, "session/project backfill failed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(pid: u32, channels: &[&str], stopped: bool) -> PreemptedInstance {
        PreemptedInstance {
            pid,
            channels: channels.iter().map(|c| (*c).to_string()).collect(),
            stopped,
        }
    }

    /// A stop that worked reads as done, names the channels it freed, and says
    /// how to get them back (#2607). The window is the one place this is
    /// visible, so the sentence has to carry all three.
    #[test]
    fn preemption_notice_reports_a_completed_stop() {
        let line = preemption_notice(&inst(4242, &["telegram", "discord"], true));
        assert!(line.contains("PID 4242"), "{line}");
        assert!(line.contains("telegram, discord"), "{line}");
        assert!(line.contains("stays down"), "{line}");
    }

    /// A stop that failed must not claim the channels are free: the other
    /// process may still be polling the same bot token.
    #[test]
    fn preemption_notice_does_not_claim_success_when_it_failed() {
        let line = preemption_notice(&inst(7, &[], false));
        assert!(line.contains("did not stop"), "{line}");
        assert!(!line.contains("stays down"), "{line}");
        assert!(line.contains("channels"), "{line}");
    }
}
