//! Slash Command Tool
//!
//! Lets the agent invoke any slash command programmatically — both built-in
//! (/cd, /compact) and user-defined commands from commands.toml.
//! New commands added via `config_manager add_command` are automatically available.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

/// The config section a provider id writes to.
///
/// Custom keys are checked first, matching `is_known_provider_name`, so a
/// section the user declared themselves wins for its own name. Returns `None`
/// for anything the registry does not know — the caller reports that rather
/// than picking a section.
pub(crate) fn section_for_provider(
    config: &crate::config::Config,
    provider: &str,
) -> Option<String> {
    let bare = provider.strip_prefix("custom:").unwrap_or(provider);
    if config
        .providers
        .custom
        .as_ref()
        .is_some_and(|m| m.contains_key(bare))
    {
        return Some(format!("providers.custom.{bare}"));
    }
    crate::utils::providers::config_section(provider)
}

/// Resolve `/models <arg>` to the `(provider, model)` it targets.
///
/// A slash means `<provider>/<model>`, and the provider must be one the user
/// has declared. No slash means the model applies to the active provider.
///
/// The prefix is checked against declared config sections rather than the
/// registry of everything this software supports. `anthropic` is both a
/// provider id and an OpenRouter vendor, so a registry check routes
/// `anthropic/claude-sonnet-4` at a provider the user may never have set up.
/// Asking whether THEY configured it resolves that: if they did, the prefix is
/// what they meant; if they did not, this is an OpenRouter-style id and they
/// are told to qualify it as `openrouter/anthropic/claude-sonnet-4`.
///
/// Matches `direct_model_switch`, the path a user's `/models` takes, whose
/// error already promised "it must be a configured provider section".
///
/// There is no hardcoded provider ladder. The previous one tested six providers
/// by name out of the twenty-two the registry knows, so a user on any of the
/// other sixteen fell through to "first enabled custom provider" and had their
/// model written into an unrelated section (#939). Anything unresolvable is an
/// error naming the problem, never a guess.
pub(crate) fn resolve_model_target(
    config: &crate::config::Config,
    arg: &str,
) -> std::result::Result<(String, String), String> {
    if let Ok((provider, model)) = crate::utils::provider_pair::parse_pair(arg) {
        if config.providers.is_declared(&provider) {
            return Ok((provider, model));
        }
        return Err(format!(
            "Unknown provider '{provider}' — it must be a configured provider section. \
             If '{arg}' is the whole model name, qualify it with the provider that serves \
             it, e.g. 'openrouter/{arg}'."
        ));
    }
    let (active, _) = config.providers.active_provider_and_model();
    if active == "none" {
        return Err(format!(
            "No active provider, so there is nothing to apply '{arg}' to. \
             Enable and configure one via config_manager or /onboard, or name it \
             explicitly as '<provider>/{arg}'."
        ));
    }
    Ok((active, arg.to_string()))
}

/// Split a possibly multi-word `command` field into `(command, args)`.
///
/// Models sometimes pass `command='/goal debug the build'` with an empty
/// `args` instead of `command='/goal'`, `args='debug the build'`. Since the
/// dispatch matches the command verbatim, peel the first whitespace-delimited
/// token as the command and fold any trailing words back into `args` (ahead of
/// whatever was already there). Single-token commands pass through unchanged.
pub(crate) fn normalize_command(raw_command: &str, raw_args: &str) -> (String, String) {
    let trimmed = raw_command.trim();
    let raw_args = raw_args.trim();
    let (mut command, merged) = match trimmed.split_once(char::is_whitespace) {
        Some((head, tail)) => {
            let tail = tail.trim();
            let merged = match (tail.is_empty(), raw_args.is_empty()) {
                (true, _) => raw_args.to_string(),
                (false, true) => tail.to_string(),
                (false, false) => format!("{tail} {raw_args}"),
            };
            (head.to_string(), merged)
        }
        None => (trimmed.to_string(), raw_args.to_string()),
    };
    // Models repeatedly drop the leading `/` ("status", "models list"). The
    // valid-command set is closed, so a nonempty missing-slash input maps
    // unambiguously to one command — prepend instead of rejecting (#1167).
    if !command.is_empty() && !command.starts_with('/') {
        tracing::info!("slash_command: prepended '/' to '{}'", command);
        command.insert(0, '/');
    }
    (command, merged)
}

pub struct SlashCommandTool;

impl SlashCommandTool {
    /// Run the doctor health check and return the result as plain text.
    /// Used by channel commands to avoid going through the LLM.
    pub fn doctor_text() -> String {
        let config = match crate::config::Config::load() {
            Ok(c) => c,
            Err(e) => return format!("Failed to load config: {}", e),
        };

        let version = env!("CARGO_PKG_VERSION");
        let mut lines = vec![format!("Health Check (v{version})"), String::new()];

        // Check keys.toml validity
        let keys_path = crate::config::keys_path();
        if keys_path.exists() {
            match std::fs::read_to_string(&keys_path) {
                Ok(content) => match toml::from_str::<toml::Value>(&content) {
                    Ok(_) => lines.push("keys.toml — OK".to_string()),
                    Err(e) => lines.push(format!("keys.toml — PARSE ERROR: {e}")),
                },
                Err(e) => lines.push(format!("keys.toml — READ ERROR: {e}")),
            }
        } else {
            lines.push("keys.toml — NOT FOUND".to_string());
        }
        lines.push(String::new());

        // Check providers
        let providers = [
            ("anthropic", &config.providers.anthropic),
            ("openai", &config.providers.openai),
            ("gemini", &config.providers.gemini),
            ("openrouter", &config.providers.openrouter),
            ("minimax", &config.providers.minimax),
        ];

        lines.push("Providers:".to_string());
        for (name, provider_opt) in &providers {
            if let Some(provider) = provider_opt
                && provider.enabled
            {
                let has_key = provider.api_key.as_ref().is_some_and(|k| !k.is_empty());
                let model = provider.default_model.as_deref().unwrap_or("(not set)");
                let status = if has_key { "OK" } else { "MISSING API KEY" };
                lines.push(format!("  {} — {} (model: {})", name, status, model));
            }
        }

        if let Some(ref custom) = config.providers.custom {
            for (name, provider) in custom {
                if provider.enabled {
                    let has_key = provider.api_key.as_ref().is_some_and(|k| !k.is_empty());
                    let model = provider.default_model.as_deref().unwrap_or("(not set)");
                    let status = if has_key { "OK" } else { "MISSING API KEY" };
                    lines.push(format!("  custom/{} — {} (model: {})", name, status, model));
                }
            }
        }

        // Check channels
        lines.push(String::new());
        lines.push("Channels:".to_string());
        let ch = &config.channels;
        if ch.telegram.enabled {
            lines.push("  telegram — enabled".to_string());
        }
        if ch.discord.enabled {
            lines.push("  discord — enabled".to_string());
        }
        if ch.slack.enabled {
            lines.push("  slack — enabled".to_string());
        }
        if ch.whatsapp.enabled {
            lines.push("  whatsapp — enabled".to_string());
        }
        if ch.trello.enabled {
            lines.push("  trello — enabled".to_string());
        }

        // Voice config
        lines.push(String::new());
        lines.push(format!(
            "Voice: STT={}, TTS={}",
            config.voice_config().stt_enabled,
            config.voice_config().tts_enabled
        ));

        // Approval policy
        lines.push(format!("Approval: {}", config.agent.approval_policy));

        // Provider health
        lines.push(String::new());
        lines.push("Provider Health:".to_string());
        let health_state: crate::config::health::HealthState =
            std::fs::read_to_string(crate::config::opencrabs_home().join("provider_health.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        if health_state.providers.is_empty() {
            lines.push("  (no data yet)".to_string());
        } else {
            // Only show providers that have successfully handled at least one
            // request (last_success) or are currently configured with a key.
            // Keyless config stubs never make API calls so they never get here,
            // but this filter also hides stale entries from providers whose
            // keys were later removed.
            for (name, h) in &health_state.providers {
                if h.last_success.is_none() {
                    continue;
                }
                let status = if h.consecutive_failures > 0 {
                    format!("FAILING ({}x)", h.consecutive_failures)
                } else {
                    "OK".to_string()
                };
                lines.push(format!("  {} — {}", name, status));
            }
        }

        // Memory embeddings (#1067). A broken embedding key never surfaced
        // anywhere: search silently degrades to keyword-only FTS and keeps
        // answering, so nothing in this report ever went red for it.
        lines.push(String::new());
        lines.push("Memory:".to_string());
        for line in crate::memory::doctor_lines() {
            lines.push(format!("  {line}"));
        }

        // Config keys (#1725) — report unrecognized keys so the agent-side
        // /doctor matches what the Telegram config alert already caught.
        lines.push(String::new());
        lines.push("Config:".to_string());
        {
            let config_path = crate::config::opencrabs_home().join("config.toml");
            if config_path.exists() {
                match std::fs::read_to_string(&config_path) {
                    Ok(raw) => match crate::config::sections::ignored_key_paths(&raw) {
                        Ok(paths) if paths.is_empty() => {
                            lines.push("  ✅ All keys recognized".to_string());
                        }
                        Ok(paths) => {
                            lines.push(format!(
                                "  ❌ {} unrecognized key(s): {}",
                                paths.len(),
                                paths.join(", ")
                            ));
                        }
                        Err(e) => {
                            lines.push(format!("  ❌ Could not parse config.toml: {e}"));
                        }
                    },
                    Err(e) => {
                        lines.push(format!("  ❌ Could not read config.toml: {e}"));
                    }
                }
            } else {
                lines.push("  ⬚ No config.toml found (using defaults)".to_string());
            }
        }

        // Last known good config
        let has_good = crate::config::opencrabs_home()
            .join("config.last_good.toml")
            .exists();
        lines.push(format!(
            "Config recovery: {}",
            if has_good {
                "snapshot available"
            } else {
                "no snapshot"
            }
        ));

        lines.join("\n")
    }
}

#[async_trait]
impl Tool for SlashCommandTool {
    fn name(&self) -> &str {
        "slash_command"
    }

    fn description(&self) -> &str {
        "Execute any OpenCrabs slash command. Built-in: /help, /models (view/switch), \
         /usage (session stats), /doctor (health check), /sessions (list), \
         /profiles (list/switch/create profiles), /approve (get/set policy), \
         /cd (change dir), /compact. \
         Also executes user-defined commands from commands.toml. \
         /models with args='model-name' switches the active model."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The slash command to execute (e.g. '/cd', '/compact', '/deploy'). Must start with '/'."
                },
                "args": {
                    "type": "string",
                    "description": "Optional arguments for the command (e.g. a directory path for /cd)"
                }
            },
            "required": ["command"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WriteFiles]
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let raw_command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let raw_args = input.get("args").and_then(|v| v.as_str()).unwrap_or("");

        // Some models put the whole "/goal debug the build" into `command=`
        // instead of using the separate `args` field. The dispatch below
        // matches the command verbatim ("/goal"), so a multi-word command
        // would fall through to handle_user_command and fail. Normalize first:
        // peel the first token as the command, fold the rest into args.
        let (command, args) = normalize_command(raw_command, raw_args);
        let command = command.as_str();
        let args = args.as_str();

        if !command.starts_with('/') {
            return Ok(ToolResult::error(format!(
                "Command must start with '/'. Got: '{}'",
                command
            )));
        }

        // Setup commands: `/image`, `/voice`, `/channels` (channel-capable),
        // `/brain`, `/workspace`, `/daemon` (TUI-only answers), plus the legacy
        // `/onboard:<step>` spelling as a fallback (#1981). The TUI wizard is
        // interactive; these text handlers write the same config so setup
        // works over Telegram/Discord/etc.
        if let Some(step) = super::slash_onboard::setup_step_for(command) {
            return super::slash_onboard::dispatch(step, args);
        }

        match command {
            "/cd" => self.handle_cd(args, context),
            "/compact" => Ok(ToolResult::success(
                "Compaction requested. Summarize the current conversation for continuity, \
                 then the system will trim context automatically."
                    .into(),
            )),
            "/evolve" => Ok(ToolResult::success(
                "Use the `evolve` tool to check for and install the latest release. \
                 It downloads the pre-built binary from GitHub and hot-restarts."
                    .into(),
            )),
            "/approve" => self.handle_approve(args),
            "/help" => self.handle_help(),
            "/models" => self.handle_models(args),
            "/usage" => self.handle_usage(context).await,
            "/doctor" => self.handle_doctor().await,
            "/sessions" => self.handle_sessions(context).await,
            "/settings" => Ok(ToolResult::success(
                "Settings is a TUI screen (press S). Use config_manager read_config \
                 to view settings programmatically."
                    .into(),
            )),
            "/stop" => Ok(ToolResult::success(
                "Use the cancel mechanism to stop the current operation. \
                 On channels, users type /stop. On TUI, press Escape twice."
                    .into(),
            )),
            "/goal" => self.handle_goal(args, context).await,
            "/profiles" => self.handle_profiles(context).await,
            // Bare `/onboard` and an empty `/onboard:`; the step spellings are
            // routed above (#889, #1981).
            c if c == "/onboard" || c.starts_with("/onboard:") => Ok(ToolResult::success(
                "Onboarding wizard is a TUI-only interactive screen. \
                 However, you can read and modify all settings via config_manager \
                 (read_config, write_config) and manage API keys directly."
                    .into(),
            )),
            "/whisper" => Ok(ToolResult::success(
                "WhisperCrabs is a TUI-triggered command. Tell the user to type /whisper \
                 in the input box to launch the floating voice-to-text tool."
                    .into(),
            )),
            // Plan-lifecycle and session commands are driven by the user on the
            // channel / TUI (and, for the plan itself, by the `plan` tool), not
            // executable from this tool. They used to fall through to the
            // "Unknown command" error arm even though they are real commands the
            // model sees in use (#574); return actionable guidance instead, the
            // same way /settings and /onboard do.
            "/plan" | "/execute" | "/discard" | "/show-plan" | "/showplan" | "/show_plan"
            | "/new" | "/clear" | "/cowork" | "/skills" => {
                Ok(ToolResult::success(channel_command_guidance(command)))
            }
            _ => self.handle_user_command(command, args),
        }
    }
}

/// Actionable guidance for channel/TUI/plan commands this tool cannot execute
/// directly, so the model gets a next step instead of an "Unknown command"
/// error (#574).
fn channel_command_guidance(command: &str) -> String {
    match command {
        "/plan" | "/execute" | "/discard" | "/show-plan" | "/showplan" | "/show_plan" => {
            "Plan mode is driven by the user on the channel and by the `plan` tool, not by \
             this command tool. To build or change the plan yourself, use the `plan` tool \
             (create, add_tasks, start, update). Entering plan mode (/plan), approving \
             (/execute), and discarding (/discard) are user actions — you cannot toggle them \
             from here. /show-plan just opens the plan view for the user."
                .to_string()
        }
        "/new" | "/clear" => {
            "Starting a new session or clearing history is a user/TUI action; this tool \
             cannot create or reset sessions. Continue working in the current session."
                .to_string()
        }
        "/cowork" => "Cowork is a TUI-only interactive mode. Tell the user to type /cowork \
             in the input box to launch it."
            .to_string(),
        other => format!("'{other}' is a channel/TUI command and cannot be run from this tool."),
    }
}

impl SlashCommandTool {
    fn handle_cd(&self, args: &str, context: &ToolExecutionContext) -> Result<ToolResult> {
        let path_str = args.trim();
        if path_str.is_empty() {
            return Ok(ToolResult::error(
                "No directory specified. Usage: slash_command /cd with args='/path/to/dir'".into(),
            ));
        }

        let path = std::path::PathBuf::from(path_str);
        if !path.exists() {
            return Ok(ToolResult::error(format!(
                "Path does not exist: {}",
                path_str
            )));
        }
        if !path.is_dir() {
            return Ok(ToolResult::error(format!(
                "Path is not a directory: {}",
                path_str
            )));
        }

        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("Failed to resolve path: {}", e))),
        };

        // Update runtime working directory
        if let Some(ref shared_wd) = context.shared_working_directory {
            *shared_wd.write().expect("working_directory lock poisoned") = canonical.clone();
        }

        // Persist to session DB — that's the source of truth for per-session WD.
        let mut assigned_project: Option<String> = None;
        if let Some(ref svc_ctx) = context.service_context {
            let svc_ctx = svc_ctx.clone();
            let session_svc = crate::services::SessionService::new(svc_ctx.clone());
            let project_svc = crate::services::ProjectService::new(svc_ctx);
            let sid = context.session_id;
            let dir_str = canonical.to_string_lossy().to_string();
            assigned_project = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let _ = session_svc
                        .update_session_working_directory(sid, Some(dir_str.clone()))
                        .await;

                    // Auto-assign session to project if WD matches a known project dir (#220).
                    // Through the shared matcher so this and the first-turn
                    // link (#1445) cannot answer the same question differently,
                    // and through the same git identity so /cd records the
                    // remote it just proved (#1510).
                    let identity =
                        crate::services::project_match::resolve_directory_identity(&dir_str);
                    if let Ok(projects) = project_svc.list_projects().await
                        && let Some(project) =
                            crate::services::project_match::match_by_directory(&identity, &projects)
                    {
                        if let Err(e) = project_svc.assign_session(sid, project.id).await {
                            tracing::warn!(error = %e, "failed to assign session to project");
                        }
                        if project.repo_remote.is_none()
                            && let Some(ref remote) = identity.remote
                            && let Err(e) = project_svc
                                .set_project_repo_remote(project.id, remote)
                                .await
                        {
                            tracing::warn!(
                                error = %e,
                                "failed to adopt repo remote for project '{}'",
                                project.name
                            );
                        }
                        tracing::info!(
                            "Auto-assigned session {} to project '{}'",
                            sid,
                            project.name
                        );
                        return Some(project.name.clone());
                    }
                    None
                })
            });
        }

        if let Some(ref name) = assigned_project {
            Ok(ToolResult::success(format!(
                "Working directory changed to: {}\nAssigned to project: {}",
                canonical.display(),
                name
            )))
        } else {
            Ok(ToolResult::success(format!(
                "Working directory changed to: {}",
                canonical.display()
            )))
        }
    }

    fn handle_approve(&self, args: &str) -> Result<ToolResult> {
        let policy = args.trim();
        if policy.is_empty() {
            // Read current policy
            return match crate::config::Config::load() {
                Ok(cfg) => Ok(ToolResult::success(format!(
                    "Current approval policy: {}",
                    cfg.agent.approval_policy
                ))),
                Err(e) => Ok(ToolResult::error(format!("Failed to read config: {}", e))),
            };
        }

        // Set policy
        match policy {
            "approve-only" | "auto-session" | "auto-always" => {
                match crate::config::Config::write_key("agent", "approval_policy", policy) {
                    Ok(_written) => Ok(ToolResult::success(format!(
                        "Approval policy set to: {}",
                        policy
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to write config: {}", e))),
                }
            }
            _ => Ok(ToolResult::error(format!(
                "Invalid policy: '{}'. Valid: approve-only, auto-session, auto-always",
                policy
            ))),
        }
    }

    fn handle_help(&self) -> Result<ToolResult> {
        Ok(ToolResult::success(
            "Available commands:\n\
             /help     — Show this list\n\
             /models   — Show current provider/model + available models (args: model name to switch)\n\
             /usage    — Session token & cost stats\n\
             /stop     — Abort current operation (channels: /stop, TUI: Esc×2)\n\
             /doctor   — Run connection health check on all providers/channels\n\
             /sessions — List all sessions with stats\n\
             /approve  — Get or set approval policy (args: approve-only|auto-session|auto-always)\n\
             /cd       — Change working directory (args: path)\n\
             /compact  — Compact context (summarize + trim)\n\
             /evolve   — Upgrade to the latest RELEASE & hot-restart (normal update path)\n\
             /goal     — Set/view/pause/clear session goal\n\
             /profiles — List/switch/create/manage profiles\n\
             /whisper  — Voice-to-text (TUI only)\n\
             /onboard  — Setup wizard (TUI only, use config_manager for programmatic changes)\n\
             /channels — Channel setup from chat (args: telegram|discord|whatsapp ...)\n\
             /voice    — Voice STT/TTS setup from chat (args: stt|tts ...)\n\
             /image    — Vision + image generation setup from chat\n\
             /brain /workspace /daemon — TUI only\n\n\
             You can also use config_manager to read/write any config setting directly."
                .into(),
        ))
    }

    fn handle_models(&self, args: &str) -> Result<ToolResult> {
        let config = match crate::config::Config::load() {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::error(format!("Failed to load config: {}", e))),
        };

        let model_arg = args.trim();

        // If a model name was provided, switch to it
        if !model_arg.is_empty() {
            let (provider_id, model) = match resolve_model_target(&config, model_arg) {
                Ok(pair) => pair,
                Err(e) => return Ok(ToolResult::error(e)),
            };
            let Some(section) = section_for_provider(&config, &provider_id) else {
                // Reachable only if a provider is known but has no section —
                // a registry gap. Name it rather than writing somewhere else.
                return Ok(ToolResult::error(format!(
                    "Provider '{provider_id}' has no config section to write to. \
                     This is a provider-registry gap — please report it."
                )));
            };
            return match crate::config::Config::write_key(&section, "default_model", &model) {
                Ok(_written) => Ok(ToolResult::success(format!(
                    "Model switched to '{model}' on provider '{provider_id}'. \
                     Config updated at [{section}].default_model. \
                     The change takes effect on the next request."
                ))),
                Err(e) => Ok(ToolResult::error(format!(
                    "Failed to write [{section}].default_model: {e}"
                ))),
            };
        }

        // No args — return current provider/model info
        let mut lines = Vec::new();

        let providers_info = [
            ("anthropic", &config.providers.anthropic),
            ("openai", &config.providers.openai),
            ("gemini", &config.providers.gemini),
            ("openrouter", &config.providers.openrouter),
            ("minimax", &config.providers.minimax),
            ("claude-cli", &config.providers.claude_cli),
        ];

        for (name, provider_opt) in &providers_info {
            if let Some(provider) = provider_opt {
                let status = if provider.enabled {
                    "active"
                } else {
                    "disabled"
                };
                let model = provider.default_model.as_deref().unwrap_or("(not set)");
                let models_list = if provider.models.is_empty() {
                    String::new()
                } else {
                    format!("\n    Available: {}", provider.models.join(", "))
                };
                lines.push(format!(
                    "  {} [{}]: model={}{}",
                    name, status, model, models_list
                ));
            }
        }

        // Custom providers
        if let Some(ref custom) = config.providers.custom {
            for (name, provider) in custom {
                let status = if provider.enabled {
                    "active"
                } else {
                    "disabled"
                };
                let model = provider.default_model.as_deref().unwrap_or("(not set)");
                let models_list = if provider.models.is_empty() {
                    String::new()
                } else {
                    format!("\n    Available: {}", provider.models.join(", "))
                };
                lines.push(format!(
                    "  custom/{} [{}]: model={}{}",
                    name, status, model, models_list
                ));
            }
        }

        if lines.is_empty() {
            lines.push("  No providers configured.".to_string());
        }

        Ok(ToolResult::success(format!(
            "Providers:\n{}\n\n\
             To switch model: use slash_command /models with args='<model-name>'\n\
             To change provider: use config_manager write_config on the provider section.",
            lines.join("\n")
        )))
    }

    async fn handle_usage(&self, context: &ToolExecutionContext) -> Result<ToolResult> {
        let svc_ctx = match &context.service_context {
            Some(ctx) => ctx.clone(),
            None => {
                return Ok(ToolResult::error(
                    "Service context not available — cannot query session data.".into(),
                ));
            }
        };

        let session_svc = crate::services::SessionService::new(svc_ctx);
        let session_id = context.session_id;

        let mut lines = vec!["Usage Stats".to_string(), String::new()];

        // Current session
        match session_svc.get_session(session_id).await {
            Ok(Some(session)) => {
                let name = session.title.as_deref().unwrap_or("Current Session");
                let model = session.model.as_deref().unwrap_or("(unknown)");
                lines.push(format!("Current Session: {}", name));
                lines.push(format!("  Model: {}", model));
                lines.push(format!("  Lifetime tokens: {}", session.token_count));
                lines.push(format!("  Cost: ${:.4}", session.total_cost));
            }
            _ => {
                lines.push("Current Session: (not found)".to_string());
            }
        }

        // All-time stats
        lines.push(String::new());
        {
            use crate::db::repository::UsageLedgerRepository;
            let ledger = UsageLedgerRepository::new(session_svc.pool());
            let ledger_stats = ledger.stats_by_model().await.unwrap_or_default();

            let all_tokens: i64 = ledger_stats.iter().map(|s| s.total_tokens).sum();
            let all_cost: f64 = ledger_stats.iter().map(|s| s.total_cost).sum();

            let total_sessions = session_svc
                .list_sessions(crate::db::repository::SessionListOptions::default())
                .await
                .map(|s| s.len())
                .unwrap_or(0);

            lines.push(format!(
                "All-Time: {} sessions, {} tokens, ${:.4}",
                total_sessions, all_tokens, all_cost
            ));

            for stats in ledger_stats.iter().take(10) {
                lines.push(format!(
                    "  {} — {} tokens, ${:.4}",
                    stats.model, stats.total_tokens, stats.total_cost
                ));
            }
        }

        // Decision reuse (#1648 PR3): per-tier counters, silent until
        // something has been measured.
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
            if let Some(block) = crate::decisions::report::render_usage_lines(
                &stats,
                &cached_rows,
                &cfg.decisions.tiers,
            ) {
                lines.push(String::new());
                lines.extend(block);
            }
        }

        Ok(ToolResult::success(lines.join("\n")))
    }

    async fn handle_doctor(&self) -> Result<ToolResult> {
        let config = match crate::config::Config::load() {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::error(format!("Failed to load config: {}", e))),
        };

        let version = env!("CARGO_PKG_VERSION");
        let mut lines = vec![format!("Health Check (v{version})"), String::new()];

        // Check keys.toml validity
        let keys_path = crate::config::keys_path();
        if keys_path.exists() {
            match std::fs::read_to_string(&keys_path) {
                Ok(content) => match toml::from_str::<toml::Value>(&content) {
                    Ok(_) => lines.push("keys.toml — OK".to_string()),
                    Err(e) => lines.push(format!("keys.toml — PARSE ERROR: {e}")),
                },
                Err(e) => lines.push(format!("keys.toml — READ ERROR: {e}")),
            }
        } else {
            lines.push("keys.toml — NOT FOUND".to_string());
        }
        lines.push(String::new());

        // Check providers
        let providers = [
            ("anthropic", &config.providers.anthropic),
            ("openai", &config.providers.openai),
            ("gemini", &config.providers.gemini),
            ("openrouter", &config.providers.openrouter),
            ("minimax", &config.providers.minimax),
        ];

        lines.push("Providers:".to_string());
        for (name, provider_opt) in &providers {
            if let Some(provider) = provider_opt
                && provider.enabled
            {
                let has_key = provider.api_key.as_ref().is_some_and(|k| !k.is_empty());
                let model = provider.default_model.as_deref().unwrap_or("(not set)");
                let status = if has_key { "OK" } else { "MISSING API KEY" };
                lines.push(format!("  {} — {} (model: {})", name, status, model));
            }
        }

        if let Some(ref custom) = config.providers.custom {
            for (name, provider) in custom {
                if provider.enabled {
                    let has_key = provider.api_key.as_ref().is_some_and(|k| !k.is_empty());
                    let model = provider.default_model.as_deref().unwrap_or("(not set)");
                    let status = if has_key { "OK" } else { "MISSING API KEY" };
                    lines.push(format!("  custom/{} — {} (model: {})", name, status, model));
                }
            }
        }

        // Check channels
        lines.push(String::new());
        lines.push("Channels:".to_string());
        let ch = &config.channels;
        if ch.telegram.enabled {
            lines.push("  telegram — enabled".to_string());
        }
        if ch.discord.enabled {
            lines.push("  discord — enabled".to_string());
        }
        if ch.slack.enabled {
            lines.push("  slack — enabled".to_string());
        }
        if ch.whatsapp.enabled {
            lines.push("  whatsapp — enabled".to_string());
        }
        if ch.trello.enabled {
            lines.push("  trello — enabled".to_string());
        }

        // Voice config
        lines.push(String::new());
        lines.push(format!(
            "Voice: STT={}, TTS={}",
            config.voice_config().stt_enabled,
            config.voice_config().tts_enabled
        ));

        // Approval policy
        lines.push(format!("Approval: {}", config.agent.approval_policy));

        Ok(ToolResult::success(lines.join("\n")))
    }

    async fn handle_sessions(&self, context: &ToolExecutionContext) -> Result<ToolResult> {
        let svc_ctx = match &context.service_context {
            Some(ctx) => ctx.clone(),
            None => {
                return Ok(ToolResult::error(
                    "Service context not available — cannot query sessions.".into(),
                ));
            }
        };

        let session_svc = crate::services::SessionService::new(svc_ctx);
        match session_svc
            .list_sessions(crate::db::repository::SessionListOptions::default())
            .await
        {
            Ok(sessions) => {
                if sessions.is_empty() {
                    return Ok(ToolResult::success("No sessions found.".into()));
                }

                let current_id = context.session_id;
                let mut lines = vec![format!("{} session(s):\n", sessions.len())];
                for s in sessions.iter().take(20) {
                    let title = s.title.as_deref().unwrap_or("(untitled)");
                    let model = s.model.as_deref().unwrap_or("?");
                    let marker = if s.id == current_id {
                        " ← current"
                    } else {
                        ""
                    };
                    lines.push(format!(
                        "  {} [{}] — {} tokens, ${:.4}{}",
                        title, model, s.token_count, s.total_cost, marker
                    ));
                }
                if sessions.len() > 20 {
                    lines.push(format!("  ... and {} more", sessions.len() - 20));
                }
                Ok(ToolResult::success(lines.join("\n")))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to list sessions: {}", e))),
        }
    }

    async fn handle_goal(&self, args: &str, context: &ToolExecutionContext) -> Result<ToolResult> {
        let svc_ctx = match &context.service_context {
            Some(ctx) => ctx.clone(),
            None => {
                return Ok(ToolResult::error(
                    "Service context not available — cannot manage goals.".into(),
                ));
            }
        };

        let goal_mgr = crate::brain::goal::GoalManager::new(svc_ctx);
        let session_id = context.session_id;
        let trimmed = args.trim();

        // Subcommands
        match trimmed.to_lowercase().as_str() {
            "" | "status" => {
                // Show current goal
                match goal_mgr.get_goal(session_id).await {
                    Ok(Some(goal)) => {
                        let elapsed = chrono::Utc::now()
                            .signed_duration_since(
                                chrono::DateTime::parse_from_rfc3339(&goal.created_at)
                                    .unwrap_or_default(),
                            )
                            .num_minutes();
                        Ok(ToolResult::success(format!(
                            "🎯 Active Goal ({}):\n\n{}\n\nState: {} | {} | Elapsed: {}m",
                            goal.id,
                            goal.goal_text,
                            goal.state,
                            crate::brain::goal::driver_line(&goal),
                            elapsed,
                        )))
                    }
                    Ok(None) => Ok(ToolResult::success(
                        "No active goal for this session. \
                         Use `/goal <description>` to set one."
                            .into(),
                    )),
                    Err(e) => Ok(ToolResult::error(format!("Failed to get goal: {}", e))),
                }
            }
            "clear" | "cancel" | "stop" => match goal_mgr.clear_goal(session_id).await {
                Ok(()) => Ok(ToolResult::success("Goal cleared.".into())),
                Err(e) => Ok(ToolResult::error(format!("Failed to clear goal: {}", e))),
            },
            "pause" => match goal_mgr.pause_goal(session_id).await {
                Ok(()) => Ok(ToolResult::success("Goal paused.".into())),
                Err(e) => Ok(ToolResult::error(format!("Failed to pause goal: {}", e))),
            },
            "resume" => match goal_mgr.resume_goal(session_id).await {
                Ok(()) => Ok(ToolResult::success("Goal resumed.".into())),
                Err(e) => Ok(ToolResult::error(format!("Failed to resume goal: {}", e))),
            },
            _ => {
                // Set a new goal
                let (max_turns, goal_text) = parse_goal_args(trimmed);
                if goal_text.is_empty() {
                    return Ok(ToolResult::error(
                        "Please specify a goal description, e.g. `/goal fix all failing tests` or `/goal --turns 30 fix tests`.".into(),
                    ));
                }
                match goal_mgr
                    .set_goal(session_id, goal_text.to_string(), None, None, max_turns)
                    .await
                {
                    Ok(goal) => Ok(ToolResult::success(format!(
                        "🎯 Goal set (ID: {}):\n\n{}\n\n\
                         The agent will work toward this goal autonomously \
                         for up to {} turns. Use `/goal status` to check \
                         progress, `/goal pause` to pause, `/goal clear` to remove.",
                        goal.id, goal.goal_text, goal.max_turns,
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to set goal: {}", e))),
                }
            }
        }
    }

    async fn handle_profiles(&self, _context: &ToolExecutionContext) -> Result<ToolResult> {
        use crate::config::profile::{active_profile, list_profiles};

        let active = active_profile().unwrap_or("default");
        let profiles = list_profiles().unwrap_or_default();

        if profiles.is_empty() {
            return Ok(ToolResult::success(
                "No profiles found. Use `opencrabs profile create <name>` to create one.".into(),
            ));
        }

        let mut lines = vec![format!("👤 Profiles (active: `{}`)", active), String::new()];

        for p in &profiles {
            let marker = if p.name == active { "▸ " } else { "  " };
            let desc = p
                .description
                .as_deref()
                .filter(|d| !d.is_empty())
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            lines.push(format!("{}{}{}", marker, p.name, desc));
        }

        lines.push(String::new());
        lines.push(
            "Manage via CLI: `opencrabs profile create <name>`, \
             `opencrabs profile switch <name>`, \
             `opencrabs -p <name>` to launch under a profile."
                .to_string(),
        );

        Ok(ToolResult::success(lines.join("\n")))
    }

    fn handle_user_command(&self, command: &str, args: &str) -> Result<ToolResult> {
        let brain_path = crate::brain::BrainLoader::resolve_path();
        let loader = crate::brain::CommandLoader::from_brain_path(&brain_path);
        let commands = loader.load();

        if let Some(cmd) = commands.iter().find(|c| c.name == command) {
            match cmd.action.as_str() {
                "system" => Ok(ToolResult::success(with_user_args(
                    format!("[System message] {}", cmd.prompt),
                    args,
                ))),
                _ => {
                    // "prompt" action — return the prompt for the agent to execute
                    Ok(ToolResult::success(with_user_args(
                        format!(
                            "User command '{}' ({}): {}",
                            cmd.name, cmd.description, cmd.prompt
                        ),
                        args,
                    )))
                }
            }
        } else if let Some(skill) = crate::brain::skills::load_all_skills()
            .into_iter()
            .find(|s| s.slash_name == command)
        {
            // Skills are invoked by the same `/<name>` syntax as commands, but
            // only commands.toml was consulted, so every skill reached the
            // "Unknown command" arm (#889). `/servers` and `/channels` are
            // skills on disk and failed for exactly this reason.
            Ok(ToolResult::success(with_user_args(
                format!(
                    "Skill '{}' ({}): {}",
                    skill.name, skill.description, skill.body
                ),
                args,
            )))
        } else {
            // List available commands for context
            let available: Vec<String> = commands.iter().map(|c| c.name.clone()).collect();
            let builtin = [
                "/cd",
                "/compact",
                "/evolve",
                "/approve",
                "/models",
                "/sessions",
                "/help",
                "/usage",
                "/doctor",
                "/stop",
                "/settings",
                "/onboard",
                "/channels",
                "/voice",
                "/image",
                "/brain",
                "/workspace",
                "/daemon",
                "/whisper",
                "/goal",
                "/profiles",
            ];
            Ok(ToolResult::error(format!(
                "Unknown command: '{}'. Not a built-in, a commands.toml entry, or a skill. \
                 Built-in: {}. User-defined: {}",
                command,
                builtin.join(", "),
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            )))
        }
    }
}

fn parse_goal_args(args: &str) -> (Option<u32>, &str) {
    let s = args.trim();
    if let Some(rest) = s.strip_prefix("--turns ").or_else(|| s.strip_prefix("-t ")) {
        if let Some((turns_str, rem)) = rest.trim_start().split_once(' ')
            && let Ok(turns) = turns_str.parse::<u32>()
        {
            return (Some(turns), rem.trim());
        }
    } else if let Some(rest) = s.strip_prefix("--max-turns ")
        && let Some((turns_str, rem)) = rest.trim_start().split_once(' ')
        && let Ok(turns) = turns_str.parse::<u32>()
    {
        return (Some(turns), rem.trim());
    }
    (None, s)
}

/// #1868: everything typed after the command name was dropped on the floor
/// (`_args` in `handle_user_command`), so `/findfile *.rs` handed the agent
/// the command prompt with none of the user's request attached. The agent
/// gets both halves now: the rendered prompt and the args that qualify it.
/// Empty input changes nothing.
pub(crate) fn with_user_args(rendered: String, args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        rendered
    } else {
        format!("{rendered}\n\nUser args: {args}")
    }
}
