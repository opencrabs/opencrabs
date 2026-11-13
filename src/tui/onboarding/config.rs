use std::path::PathBuf;

use crate::config::Config;

use super::types::*;
use super::wizard::OnboardingWizard;
use crate::tui::provider_selector::CUSTOM_PROVIDER_IDX;

/// Try to write a config key, collecting errors into a Vec for later reporting.
macro_rules! try_write {
    ($errors:expr, $section:expr, $key:expr, $val:expr) => {
        if let Err(e) = Config::write_key($section, $key, $val) {
            tracing::warn!("Failed to write {}.{}: {}", $section, $key, e);
            // Keep the REASON, not just the key name (#915). The error was
            // logged and discarded, so the user was handed a list of keys and
            // a guess about file permissions with nothing behind it.
            $errors.push(format!("{}.{}: {}", $section, $key, e));
        }
    };
}

/// Try to write a keys.toml key, collecting errors into a Vec for later reporting.
macro_rules! try_write_keys {
    ($errors:expr, $section:expr, $key:expr, $val:expr) => {
        if let Err(e) = Config::write_keys_key($section, $key, $val) {
            tracing::warn!("Failed to write keys.toml {}.{}: {}", $section, $key, e);
            $errors.push(format!("keys.toml {}.{}: {}", $section, $key, e));
        }
    };
}

/// Try to write a config array, collecting errors into a Vec for later reporting.
macro_rules! try_write_array {
    ($errors:expr, $section:expr, $key:expr, $val:expr) => {
        if let Err(e) = Config::write_array($section, $key, $val) {
            tracing::warn!("Failed to write {}.{}: {}", $section, $key, e);
            $errors.push(format!("{}.{}", $section, $key));
        }
    };
}

impl OnboardingWizard {
    /// Ensure config.toml and keys.toml exist in the workspace directory
    pub(super) fn ensure_config_files(&mut self) -> Result<(), String> {
        let workspace_path = std::path::PathBuf::from(&self.workspace_path);

        // Create workspace directory if it doesn't exist
        if !workspace_path.exists() {
            std::fs::create_dir_all(&workspace_path)
                .map_err(|e| format!("Failed to create workspace directory: {}", e))?;
        }

        // Seeding is shared with every other path that can create these files
        // (#1437). It used to live here alone, which meant a run that never
        // reached this step, or a file some earlier write had already brought
        // into being, left the user without the provider sections the example
        // exists to give them.
        crate::config::seed::ensure_config_seeded();
        crate::config::seed::ensure_keys_seeded();

        // Ensure usage_pricing.toml exists and is up to date
        // (also called on startup, but onboarding may run before that path)
        crate::usage::pricing::PricingConfig::seed_from_example();

        // Reload models for the selected provider from the newly created config
        self.ps.reload_config_models();

        Ok(())
    }

    /// Initialize health check results
    pub fn start_health_check(&mut self) {
        // Reload config from disk so re-check picks up external changes
        if self.quick_jump
            && let Ok(config) = crate::config::Config::load()
        {
            let fresh = Self::from_config(&config);
            self.ps.api_key_input = fresh.ps.api_key_input;
            self.ps.selected_provider = fresh.ps.selected_provider;
            self.workspace_path = fresh.workspace_path;
            self.channel_toggles = fresh.channel_toggles;
            self.telegram_token_input = fresh.telegram_token_input;
            self.telegram_user_id_input = fresh.telegram_user_id_input;
            self.discord_token_input = fresh.discord_token_input;
            self.discord_channel_id_input = fresh.discord_channel_id_input;
            self.slack_bot_token_input = fresh.slack_bot_token_input;
            self.slack_app_token_input = fresh.slack_app_token_input;
            self.slack_channel_id_input = fresh.slack_channel_id_input;
            self.trello_api_key_input = fresh.trello_api_key_input;
            self.trello_api_token_input = fresh.trello_api_token_input;
            self.trello_board_id_input = fresh.trello_board_id_input;
            self.whatsapp_connected = fresh.whatsapp_connected;
            self.image_vision_enabled = fresh.image_vision_enabled;
            self.image_generation_enabled = fresh.image_generation_enabled;
            self.image_api_key_input = fresh.image_api_key_input;
        }

        let auth_label = if self.ps.is_cli() {
            "CLI Binary Found"
        } else if self.ps.is_keyless() {
            "Keyless Provider"
        } else {
            "API Key Present"
        };
        let mut checks = vec![
            (auth_label.to_string(), HealthStatus::Pending),
            ("Config File".to_string(), HealthStatus::Pending),
            ("Workspace Directory".to_string(), HealthStatus::Pending),
            ("Template Files".to_string(), HealthStatus::Pending),
        ];

        // Add channel-specific checks for enabled channels
        if self.is_telegram_enabled() {
            checks.push(("Telegram Token".to_string(), HealthStatus::Pending));
            checks.push(("Telegram User ID".to_string(), HealthStatus::Pending));
        }
        if self.is_discord_enabled() {
            checks.push(("Discord Token".to_string(), HealthStatus::Pending));
            checks.push(("Discord Channel ID".to_string(), HealthStatus::Pending));
        }
        if self.is_slack_enabled() {
            checks.push(("Slack Bot Token".to_string(), HealthStatus::Pending));
            checks.push(("Slack Channel ID".to_string(), HealthStatus::Pending));
        }
        if self.is_whatsapp_enabled() {
            checks.push(("WhatsApp Connected".to_string(), HealthStatus::Pending));
        }
        if self.is_trello_enabled() {
            checks.push(("Trello API Key".to_string(), HealthStatus::Pending));
            checks.push(("Trello API Token".to_string(), HealthStatus::Pending));
            checks.push(("Trello Board ID".to_string(), HealthStatus::Pending));
        }
        if self.image_vision_enabled || self.image_generation_enabled {
            checks.push(("Google Image API Key".to_string(), HealthStatus::Pending));
        }

        self.health_results = checks;
        self.health_running = true;
        self.health_complete = false;
    }

    /// Resolve pending health checks (call from tick to show Pending state for one frame).
    pub fn tick_health_check(&mut self) {
        if self.health_running && !self.health_complete {
            self.run_health_checks();
        }
    }

    /// Execute all health checks
    fn run_health_checks(&mut self) {
        // Check 1: API key / CLI binary present
        self.health_results[0].1 = if self.ps.is_cli() {
            // CLI providers: check if the binary is installed
            let binary = match self.ps.provider_id() {
                "claude-cli" => "claude",
                "codex-cli" => "codex",
                "command-code-cli" => "command-code",
                _ => "opencode",
            };
            if which::which(binary).is_ok() {
                HealthStatus::Pass
            } else {
                HealthStatus::Fail(format!("'{}' CLI not found in PATH", binary))
            }
        } else if self.ps.is_keyless() {
            // Keyless / local providers (Ollama, llama.cpp, etc.) need no API
            // key — the local server supplies it. Failing them on "No API key
            // provided" wrongly blocked onboarding.
            HealthStatus::Pass
        } else if !self.ps.api_key_input.is_empty() || !self.ps.base_url.is_empty() {
            // A key, or an explicit endpoint (custom provider, or a local /
            // self-hosted base_url like Ollama on localhost) both count: a
            // configured endpoint commonly needs no key.
            HealthStatus::Pass
        } else {
            HealthStatus::Fail("No API key provided".to_string())
        };

        // Check 2: Config path writable
        let config_path = crate::config::opencrabs_home().join("config.toml");
        self.health_results[1].1 = if let Some(parent) = config_path.parent() {
            if parent.exists() || std::fs::create_dir_all(parent).is_ok() {
                HealthStatus::Pass
            } else {
                HealthStatus::Fail(format!("Cannot create {}", parent.display()))
            }
        } else {
            HealthStatus::Fail("Invalid config path".to_string())
        };

        // Check 3: Workspace directory
        let workspace = PathBuf::from(&self.workspace_path);
        self.health_results[2].1 =
            if workspace.exists() || std::fs::create_dir_all(&workspace).is_ok() {
                HealthStatus::Pass
            } else {
                HealthStatus::Fail(format!("Cannot create {}", workspace.display()))
            };

        // Check 4: Template files available (they're compiled in, always present)
        self.health_results[3].1 = HealthStatus::Pass;

        // Channel checks (by name, since indices depend on which channels are enabled)
        for i in 0..self.health_results.len() {
            let name = self.health_results[i].0.clone();
            self.health_results[i].1 = match name.as_str() {
                "Telegram Token" => {
                    if !self.telegram_token_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail("No token provided".to_string())
                    }
                }
                "Telegram User ID" => {
                    if !self.telegram_user_id_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail("No user ID — bot won't know who to talk to".to_string())
                    }
                }
                "Discord Token" => {
                    if !self.discord_token_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail("No token provided".to_string())
                    }
                }
                "Discord Channel ID" => {
                    if !self.discord_channel_id_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail(
                            "No channel ID — bot won't know where to post".to_string(),
                        )
                    }
                }
                "Slack Bot Token" => {
                    if !self.slack_bot_token_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail("No bot token provided".to_string())
                    }
                }
                "Slack Channel ID" => {
                    if !self.slack_channel_id_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail(
                            "No channel ID — bot won't know where to post".to_string(),
                        )
                    }
                }
                "WhatsApp Connected" => {
                    if self.whatsapp_connected {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail("Not paired — scan QR code to connect".to_string())
                    }
                }
                "Trello API Key" => {
                    if !self.trello_api_key_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail("No API Key provided".to_string())
                    }
                }
                "Trello API Token" => {
                    if !self.trello_api_token_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail("No API Token provided".to_string())
                    }
                }
                "Trello Board ID" => {
                    if !self.trello_board_id_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail(
                            "No Board ID — agent won't know which board to poll".to_string(),
                        )
                    }
                }
                "Google Image API Key" => {
                    if !self.image_api_key_input.is_empty() {
                        HealthStatus::Pass
                    } else {
                        HealthStatus::Fail(
                            "No API key — vision and image generation need a Google AI key"
                                .to_string(),
                        )
                    }
                }
                _ => continue, // Already set above
            };
        }

        self.health_running = false;
        self.health_complete = true;
    }

    /// Check if all health checks passed
    pub fn all_health_passed(&self) -> bool {
        self.health_complete
            && self
                .health_results
                .iter()
                .all(|(_, s)| matches!(s, HealthStatus::Pass))
    }

    /// Apply wizard configuration — creates config.toml, stores API key, seeds workspace
    /// Merges with existing config to preserve settings not modified in wizard.
    ///
    /// In quick_jump mode, only writes settings relevant to the current step to avoid
    /// overwriting unrelated channel/provider settings loaded with defaults.
    pub fn apply_config(&self) -> Result<(), String> {
        // Determine which sections to write based on quick_jump + current step
        // Provider flags are disk truth the moment any writer lands them, on
        // the same reasoning as voice below. A quick-jump run that opened a
        // DIFFERENT page and walked to Complete used to count as a real
        // provider save, sweep every flag to false, and leave one provider on
        // (#2265). Only a run that actually went through ProviderAuth owns
        // these writes.
        let write_provider = if self.quick_jump {
            matches!(self.step, OnboardingStep::ProviderAuth) || self.provider_step_touched
        } else {
            true
        };
        let write_channels = !self.quick_jump
            || matches!(
                self.step,
                OnboardingStep::Channels
                    | OnboardingStep::TelegramSetup
                    | OnboardingStep::DiscordSetup
                    | OnboardingStep::WhatsAppSetup
                    | OnboardingStep::SlackSetup
                    | OnboardingStep::TrelloSetup
                    | OnboardingStep::Complete
            );
        // Voice flags are disk truth the moment any writer lands them (#1233).
        // Write here only if THIS RUN touched VoiceSetup; reaching Complete via
        // other steps must preserve on-disk [providers.stt.*] / [providers.tts.*]
        // flags instead of recomputing them from stale wizard page state.
        let write_voice =
            self.voice_step_touched || matches!(self.step, OnboardingStep::VoiceSetup);
        let write_image = !self.quick_jump
            || matches!(
                self.step,
                OnboardingStep::ImageSetup | OnboardingStep::Complete
            );

        self.write_scoped_config(
            write_provider,
            write_channels,
            write_voice,
            write_image,
            true,
        )
    }

    /// Step-scoped save (#926): commit ONLY the config section owned by
    /// `completed_step`. Called on every successful wizard transition so an
    /// interrupted onboard keeps each step already confirmed, instead of
    /// losing all of it to the single end-of-wizard write.
    ///
    /// Reuses the exact write path of `apply_config` (merge via write_key,
    /// never whole-file overwrite), so the two cannot drift apart. Template
    /// seeding and daemon install are completion tasks, not config sections,
    /// so they stay out of step-scoped saves. Steps that own no section
    /// (mode, workspace, daemon, health, brain, complete) save nothing.
    pub fn apply_step_config(&self, completed_step: OnboardingStep) -> Result<(), String> {
        let (section, write_provider, write_channels, write_voice, write_image) =
            match completed_step {
                OnboardingStep::ProviderAuth => ("provider", true, false, false, false),
                OnboardingStep::Channels
                | OnboardingStep::TelegramSetup
                | OnboardingStep::DiscordSetup
                | OnboardingStep::WhatsAppSetup
                | OnboardingStep::SlackSetup
                | OnboardingStep::TrelloSetup => ("channels", false, true, false, false),
                OnboardingStep::VoiceSetup => ("voice", false, false, true, false),
                OnboardingStep::ImageSetup => ("image", false, false, false, true),
                // Owns no config section, nothing to persist.
                _ => return Ok(()),
            };
        self.write_scoped_config(
            write_provider,
            write_channels,
            write_voice,
            write_image,
            false,
        )
        .map_err(|e| format!("{} step: {}", section, e))
    }

    /// Config section for the provider this run has selected.
    pub(super) fn selected_provider_section(&self) -> String {
        if self.ps.selected_provider < CUSTOM_PROVIDER_IDX {
            let id = PROVIDERS[self.ps.selected_provider].id;
            crate::utils::providers::find_provider_meta(id)
                .map(|m| m.config_section.to_string())
                .unwrap_or_else(|| "providers.anthropic".to_string())
        } else {
            format!("providers.custom.{}", self.ps.custom_name)
        }
    }

    /// Providers enabled on disk right now that are NOT the one this run
    /// selected, i.e. the flags a provider save is about to clear (#2265).
    ///
    /// Reads the file instead of the merged `Config`: the sweep writes through
    /// `write_key`, so what matters is what is on disk before the save.
    pub fn providers_to_be_disabled(&self) -> Vec<String> {
        let path = crate::config::opencrabs_home().join("config.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let Ok(doc) = text.parse::<toml::Value>() else {
            return Vec::new();
        };
        let Some(table) = doc.get("providers").and_then(|p| p.as_table()) else {
            return Vec::new();
        };
        let selected = self.selected_provider_section();
        let mut out = Vec::new();
        for (name, value) in table {
            if name == "custom" {
                if let Some(customs) = value.as_table() {
                    for (custom_name, custom_value) in customs {
                        let section = format!("providers.custom.{custom_name}");
                        if section != selected
                            && custom_value.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        {
                            out.push(format!("custom.{custom_name}"));
                        }
                    }
                }
                continue;
            }
            let section = format!("providers.{name}");
            if section != selected && value.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
                out.push(name.clone());
            }
        }
        out
    }

    /// Shared write path behind `apply_config` and `apply_step_config`.
    /// `finalize` also runs the completion-only tasks (template seeding,
    /// daemon install) in their original position; step-scoped saves pass
    /// false so a mid-wizard save never seeds templates or installs services.
    fn write_scoped_config(
        &self,
        write_provider: bool,
        write_channels: bool,
        write_voice: bool,
        write_image: bool,
        finalize: bool,
    ) -> Result<(), String> {
        // Empty-custom_name guard: a custom provider with no name typed yet
        // would format the section as `providers.custom.` (empty subkey) and
        // corrupt config.toml, so the write cannot proceed without one.
        //
        // This used to set `write_provider = false` and carry on: the ENTIRE
        // provider write — key, base URL, model, context window — was dropped
        // with only a `tracing::warn!`, while the wizard reported success. A
        // first-time user configured a custom provider, saw no error, and ended
        // up with nothing saved (#914).
        //
        // Refusing loudly is the only honest option. Silently discarding what
        // someone just typed and calling it saved is worse than failing.
        if write_provider
            && self.ps.selected_provider >= CUSTOM_PROVIDER_IDX
            && self.ps.custom_name.trim().is_empty()
        {
            tracing::warn!("Refusing provider write: custom provider selected with an empty name");
            return Err(
                "Custom provider needs a name before it can be saved — nothing was written. \
                 Go back to the Name field, enter one (e.g. 'lm_studio'), and save again."
                    .to_string(),
            );
        }

        // Groq key for STT/TTS
        let groq_key = if !self.groq_api_key_input.is_empty() && !self.has_existing_groq_key() {
            Some(self.groq_api_key_input.clone())
        } else {
            None
        };

        // Write config.toml via merge (write_key) — never overwrite entire file
        let mut write_errors: Vec<String> = Vec::new();

        // Provider settings — only when relevant step is active
        let custom_section;
        let section = if self.ps.selected_provider < CUSTOM_PROVIDER_IDX {
            let id = PROVIDERS[self.ps.selected_provider].id;
            crate::utils::providers::find_provider_meta(id)
                .map(|m| m.config_section)
                .unwrap_or("providers.anthropic")
        } else {
            custom_section = format!("providers.custom.{}", self.ps.custom_name);
            &custom_section
        };

        if write_provider {
            // Disable all providers first, then enable selected one. The
            // exclusive enable itself stays: a save that went through this
            // step must leave exactly one provider on, for the resolver
            // ladder (#2262). What must not happen is the sweep running for a
            // run that never opened this page (#2265).
            {
                let all_sections = if let Ok(cfg) = Config::load() {
                    crate::utils::providers::all_config_sections(&cfg.providers)
                } else {
                    crate::utils::providers::KNOWN_PROVIDERS
                        .iter()
                        .map(|p| p.config_section.to_string())
                        .collect()
                };
                for s in &all_sections {
                    if let Err(e) = Config::write_key(s, "enabled", "false") {
                        tracing::warn!("Failed to write {}.enabled: {}", s, e);
                        write_errors.push(format!("{}.enabled", s));
                    }
                }
            }

            // Enable + configure the selected provider
            let section_owned = self.selected_provider_section();
            let section = section_owned.as_str();
            try_write!(write_errors, section, "enabled", "true");
            let model = self.ps.selected_model_name().to_string();
            if !model.is_empty() {
                try_write!(write_errors, section, "default_model", &model);
            }

            // Write base_url / extra config for providers that need it
            match self.ps.provider_id() {
                "github" => {
                    try_write!(
                        write_errors,
                        section,
                        "base_url",
                        "https://api.githubcopilot.com/chat/completions"
                    );
                }
                "openrouter" => {
                    try_write!(
                        write_errors,
                        section,
                        "base_url",
                        "https://openrouter.ai/api/v1/chat/completions"
                    );
                }
                "minimax" => {
                    try_write!(
                        write_errors,
                        section,
                        "base_url",
                        "https://api.minimax.io/v1"
                    );
                }
                "opencode" => {
                    // The plan picks the catalog and the chat endpoint together (#2040).
                    let (plan, base) =
                        crate::tui::provider_selector::ProviderSelectorState::opencode_endpoint(
                            self.ps.opencode_endpoint_type,
                        );
                    try_write!(write_errors, section, "endpoint_type", plan);
                    try_write!(write_errors, section, "base_url", base);
                }
                "zai" => {
                    let endpoint_type = if self.ps.zhipu_endpoint_type == 1 {
                        "coding"
                    } else {
                        "api"
                    };
                    try_write!(write_errors, section, "endpoint_type", endpoint_type);
                }
                "xiaomi" => {
                    // Endpoint type: "api" (default) or "token-plan"
                    let endpoint_type = if self.ps.xiaomi_endpoint_type == 1 {
                        "token-plan"
                    } else {
                        "api"
                    };
                    try_write!(write_errors, section, "endpoint_type", endpoint_type);
                    // Cap MiMo at 200k. It advertises ~1M but degrades past
                    // ~200-300k, and OpenCrabs' transparent compaction already
                    // gives effectively-infinite memory — the extra window only
                    // hurts. Pin it on the written section so it always survives.
                    try_write!(write_errors, section, "context_window", "200000");
                }
                "moonshot" => {
                    // Plan: "api" (default, platform.moonshot.ai pay-per-token)
                    // or "coding" (api.kimi.com/coding/v1 token subscription)
                    let endpoint_type = if self.ps.moonshot_endpoint_type == 1 {
                        "coding"
                    } else {
                        "api"
                    };
                    try_write!(write_errors, section, "endpoint_type", endpoint_type);
                    // Coding plan carries a subscription tier that sets the
                    // context-window budget; the API plan has none. Persist the
                    // derived window alongside the tier so it is explicit in
                    // config (the factory still derives it as a fallback for
                    // hand-edited configs that set only `plan`).
                    if self.ps.moonshot_endpoint_type == 1 {
                        let tier = crate::brain::provider::kimi_plan::PLAN_TIERS
                            .get(self.ps.moonshot_plan)
                            .copied()
                            .unwrap_or("moderato");
                        try_write!(write_errors, section, "plan", tier);
                        let model_hint = if model.is_empty() {
                            None
                        } else {
                            Some(model.as_str())
                        };
                        if let Some(cw) = crate::brain::provider::kimi_plan::context_window_for_plan(
                            tier, model_hint,
                        ) {
                            try_write!(write_errors, section, "context_window", &cw.to_string());
                        }
                    }
                }
                "" => {
                    if !self.ps.base_url.is_empty() {
                        try_write!(write_errors, section, "base_url", &self.ps.base_url);
                    }
                    if !self.ps.custom_model.is_empty() {
                        try_write!(
                            write_errors,
                            section,
                            "default_model",
                            &self.ps.custom_model
                        );
                    }
                    if !self.ps.context_window.is_empty() {
                        try_write!(
                            write_errors,
                            section,
                            "context_window",
                            &self.ps.context_window
                        );
                    }
                }
                _ => {}
            }

            // Persist the model list. Use the live-fetched catalogue merged
            // with any config-persisted names (`all_model_names`: fetched on
            // top, config-only appended), NOT the stale `config_models` that
            // was loaded from disk. Writing `config_models` back meant a
            // successful `/v1/models` fetch was thrown away, so custom
            // providers stayed frozen on whatever list was there first while
            // Telegram/`/models` kept showing stale placeholder names (#267).
            // Fetched names first, then any config-only names appended. No
            // static-catalogue fallback here: if nothing was fetched and
            // nothing is in config, write nothing (preserves prior behavior).
            let mut models_to_write: Vec<String> = self.ps.models.clone();
            for m in &self.ps.config_models {
                if !models_to_write.contains(m) {
                    models_to_write.push(m.clone());
                }
            }
            if !models_to_write.is_empty()
                && (matches!(
                    self.ps.provider_id(),
                    "github" | "minimax" | "zai" | "moonshot" | ""
                ) || self.ps.selected_provider >= CUSTOM_PROVIDER_IDX)
            {
                try_write_array!(write_errors, section, "models", &models_to_write);
            }
            // Persist the Qwen thinking default so it is visible and editable
            // in config.toml. It is a preference, not the wire shape: which
            // knob actually ships is resolved per model by `qwen_reasoning`,
            // which drops the switch on families that read `reasoning_effort`
            // instead (#1034).
            if self.ps.provider_id() == "qwen" {
                try_write!(write_errors, section, "enable_thinking", "true");
            }
            // Clean up ghost custom provider entries (empty name/url/model)
            Config::cleanup_empty_custom_providers();
        } // end if write_provider

        // Agent defaults — ensure these are persisted on fresh install
        // (serde defaults handle runtime, but we persist so they're visible in config.toml)
        try_write!(write_errors, "agent", "approval_policy", "auto-always");

        if write_channels {
            // Channel enabled flags (from channel_toggles: 0=Telegram, 1=Discord, 2=WhatsApp, 3=Slack)
            try_write!(
                write_errors,
                "channels.telegram",
                "enabled",
                &self.is_telegram_enabled().to_string()
            );
            try_write!(
                write_errors,
                "channels.discord",
                "enabled",
                &self.is_discord_enabled().to_string()
            );
            try_write!(
                write_errors,
                "channels.whatsapp",
                "enabled",
                &self.channel_toggles.get(2).is_some_and(|t| t.1).to_string()
            );
            try_write!(
                write_errors,
                "channels.slack",
                "enabled",
                &self.is_slack_enabled().to_string()
            );
            try_write!(
                write_errors,
                "channels.trello",
                "enabled",
                &self.is_trello_enabled().to_string()
            );

            // Rich text experience (#418): client-side capability, so the
            // wizard checkbox is the source of truth; hot-reload applies it.
            try_write!(
                write_errors,
                "channels.telegram",
                "rich_messages",
                &self.telegram_rich_text.to_string()
            );

            // respond_to per channel
            let respond_to_values = ["all", "dm_only", "mention", "auto"];
            try_write!(
                write_errors,
                "channels.telegram",
                "respond_to",
                respond_to_values[self.telegram_respond_to.min(3)]
            );
            try_write!(
                write_errors,
                "channels.discord",
                "respond_to",
                respond_to_values[self.discord_respond_to.min(3)]
            );
            try_write!(
                write_errors,
                "channels.slack",
                "respond_to",
                respond_to_values[self.slack_respond_to.min(3)]
            );
        } // end if write_channels

        if write_voice {
            // Voice config — uses named SttProvider/TtsProvider variants

            // ── STT providers ──
            let groq_key_exists =
                !self.groq_api_key_input.is_empty() || self.has_existing_groq_key();

            // STT: Groq
            try_write!(
                write_errors,
                "providers.stt.groq",
                "enabled",
                &(self.stt_provider == SttProvider::Groq && groq_key_exists).to_string()
            );
            if self.stt_provider == SttProvider::Groq && groq_key_exists {
                try_write!(
                    write_errors,
                    "providers.stt.groq",
                    "default_model",
                    "whisper-large-v3-turbo"
                );
                // Write Groq API key to keys.toml (only if newly entered)
                if !self.groq_api_key_input.is_empty() && !self.has_existing_groq_key() {
                    try_write_keys!(
                        write_errors,
                        "providers.stt.groq",
                        "api_key",
                        &self.groq_api_key_input
                    );
                }
            }

            // STT: Local
            try_write!(
                write_errors,
                "providers.stt.local",
                "enabled",
                &(self.stt_provider == SttProvider::Local).to_string()
            );
            if self.stt_provider == SttProvider::Local {
                #[cfg(feature = "local-stt")]
                {
                    use crate::channels::voice::local_whisper::LOCAL_MODEL_PRESETS;
                    if self.selected_local_stt_model < LOCAL_MODEL_PRESETS.len() {
                        try_write!(
                            write_errors,
                            "providers.stt.local",
                            "model",
                            LOCAL_MODEL_PRESETS[self.selected_local_stt_model].id
                        );
                    }
                }
            }

            // STT: OpenAI-compatible
            try_write!(
                write_errors,
                "providers.stt.openai_compatible",
                "enabled",
                &(self.stt_provider == SttProvider::OpenAiCompatible).to_string()
            );
            if self.stt_provider == SttProvider::OpenAiCompatible {
                if !self.stt_openai_compat_base_url.is_empty() {
                    try_write!(
                        write_errors,
                        "providers.stt.openai_compatible",
                        "base_url",
                        &self.stt_openai_compat_base_url
                    );
                }
                if !self.stt_openai_compat_model.is_empty() {
                    try_write!(
                        write_errors,
                        "providers.stt.openai_compatible",
                        "model",
                        &self.stt_openai_compat_model
                    );
                }
                // Write API key to keys.toml (only if newly entered)
                if !self.stt_openai_compat_key_input.is_empty() {
                    try_write_keys!(
                        write_errors,
                        "providers.stt.openai_compatible",
                        "api_key",
                        &self.stt_openai_compat_key_input
                    );
                }
            }

            // STT: Voicebox
            try_write!(
                write_errors,
                "providers.stt.voicebox",
                "enabled",
                &(self.stt_provider == SttProvider::Voicebox).to_string()
            );
            if self.stt_provider == SttProvider::Voicebox && !self.stt_voicebox_base_url.is_empty()
            {
                try_write!(
                    write_errors,
                    "providers.stt.voicebox",
                    "base_url",
                    &self.stt_voicebox_base_url
                );
            }

            // The chain travels with the flags (#1399): selected engine first,
            // then whatever else can run, so a later switch never leaves the
            // chain pointing at an engine this same write just disabled.
            let stt_chain = super::voice_chain::stt_chain(
                self.stt_provider,
                super::voice_chain::SttReady {
                    groq_key: groq_key_exists,
                    local: crate::channels::voice::local_stt_available(),
                    openai_compatible: !self.stt_openai_compat_base_url.is_empty(),
                    voicebox: !self.stt_voicebox_base_url.is_empty(),
                },
            );
            try_write_array!(write_errors, "providers.stt", "fallback_chain", &stt_chain);

            // ── TTS providers ──

            // TTS: OpenAI
            try_write!(
                write_errors,
                "providers.tts.openai",
                "enabled",
                &(self.tts_provider == TtsProvider::OpenAi).to_string()
            );
            if self.tts_provider == TtsProvider::OpenAi {
                try_write!(
                    write_errors,
                    "providers.tts.openai",
                    "default_model",
                    "gpt-4o-mini-tts"
                );
                // Write selected voice under providers.tts.openai.voice — the
                // struct registry's real location. The legacy `[voice]` table
                // is migration-only; writing it directly would now be an
                // orphan table the write guard rejects (#83, #1385, #1387).
                try_write!(
                    write_errors,
                    "providers.tts.openai",
                    "voice",
                    &self.tts_api_voice
                );
                // Write API key to keys.toml (only what the user actually typed).
                // The equality check this replaces let a seeded field that was
                // typed into through persist as `__EXISTING_KEY__<key>`.
                if let Some(key) = super::key_field::typed_secret(&self.tts_api_key_input) {
                    try_write_keys!(write_errors, "providers.tts.openai", "api_key", key);
                }
            }

            // TTS: Local Piper
            try_write!(
                write_errors,
                "providers.tts.local",
                "enabled",
                &(self.tts_provider == TtsProvider::Local).to_string()
            );
            if self.tts_provider == TtsProvider::Local {
                #[cfg(feature = "local-tts")]
                {
                    use crate::channels::voice::local_tts::PIPER_VOICES;
                    if self.selected_tts_voice < PIPER_VOICES.len() {
                        try_write!(
                            write_errors,
                            "providers.tts.local",
                            "voice",
                            PIPER_VOICES[self.selected_tts_voice].id
                        );
                    }
                }
            }

            // TTS: OpenAI-compatible
            try_write!(
                write_errors,
                "providers.tts.openai_compatible",
                "enabled",
                &(self.tts_provider == TtsProvider::OpenAiCompatible).to_string()
            );
            if self.tts_provider == TtsProvider::OpenAiCompatible {
                if !self.tts_openai_compat_base_url.is_empty() {
                    try_write!(
                        write_errors,
                        "providers.tts.openai_compatible",
                        "base_url",
                        &self.tts_openai_compat_base_url
                    );
                }
                if !self.tts_openai_compat_model.is_empty() {
                    try_write!(
                        write_errors,
                        "providers.tts.openai_compatible",
                        "model",
                        &self.tts_openai_compat_model
                    );
                }
                if !self.tts_openai_compat_voice.is_empty() {
                    try_write!(
                        write_errors,
                        "providers.tts.openai_compatible",
                        "voice",
                        &self.tts_openai_compat_voice
                    );
                }
                // Write API key to keys.toml (only if newly entered)
                if !self.tts_openai_compat_key_input.is_empty() {
                    try_write_keys!(
                        write_errors,
                        "providers.tts.openai_compatible",
                        "api_key",
                        &self.tts_openai_compat_key_input
                    );
                }
            }

            // TTS: Voicebox
            try_write!(
                write_errors,
                "providers.tts.voicebox",
                "enabled",
                &(self.tts_provider == TtsProvider::Voicebox).to_string()
            );
            if self.tts_provider == TtsProvider::Voicebox {
                if !self.tts_voicebox_base_url.is_empty() {
                    try_write!(
                        write_errors,
                        "providers.tts.voicebox",
                        "base_url",
                        &self.tts_voicebox_base_url
                    );
                }
                if !self.tts_voicebox_profile_id.is_empty() {
                    try_write!(
                        write_errors,
                        "providers.tts.voicebox",
                        "profile_id",
                        &self.tts_voicebox_profile_id
                    );
                }
                if !self.tts_voicebox_engine.is_empty() {
                    try_write!(
                        write_errors,
                        "providers.tts.voicebox",
                        "engine",
                        &self.tts_voicebox_engine
                    );
                }
            }

            let tts_chain = super::voice_chain::tts_chain(
                self.tts_provider,
                super::voice_chain::TtsReady {
                    openai_key: super::key_field::is_stored(&self.tts_api_key_input)
                        || super::key_field::typed_secret(&self.tts_api_key_input).is_some(),
                    local: crate::channels::voice::local_tts_available(),
                    openai_compatible: !self.tts_openai_compat_base_url.is_empty(),
                    voicebox: !self.tts_voicebox_base_url.is_empty(),
                },
            );
            try_write_array!(write_errors, "providers.tts", "fallback_chain", &tts_chain);
        } // end if write_voice

        if write_image {
            // Image config
            let default_model = "gemini-3.1-flash-image-preview";
            // Wizard input wins; empty stays on the seeded default.
            let trimmed = self.image_generation_model_input.trim();
            let generation_model = if trimmed.is_empty() {
                default_model
            } else {
                trimmed
            };
            if self.image_generation_enabled {
                try_write!(write_errors, "image.generation", "enabled", "true");
                try_write!(write_errors, "image.generation", "model", generation_model);
            }
            if self.image_vision_enabled {
                try_write!(write_errors, "image.vision", "enabled", "true");
                try_write!(write_errors, "image.vision", "model", default_model);
            }
            // Save image API key to keys.toml (only if newly entered)
            if !self.image_api_key_input.is_empty()
                && !self.has_existing_image_key()
                && let Err(e) = crate::config::write_secret_key(
                    "providers.image.gemini",
                    "api_key",
                    &self.image_api_key_input,
                )
            {
                tracing::warn!("Failed to save image API key to keys.toml: {}", e);
            }
        } // end if write_image

        // Save API key to keys.toml via merge — never overwrite
        if write_provider
            && !self.ps.has_existing_key_sentinel()
            && !self.ps.api_key_input.is_empty()
            && let Err(e) =
                crate::config::write_secret_key(section, "api_key", &self.ps.api_key_input)
        {
            tracing::warn!("Failed to save API key to keys.toml: {}", e);
        }

        // Custom-provider RENAME cleanup: the new section was written above
        // under the current `custom_name`. If the user edited the name of an
        // existing entry, drop the stale OLD section from BOTH config.toml and
        // keys.toml — otherwise merge_provider_keys resurrects the old name as
        // a phantom entry on the next Config::load (the 2026-06-05
        // modelscope-qwen → modelscope ghost-entry bug).
        if write_provider
            && self.ps.selected_provider >= CUSTOM_PROVIDER_IDX
            && !self.ps.custom_name.is_empty()
            && let Some(old_name) = self.ps.editing_custom_key.as_deref()
            && old_name != self.ps.custom_name
        {
            let old_section = format!("providers.custom.{}", old_name);
            if let Err(e) = Config::remove_section(&old_section) {
                tracing::warn!(
                    "Failed to remove old config.toml section {} after rename: {}",
                    old_section,
                    e
                );
            }
            if let Err(e) = Config::remove_secret_section(&old_section) {
                tracing::warn!(
                    "Failed to remove old keys.toml section {} after rename: {}",
                    old_section,
                    e
                );
            }
        }

        // (GitHub Copilot OAuth token is saved directly via the device flow handler)

        // Save STT/TTS keys to keys.toml
        // Every write below is gated on `is_new_secret`, never on emptiness.
        // The inputs are seeded with EXISTING_KEY_SENTINEL when a key is
        // already stored, so an emptiness check treats "unchanged" as a value
        // and persists the literal marker over a working key (#1039).
        if write_voice {
            use super::key_field::typed_secret;

            if let Some(key) = groq_key.as_deref().and_then(typed_secret)
                && let Err(e) =
                    crate::config::write_secret_key("providers.stt.groq", "api_key", key)
            {
                tracing::warn!("Failed to save Groq key to keys.toml: {}", e);
            }
            // providers.tts.openai is deliberately NOT written here. It used to
            // receive `groq_key`, which is a different provider's credential:
            // that section is real OpenAI (no base_url), so a Groq key there
            // only ever produced a 401. The dialog has no OpenAI TTS key input
            // to put in its place, so set it with
            // `/voice tts openai <key>` rather than writing something
            // that cannot work.
            if let Some(key) = typed_secret(&self.stt_openai_compat_key_input)
                && let Err(e) = crate::config::write_secret_key(
                    "providers.stt.openai_compatible",
                    "api_key",
                    key,
                )
            {
                tracing::warn!("Failed to save OpenAI-compatible STT key: {}", e);
            }
            if let Some(key) = typed_secret(&self.tts_openai_compat_key_input)
                && let Err(e) = crate::config::write_secret_key(
                    "providers.tts.openai_compatible",
                    "api_key",
                    key,
                )
            {
                tracing::warn!("Failed to save OpenAI-compatible TTS key: {}", e);
            }
        } // end voice keys

        if write_channels {
            // Persist channel tokens to keys.toml (if new)
            if !self.telegram_token_input.is_empty()
                && !self.has_existing_telegram_token()
                && let Err(e) = crate::config::write_secret_key(
                    "channels.telegram",
                    "token",
                    &self.telegram_token_input,
                )
            {
                tracing::warn!("Failed to save Telegram token to keys.toml: {}", e);
            }
            if !self.discord_token_input.is_empty()
                && !self.has_existing_discord_token()
                && let Err(e) = crate::config::write_secret_key(
                    "channels.discord",
                    "token",
                    &self.discord_token_input,
                )
            {
                tracing::warn!("Failed to save Discord token to keys.toml: {}", e);
            }
            if !self.slack_bot_token_input.is_empty()
                && !self.has_existing_slack_bot_token()
                && let Err(e) = crate::config::write_secret_key(
                    "channels.slack",
                    "token",
                    &self.slack_bot_token_input,
                )
            {
                tracing::warn!("Failed to save Slack bot token to keys.toml: {}", e);
            }
            if !self.slack_app_token_input.is_empty()
                && !self.has_existing_slack_app_token()
                && let Err(e) = crate::config::write_secret_key(
                    "channels.slack",
                    "app_token",
                    &self.slack_app_token_input,
                )
            {
                tracing::warn!("Failed to save Slack app token to keys.toml: {}", e);
            }
            // Trello API Key (saved as app_token) + API Token
            if !self.trello_api_key_input.is_empty()
                && !self.has_existing_trello_api_key()
                && let Err(e) = crate::config::write_secret_key(
                    "channels.trello",
                    "app_token",
                    &self.trello_api_key_input,
                )
            {
                tracing::warn!("Failed to save Trello API Key to keys.toml: {}", e);
            }
            if !self.trello_api_token_input.is_empty()
                && !self.has_existing_trello_api_token()
                && let Err(e) = crate::config::write_secret_key(
                    "channels.trello",
                    "token",
                    &self.trello_api_token_input,
                )
            {
                tracing::warn!("Failed to save Trello API Token to keys.toml: {}", e);
            }

            // Persist channel IDs/user IDs to config.toml (if new)
            // telegram_user_id_input is never a sentinel — write whatever
            // the user left in the field (empty = "allow any user").
            if !self.telegram_user_id_input.is_empty() {
                // Union with the existing allowed_users in config.toml (the
                // source of truth) — NEVER overwrite. The input field only ever
                // holds one id (the owner, loaded from allowed_users.first()),
                // so a bare write collapsed the whole allowlist to a single
                // entry on every re-apply, silently locking out everyone else.
                let mut users: Vec<String> = Config::load()
                    .map(|c| c.channels.telegram.allowed_users)
                    .unwrap_or_default();
                if !users.contains(&self.telegram_user_id_input) {
                    users.push(self.telegram_user_id_input.clone());
                }
                try_write_array!(write_errors, "channels.telegram", "allowed_users", &users);
            }
            if !self.discord_channel_id_input.is_empty() && !self.has_existing_discord_channel_id()
            {
                try_write_array!(
                    write_errors,
                    "channels.discord",
                    "allowed_channels",
                    std::slice::from_ref(&self.discord_channel_id_input)
                );
            }
            if !self.slack_channel_id_input.is_empty() && !self.has_existing_slack_channel_id() {
                try_write_array!(
                    write_errors,
                    "channels.slack",
                    "allowed_channels",
                    std::slice::from_ref(&self.slack_channel_id_input)
                );
            }
            if !self.discord_allowed_list_input.is_empty()
                && !self.has_existing_discord_allowed_list()
            {
                try_write_array!(
                    write_errors,
                    "channels.discord",
                    "allowed_users",
                    std::slice::from_ref(&self.discord_allowed_list_input)
                );
            }
            if !self.slack_allowed_list_input.is_empty() && !self.has_existing_slack_allowed_list()
            {
                try_write_array!(
                    write_errors,
                    "channels.slack",
                    "allowed_users",
                    std::slice::from_ref(&self.slack_allowed_list_input)
                );
            }
            if !self.whatsapp_phone_input.is_empty() && !self.has_existing_whatsapp_phone() {
                try_write_array!(
                    write_errors,
                    "channels.whatsapp",
                    "allowed_phones",
                    std::slice::from_ref(&self.whatsapp_phone_input)
                );
            }
            if !self.trello_board_id_input.is_empty() && !self.has_existing_trello_board_id() {
                let boards: Vec<String> = self
                    .trello_board_id_input
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !boards.is_empty() {
                    try_write_array!(write_errors, "channels.trello", "board_ids", &boards);
                }
            }
            if !self.trello_allowed_users_input.is_empty()
                && !self.has_existing_trello_allowed_users()
            {
                let users: Vec<String> = self
                    .trello_allowed_users_input
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !users.is_empty() {
                    try_write_array!(write_errors, "channels.trello", "allowed_users", &users);
                }
            }
        } // end if write_channels

        // Seed workspace templates (use AI-generated content when available).
        // Completion-only: a step-scoped save (finalize=false) must not seed
        // early. Leaving the provider step would otherwise write static
        // brain files before BrainSetup has generated anything (#926).
        if finalize && self.seed_templates {
            let workspace = PathBuf::from(&self.workspace_path);
            std::fs::create_dir_all(&workspace)
                .map_err(|e| format!("Failed to create workspace: {}", e))?;

            for (filename, content) in TEMPLATE_FILES {
                let file_path = workspace.join(filename);
                // Use AI-generated content when available, static template as fallback
                let generated = match *filename {
                    "SOUL.md" => self.generated_soul.as_deref(),
                    "USER.md" => self.generated_user.as_deref(),
                    "AGENTS.md" => self.generated_agents.as_deref(),
                    "TOOLS.md" => self.generated_tools.as_deref(),
                    "MEMORY.md" => self.generated_memory.as_deref(),
                    _ => None,
                };
                // Write if: AI-generated (always overwrite) or file doesn't exist (seed template)
                if generated.is_some() || !file_path.exists() {
                    let final_content = generated.unwrap_or(content);
                    std::fs::write(&file_path, final_content)
                        .map_err(|e| format!("Failed to write {}: {}", filename, e))?;
                }
            }
        }

        // Install daemon if requested. Completion-only, same as seeding (#926).
        if finalize
            && self.install_daemon
            && let Err(e) = install_daemon_service()
        {
            tracing::warn!("Failed to install daemon: {}", e);
            // Non-fatal — don't block onboarding completion
        }

        if !write_errors.is_empty() {
            tracing::error!(
                "Onboarding: failed to write {} config keys: {}",
                write_errors.len(),
                write_errors.join(", ")
            );
            // Report what actually failed and why. "Check file permissions"
            // was a guess with no evidence behind it, and it sent users
            // chasing a cause the error never claimed (#915).
            return Err(format!(
                "{} setting(s) could not be saved:\n  {}",
                write_errors.len(),
                write_errors.join("\n  ")
            ));
        }

        Ok(())
    }
}

/// Install the appropriate daemon service for the current platform.
///
/// `pub` so the GUI bin's Start daemon chip (#2452) reuses this exact
/// path instead of growing a second installer.
pub fn install_daemon_service() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        install_systemd_service()
    }

    #[cfg(target_os = "macos")]
    {
        install_launchagent()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("Daemon installation not supported on this platform".to_string())
    }
}

/// Build the onboarding wizard's systemd user unit.
///
/// Pure (no OS calls) so it is unit-testable on every platform; the
/// `#[cfg(target_os = "linux")]` caller below is the only thing that writes it
/// to disk. `OOMPolicy=continue` keeps a single child's OOM kill from
/// terminating the whole daemon: the daemon is the parent of every tool and
/// cron child, and systemd's default policy (`stop`) would tear the unit down.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn build_onboarding_systemd_unit(exe_path: &str) -> String {
    format!(
        r#"[Unit]
Description=OpenCrabs AI Orchestration Agent
After=network.target

[Service]
Type=simple
ExecStart={exe_path} daemon
Restart=on-failure
RestartSec=5
OOMPolicy=continue

[Install]
WantedBy=default.target
"#
    )
}

#[cfg(target_os = "linux")]
fn install_systemd_service() -> Result<(), String> {
    let service_dir = dirs::config_dir()
        .ok_or("Cannot determine config dir")?
        .parent()
        .ok_or("Cannot determine parent of config dir")?
        .join(".config")
        .join("systemd")
        .join("user");

    // Try the standard XDG path first
    let service_dir = if service_dir.exists() {
        service_dir
    } else {
        dirs::home_dir()
            .ok_or("Cannot determine home dir")?
            .join(".config")
            .join("systemd")
            .join("user")
    };

    std::fs::create_dir_all(&service_dir)
        .map_err(|e| format!("Failed to create systemd dir: {}", e))?;

    let exe_path = std::env::current_exe().map_err(|e| format!("Failed to get exe path: {}", e))?;

    let service_content = build_onboarding_systemd_unit(&exe_path.display().to_string());

    let service_path = service_dir.join("opencrabs.service");
    std::fs::write(&service_path, service_content)
        .map_err(|e| format!("Failed to write service file: {}", e))?;

    // Enable and start the service. Check the EXIT STATUS, not just whether
    // systemctl spawned: on a headless box `systemctl --user` fails with
    // "Failed to connect to bus" (no per-user systemd), and silently ignoring
    // that exit code used to report the install as successful when nothing ran.
    for op in ["enable", "start"] {
        let out = std::process::Command::new("systemctl")
            .args(["--user", op, "opencrabs"])
            .output()
            .map_err(|e| format!("Failed to run systemctl {op}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "systemctl --user {op} opencrabs failed: {}. A user service needs \
                 a running per-user systemd instance, which a headless SSH session \
                 usually lacks. Enable lingering with `sudo loginctl enable-linger \
                 $(whoami)` then retry, or install a system service with `sudo \
                 opencrabs service install`.",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn install_launchagent() -> Result<(), String> {
    let agents_dir = dirs::home_dir()
        .ok_or("Cannot determine home dir")?
        .join("Library")
        .join("LaunchAgents");

    std::fs::create_dir_all(&agents_dir)
        .map_err(|e| format!("Failed to create LaunchAgents dir: {}", e))?;

    let exe_path = std::env::current_exe().map_err(|e| format!("Failed to get exe path: {}", e))?;

    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.opencrabs.agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#,
        exe_path.display()
    );

    let plist_path = agents_dir.join("com.opencrabs.agent.plist");
    std::fs::write(&plist_path, plist_content)
        .map_err(|e| format!("Failed to write plist: {}", e))?;

    let out = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path.to_string_lossy()])
        .output()
        .map_err(|e| format!("Failed to run launchctl load: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "launchctl load failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    Ok(())
}
