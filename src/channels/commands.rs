//! Shared slash-command handlers for channel platforms (Telegram, Discord, Slack).
//!
//! Each channel handler calls [`handle_command`] before forwarding to the agent.
//! If the message is a known command, the channel renders the response directly.

use uuid::Uuid;

use crate::brain::agent::AgentService;
use crate::config::Config;
use crate::db::repository::SessionListOptions;
use crate::services::SessionService;

/// Sync the channel agent's provider for a specific session.
///
/// If the session has its own provider/model stored, restore that — so each
/// channel keeps its own provider independently of the TUI or other channels.
/// Only falls back to the global config if the session has no provider set.
pub async fn sync_provider_for_session(
    agent: &AgentService,
    session_id: Uuid,
    session_provider: Option<&str>,
    session_model: Option<&str>,
) {
    // The in-memory mirror, not `Config::load()`: this runs on every inbound
    // channel message (#2184, #2188), and the watcher keeps the mirror current,
    // so a disk read and TOML parse per message buys nothing.
    let config = Config::current();

    // Try the session's stored provider/model first.
    // [agent] default_provider/default_model is ONLY a fallback when the session's
    // provider fails to load (e.g., missing API key, provider not available).
    // This preserves session isolation: split panes, different channels, and
    // testing different providers in parallel all keep their own provider/model.
    let (effective_provider, effective_model) = (session_provider, session_model);

    // If the session has an explicit provider, restore it (ignoring global config)
    if let Some(sess_prov) = effective_provider {
        // Compare against the provider the session is CONFIGURED with, not the
        // entry a sticky fallback happens to be serving (#2184). The live name
        // reports the promoted fallback, so it can never equal the pinned name:
        // the fast path was unreachable for any session with a fallback chain,
        // and every inbound message rebuilt the primary from config (0.6-1.9s),
        // which also threw away the chain's cooldown record and made the failed
        // primary get retried and re-promoted on every message.
        let agent_provider = agent.configured_provider_name_for_session(session_id);
        let agent_model = agent.provider_model_for_session(session_id);
        let sess_prov_norm = normalize_provider_name(sess_prov);
        let agent_prov_norm = normalize_provider_name(&agent_provider);
        let same_provider = provider_names_match(&sess_prov_norm, &agent_prov_norm);
        let same_model = effective_model.is_none_or(|m| m == agent_model);

        if same_provider && same_model {
            tracing::debug!(
                "sync_provider_for_session[{}]: already on {}/{} — no swap needed",
                session_id,
                agent_provider,
                agent_model
            );
            return;
        }

        tracing::info!(
            "sync_provider_for_session[{}]: session wants {}/{}, agent currently on {}/{} — attempting restore",
            session_id,
            sess_prov,
            effective_model.unwrap_or("<default>"),
            agent_provider,
            agent_model,
        );
        // Log whether the config actually has an api_key for this provider
        // to diagnose "Auth error on restart" issues.
        let has_key = config
            .providers
            .custom
            .as_ref()
            .and_then(|c| c.get(sess_prov))
            .and_then(|p| p.api_key.as_ref())
            .is_some_and(|k| crate::config::stored_key::is_real_key(k));
        tracing::info!(
            "sync_provider_for_session[{}]: create_provider_by_name('{}') — config has api_key: {}",
            session_id,
            sess_prov,
            has_key
        );

        match crate::brain::provider::factory::create_provider_by_name(&config, sess_prov).await {
            Ok(new_provider) => {
                tracing::info!(
                    "sync_provider_for_session[{}]: restored {}/{} (was {}/{})",
                    session_id,
                    sess_prov,
                    effective_model.unwrap_or("<default>"),
                    agent_provider,
                    agent_model,
                );
                // Restore the saved provider+model pair atomically.
                let model = effective_model
                    .map(str::to_string)
                    .unwrap_or_else(|| new_provider.default_model().to_string());
                agent.swap_provider_for_session(session_id, new_provider, model);
            }
            Err(e) => {
                // Session has a stored provider but we couldn't create it.
                // Try [agent] default_provider as a fallback before giving up.
                tracing::warn!(
                    "sync_provider_for_session[{}]: create_provider_by_name('{}') failed: {}",
                    session_id,
                    sess_prov,
                    e
                );

                if let (Some(fallback_provider), fallback_model) = (
                    config.agent.default_provider.as_deref(),
                    config.agent.default_model.as_deref(),
                ) {
                    tracing::info!(
                        "sync_provider_for_session[{}]: trying [agent] fallback {}/{}",
                        session_id,
                        fallback_provider,
                        fallback_model.unwrap_or("<default>"),
                    );

                    match crate::brain::provider::factory::create_provider_by_name(
                        &config,
                        fallback_provider,
                    )
                    .await
                    {
                        Ok(fallback_prov) => {
                            let model = fallback_model
                                .map(str::to_string)
                                .unwrap_or_else(|| fallback_prov.default_model().to_string());
                            tracing::info!(
                                "sync_provider_for_session[{}]: fallback to {}/{} succeeded",
                                session_id,
                                fallback_provider,
                                model,
                            );
                            agent.swap_provider_for_session(session_id, fallback_prov, model);
                        }
                        Err(fallback_err) => {
                            tracing::warn!(
                                "sync_provider_for_session[{}]: fallback {}/{} also failed: {} — keeping current provider",
                                session_id,
                                fallback_provider,
                                fallback_model.unwrap_or("<default>"),
                                fallback_err,
                            );
                        }
                    }
                }
            }
        }
    } else {
        // Session has no stored provider — this is the ONLY case where global config applies
        tracing::debug!(
            "sync_provider_for_session[{}]: session has no stored provider — using global config",
            session_id
        );
        let (cfg_provider, cfg_model) = config.providers.active_provider_and_model();
        // Same configured-vs-live distinction as above: a session riding the
        // global chain's sticky fallback has not changed provider (#2184).
        let agent_provider = agent.configured_provider_name_for_session(session_id);
        let agent_model = agent.provider_model_for_session(session_id);

        let cfg_provider_norm = normalize_provider_name(&cfg_provider);
        let agent_provider_norm = normalize_provider_name(&agent_provider);
        let same_provider = provider_names_match(&cfg_provider_norm, &agent_provider_norm);

        if !same_provider || cfg_model != agent_model {
            match crate::brain::provider::create_provider(&config).await {
                Ok(new_provider) => {
                    tracing::info!(
                        "sync_provider_for_session[{}]: synced to config provider {} (was {})",
                        session_id,
                        cfg_provider,
                        agent_provider,
                    );
                    agent.swap_provider_for_session(session_id, new_provider, cfg_model.clone());
                }
                Err(e) => {
                    tracing::warn!(
                        "sync_provider_for_session[{}]: create_provider(active config) failed: {}",
                        session_id,
                        e
                    );
                }
            }
        }
    }
}

/// Normalize provider names/aliases to stable IDs used by config.
pub(crate) fn normalize_provider_name(name: &str) -> String {
    crate::utils::providers::normalize_provider_name(name)
}

/// Compare normalized provider names, handling custom runtime names (`deepseek`).
pub(crate) fn provider_names_match(config_provider: &str, runtime_provider: &str) -> bool {
    config_provider == runtime_provider
        || config_provider
            .strip_prefix("custom:")
            .is_some_and(|name| name == runtime_provider)
}

/// Result of matching a channel message against known commands.
pub enum ChannelCommand {
    /// `/help` — formatted help text
    Help(String),
    /// `/usage` — formatted session/cost stats
    Usage(String),
    /// `/mission-control`: full mission control report (analytics, activity, inbox, schedule)
    MissionControl(String),
    /// `/models` — provider picker (step 1: choose provider, step 2: choose model)
    Models(ProvidersResponse),
    /// `/new` — create a new session and switch to it
    NewSession,
    /// `/sessions` — list recent sessions to switch between
    Sessions(SessionsResponse),
    /// `/stop` — cancel the running agent task
    Stop,
    /// `/stop <session-id-or-unique-prefix>` — cancel a DIFFERENT session's
    /// running turn (#2108). Plain `/stop` can only reach the session the
    /// message landed in, so a stuck turn in another chat was unstoppable.
    StopTarget(String),
    /// `/compact` — trigger context compaction via the agent
    Compact,
    /// `/clear` — cut the agent's context here without a summariser call;
    /// history, session id and title stay (#1585)
    ClearContext,
    /// `/doctor` — health check (no LLM needed)
    Doctor,
    /// `/evolve` — check for updates and install directly (no LLM needed)
    Evolve,
    /// `/restart` — cycle the process, same binary and arguments
    Restart,
    /// `/exit` — shut the process down
    Exit,
    /// `/rtk` — show RTK token savings statistics
    Rtk(String),
    /// User-defined command with action "prompt" — forward prompt text to the agent
    UserPrompt(String),
    /// User-defined command with action "system" — display text directly
    UserSystem(String),
    /// `/models <provider/model>` — direct switch applied, reply text (#467)
    ModelSwitched(String),
    /// Unknown slash command — warn the user, don't forward to agent
    UnknownCommand(String),
    /// `/rename <title>` — rename the current session
    Rename(String),
    /// `/cd [path]` — directory browser (inline keyboard)
    ChangeDir(DirBrowserResponse),
    /// `/architecture [path]` — mechanical directory tree, no LLM (#933)
    Architecture(Option<String>),
    /// `/attach <paths...>` — mechanical file attach, no LLM (#933)
    Attach(String),
    /// `/audit [N]` — mechanical audit viewer: ACTION rows always, READ +
    /// OUTCOME when `[features] audit_recording` is on (#1705)
    Audit(String),
    /// `/profiles` — profile manager (inline keyboard)
    Profiles(ProfilesResponse),
    /// `/respond_to [all|mention|auto]` — show/switch auto-mention mode (#244)
    RespondTo(String),
    /// `/redact [global|group|dm] [on|off]` — show/switch scoped redaction (#677)
    Redact(String),
    /// `/plan` — enter durable pre-init Plan mode (reply text)
    PlanMode(String),
    /// `/plan <query>` — enter pre-init Plan mode AND dispatch the trailing
    /// text as the first planning turn, so the agent drafts the design from
    /// the query in one step (the channel handler runs it as an agent turn).
    PlanModeWithQuery(String),
    /// `/show-plan` — plan state summary (reply text)
    ShowPlan(String),
    /// `/execute` — Approve the design plan / retry the seed. The channel
    /// handler owns the busy check (refuse while a turn runs, never queue)
    /// and dispatches the visible seed turn.
    ExecutePlan,
    /// `/discard` — cancel the in-flight turn when needed, then engine
    /// cleanup back to NoPlan (handled by the channel handler).
    DiscardPlan,
    /// Not a recognised command — pass through to agent
    NotACommand,
}

/// Data for rendering a provider-picker on the channel platform.
pub struct ProvidersResponse {
    pub current_provider: String,
    pub current_model: String,
    /// All known providers as `(id, display_label, configured)` triples.
    /// `configured = false` entries surface providers the user hasn't set
    /// up yet (issue #126) so the picker shows e.g. `🔒 OpenCode` alongside
    /// active providers. Tapping a locked entry shows the help text from
    /// `unconfigured_provider_help()` instead of swapping.
    pub providers: Vec<(String, String, bool)>,
    /// Fallback text when platform buttons are unavailable.
    pub text: String,
}

/// Data for rendering a session-picker on the channel platform.
pub struct SessionsResponse {
    pub current_session_id: Uuid,
    /// (session_id, display_label)
    pub sessions: Vec<(Uuid, String)>,
    /// Fallback text when platform buttons are unavailable.
    pub text: String,
}

/// Data for rendering a model-picker after a provider is selected.
pub struct ModelsResponse {
    pub provider_name: String,
    pub current_model: String,
    pub models: Vec<String>,
    /// Fallback text when platform buttons are unavailable.
    pub text: String,
    /// When true, the provider has too many models for inline buttons (OpenRouter, custom).
    /// Channels should switch to default immediately and let the agent handle follow-up.
    pub agent_handled: bool,
}

/// Entry in the directory browser — either a directory or a file.
pub struct DirBrowserEntry {
    pub name: String,
    pub is_dir: bool,
    /// Index in the full (sorted) entry list — used in callback data
    pub index: usize,
}

/// Data for rendering a directory browser on the channel platform.
pub struct DirBrowserResponse {
    /// Current path being browsed
    pub current_path: String,
    /// Entries on the current page
    pub entries: Vec<DirBrowserEntry>,
    /// Current page (0-indexed)
    pub page: usize,
    /// Total number of pages
    pub total_pages: usize,
    /// Active filter text (if any)
    pub filter: Option<String>,
    /// Number of total entries (before paging)
    pub total_entries: usize,
    /// Fallback text when platform buttons are unavailable.
    pub text: String,
}

/// Entry in the profiles browser.
pub struct ProfileBrowserEntry {
    /// Profile name (key in registry)
    pub name: String,
    /// Optional description
    pub description: Option<String>,
    /// Whether this is the currently active profile
    pub is_active: bool,
    /// Whether this is the default (root ~/.opencrabs/) profile
    pub is_default: bool,
    /// When it was created
    pub created_at: String,
    /// Last time it was used (if ever)
    pub last_used: Option<String>,
}

/// Data for rendering a profile manager on the channel platform.
pub struct ProfilesResponse {
    /// Currently active profile name
    pub active_profile: String,
    /// All profiles
    pub entries: Vec<ProfileBrowserEntry>,
    /// Fallback text when platform buttons are unavailable.
    pub text: String,
}

/// Strip a `@botname` suffix from the COMMAND TOKEN only, preserving the
/// arguments: `/cd@yourbot /tmp` becomes `/cd /tmp`. Telegram appends the
/// handle when a command is picked from the menu in groups. Cutting at the
/// first `@` in the whole line (the previous behaviour) also discarded the
/// arguments, turning `/respond_to@yourbot mention` into a bare
/// `/respond_to`, and truncated arguments that legitimately contain `@`
/// (#265). Non-command text is returned trimmed but otherwise untouched.
pub(crate) fn strip_command_handle(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return trimmed.to_string();
    }
    let (head, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((h, r)) => (h, Some(r)),
        None => (trimmed, None),
    };
    match (head.find('@'), rest) {
        (Some(at), Some(r)) => format!("{} {}", &head[..at], r),
        (Some(at), None) => head[..at].to_string(),
        (None, _) => trimmed.to_string(),
    }
}

/// How a `/stop <target>` argument resolved against a set of session ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetMatch {
    /// Exactly one session matched.
    Unique(Uuid),
    /// Several sessions matched the prefix; the caller must disambiguate.
    Ambiguous(Vec<Uuid>),
    /// Nothing matched.
    None,
}

/// Resolve a session id, or a unique id prefix, against `candidates` (#2108).
///
/// A prefix has to be at least [`MIN_TARGET_PREFIX`] characters so a stray
/// short token cannot silently select a session, and it only resolves when
/// exactly one candidate matches: an ambiguous prefix is reported, never
/// guessed, because the consequence of guessing wrong is cancelling the wrong
/// turn.
pub(crate) fn resolve_session_prefix<'a>(
    needle: &str,
    candidates: impl Iterator<Item = &'a Uuid>,
) -> TargetMatch {
    let needle = needle.trim().to_ascii_lowercase();
    if needle.len() < MIN_TARGET_PREFIX {
        return TargetMatch::None;
    }
    let mut hits: Vec<Uuid> = candidates
        .filter(|id| id.to_string().starts_with(&needle))
        .copied()
        .collect();
    hits.sort();
    hits.dedup();
    match hits.len() {
        0 => TargetMatch::None,
        1 => TargetMatch::Unique(hits[0]),
        _ => TargetMatch::Ambiguous(hits),
    }
}

/// Shortest session-id prefix `/stop` will act on.
pub(crate) const MIN_TARGET_PREFIX: usize = 4;

/// The shortest form of `id` that is still unambiguous among `candidates`.
pub(crate) fn shortest_unique_prefix<'a>(
    id: Uuid,
    candidates: impl Iterator<Item = &'a Uuid> + Clone,
) -> String {
    let full = id.to_string();
    for len in MIN_TARGET_PREFIX..=full.len() {
        let prefix = &full[..len];
        if candidates
            .clone()
            .filter(|other| **other != id)
            .all(|other| !other.to_string().starts_with(prefix))
        {
            return prefix.to_string();
        }
    }
    full
}

/// Detect a targeted stop typed as prose: an owner message that OPENS with a
/// stop word and names one of `running`, but is not itself a recognised stop
/// intent (#2108).
///
/// Returns the exact command to send back. This never cancels anything: a
/// sentence like "stop a1b2c3d4" is ambiguous enough that guessing which
/// session the sender meant is worse than telling them the one spelling that
/// works. A plain "stop" is a recognised intent and is handled by the normal
/// kill switch, so it is not caught here; "stop the docker container" names no
/// session, so it is not caught either.
pub(crate) fn targeted_stop_hint(text: &str, running: &[Uuid]) -> Option<String> {
    use crate::utils::stop_intent::{is_stop_intent, normalize};

    if running.is_empty() || is_stop_intent(text) {
        return None;
    }
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let (first, rest) = tokens.split_first()?;
    // The message has to OPEN with a stop word. A bare stop word is a real
    // intent and was filtered above, so anything still here carries an object.
    if !is_stop_intent(&normalize(first)) {
        return None;
    }
    for raw in rest {
        let token: String = raw
            .chars()
            .filter(|c| c.is_ascii_hexdigit() || *c == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        if token.len() < MIN_TARGET_PREFIX {
            continue;
        }
        if let TargetMatch::Unique(id) = resolve_session_prefix(&token, running.iter()) {
            let prefix = shortest_unique_prefix(id, running.iter());
            return Some(format!(
                "That reads like a targeted stop. Send `/stop {prefix}` to stop session `{prefix}`."
            ));
        }
    }
    None
}

/// Check if a message is a known channel command and return the response.
/// Commands that produce output are persisted to session history so they
/// appear in TUI and give the agent context about what happened.
pub async fn handle_command(
    text: &str,
    session_id: Uuid,
    agent: &AgentService,
    session_svc: &SessionService,
    is_owner: bool,
    chat_id: Option<&str>,
) -> ChannelCommand {
    // Strip structured `<<IMG:…>>` / `<<VID:…>>` markers before matching. The
    // channel handler appends these when a photo/video is in context — attached
    // to THIS message OR pulled from recent chat / a reply — so a command sent
    // while an image sits in context arrives as `/models\n<<IMG:…>>`. An exact
    // command match then fails, the command falls through to the agent, and the
    // switch silently runs as a prose reply instead of executing (confirmed in
    // logs: a `/models` with an auto-injected recent-photo marker returned
    // NotACommand). The markers are machine context, not part of the typed
    // command, so drop them for detection; the image still reaches the agent
    // via the handler's own text when the message is genuinely not a command.
    let (text_no_media, _imgs) = crate::utils::extract_img_markers(text);
    let (text_no_media, _vids) = crate::utils::extract_vid_markers(&text_no_media);
    // Strip @botname from the command token — defense-in-depth: handler.rs
    // already strips this bot's own handle, but if bot_username() returns
    // None there, this catch ensures commands still match.
    let normalized = strip_command_handle(&text_no_media);
    let trimmed = normalized.as_str();
    let result = match trimmed {
        "/compact" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Compact
            }
        }
        "/clear" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::ClearContext
            }
        }
        "/doctor" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Doctor
            }
        }
        "/evolve" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Evolve
            }
        }
        "/exit" | "/quit" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Exit
            }
        }
        "/help" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Help(format_help())
            }
        }
        "/restart" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Restart
            }
        }
        "/models" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Models(format_providers(agent, session_id))
            }
        }
        // `/models <provider/model>` — direct switch for the CURRENT session,
        // no inline keyboard round trip (#467). Bare /models keeps the picker.
        t if t.starts_with("/models ") || t.starts_with("/model ") => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                let arg = t.split_once(' ').map(|x| x.1.trim()).unwrap_or("");
                ChannelCommand::ModelSwitched(direct_model_switch(agent, session_id, arg).await)
            }
        }
        "/new" => {
            // Owner-gated like every sibling. It was the one command without
            // the check, so anyone in an allowlisted group could reset the
            // owner's session out from under them (#782).
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::NewSession
            }
        }
        "/plan" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::PlanMode(crate::utils::plan_mode::enter_plan_mode(session_id).await)
            }
        }
        // `/plan <query>`: enter Plan mode AND use the trailing text as the
        // planning intent, so the agent drafts the design in one step instead
        // of waiting for a second message (#579).
        t if t.starts_with("/plan ") => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                let query = t.strip_prefix("/plan ").unwrap_or("").trim().to_string();
                if query.is_empty() {
                    ChannelCommand::PlanMode(
                        crate::utils::plan_mode::enter_plan_mode(session_id).await,
                    )
                } else {
                    // Arm pre-init Plan mode now (side effect: set_pre_init_editing;
                    // the returned prompt text is unused here because the agent's
                    // planning turn is the reply). The handler dispatches `query`.
                    crate::utils::plan_mode::enter_plan_mode(session_id).await;
                    ChannelCommand::PlanModeWithQuery(query)
                }
            }
        }
        "/show-plan" | "/showplan" | "/show_plan" => {
            ChannelCommand::ShowPlan(crate::utils::plan_mode::show_plan(session_id).await)
        }
        "/execute" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::ExecutePlan
            }
        }
        "/discard" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::DiscardPlan
            }
        }
        "/rtk" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Rtk(format_rtk().await)
            }
        }
        "/sessions" => {
            if !is_owner {
                ChannelCommand::UnknownCommand(
                    "🔒 `/sessions` is restricted to the bot owner.".to_string(),
                )
            } else {
                ChannelCommand::Sessions(format_sessions(session_id, session_svc, None).await)
            }
        }
        cmd if cmd.starts_with("/sessions:") || cmd.starts_with("/sessions ") => {
            if !is_owner {
                ChannelCommand::UnknownCommand(
                    "🔒 `/sessions` is restricted to the bot owner.".to_string(),
                )
            } else {
                let query = cmd
                    .strip_prefix("/sessions:")
                    .or_else(|| cmd.strip_prefix("/sessions "))
                    .filter(|q| !q.is_empty());
                ChannelCommand::Sessions(format_sessions(session_id, session_svc, query).await)
            }
        }
        "/stop" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Stop
            }
        }
        cmd if cmd.starts_with("/stop ") || cmd.starts_with("/stop:") => {
            // #2108: target another session's running turn. Owner-only for the
            // same reason plain /stop is: cancelling someone else's work is a
            // destructive action on state the sender cannot see.
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                let target = cmd
                    .strip_prefix("/stop:")
                    .or_else(|| cmd.strip_prefix("/stop "))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if target.is_empty() {
                    ChannelCommand::UnknownCommand(
                        "`/stop <session-id>` needs a target. Run `/sessions` for the ids."
                            .to_string(),
                    )
                } else {
                    ChannelCommand::StopTarget(target)
                }
            }
        }
        cmd if cmd == "/usage" || cmd.starts_with("/usage ") => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else if let Some(args) = trimmed.strip_prefix("/usage ").map(str::trim)
                && !args.is_empty()
            {
                ChannelCommand::Usage(format_usage_breakdown(args, session_svc).await)
            } else {
                ChannelCommand::Usage(format_usage(session_id, agent, session_svc).await)
            }
        }
        cmd if cmd == "/cd" || cmd.starts_with("/cd ") => {
            // Owner-only: /cd browses the host filesystem, so a non-owner on the
            // allowlist could otherwise navigate the owner's private files.
            if !is_owner {
                ChannelCommand::UnknownCommand(
                    "🔒 `/cd` is restricted to the bot owner.".to_string(),
                )
            } else {
                let path_arg = cmd.strip_prefix("/cd").unwrap_or("").trim();
                ChannelCommand::ChangeDir(
                    format_cd_browser(path_arg, session_id, session_svc).await,
                )
            }
        }
        cmd if cmd == "/architecture" || cmd.starts_with("/architecture ") => {
            // Owner-only: /architecture exposes the host filesystem layout,
            // the same class of surface as /cd (#933).
            if !is_owner {
                ChannelCommand::UnknownCommand(
                    "🔒 `/architecture` is restricted to the bot owner.".to_string(),
                )
            } else {
                let path_arg = cmd.strip_prefix("/architecture").unwrap_or("").trim();
                ChannelCommand::Architecture(if path_arg.is_empty() {
                    None
                } else {
                    Some(path_arg.to_string())
                })
            }
        }
        cmd if cmd == "/attach" || cmd.starts_with("/attach ") => {
            // Owner-only: /attach pulls server-side files toward the model's
            // reach, the same trust class as /cd and /architecture (#933).
            if !is_owner {
                ChannelCommand::UnknownCommand(
                    "🔒 `/attach` is restricted to the bot owner.".to_string(),
                )
            } else {
                let args = cmd.strip_prefix("/attach").unwrap_or("").trim();
                ChannelCommand::Attach(run_attach_channel(args, session_id, agent).await)
            }
        }
        cmd if cmd == "/audit" || cmd.starts_with("/audit ") => {
            // Owner-only (#1705): audit rows carry session ids, tool targets
            // (file paths, queries) and turn verdicts — the same trust class
            // as /usage breakdowns, not for arbitrary channel members.
            if !is_owner {
                ChannelCommand::UnknownCommand(
                    "🔒 `/audit` is restricted to the bot owner.".to_string(),
                )
            } else {
                let args = cmd.strip_prefix("/audit").unwrap_or("").trim();
                let n = args
                    .split_whitespace()
                    .next()
                    .and_then(|a| a.parse::<u32>().ok());
                ChannelCommand::Audit(run_audit_channel(n, agent).await)
            }
        }
        "/profiles" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::Profiles(format_profiles_browser().await)
            }
        }
        cmd if cmd == "/respond_to" || cmd.starts_with("/respond_to ") => {
            if !is_owner {
                ChannelCommand::UnknownCommand(
                    "🔒 `/respond_to` is restricted to the bot owner.".to_string(),
                )
            } else {
                let arg = cmd.strip_prefix("/respond_to").unwrap_or("").trim();
                ChannelCommand::RespondTo(handle_respond_to(arg, chat_id).await)
            }
        }
        cmd if cmd == "/redact" || cmd.starts_with("/redact ") => {
            if !is_owner {
                ChannelCommand::UnknownCommand(
                    "🔒 `/redact` is restricted to the bot owner.".to_string(),
                )
            } else {
                let arg = cmd.strip_prefix("/redact").unwrap_or("").trim();
                ChannelCommand::Redact(handle_redact(arg).await)
            }
        }
        // `/goal` works on every surface. The TUI intercepts it in
        // handle_slash_command; here we mirror that so Telegram/Discord/
        // WhatsApp/Slack users hit the same behaviour. A bare `/goal` is
        // DENIED: the usage warning is shown for display only via
        // UnknownCommand, which renders the text but returns None for
        // history — so a mistyped/abandoned /goal never persists and leaves
        // the conversation context untouched. Only a real `/goal <text>` is
        // forwarded as a directive that calls the slash_command tool.
        cmd if cmd == "/goal" || cmd.starts_with("/goal ") => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                let args = cmd.strip_prefix("/goal").unwrap_or("");
                if crate::brain::goal::is_bare(args) {
                    ChannelCommand::UnknownCommand(crate::brain::goal::goal_usage_warning())
                } else {
                    ChannelCommand::UserPrompt(crate::brain::goal::goal_command_prompt(args))
                }
            }
        }
        // Telegram registers the hyphen-free `mission_control` (its command
        // names allow no hyphens), so a menu tap sends `/mission_control`;
        // accept both that and the canonical typed `/mission-control`.
        "/mission-control" | "/mission_control" => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                ChannelCommand::MissionControl(format_mission_control(agent).await)
            }
        }
        cmd if cmd.starts_with("/rename ") => {
            if !is_owner {
                ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string())
            } else {
                let title = cmd.strip_prefix("/rename ").unwrap_or("").trim();
                if title.is_empty() {
                    ChannelCommand::Rename("Usage: `/rename <new title>`".to_string())
                } else {
                    match session_svc
                        .update_session_title(session_id, Some(title.to_string()))
                        .await
                    {
                        Ok(()) => {
                            ChannelCommand::Rename(format!("✅ Session renamed to: `{}`", title))
                        }
                        Err(e) => ChannelCommand::Rename(format!("⚠️ Failed to rename: {}", e)),
                    }
                }
            }
        }
        _ if trimmed.starts_with('/') && !crate::utils::string::looks_like_file_path(trimmed) => {
            match_user_command(trimmed, is_owner)
        }
        _ => ChannelCommand::NotACommand,
    };

    // Record skill activation so the full body survives compaction (#219).
    // When a skill is invoked (returns UserPrompt), its name is tracked on
    // the AgentService so the tool loop can re-inject the full skill body
    // into the system prompt after compaction.
    if matches!(&result, ChannelCommand::UserPrompt(_)) && trimmed.starts_with('/') {
        let (cmd_name, _) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let key = norm_command_key(cmd_name);
        // Only register as skill if no user-defined command took priority
        let loader = crate::brain::CommandLoader::from_brain_path(
            &crate::brain::BrainLoader::resolve_path(),
        );
        let user_commands = loader.load();
        if !user_commands
            .iter()
            .any(|c| norm_command_key(&c.name) == key)
        {
            let skills = crate::brain::skills::load_all_skills();
            if let Some(skill) = skills
                .iter()
                .find(|s| norm_command_key(&s.slash_name) == key)
            {
                agent.register_active_skill(session_id, &skill.slash_name);
            }
        }
    }

    // Persist command + response to session history
    let response_text = match &result {
        ChannelCommand::Help(body)
        | ChannelCommand::Usage(body)
        | ChannelCommand::MissionControl(body)
        | ChannelCommand::Rename(body) => Some(body.clone()),
        ChannelCommand::Models(resp) => Some(resp.text.clone()),
        ChannelCommand::Sessions(resp) => Some(resp.text.clone()),
        ChannelCommand::NewSession => Some("New session started.".to_string()),
        ChannelCommand::Stop => Some("Operation stopped.".to_string()),
        // The receipt names the target and its state, so it is built by the
        // channel handler that actually holds the state (#2108).
        ChannelCommand::StopTarget(_) => None,
        ChannelCommand::UserSystem(body) => Some(body.clone()),
        ChannelCommand::Doctor => Some("Running health check...".to_string()),
        ChannelCommand::Architecture(_) => Some("Building directory tree...".to_string()),
        ChannelCommand::Attach(body) => Some(body.clone()),
        ChannelCommand::Audit(body) => Some(body.clone()),
        ChannelCommand::Evolve => Some("Checking for updates...".to_string()),
        ChannelCommand::Rtk(body) => Some(body.clone()),
        ChannelCommand::ModelSwitched(body) => Some(body.clone()),
        ChannelCommand::ChangeDir(resp) => Some(resp.text.clone()),
        ChannelCommand::Profiles(resp) => Some(resp.text.clone()),
        // No acknowledgement: these announce themselves and then go down, so a
        // separate ack would be a second message racing the shutdown.
        ChannelCommand::Restart
        | ChannelCommand::Exit
        | ChannelCommand::Compact
        // The channel handler writes the marker and replies with the receipt.
        | ChannelCommand::ClearContext
        | ChannelCommand::UserPrompt(_)
        | ChannelCommand::PlanModeWithQuery(_)
        | ChannelCommand::NotACommand
        | ChannelCommand::UnknownCommand(_) => None,
        ChannelCommand::RespondTo(body) => Some(body.clone()),
        ChannelCommand::Redact(body) => Some(body.clone()),
        ChannelCommand::PlanMode(body) | ChannelCommand::ShowPlan(body) => Some(body.clone()),
        // Handled (and persisted) by the channel handler: busy check +
        // seed dispatch / cancel + cleanup happen there.
        ChannelCommand::ExecutePlan | ChannelCommand::DiscardPlan => None,
    };

    if let Some(response) = response_text {
        persist_command_to_history(agent, session_id, trimmed, &response).await;
    }

    result
}

/// Save the user command and bot response to session message history,
/// then notify TUI so it refreshes live.
async fn persist_command_to_history(
    agent: &AgentService,
    session_id: Uuid,
    command: &str,
    response: &str,
) {
    let msg_svc = crate::services::MessageService::new(agent.context().clone());
    if let Err(e) = msg_svc
        .create_message(session_id, "user".to_string(), command.to_string())
        .await
    {
        tracing::warn!("Failed to persist channel command to history: {}", e);
    }
    if let Err(e) = msg_svc
        .create_message(session_id, "assistant".to_string(), response.to_string())
        .await
    {
        tracing::warn!(
            "Failed to persist channel command response to history: {}",
            e
        );
    }
    // Notify TUI to reload session messages (same mechanism as agent responses)
    if let Some(tx) = agent.session_updated_tx() {
        let _ = tx.send(crate::brain::agent::ChannelSessionEvent::Updated(
            session_id,
        ));
    }
}

// ── User-defined commands ───────────────────────────────────────────────────

/// Normalize a slash-command token for matching: lowercase, strip a leading
/// `/`, and fold `-` to `_`. Telegram's command menu only allows `[a-z0-9_]`,
/// so `/x-engage` is registered (and tapped) as `/x_engage`; folding both to
/// the same key lets the menu entry, the dash form, and direct typing all match
/// the same definition.
fn norm_command_key(s: &str) -> String {
    s.trim_start_matches('/').to_lowercase().replace('-', "_")
}

fn match_user_command(text: &str, is_owner: bool) -> ChannelCommand {
    let brain_path = crate::brain::BrainLoader::resolve_path();
    let loader = crate::brain::CommandLoader::from_brain_path(&brain_path);
    let commands = loader.load();
    let skills = crate::brain::skills::load_all_skills();
    gate_user_command(match_user_command_inner(text, &commands, &skills), is_owner)
}

/// Refuse a matched user command or skill for a non-owner (#975).
///
/// User-defined commands and skills drive the agent, and with
/// `action="system"` even canned UI text, on channels where many non-owner
/// users are allowlisted. Every built-in command is individually owner-gated;
/// the catch-all arm that reaches `commands.toml` and skill slugs was not, so
/// any allowlisted group member could invoke any installed skill and have its
/// body executed under the session's approval policy.
///
/// Gates AFTER matching, never before. An unmatched slash keeps the ordinary
/// "Unknown command" reply, so the refusal itself never reveals which commands
/// or skills exist. The refusal string is the one the built-ins use, so a
/// non-owner cannot distinguish a gated user command from a gated built-in.
///
/// Split out from [`match_user_command`], which loads from disk and cannot be
/// exercised in a test — the reason this path shipped with the gate missing and
/// stayed untested afterwards.
pub(crate) fn gate_user_command(matched: ChannelCommand, is_owner: bool) -> ChannelCommand {
    if !is_owner
        && matches!(
            matched,
            ChannelCommand::UserPrompt(_) | ChannelCommand::UserSystem(_)
        )
    {
        return ChannelCommand::UnknownCommand("🔒 Owner-only command.".to_string());
    }
    matched
}

pub(crate) fn match_user_command_inner(
    text: &str,
    commands: &[crate::brain::commands::UserCommand],
    skills: &[crate::brain::skills::Skill],
) -> ChannelCommand {
    // Split "/command args" into command name and optional args
    let (cmd_name, args) = text
        .split_once(' ')
        .map(|(c, a)| (c, a.trim()))
        .unwrap_or((text, ""));

    // Telegram's bot-command menu can only contain `[a-z0-9_]`, so a command
    // defined as `/x-engage` is registered (and tapped) as `/x_engage`. Match
    // on a normalized key — lowercased, leading slash stripped, dashes folded to
    // underscores — so the menu entry, the dash form, and direct typing all hit
    // the same definition.
    let key = norm_command_key(cmd_name);

    // 1. Explicit user-defined commands win — they're how a user overrides
    //    a built-in skill (rename, retarget, swap to action=system, etc.).
    if let Some(cmd) = commands.iter().find(|c| norm_command_key(&c.name) == key) {
        let prompt = if args.is_empty() {
            cmd.prompt.clone()
        } else {
            format!("{} {}", cmd.prompt, args)
        };
        return match cmd.action.as_str() {
            "system" => ChannelCommand::UserSystem(prompt),
            _ => ChannelCommand::UserPrompt(prompt),
        };
    }

    // 2. Auto-registered skills — `/<name>` matches a SKILL.md slug, the body
    //    becomes the prompt. Args (anything after the first space) are
    //    appended so callers can pass extra context without writing a
    //    custom commands.toml wrapper.
    if let Some(skill) = skills
        .iter()
        .find(|s| norm_command_key(&s.slash_name) == key)
    {
        // `prompt_body()` prepends the review-gate reminder when the skill
        // declares `review_gate: true`: slash invocation is the user asking
        // to review the output before any side effects.
        let base = skill.prompt_body();
        let prompt = if args.is_empty() {
            base
        } else {
            format!("{base}\n\n{args}")
        };
        return ChannelCommand::UserPrompt(prompt);
    }

    // 3. Unknown slash command — warn user, don't forward to agent
    ChannelCommand::UnknownCommand(format!(
        "⚡ Unknown command: {}. Type /help for available commands.",
        cmd_name
    ))
}

// ── /help ───────────────────────────────────────────────────────────────────

pub(crate) fn format_help() -> String {
    let builtins: &[(&str, &str)] = &[
        ("/new", "Start a new session"),
        ("/cd", "Browse and change working directory"),
        (
            "/sessions",
            "Switch between sessions (`/sessions:<query>` to filter)",
        ),
        (
            "/stop",
            "Abort current operation (`/stop <id>` targets another session)",
        ),
        (
            "/architecture",
            "Directory tree of a path (mechanical, no API cost)",
        ),
        (
            "/attach",
            "Attach docs file(s) to the session (.md or docs/; mechanical, no API cost)",
        ),
        (
            "/audit",
            "Audit trail viewer: ACTION + READ + OUTCOME rows (mechanical, no API cost)",
        ),
        (
            "/clear",
            "Clear context here at no cost; history and title stay",
        ),
        ("/compact", "Compact context (summarize & trim)"),
        (
            "/cowork",
            "Create a cowork workspace with QR invite (Telegram only)",
        ),
        ("/discard", "Discard the live plan (back to no plan)"),
        ("/evolve", "Download latest release & restart"),
        (
            "/execute",
            "Approve the design plan / retry the checklist seed",
        ),
        ("/exit", "Exit OpenCrabs"),
        (
            "/goal",
            "Set/track an autonomous goal (`/goal <text>`, status, pause, resume, clear)",
        ),
        ("/help", "Show this message"),
        (
            "/mission-control",
            "Mission control: analytics, activity, inbox & schedule",
        ),
        ("/models", "Switch AI model"),
        ("/plan", "Enter Plan mode (design a plan for approval)"),
        ("/profiles", "Manage profiles (create, switch, migrate)"),
        (
            "/redact",
            "Show/switch scoped secret redaction (`/redact <global|group|dm> <on|off>`)",
        ),
        ("/rename", "Rename current session (`/rename <new title>`)"),
        (
            "/respond_to",
            "Show/switch auto-mention mode (`/respond_to <all|mention|auto>`)",
        ),
        ("/restart", "Restart OpenCrabs"),
        ("/rtk", "Show RTK token savings statistics"),
        ("/show-plan", "Show the current plan state"),
        (
            "/usage",
            "Token & cost stats (`provider [name]` / `model <name>` / `7d`)",
        ),
    ];
    let rows: Vec<Vec<String>> = builtins
        .iter()
        .map(|(cmd, desc)| vec![format!("`{cmd}`"), desc.to_string()])
        .collect();

    // Heading + table. When Telegram rich rendering is on this becomes a
    // command/description table (a key-value list on a phone); the legacy
    // path still renders it readably. `\n\n` around blocks so the markdown
    // parser keeps the table separate from the heading and trailer.
    let mut out = format!(
        "# 📖 Available Commands\n\nOpenCrabs v{}\n\n{}",
        env!("CARGO_PKG_VERSION"),
        md_table(&["Command", "Description"], &rows)
    );

    // Append user-defined commands from commands.toml
    let brain_path = crate::brain::BrainLoader::resolve_path();
    let loader = crate::brain::CommandLoader::from_brain_path(&brain_path);
    let mut user_cmds = loader.load();
    if !user_cmds.is_empty() {
        user_cmds.sort_by(|a, b| a.name.cmp(&b.name));
        let rows: Vec<Vec<String>> = user_cmds
            .iter()
            .map(|c| vec![format!("`{}`", c.name), c.description.clone()])
            .collect();
        out.push_str(&format!(
            "\n## 📌 Custom Commands\n\n{}",
            md_table(&["Command", "Description"], &rows)
        ));
    }

    out.push_str("\n🦀 Any other message is sent to OpenCrabs. 🦀");
    out
}

pub(crate) use crate::utils::string::md_table;

// ── /rtk ────────────────────────────────────────────────────────────────────

#[cfg(feature = "rtk")]
async fn format_rtk() -> String {
    if !crate::rtk::is_rtk_available().await {
        return crate::rtk::RTK_NOT_INSTALLED_HELP.to_string();
    }
    match tokio::process::Command::new("rtk")
        .arg("gain")
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if output.status.success() {
                format!("📊 *RTK Token Savings:*\n\n```\n{}\n```", stdout.trim())
            } else {
                format!("⚠️ RTK gain command failed:\n\n```\n{}\n```", stderr.trim())
            }
        }
        Err(e) => {
            format!("⚠️ Failed to run rtk gain: {}", e)
        }
    }
}

#[cfg(not(feature = "rtk"))]
async fn format_rtk() -> String {
    "⚠️ RTK feature is not enabled. Rebuild with --features rtk to enable token savings tracking."
        .to_string()
}

// ── /usage ──────────────────────────────────────────────────────────────────

/// Build the `/mission-control` report (analytics, activity feed, inbox proposals,
/// schedule) as Markdown. Same data as the TUI Mission Control view, exposed to
/// channels as a single command.
async fn format_mission_control(agent: &AgentService) -> String {
    use crate::brain::mission_control::{
        TimeWindow, activity_service, analytics_service, inbox_service, schedule_service,
    };
    use crate::brain::tools::mission_control_report::render_markdown;

    let pool = agent.context().pool();
    let analytics = analytics_service::summary(pool.clone(), TimeWindow::All).await;
    let activity = activity_service::recent(5);
    let inbox = inbox_service::list();
    let schedule = schedule_service::list(pool.clone()).await;

    render_markdown(&analytics, &activity, &inbox, &schedule)
}

/// `/usage <args>` breakdowns (#402): by provider, by model, with an
/// optional trailing time filter.
///
///   /usage provider [name]     totals per provider (optionally prefix-filtered)
///   /usage model <name>        which providers served this model
///   /usage period 7d|30d|24h   provider totals within the window
///   any form accepts a trailing period token, e.g. `/usage provider nvidia 7d`
async fn format_usage_breakdown(args: &str, session_svc: &SessionService) -> String {
    use crate::db::repository::UsageLedgerRepository;
    use crate::usage::data::{fmt_cost, fmt_tokens};

    fn parse_period(tok: &str) -> Option<i64> {
        let now = chrono::Utc::now().timestamp();
        // The unit is the final character, which may be multi-byte. Slicing at
        // `len - 1` would land inside it and panic.
        let (last, _) = tok.char_indices().next_back()?;
        let (num, unit) = tok.split_at(last);
        let n: i64 = num.parse().ok()?;
        match unit {
            "d" => Some(now - n * 86_400),
            "h" => Some(now - n * 3_600),
            "w" => Some(now - n * 7 * 86_400),
            _ => None,
        }
    }

    let mut words: Vec<&str> = args.split_whitespace().collect();
    // A trailing period token applies to any form.
    let mut since: Option<i64> = None;
    if let Some(last) = words.last()
        && let Some(s) = parse_period(last)
    {
        since = Some(s);
        words.pop();
    }
    let window = since.map_or("All-Time".to_string(), |_| {
        format!("last {}", args.split_whitespace().last().unwrap_or(""))
    });

    let ledger = UsageLedgerRepository::new(session_svc.pool());
    match words.as_slice() {
        ["provider"] | ["providers"] | [] => match ledger.by_provider(since, None).await {
            Ok(rows) if rows.is_empty() => format!("No usage recorded ({window})."),
            Ok(rows) => {
                let mut out = format!("# 📊 Usage by Provider ({window})\n\n");
                out.push_str("| Provider | Tokens | Cost |\n|---|---|---|\n");
                for (p, t, c) in rows {
                    out.push_str(&format!(
                        "| {} | {} | {} |\n",
                        p,
                        fmt_tokens(t),
                        fmt_cost(c)
                    ));
                }
                out
            }
            Err(e) => format!("⚠️ Failed to load provider breakdown: {e}"),
        },
        ["provider", name] | ["providers", name] => {
            match ledger.by_provider_model(since, Some(name), None).await {
                Ok(rows) if rows.is_empty() => {
                    format!("No usage recorded for providers matching `{name}` ({window}).")
                }
                Ok(rows) => {
                    let mut out = format!("# 📊 Usage for providers `{name}*` ({window})\n\n");
                    out.push_str("| Provider | Model | Tokens | Cost |\n|---|---|---|---|\n");
                    for (p, m, t, c) in rows {
                        out.push_str(&format!(
                            "| {} | {} | {} | {} |\n",
                            p,
                            m,
                            fmt_tokens(t),
                            fmt_cost(c)
                        ));
                    }
                    out
                }
                Err(e) => format!("⚠️ Failed to load provider breakdown: {e}"),
            }
        }
        ["model", name] => match ledger.by_provider_model(since, None, Some(name)).await {
            Ok(rows) if rows.is_empty() => {
                format!("No usage recorded for model `{name}` ({window}).")
            }
            Ok(rows) => {
                let mut out = format!("# 📊 Providers serving `{name}` ({window})\n\n");
                out.push_str("| Provider | Tokens | Cost |\n|---|---|---|\n");
                for (p, _m, t, c) in rows {
                    out.push_str(&format!(
                        "| {} | {} | {} |\n",
                        p,
                        fmt_tokens(t),
                        fmt_cost(c)
                    ));
                }
                out
            }
            Err(e) => format!("⚠️ Failed to load model breakdown: {e}"),
        },
        ["period"] => "Usage: `/usage period 7d` (units: h, d, w).".to_string(),
        _ => "Usage: `/usage provider [name]` · `/usage model <name>` · \
              `/usage period 7d|30d` · trailing period works on any form, \
              e.g. `/usage provider nvidia 7d`."
            .to_string(),
    }
}

async fn format_usage(
    session_id: Uuid,
    agent: &AgentService,
    session_svc: &SessionService,
) -> String {
    use crate::usage::data::{DashboardData, Period, fmt_cost, fmt_tokens};

    // Each entry is a markdown block (heading, paragraph, or `### head`+table).
    // Joined with blank lines so the rich renderer parses them as separate
    // blocks — headings become bold, tables become phone-friendly grids /
    // key-value lists. Authored once; renders well under rich or legacy mode.
    let mut blocks: Vec<String> = vec![format!(
        "# 📊 Usage Dashboard\n\nOpenCrabs v{}",
        env!("CARGO_PKG_VERSION")
    )];

    // ── Current session ──────────────────────────────────────────────
    let current_model = agent.provider_model();
    match session_svc.get_session(session_id).await {
        Ok(Some(session)) => {
            let name = session.title.as_deref().unwrap_or("Current Session");
            let model = session
                .model
                .as_deref()
                .filter(|m| !m.is_empty())
                .unwrap_or(&current_model);
            let tokens = session.token_count;
            let cost = if session.total_cost > 0.0 {
                session.total_cost
            } else if tokens > 0 {
                estimate_cost(model, tokens)
            } else {
                0.0
            };
            blocks.push(format!(
                "**Current:** {} — `{}` · {} tok · ${:.4}",
                name,
                model,
                format_number(tokens),
                cost
            ));
        }
        _ => {
            blocks.push("**Current:** (session not found)".to_string());
        }
    }

    // ── Period sections (Today + All-Time) ───────────────────────────
    // TUI /usage lets the user cycle T/W/M/A — for the channel dump we show
    // Today and All-Time, the two most useful snapshots. Each is a section
    // heading + a summary line + a table per breakdown.
    for period in [Period::Today, Period::AllTime] {
        let pool = session_svc.pool();
        let data = match DashboardData::fetch(&pool, period).await {
            Ok(d) => d,
            Err(e) => {
                blocks.push(format!("## {}\n\n(failed to load: {})", period.label(), e));
                continue;
            }
        };

        blocks.push(format!("## {}", period.label()));
        blocks.push(format!(
            "{} tok · {} · {} sessions · {} calls",
            fmt_tokens(data.summary.total_tokens),
            fmt_cost(data.summary.total_cost),
            format_number(data.summary.session_count),
            format_number(data.summary.call_count),
        ));

        // Cache efficiency — parity with the TUI dashboard's Cache card. Only
        // shown when there were caching-capable requests in the window.
        if let Some(cache) = &data.cache
            && cache.total_input_tokens > 0
        {
            blocks.push(format!(
                "💾 Cache: {:.0}% hit · {} of {} input cached",
                cache.cache_hit_pct,
                fmt_tokens(cache.cached_tokens),
                fmt_tokens(cache.total_input_tokens),
            ));

            // Per-model cache breakdown (top 5 by hit rate, matching TUI card).
            if !cache.per_model.is_empty() {
                let rows: Vec<Vec<String>> = cache
                    .per_model
                    .iter()
                    .take(5)
                    .map(|ms| {
                        let name: String = ms.model.chars().take(24).collect();
                        vec![
                            format!("`{name}`"),
                            format!("{:.0}%", ms.cache_hit_pct),
                            fmt_tokens(ms.cached_tokens),
                        ]
                    })
                    .collect();
                blocks.push(format!(
                    "### Cache by Model\n\n{}",
                    md_table(&["Model", "Hit %", "Cached"], &rows).trim_end()
                ));
            }
        }

        let section = |title: &str, headers: &[&str], rows: Vec<Vec<String>>| -> Option<String> {
            if rows.is_empty() {
                None
            } else {
                Some(format!(
                    "### {title}\n\n{}",
                    md_table(headers, &rows).trim_end()
                ))
            }
        };

        if period != Period::Today {
            // Last 7 days of the window, oldest-first.
            let window: Vec<_> = data.daily.iter().rev().take(7).collect();
            let rows: Vec<Vec<String>> = window
                .iter()
                .rev()
                .map(|d| vec![d.date.clone(), fmt_tokens(d.tokens), fmt_cost(d.cost)])
                .collect();
            blocks.extend(section("Daily", &["Date", "Tokens", "Cost"], rows));
        }

        let model_rows: Vec<Vec<String>> = data
            .models
            .iter()
            .take(5)
            .map(|m| {
                vec![
                    format!("`{}`", m.model),
                    fmt_tokens(m.tokens),
                    format!(
                        "{}{}",
                        fmt_cost(m.cost),
                        if m.estimated { " ~" } else { "" }
                    ),
                ]
            })
            .collect();
        blocks.extend(section(
            "By Model",
            &["Model", "Tokens", "Cost"],
            model_rows,
        ));

        let tool_rows: Vec<Vec<String>> = data
            .tools
            .iter()
            .take(5)
            .map(|t| vec![format!("`{}`", t.tool_name), format_number(t.call_count)])
            .collect();
        blocks.extend(section("Core Tools", &["Tool", "Calls"], tool_rows));

        let project_rows: Vec<Vec<String>> = data
            .projects
            .iter()
            .take(5)
            .map(|p| {
                vec![
                    format!("`{}`", p.project),
                    fmt_cost(p.cost),
                    p.sessions.to_string(),
                ]
            })
            .collect();
        blocks.extend(section(
            "By Project",
            &["Project", "Cost", "Sessions"],
            project_rows,
        ));

        let activity_rows: Vec<Vec<String>> = data
            .activities
            .iter()
            .take(5)
            .map(|a| {
                vec![
                    a.category.clone(),
                    fmt_cost(a.cost),
                    a.turns.to_string(),
                    format!("{:.0}%", a.one_shot_pct),
                ]
            })
            .collect();
        blocks.extend(section(
            "By Activity",
            &["Activity", "Cost", "Turns", "1-shot"],
            activity_rows,
        ));
    }

    // Decision reuse (#1648 PR3): per-tier shadow/live counters. Silent
    // until the feature has actually measured something.
    {
        use crate::db::repository::{DecisionCacheRepository, DecisionStatsRepository};
        let pool = session_svc.pool();
        let stats = DecisionStatsRepository::new(pool.clone())
            .all()
            .await
            .unwrap_or_default();
        let cached_rows = DecisionCacheRepository::new(pool)
            .count_by_tier()
            .await
            .unwrap_or_default();
        let cfg = crate::config::Config::current();
        if let Some(block) =
            crate::decisions::report::render_usage_block(&stats, &cached_rows, &cfg.decisions.tiers)
        {
            blocks.push(block);
        }
    }

    blocks.join("\n\n")
}

fn estimate_cost(model: &str, token_count: i64) -> f64 {
    crate::usage::pricing::PricingConfig::load()
        .map(|cfg| cfg.estimate_cost(model, token_count))
        .unwrap_or(0.0)
}

pub(crate) fn format_number(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ── /cd directory browser ──────────────────────────────────────────────────

/// Number of entries per page in the directory browser
const CD_PAGE_SIZE: usize = 6;

/// Directories and file names to skip in the browser (auto-hidden)
const CD_SKIP_DIRS: &[&str] = &["node_modules", ".git", "target", ".DS_Store", "__pycache__"];

/// Build a directory listing for the `/cd` command.
///
/// - `path_arg` is the raw text after `/cd` (may be empty → use session WD or home).
/// - Results are sorted: directories first, then files, both alphabetical.
/// - Hidden dotfiles are excluded unless the filter starts with `.`.
/// - Returns page 0 of the listing.
pub(crate) async fn format_cd_browser(
    path_arg: &str,
    session_id: Uuid,
    session_svc: &SessionService,
) -> DirBrowserResponse {
    // Resolve the target path
    let target = if path_arg.is_empty() {
        // Try session working directory, fall back to home
        session_svc
            .get_session(session_id)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.working_directory)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")))
    } else {
        let expanded = crate::brain::tools::error::expand_tilde(path_arg);
        if expanded.is_dir() {
            expanded
        } else {
            // Try as partial path — go to parent and filter
            return DirBrowserResponse {
                current_path: path_arg.to_string(),
                entries: vec![],
                page: 0,
                total_pages: 0,
                filter: None,
                total_entries: 0,
                text: format!("❌ Not a directory: {}", path_arg),
            };
        }
    };

    let canonical = target.canonicalize().unwrap_or(target);
    let (entries, err) = read_dir_entries(&canonical, None);
    let total = entries.len();
    let total_pages = total.div_ceil(CD_PAGE_SIZE).max(1);
    let page_entries: Vec<DirBrowserEntry> = entries.into_iter().take(CD_PAGE_SIZE).collect();

    let mut text_lines = vec![format!("📂 *{}*", canonical.display())];
    if let Some(e) = &err {
        text_lines.push(format!("⚠️ {}", e));
    }
    if total == 0 && err.is_none() {
        text_lines.push("(empty directory)".to_string());
    } else if total > 0 {
        text_lines.push(format!(
            "{} item{} · Page 1/{}",
            total,
            if total == 1 { "" } else { "s" },
            total_pages
        ));
    }

    DirBrowserResponse {
        current_path: canonical.to_string_lossy().to_string(),
        entries: page_entries,
        page: 0,
        total_pages,
        filter: None,
        total_entries: total,
        text: text_lines.join("\n"),
    }
}

/// Maximum directory depth to prevent symlink cycles
const CD_MAX_DEPTH: usize = 30;

/// Read directory entries, sorted dirs-first then alphabetical, skipping
/// hidden dotfiles and known junk dirs. Optional filter applies substring
/// match on filename.
///
/// Returns `(entries, error_message)`. If the directory can't be read,
/// `error_message` contains the reason (e.g. "Permission denied").
pub(crate) fn read_dir_entries(
    dir: &std::path::Path,
    filter: Option<&str>,
) -> (Vec<DirBrowserEntry>, Option<String>) {
    let mut error_msg: Option<String> = None;
    let mut entries: Vec<(String, bool)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                // Skip hidden unless filter starts with '.'
                if name.starts_with('.') && !filter.is_some_and(|f| f.starts_with('.')) {
                    return None;
                }
                // Skip known junk dirs
                if CD_SKIP_DIRS.contains(&name.as_str()) {
                    return None;
                }
                // Skip symlinks to prevent cycles
                let ft = match e.file_type() {
                    Ok(ft) => ft,
                    Err(_) => return None,
                };
                if ft.is_symlink() {
                    return None;
                }
                Some((name, ft.is_dir()))
            })
            .collect(),
        Err(e) => {
            error_msg = Some(format!("Cannot read directory: {}", e));
            vec![]
        }
    };

    // Apply filter
    if let Some(f) = filter {
        let f_lower = f.to_lowercase();
        entries.retain(|(name, _)| name.to_lowercase().contains(&f_lower));
    }

    // Sort: directories first, then alphabetical
    entries.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
    });

    let result: Vec<DirBrowserEntry> = entries
        .into_iter()
        .enumerate()
        .map(|(i, (name, is_dir))| DirBrowserEntry {
            name,
            is_dir,
            index: i,
        })
        .collect();
    (result, error_msg)
}

/// Rebuild a `DirBrowserResponse` for a specific page and optional filter.
/// Used by the callback handler after the user navigates or filters.
pub fn rebuild_cd_browser(path: &str, page: usize, filter: Option<&str>) -> DirBrowserResponse {
    let dir = std::path::PathBuf::from(path);
    let depth = path.matches('/').count();
    if depth > CD_MAX_DEPTH {
        return DirBrowserResponse {
            current_path: path.to_string(),
            entries: vec![],
            page: 0,
            total_pages: 0,
            filter: filter.map(str::to_string),
            total_entries: 0,
            text: format!(
                "⚠️ Directory too deep ({} levels). Possible symlink cycle.",
                depth
            ),
        };
    }
    let (entries, err) = read_dir_entries(&dir, filter);
    let total = entries.len();
    let total_pages = total.div_ceil(CD_PAGE_SIZE).max(1);
    let page = page.min(total_pages.saturating_sub(1));
    let start = page * CD_PAGE_SIZE;
    let page_entries: Vec<DirBrowserEntry> =
        entries.into_iter().skip(start).take(CD_PAGE_SIZE).collect();

    let mut text_lines = vec![format!("📂 *{}*", dir.display())];
    if let Some(f) = filter {
        text_lines.push(format!("🔍 Filter: `{}`", f));
    }
    if let Some(e) = &err {
        text_lines.push(format!("⚠️ {}", e));
    }
    if total == 0 && err.is_none() {
        text_lines.push("(no matches)".to_string());
    } else if total > 0 {
        text_lines.push(format!(
            "{} item{} · Page {}/{}",
            total,
            if total == 1 { "" } else { "s" },
            page + 1,
            total_pages
        ));
    }

    DirBrowserResponse {
        current_path: path.to_string(),
        entries: page_entries,
        page,
        total_pages,
        filter: filter.map(str::to_string),
        total_entries: total,
        text: text_lines.join("\n"),
    }
}

// ── /profiles ───────────────────────────────────────────────────────────────

/// Build a profiles listing for the `/profiles` command.
pub(crate) async fn format_profiles_browser() -> ProfilesResponse {
    let active = crate::config::profile::active_profile().unwrap_or("default");
    let profiles = crate::config::profile::list_profiles().unwrap_or_default();

    let mut entries = Vec::new();
    let mut text_lines = vec!["👤 *Profiles*".to_string(), String::new()];

    for p in &profiles {
        let is_active = p.name == active;
        let is_default = p.name == "default";
        let marker = if is_active { "▸ " } else { "• " };
        let desc = p.description.as_deref().unwrap_or("");
        let suffix = if is_active { " ✓" } else { "" };

        text_lines.push(format!(
            "{}`{}`{} {}",
            marker,
            p.name,
            suffix,
            if desc.is_empty() {
                String::new()
            } else {
                format!("— {}", desc)
            }
        ));

        entries.push(ProfileBrowserEntry {
            name: p.name.clone(),
            description: p.description.clone(),
            is_active,
            is_default,
            created_at: p.created_at.clone(),
            last_used: p.last_used.clone(),
        });
    }

    if entries.is_empty() {
        text_lines.push("No profiles found.".to_string());
    }

    text_lines.push(String::new());
    text_lines.push("Tap a profile for details. Use buttons below to manage.".to_string());

    ProfilesResponse {
        active_profile: active.to_string(),
        entries,
        text: text_lines.join("\n"),
    }
}

// ── /sessions ──────────────────────────────────────────────────────────────

async fn format_sessions(
    current_session_id: Uuid,
    session_svc: &SessionService,
    query: Option<&str>,
) -> SessionsResponse {
    let sessions = session_svc
        .list_sessions(SessionListOptions {
            include_archived: false,
            limit: Some(10),
            offset: 0,
            query: query.map(str::to_string),
            include_subagents: false,
        })
        .await
        .unwrap_or_default();

    // Issue #129: the body used to enumerate every session — same list
    // the inline keyboard renders directly below as tappable buttons.
    // Pure duplication. Strip the body to a header + a one-line current
    // indicator and let the buttons carry the per-session labels.
    let mut text_lines = vec!["📂 *Sessions*".to_string()];
    if let Some(q) = query {
        text_lines.push(format!("Filter: `{}`", q));
    }
    let mut items = Vec::new();

    let current = sessions.iter().find(|s| s.id == current_session_id);
    if let Some(s) = current {
        let title = s.title.as_deref().unwrap_or("Untitled");
        text_lines.push(format!("Current: `{}`", title));
    }
    text_lines.push(String::new());

    for s in &sessions {
        let title = s.title.as_deref().unwrap_or("Untitled");
        let date = s.updated_at.format("%b %d %H:%M");
        let label = format!("{} ({})", title, date);
        items.push((s.id, label));
    }

    if sessions.is_empty() {
        text_lines.push("No sessions found.".to_string());
    }

    SessionsResponse {
        current_session_id,
        sessions: items,
        text: text_lines.join("\n"),
    }
}

// ── /models ─────────────────────────────────────────────────────────────────

fn format_providers(agent: &AgentService, session_id: Uuid) -> ProvidersResponse {
    // Use the session's ACTUAL current provider/model, not the global config.
    // Channel model switches call agent.swap_provider_for_session() without touching config,
    // so reading from Config::load() or agent.provider_name() shows stale data after a channel switch.
    let current_provider = agent.provider_name_for_session(session_id);
    let current_model = agent.provider_model_for_session(session_id);

    let providers = all_known_providers_with_status_loaded();

    // Body carries heading + status + hint only (#1149): every channel renders
    // one button per provider right below, using the same ✓/•/🔒 semantics via
    // `provider_marker`, so enumerating the list in text too was pure
    // duplication — identical to the cleanup #129 gave /sessions.
    let text = format!(
        "🤖 *Switch Provider*\n\n\
         Current: `{}` / `{}`\n\n\
         Tap a provider below. 🔒 = needs API key.",
        current_provider, current_model
    );

    ProvidersResponse {
        current_provider: current_provider.clone(),
        current_model: current_model.clone(),
        providers,
        text,
    }
}

/// Loaded version of `crate::utils::providers::all_known_providers_with_status`.
fn all_known_providers_with_status_loaded() -> Vec<(String, String, bool)> {
    let config = match crate::config::Config::load() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    crate::utils::providers::all_known_providers_with_status(&config.providers)
}

/// Help text shown when a user taps a locked (🔒) provider in the channel
/// picker. Tells them where to add the API key. Bots cannot delete user
/// messages in DMs, so we deliberately do NOT prompt for the key inline —
/// pasting it into chat would persist the secret in Telegram history.
pub fn unconfigured_provider_help(provider_name: &str) -> String {
    let display = provider_display_name(provider_name);
    let section = provider_name.replace("-", "_");
    let path = crate::utils::providers::keys_toml_path_hint();
    format!(
        "🔒 *{display}* is not configured yet.\n\n\
         To enable it, add this to `{path}`:\n\n\
         ```toml\n[providers.{section}]\napi_key = \"YOUR-{display}-KEY\"\n```\n\n\
         Then restart OpenCrabs. Do NOT paste your API key here — \
         Telegram keeps message history that bots cannot delete in DMs."
    )
}

/// Model-picker message body (#1149): heading + current + hint, no model
/// enumeration — every channel draws one button per model immediately below,
/// so a numbered text copy was the same duplication #129 removed from
/// /sessions (and the reason OpenRouter's catalogue overflowed Telegram).
fn models_text(display_name: &str, current_model: &str) -> String {
    format!(
        "🤖 *{display_name} Models*\n\nCurrent: `{current_model}`\n\nTap a model below (✓ = current)."
    )
}

/// Fetch models for a specific provider (called from callback handler).
/// The Current model and the list for a CLI provider's `/models` menu (#2041).
///
/// The list is the live discovered one, the same the TUI shows. Current is the
/// provider's configured default, falling back to the canonical default, and is
/// never the first list entry. A saved `models` array is not an input here: it
/// is a frozen snapshot that would hide new releases.
pub(crate) fn cli_menu_choice(
    discovered: Vec<String>,
    canonical_default: &str,
    configured_default: Option<String>,
) -> (String, Vec<String>) {
    (
        configured_default.unwrap_or_else(|| canonical_default.to_string()),
        discovered,
    )
}

pub async fn models_for_provider(provider_name: &str) -> ModelsResponse {
    let config = match crate::config::Config::load() {
        Ok(c) => c,
        Err(_) => {
            return ModelsResponse {
                provider_name: provider_name.to_string(),
                current_model: String::new(),
                models: vec![],
                text: "Failed to load config.".to_string(),
                agent_handled: false,
            };
        }
    };

    let display_name = provider_display_name(provider_name);
    let config_models = provider_config_models(&config, provider_name);

    // CLI providers (Claude CLI, OpenCode CLI) don't need the binary to list models.
    // They have hardcoded supported_models() and don't require API keys.
    // If the binary isn't installed on the server, create_provider_by_name would fail,
    // but we can still show models from config or hardcoded defaults.
    let is_cli_provider = matches!(
        provider_name,
        "claude-cli"
            | "claude_cli"
            | "opencode-cli"
            | "opencode_cli"
            | "command-code-cli"
            | "command_code_cli"
    );

    if is_cli_provider {
        // Read the canonical model list straight from the provider module's
        // const tables via `cli_supported_models`. Single source of truth
        // for both the provider's `supported_models()` and this menu —
        // can't drift, can't show Claude names for OpenCode CLI again.
        let (canonical_models, canonical_default) =
            crate::utils::providers::cli_supported_models(provider_name)
                .unwrap_or_else(|| (Vec::new(), ""));

        // Current is the provider's configured default, never the first list
        // entry (#2041). The list is the live discovered one the TUI shows; a
        // saved `models` array is a frozen snapshot and would hide new releases.
        let configured_default =
            crate::utils::providers::config_for(&config.providers, provider_name)
                .and_then(|c| c.default_model.clone());
        let (current_model, models) =
            cli_menu_choice(canonical_models, canonical_default, configured_default);

        // Positions are what long-name buttons encode, so record them.
        crate::channels::model_menu::remember(provider_name, &models);

        let text = models_text(display_name, &current_model);
        return ModelsResponse {
            provider_name: provider_name.to_string(),
            current_model,
            models,
            text,
            agent_handled: false,
        };
    }

    // OpenRouter (300+ models) and custom providers skip live fetch on channels.
    // Show config models if available, otherwise fall back to the provider's
    // actual default_model from config (never invent fake '-default' names).
    if provider_name == "openrouter" || provider_name.starts_with("custom:") {
        let config_default = crate::utils::providers::config_for(&config.providers, provider_name)
            .and_then(|c| c.default_model.clone());

        // Real-config check: do we actually have any model to offer?
        // Pre-fix: when both config_models and default_model were empty
        // for a custom provider, we returned a single button labeled
        // "unknown (no models configured)" — clicking it stored that
        // literal string as the session's model, leaving the agent in
        // a half-broken state. 2026-05-28 user report: Telegram model
        // switch did nothing for a freshly merge-created custom provider
        // (`qwen-mlx`) whose config had neither default_model nor a
        // populated models list.
        let has_real_model = !config_models.is_empty() || config_default.is_some();

        if !has_real_model {
            let section = provider_name.replace(':', ".");
            let path =
                crate::utils::providers::keys_toml_path_hint().replace("keys.toml", "config.toml");
            let text = format!(
                "🤖 *{display_name} Models*\n\n\
                 No models configured for this provider.\n\n\
                 Add a `default_model` to `[providers.{section}]` in `{path}`, \
                 then restart OpenCrabs. Example:\n\n\
                 ```toml\n[providers.{section}]\ndefault_model = \"YOUR-MODEL-NAME\"\n```",
            );
            return ModelsResponse {
                provider_name: provider_name.to_string(),
                current_model: String::new(),
                models: vec![], // empty → channel renders the help text, no buttons
                text,
                agent_handled: false,
            };
        }

        // The configured `default_model` is the authoritative current model:
        // it is what the TUI shows as selected. Prefer it over the first entry
        // of the stored `models` list, which can be a stale placeholder that
        // does not match the real default (#267).
        let current_model = config_default
            .or_else(|| config_models.first().cloned())
            .expect("has_real_model guard ensures one of these is Some");

        // Surface the current/default model on top, marked as selected, even
        // when it is absent from (or buried inside) the stored list.
        let mut models = if !config_models.is_empty() {
            config_models
        } else {
            vec![current_model.clone()]
        };
        models.retain(|m| m != &current_model);
        models.insert(0, current_model.clone());

        // Positions are what long-name buttons encode, so record them.
        crate::channels::model_menu::remember(provider_name, &models);

        let text = models_text(display_name, &current_model);
        return ModelsResponse {
            provider_name: provider_name.to_string(),
            current_model,
            models,
            text,
            agent_handled: false,
        };
    }

    // Standard API providers: create provider and fetch models
    let provider = match crate::brain::provider::factory::create_provider_by_name(
        &config,
        provider_name,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            return ModelsResponse {
                provider_name: provider_name.to_string(),
                current_model: String::new(),
                models: vec![],
                text: format!("Failed to create provider: {}", e),
                agent_handled: false,
            };
        }
    };

    let current_model = provider.default_model().to_string();

    // Standard providers: config models are a seed and an ordering preference,
    // not the inventory. A `models = [...]` array used to short-circuit the
    // live fetch entirely, which froze the menu at whatever was written to
    // config: a model the provider released later was unreachable from the
    // chat channels even though the TUI could select it (#761). So the live
    // list is always fetched and unioned in behind the configured order.
    //
    // The wait is budgeted on whether there is anything to fall back to. With
    // a config list already in hand the menu can render without the live
    // answer, so the reconcile gets a short budget and a slow provider costs
    // little; with nothing in hand it is worth waiting, because the
    // alternative is a one-item menu.
    let fetch_budget = if config_models.is_empty() {
        std::time::Duration::from_secs(10)
    } else {
        std::time::Duration::from_secs(3)
    };
    let fetched = match tokio::time::timeout(fetch_budget, provider.fetch_models()).await {
        Ok(live) => live,
        Err(_) => {
            // Not fatal: the config list (or the current model) still renders.
            tracing::warn!(
                "fetch_models timed out after {}s for '{}', showing the configured list",
                fetch_budget.as_secs(),
                provider_name
            );
            Vec::new()
        }
    };

    let mut models = config_models;
    for m in fetched {
        if !models.contains(&m) {
            models.push(m);
        }
    }
    if models.is_empty() {
        models.push(current_model.clone());
    }

    // Ensure current model is in the list
    if !models.contains(&current_model) {
        models.insert(0, current_model.clone());
    }

    // Positions are what long-name buttons encode, so record them.
    crate::channels::model_menu::remember(provider_name, &models);

    let text = models_text(display_name, &current_model);
    ModelsResponse {
        provider_name: provider_name.to_string(),
        current_model,
        models,
        text,
        agent_handled: false,
    }
}

/// Get models from the provider's config section (for providers without /models endpoint).
fn provider_config_models(config: &crate::config::Config, name: &str) -> Vec<String> {
    crate::utils::providers::config_for(&config.providers, name)
        .map(|c| c.models.clone())
        .unwrap_or_default()
}

/// Single source for the provider-picker ✓/•/🔒 semantics (#1149). Used by
/// the per-channel button labels (telegram/discord/slack) so the three
/// renderers can't drift apart again; the text body no longer enumerates
/// providers at all.
pub fn provider_marker(name: &str, current_provider: &str, configured: bool) -> &'static str {
    if !configured {
        "🔒"
    } else if name == current_provider {
        "✓"
    } else {
        "•"
    }
}

/// Build the `allq:<provider>|<model>` callback payload behind the
/// "Apply to all sessions" button (#468), enforcing Telegram's 64-byte
/// callback_data cap centrally (#1149). `None` = payload too long; callers
/// omit the button rather than truncating — an omitted button beats a broken
/// one, and no index-fallback form exists for `allq:`. The generator (this +
/// the telegram/agent.rs picker-success path + the commands_tg textual-switch
/// path) and the `allq:` parser MUST agree on this pipe format.
pub fn apply_all_callback_data(provider_name: &str, model: &str) -> Option<String> {
    // The first tap only asks (#2042): it shows the count and Confirm/Cancel.
    let data = format!("allq:{provider_name}|{model}");
    (data.len() <= 64).then_some(data)
}

/// The Confirm button on the apply-all prompt: the only tap that writes (#2042).
pub fn apply_all_confirm_data(provider_name: &str, model: &str) -> Option<String> {
    let data = format!("allm:{provider_name}|{model}");
    (data.len() <= 64).then_some(data)
}

/// The apply-all prompt text, quoting the number of sessions the write changes.
pub fn apply_all_prompt_text(provider_name: &str, model: &str, sessions: usize) -> String {
    match sessions {
        0 => {
            format!("Every other session already uses {provider_name}/{model}. Nothing to change.")
        }
        1 => format!(
            "Apply {provider_name}/{model} to 1 other session? Their model changes on their next message."
        ),
        n => format!(
            "Apply {provider_name}/{model} to {n} other sessions? Their models change on their next message."
        ),
    }
}

/// Build the Telegram `callback_data` for a model button, guaranteed to fit
/// Telegram's 64-byte limit. Uses the literal `model:<provider>|<name>` when
/// it fits (short names — most providers), else the compact index form
/// `model:<provider>|#<index>`, which [`model_at_index`] resolves back. The
/// generator (telegram/agent.rs) and the parser MUST agree on this format.
pub fn model_button_callback_data(provider_name: &str, model: &str, index: usize) -> String {
    let literal = format!("model:{provider_name}|{model}");
    if literal.len() <= 64 {
        literal
    } else {
        format!("model:{provider_name}|#{index}")
    }
}

/// Resolve a model-picker button's index back to its model name using ONLY
/// config (no live fetch), matching the list `models_for_provider` renders
/// for custom/OpenRouter providers.
///
/// The Telegram model picker can't carry long model names in `callback_data`
/// — Telegram caps it at 64 bytes, and a name like
/// `deepseek-ai/DeepSeek-R1-Distill-Qwen-1.5B` pushes
/// `model:custom:modelscope|<name>` to 65 bytes, which makes Telegram reject
/// the WHOLE inline keyboard (BUTTON_DATA_INVALID) so the picker silently
/// renders nothing. The button encodes the index instead; this resolves it.
pub fn model_at_index(provider_name: &str, index: usize) -> Option<String> {
    // What the picker actually rendered wins: since the standard-provider list
    // is config unioned with the live inventory (#761), config can no longer
    // reconstruct the positions the buttons were built from.
    if let Some(model) = crate::channels::model_menu::resolve_index(provider_name, index) {
        return Some(model);
    }
    let config = crate::config::Config::load().ok()?;
    let models = provider_config_models(&config, provider_name);
    if !models.is_empty() {
        return models.get(index).cloned();
    }
    // Custom branch uses `[default_model]` when the models list is empty.
    if index == 0 {
        return crate::utils::providers::config_for(&config.providers, provider_name)
            .and_then(|c| c.default_model.clone());
    }
    None
}

pub fn provider_display_name(name: &str) -> &str {
    crate::utils::providers::display_name(name)
}

// ── Model switching ─────────────────────────────────────────────────────────

/// Switch the active model for this session's provider.
///
/// Persists provider + model to the session DB record so the session keeps
/// its own provider independently. Does NOT toggle global config enabled flags
/// — that would leak into other sessions/channels.
/// Saves a `[Model changed to ...]` message to the session history so the agent
/// is aware of the switch.
/// Returns an error message on failure so channels can report it to the user.
/// Resolve a spaced argument like `xiaomi mimo v2.5 pro` into a real
/// provider/model pair (#801).
///
/// Only reached when exact `provider/model` parsing failed, so it can never
/// change how an established argument behaves.
///
/// Every outcome except a single unambiguous hit is an error. Loose matching
/// that guesses between candidates is worse than none: the user gets a model
/// they did not ask for and has no reason to check it.
async fn resolve_loose_pair(
    config: &crate::config::Config,
    arg: &str,
) -> Result<(String, String), String> {
    use crate::utils::model_match::{ModelMatch, match_model, split_provider_and_model};

    let Some((provider_ref, model_ref)) = split_provider_and_model(arg) else {
        return Err(format!(
            "'{arg}' names a provider but no model. Try `/models <provider> <model>`, \
             or send /models for the picker."
        ));
    };

    if !crate::brain::provider::factory::is_known_provider_name(config, &provider_ref) {
        return Err(format!(
            "Unknown provider '{provider_ref}'. Send /models for the picker."
        ));
    }

    // The catalogue is the provider's own, so matching never invents a model
    // the provider does not serve.
    let provider = crate::brain::provider::create_provider_by_name(config, &provider_ref)
        .await
        .map_err(|e| format!("Provider '{provider_ref}' could not be loaded: {e}"))?;
    let catalogue = provider.supported_models();

    match match_model(&model_ref, &catalogue) {
        ModelMatch::One(model) => Ok((provider_ref, model)),
        ModelMatch::Ambiguous(hits) => Err(format!(
            "'{model_ref}' matches {} models on {provider_ref}: {}. Name one exactly.",
            hits.len(),
            hits.join(", ")
        )),
        ModelMatch::None => Err(format!(
            "No model on {provider_ref} matches '{model_ref}'. Send /models for the picker."
        )),
    }
}

/// Direct-argument model switch (#467): `/models <provider/model>` applies
/// the pair to the CURRENT session immediately on every channel and the
/// TUI, reusing the exact keyboard-flow switch path (per-session swap +
/// manual pin + DB persist). Returns the user-facing reply text.
pub async fn direct_model_switch(
    agent: &AgentService,
    session_id: uuid::Uuid,
    arg: &str,
) -> String {
    // Scope selector (#468): a trailing `all` token applies the pair to
    // every non-archived session after switching the current one. Default
    // stays current-session-only; all is always an explicit act.
    let (pair_arg, scope_all) = match arg.trim().strip_suffix(" all") {
        Some(rest) if !rest.trim().is_empty() => (rest.trim(), true),
        _ => (arg.trim(), false),
    };
    let config = match crate::config::Config::load() {
        Ok(c) => c,
        Err(e) => return format!("⚠️ Failed to load config: {e}"),
    };
    // Exact `provider/model` first, so the established form is untouched.
    // Only when that fails do we try the spaced form (#801), where the words
    // are what the user remembers and the punctuation is not.
    let (provider, model) = match crate::utils::provider_pair::parse_pair(pair_arg) {
        Ok(pair) => pair,
        Err(exact_err) => match resolve_loose_pair(&config, pair_arg).await {
            Ok(pair) => pair,
            Err(loose_err) => return format!("⚠️ {loose_err}\n({exact_err})"),
        },
    };
    // Declared in THIS config, not merely a provider this software supports.
    // The message already promised that; the registry check did not deliver it,
    // so `anthropic/claude-x` was accepted as a prefix with no such section and
    // failed later inside create_provider_by_name with a confusing error (#939).
    if !config.providers.is_declared(&provider) {
        return format!(
            "⚠️ Unknown provider '{provider}' — it must be a configured provider section. \
             If that is the whole model name, qualify it, e.g. 'openrouter/{provider}/…'. \
             Send /models for the picker."
        );
    }
    match switch_model(agent, &model, Some(session_id), Some(&provider)).await {
        Ok(msg) => {
            if scope_all {
                let session_svc = crate::services::SessionService::new(agent.context().clone());
                match session_svc
                    .set_provider_model_all_sessions(&provider, &model)
                    .await
                {
                    Ok(n) => format!(
                        "{msg}\nApplied to {n} other session(s); each picks it up on its \
                         next message."
                    ),
                    Err(e) => format!("{msg}\n⚠️ Scope-all write failed: {e}"),
                }
            } else {
                msg
            }
        }
        Err(e) => format!("⚠️ {e}"),
    }
}

pub async fn switch_model(
    agent: &AgentService,
    model_name: &str,
    session_id: Option<uuid::Uuid>,
    provider_name_override: Option<&str>,
) -> Result<String, String> {
    // Provider name MUST come from the caller (callback data) when available.
    // Falling back to agent state caused crossed pairs when the in-memory
    // slot was stale or another session had just nudged the global default.
    let provider_name = match provider_name_override {
        Some(p) => p.to_string(),
        None => match session_id {
            Some(sid) => agent.provider_name_for_session(sid),
            None => agent.provider_name(),
        },
    };

    let config =
        crate::config::Config::load().map_err(|e| format!("Failed to load config: {}", e))?;

    tracing::info!(
        "Channel: switched model to {} (provider: {}, session: {:?})",
        model_name,
        provider_name,
        session_id
    );

    // Create provider by name (doesn't modify global config enabled flags)
    let new_provider =
        crate::brain::provider::factory::create_provider_by_name(&config, &provider_name)
            .await
            .map_err(|e| {
                tracing::warn!("Failed to create provider after model switch: {}", e);
                format!("Model saved but failed to reload provider: {}", e)
            })?;
    let display_name = provider_display_name(&provider_name);
    // Pin per-session when possible; only touch the global slot for
    // callers without a session (kept for the bootstrap path).
    match session_id {
        Some(sid) => {
            // Provider+model are a pair: the user picked model_name, so set
            // it atomically. The freshly-created provider's default_model()
            // is the global config default, NOT the user's pick.
            agent.swap_provider_for_session(sid, new_provider, model_name.to_string());
            // Pin as a USER switch so an in-flight turn's fallback can't
            // permanently overwrite it (restored after that turn completes).
            agent.mark_manual_switch(sid, model_name.to_string());
        }
        None => agent.swap_provider(new_provider),
    }

    let change_msg = format!("[Model changed to {}/{}]", display_name, model_name);

    // Persist provider + model to session DB record so it survives restarts
    if let Some(sid) = session_id {
        let session_svc = crate::services::SessionService::new(agent.context().clone());
        if let Ok(Some(mut session)) = session_svc.get_session(sid).await {
            session.provider_name = Some(provider_name.clone());
            session.model = Some(model_name.to_string());
            if let Err(e) = session_svc.update_session(&session).await {
                tracing::warn!("Failed to persist provider to session: {}", e);
            }
        }

        // Persist change message to session history so the agent knows
        let msg_svc = crate::services::MessageService::new(agent.context().clone());
        if let Err(e) = msg_svc
            .create_message(sid, "user".to_string(), change_msg.clone())
            .await
        {
            tracing::warn!("Failed to persist model-change message: {}", e);
        }
    }

    Ok(change_msg)
}
/// Run evolve directly (no LLM needed). Returns a user-facing status message.
/// Handles the RestartReady signal by triggering a process restart via exec().
pub async fn run_evolve() -> String {
    use crate::brain::agent::ProgressEvent;
    use crate::brain::tools::{Tool, ToolExecutionContext, evolve::EvolveTool};
    use std::path::PathBuf;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    // Track whether we received a RestartReady signal, AND the binary it named.
    //
    // The path is not decoration (#1130). Evolve captures it BEFORE unlinking
    // the old inode, and after the swap it is the only clean path left: Linux
    // reports `/proc/self/exe` as `"<path> (deleted)"` and `current_exe()`
    // hands that string back verbatim, so re-deriving it here execs a literal
    // `"… (deleted)"` and ENOENTs. Matching with `{ .. }` threw it away and
    // every channel-triggered evolve on Linux failed to restart.
    let restart_ready = Arc::new(AtomicBool::new(false));
    let restart_binary: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let restart_flag = restart_ready.clone();
    let restart_binary_sink = restart_binary.clone();

    // Create a progress callback that detects RestartReady
    let progress_callback: crate::brain::agent::ProgressCallback = Arc::new(move |_sid, event| {
        if let ProgressEvent::RestartReady { binary_path, .. } = event {
            *restart_binary_sink
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = binary_path;
            restart_flag.store(true, Ordering::SeqCst);
        }
    });

    let ctx = ToolExecutionContext::new(uuid::Uuid::nil()).with_session_notify_enabled(
        crate::config::Config::current()
            .agent
            .session_notify_enabled,
    );
    let tool = EvolveTool::new(Some(progress_callback));
    let result = match tool
        .execute(serde_json::json!({"check_only": false}), &ctx)
        .await
    {
        Ok(result) => result.output,
        Err(e) => format!("Evolve failed: {}", e),
    };

    // If we received a RestartReady signal, trigger the restart into the
    // binary that signal named.
    if restart_ready.load(Ordering::SeqCst) {
        let preferred = restart_binary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Err(e) = trigger_restart(preferred) {
            return format!("{result}\n\n⚠️ Update installed but restart failed: {e}");
        }
    }

    result
}

/// Pick the binary to exec on restart (#1130).
///
/// `preferred` is the path the `RestartReady` producer captured BEFORE any
/// in-place swap. Evolve unlinks the old inode and renames the new binary over
/// it, after which `/proc/self/exe` (and so `std::env::current_exe()`) reads
/// back as `"<path> (deleted)"`. Rust returns that suffix verbatim, so execing
/// it ENOENTs, and a retry writes the next download to a real file literally
/// named `opencrabs (deleted)`.
///
/// Preferring the producer's path covers the binary-swap branch. Stripping the
/// marker afterwards covers the cargo-install branch, which reports
/// `binary_path: None` on purpose and would otherwise fall through to the same
/// poisoned `current_exe()`. Stripping is idempotent, so a clean path, the
/// only kind macOS ever produces, passes through untouched.
pub(crate) fn restart_target(
    preferred: Option<std::path::PathBuf>,
    current: std::path::PathBuf,
) -> std::path::PathBuf {
    crate::brain::self_update::strip_deleted_marker(preferred.unwrap_or(current))
}

/// Relaunch the current binary with the arguments it was started with.
///
/// The launching alias never has to be known: the shell expanded it before the
/// process existed, so `args()` already holds that expansion and `current_exe()`
/// holds the binary it resolved to. On unix `exec()` then replaces the process
/// image in place, keeping the pid, file descriptors, terminal and environment,
/// so anything the alias exported survives.
///
/// `preferred` short-circuits that resolution when a caller already knows the
/// real path; see [`restart_target`]. The args stay `args().skip(1)` rather
/// than `SelfUpdater::restart_into`, which hardcodes `chat --session <id>`: a
/// daemon was never launched that way and must come back up as a daemon.
///
/// Only returns on failure.
#[cfg(unix)]
fn trigger_restart(preferred: Option<std::path::PathBuf>) -> Result<(), String> {
    use std::os::unix::process::CommandExt;

    let current = std::env::current_exe().map_err(|e| format!("cannot locate own binary: {e}"))?;
    let exe = restart_target(preferred, current);
    let args: Vec<String> = std::env::args().skip(1).collect();

    tracing::info!("Restarting via exec(): {} {:?}", exe.display(), args);

    // exec() replaces the current process, so this never returns on success.
    let err = std::process::Command::new(&exe).args(&args).exec();
    tracing::error!("exec() failed: {err}");
    Err(format!("exec() failed: {err}"))
}

/// Windows has no `exec()`, so the replacement is spawned as a child and this
/// process exits. The pid changes and the child is briefly a grandchild of the
/// old parent, which is why unix keeps the `exec()` path instead.
#[cfg(not(unix))]
fn trigger_restart(preferred: Option<std::path::PathBuf>) -> Result<(), String> {
    let current = std::env::current_exe().map_err(|e| format!("cannot locate own binary: {e}"))?;
    let exe = restart_target(preferred, current);
    let args: Vec<String> = std::env::args().skip(1).collect();

    tracing::info!("Restarting via spawn: {} {:?}", exe.display(), args);

    std::process::Command::new(&exe)
        .args(&args)
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;
    std::process::exit(0);
}

/// How long to wait before going down, so the channel can flush the warning
/// first. Nothing survives the restart to report a failure afterwards, so the
/// message has to leave ahead of it.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_millis(1500);

/// Announce a restart, then perform it once the notice has been delivered.
fn schedule_restart() -> String {
    tokio::spawn(async {
        tokio::time::sleep(SHUTDOWN_GRACE).await;
        if let Err(e) = trigger_restart(None) {
            // Reaching here means the process is still alive and the user was
            // already told it was going down, so the correction has to be loud.
            tracing::error!("Restart failed, still running: {e}");
        }
    });
    "♻️ Restarting OpenCrabs. Unfinished work resumes automatically.".to_string()
}

/// Announce a shutdown, then perform it once the notice has been delivered.
fn schedule_exit() -> String {
    tokio::spawn(async {
        tokio::time::sleep(SHUTDOWN_GRACE).await;
        tracing::info!("Shutting down on /exit");
        std::process::exit(0);
    });
    "👋 Exiting OpenCrabs. Starting it again needs access to the machine it runs on.".to_string()
}

/// Run doctor health check directly (no LLM needed). Returns a user-facing status message.
pub fn run_doctor() -> String {
    use crate::brain::tools::slash_command::SlashCommandTool;

    // Reuse the slash command tool's doctor logic
    SlashCommandTool::doctor_text()
}

/// Build the mechanical `/architecture` tree (no LLM, zero per-use cost,
/// #933). Returns ready-to-send text in both the Ok and Err cases.
pub fn run_architecture(path: Option<&str>) -> String {
    match crate::utils::tree_view::architecture_tree(path) {
        Ok(text) => text,
        Err(e) => e,
    }
}

/// Per-path verdict for `/attach` (#933): mechanical, no LLM.
pub(crate) enum AttachVerdict {
    Ok,
    Directory,
    NotFound,
    Confidential(&'static str),
    /// Any path component is dot-hidden; #933 hides them from both surfaces.
    Hidden,
    /// Not `.md` and not under a `docs/` directory; the #933 docs-only
    /// allowlist keeps arbitrary files out of the chat log.
    NotDocs,
}

/// Classify one `/attach` path candidate: pure fs checks plus the compiled
/// confidential denylist (`is_confidential`), so keys, SSH material and env
/// files are refused in code, not by prompt text. Hidden dotfiles and the
/// docs-only allowlist are enforced here too (#933 hard requirements). No
/// agent involvement.
pub(crate) fn validate_attach_path(raw: &str) -> AttachVerdict {
    let path = std::path::Path::new(raw);
    if !path.exists() {
        return AttachVerdict::NotFound;
    }
    if let Some(reason) = crate::brain::tools::confidential::is_confidential(path) {
        return AttachVerdict::Confidential(reason);
    }
    if path.is_dir() {
        return AttachVerdict::Directory;
    }
    if is_hidden_path(path) {
        return AttachVerdict::Hidden;
    }
    if !is_docs_path(path) {
        return AttachVerdict::NotDocs;
    }
    AttachVerdict::Ok
}

/// Component strings of `path`, skipping structural components (`/`, `.`)
/// so a leading `./src/x.md` is not mistaken for a hidden path (#933).
fn path_components(path: &std::path::Path) -> impl Iterator<Item = String> {
    path.components().filter_map(|c| match c {
        std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
        _ => None,
    })
}

/// The hidden gate from #933: refuse dotFILES (`.env`, `.hidden.md`) and any
/// `..` traversal. Ancestor directory names do not count, so macOS temp dirs
/// (`.tmpXXXX`) and legitimate dot-directories (`.github/SECURITY.md`) pass;
/// secrets inside them are still caught first by the confidential gate.
fn is_hidden_path(path: &std::path::Path) -> bool {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return true;
    }
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

/// The docs-only allowlist from #933: `.md` files (case-insensitive) or any
/// file under a `docs` directory (case-insensitive).
fn is_docs_path(path: &std::path::Path) -> bool {
    let md = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    let under_docs = path_components(path).any(|c| c.eq_ignore_ascii_case("docs"));
    md || under_docs
}

/// Classify every whitespace-separated path in an `/attach` invocation and
/// build the per-path status lines. Returns `(status lines, attached paths)`.
/// Pure fs + denylist: no agent involvement, so tests can pin all verdicts
/// without a service harness (#933).
pub(crate) fn attach_status_lines(args: &str) -> (Vec<String>, Vec<String>) {
    let mut lines = Vec::new();
    let mut attached = Vec::new();
    for raw in args.split_whitespace() {
        match validate_attach_path(raw) {
            AttachVerdict::Ok => {
                attached.push(raw.to_string());
                lines.push(format!("📎 {raw}: attached"));
            }
            AttachVerdict::Directory => lines.push(format!(
                "📁 {raw}: is a directory (try /architecture {raw})"
            )),
            AttachVerdict::NotFound => lines.push(format!("❓ {raw}: no such path")),
            AttachVerdict::Confidential(reason) => {
                lines.push(format!("🔒 {raw}: refused ({reason})"))
            }
            AttachVerdict::Hidden => {
                lines.push(format!("🙈 {raw}: hidden paths are not surfaced"))
            }
            AttachVerdict::NotDocs => lines.push(format!(
                "📄 {raw}: docs-only (.md or under a docs/ directory); use file upload for other types"
            )),
        }
    }
    (lines, attached)
}

/// Execute `/attach` on a channel (no LLM, #933): validate each path, persist
/// one system message carrying a `[User attached file: <path>]` marker per
/// attached file (the marker reaches the model via normal history recall on
/// the next turn), and return the per-path status reply.
pub async fn run_attach_channel(args: &str, session_id: Uuid, agent: &AgentService) -> String {
    if args.is_empty() {
        return "attach: usage /attach <path> [more paths...]".to_string();
    }
    let (mut lines, attached) = attach_status_lines(args);
    if !attached.is_empty() {
        let content = attached
            .iter()
            .map(|p| format!("[User attached file: {p}]"))
            .collect::<Vec<_>>()
            .join("\n");
        let msg_svc = crate::services::MessageService::new(agent.context().clone());
        if let Err(e) = msg_svc
            .create_message(session_id, "system".to_string(), content)
            .await
        {
            tracing::warn!("/attach: failed to persist attachment marker: {e}");
            lines.push("⚠️ could not record the attachment in session history".to_string());
        }
    }
    lines.join("\n")
}

/// Render the `/audit` viewer (#1705): the last N ACTION rows
/// (`tool_executions`, always present) joined with the READ count / newest
/// retrieval and the mechanical OUTCOME verdict when recording is on.
/// Mechanical only: one SQL query + text render, no LLM call.
pub async fn run_audit_channel(n: Option<u32>, agent: &AgentService) -> String {
    let limit = n.unwrap_or(10).clamp(1, 100);
    let Some(pool) = crate::db::global_pool() else {
        return "audit: no database pool available".to_string();
    };
    let repo = crate::db::repository::TurnRetrievalRepository::new(pool.clone());
    let rows = match repo.recent_audit_rows(limit).await {
        Ok(rows) => rows,
        Err(e) => return format!("audit: failed to load: {e}"),
    };
    if rows.is_empty() {
        return "audit: no tool actions recorded yet".to_string();
    }
    render_audit_rows(&rows, agent.audit_recording_enabled())
}

/// Pure renderer behind `/audit`, split from the DB load so tests can pin
/// both render modes (recording on/off) without a pool (#1705).
pub(crate) fn render_audit_rows(
    rows: &[crate::db::repository::turn_retrieval::AuditRow],
    recording: bool,
) -> String {
    let mut out = String::new();
    if recording {
        out.push_str(&format!(
            "audit: last {} action(s), READ+OUTCOME recorded\n",
            rows.len()
        ));
    } else {
        out.push_str(&format!(
            "audit: last {} action(s) (ACTION only; enable [features] audit_recording for READ+OUTCOME)\n",
            rows.len()
        ));
    }
    out.push_str("TURN|ACTION|READ|OUTCOME\n");
    for r in rows {
        let turn = r.message_id.chars().take(8).collect::<String>();
        let action = if r.status == "success" {
            r.tool_name.clone()
        } else {
            format!("{} (err)", r.tool_name)
        };
        let read = if r.read_count > 0 {
            match &r.last_read {
                Some(last) => format!("{}: {last}", r.read_count),
                None => r.read_count.to_string(),
            }
        } else {
            "-".to_string()
        };
        let outcome = r.outcome.clone().unwrap_or_else(|| "-".to_string());
        out.push_str(&format!("{turn}|{action}|{read}|{outcome}\n"));
    }
    out.trim_end().to_string()
}

/// Try to execute a command that returns a simple text response (no platform-specific UI).
/// Returns `Some(text)` for commands handled here, `None` for commands that need
/// platform-specific rendering (Models, Sessions, NewSession) or agent passthrough.
/// Channels call this first — if it returns Some, send the text and return.
pub async fn try_execute_text_command(cmd: &ChannelCommand) -> Option<String> {
    match cmd {
        ChannelCommand::Help(body)
        | ChannelCommand::Usage(body)
        | ChannelCommand::MissionControl(body)
        | ChannelCommand::UserSystem(body)
        | ChannelCommand::Rtk(body) => Some(body.clone()),
        ChannelCommand::Doctor => Some(run_doctor()),
        ChannelCommand::Architecture(path) => Some(run_architecture(path.as_deref())),
        ChannelCommand::Attach(body) => Some(body.clone()),
        ChannelCommand::Audit(body) => Some(body.clone()),
        ChannelCommand::Evolve => Some(run_evolve().await),
        ChannelCommand::Restart => Some(schedule_restart()),
        ChannelCommand::Exit => Some(schedule_exit()),
        ChannelCommand::UnknownCommand(msg) => Some(msg.to_string()),
        ChannelCommand::ModelSwitched(msg) => Some(msg.to_string()),
        ChannelCommand::RespondTo(body) => Some(body.clone()),
        ChannelCommand::Redact(body) => Some(body.clone()),
        ChannelCommand::PlanMode(body) | ChannelCommand::ShowPlan(body) => Some(body.clone()),
        _ => None,
    }
}

/// Handle `/respond_to [all|mention|auto]` — show or switch the auto-mention
/// mode for the Telegram channel (#244). Owner-only (enforced by caller).
///
/// When `chat_id` is `Some` (command issued from a group), the setting is
/// persisted per-group under `[channels.telegram.groups.<chat_id>]`.
/// When `None` (command issued from a DM), it falls back to the channel-level
/// `[channels.telegram]` setting.
/// `/redact [global|group|dm] [on|off]` — show or set scoped secret redaction
/// (#677). Owner-only (gated at the call site). Bare shows current scopes;
/// `<scope> <on|off>` persists to `agent.redact_*` in config.toml.
pub(crate) async fn handle_redact(arg: &str) -> String {
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => return format!("❌ Failed to load config: {}", e),
    };
    let a = arg.trim();
    if a.is_empty() {
        let onoff = |b: bool| if b { "on" } else { "off" };
        let group = config
            .agent
            .redact_group
            .map(|b| onoff(b).to_string())
            .unwrap_or_else(|| {
                format!(
                    "follows global ({})",
                    onoff(config.agent.redact_sensitive_data)
                )
            });
        let dm = config
            .agent
            .redact_dm
            .map(|b| onoff(b).to_string())
            .unwrap_or_else(|| "off (default)".to_string());
        return format!(
            "🔒 Redaction scopes:\n\
             • 🌍 global: {}\n\
             • 👥 group: {}\n\
             • 📩 dm: {}\n\n\
             Usage: `/redact <global|group|dm> <on|off>`\n\
             Secrets are scrubbed in shared group chats and shown in your DMs by default.",
            onoff(config.agent.redact_sensitive_data),
            group,
            dm
        );
    }
    let mut parts = a.split_whitespace();
    let scope = parts.next().unwrap_or("").to_lowercase();
    let state = parts.next().unwrap_or("").to_lowercase();
    let on = match state.as_str() {
        "on" | "true" | "yes" | "enable" | "enabled" => true,
        "off" | "false" | "no" | "disable" | "disabled" => false,
        _ => return "⚠️ Usage: `/redact <global|group|dm> <on|off>`".to_string(),
    };
    let (key, label) = match scope.as_str() {
        "global" => ("redact_sensitive_data", "🌍 global"),
        "group" => ("redact_group", "👥 group"),
        "dm" => ("redact_dm", "📩 dm"),
        _ => return "⚠️ Scope must be one of: `global`, `group`, `dm`.".to_string(),
    };
    match Config::write_key("agent", key, if on { "true" } else { "false" }) {
        Ok(_written) => format!(
            "✅ Redaction for {} set to **{}**.",
            label,
            if on { "on" } else { "off" }
        ),
        Err(e) => format!("❌ Failed to save config: {}", e),
    }
}

pub(crate) async fn handle_respond_to(arg: &str, chat_id: Option<&str>) -> String {
    use crate::config::RespondTo;

    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => return format!("❌ Failed to load config: {}", e),
    };

    // When in a group, prefer per-group override; fall back to channel-level.
    // Track whether a per-group override ACTUALLY EXISTS so we don't skip the
    // write when the global fallback happens to match the requested value.
    let (current, group_has_override) = if let Some(cid) = chat_id {
        let group_cfg = config.channels.telegram.groups.get(cid);
        let group_override = group_cfg.and_then(|g| g.respond_to.as_ref());
        if let Some(override_val) = group_override {
            (*override_val, true)
        } else {
            (config.channels.telegram.respond_to, false)
        }
    } else {
        (config.channels.telegram.respond_to, true) // channel-level: "has override"
    };

    let current_label = match &current {
        RespondTo::All => "all",
        RespondTo::DmOnly => "dm_only",
        RespondTo::Mention => "mention",
        RespondTo::Auto => "auto",
    };

    if arg.is_empty() {
        let scope = if chat_id.is_some() {
            "this group"
        } else {
            "all groups (channel-level)"
        };
        return format!(
            "📢 Current respond_to mode for {}: **{}**\n\n\
             Usage: `/respond_to <all|mention|auto>`\n\n\
             • **all** — respond to every message\n\
             • **mention** — only respond when @mentioned\n\
             • **auto** — respond to all when ≤1 sender, mention-only when >1 sender\n\n\
             ⚠️ In groups with several bots, command autocomplete can target \
             ANOTHER bot's handle — then this bot never sees the command. \
             Type it manually or pick this bot's handle from the menu.",
            scope, current_label
        );
    }

    let (new_mode, new_label) = match arg.to_lowercase().as_str() {
        "all" => ("all", "all"),
        "mention" | "mentions" => ("mention", "mention"),
        "auto" => ("auto", "auto"),
        _ => {
            return format!(
                "❌ Unknown mode \"{}\". Use: `/respond_to <all|mention|auto>`",
                arg
            );
        }
    };

    // Only early-return when a real override already exists at this scope.
    // If the group has no per-group override yet but the global happens to
    // match, we must still write so the per-group section gets created.
    if current_label == new_label && group_has_override {
        return format!("ℹ️ Already in **{}** mode.", current_label);
    }

    // Write to the correct config section
    let write_result = if let Some(cid) = chat_id {
        // Per-group: write to channels.telegram.groups.<chat_id>.respond_to
        Config::write_key(
            &format!("channels.telegram.groups.{}", cid),
            "respond_to",
            new_mode,
        )
    } else {
        // Channel-level: write to channels.telegram.respond_to
        Config::write_key("channels.telegram", "respond_to", new_mode)
    };

    match write_result {
        Ok(_) => {
            let scope = if chat_id.is_some() {
                "this group"
            } else {
                "all groups"
            };
            format!(
                "✅ Respond-to mode switched to **{}** for {}.\n{}",
                new_label,
                scope,
                match new_label {
                    "all" => "Bot will respond to every message.",
                    "auto" => {
                        "Bot responds to all when ≤1 active sender. \
                         Switches to mention-only when a second sender appears."
                    }
                    _ => "Bot will only respond when @mentioned.",
                }
            )
        }
        Err(e) => format!("❌ Failed to save config: {}", e),
    }
}

/// Map a provider name to its config section key.
#[cfg(test)]
pub(crate) fn provider_section(provider_name: &str) -> Option<String> {
    crate::utils::providers::config_section(provider_name)
}
