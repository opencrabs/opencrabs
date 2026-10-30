use super::types::*;

/// Main onboarding wizard state
pub struct OnboardingWizard {
    pub step: OnboardingStep,
    pub mode: WizardMode,

    // Step 2: Provider/Auth — shared state with /models dialog
    pub ps: crate::tui::provider_selector::ProviderSelectorState,
    pub auth_field: AuthField,

    /// Step 4: Workspace
    pub workspace_path: String,
    pub seed_templates: bool,

    /// Step 5: Channels
    pub channel_toggles: Vec<(String, bool)>,

    /// Step 5b: Telegram Setup (shown when Telegram is enabled)
    pub telegram_field: TelegramField,
    pub telegram_token_input: String,
    pub telegram_user_id_input: String,

    /// Discord Setup (shown when Discord is enabled)
    pub discord_field: DiscordField,
    pub discord_token_input: String,
    pub discord_channel_id_input: String,
    pub discord_allowed_list_input: String,

    /// Rich text experience toggle (`channels.telegram.rich_messages`, #418)
    pub telegram_rich_text: bool,
    /// respond_to selection per channel (0=all, 1=dm_only, 2=mention)
    pub telegram_respond_to: usize,
    pub discord_respond_to: usize,
    pub slack_respond_to: usize,

    /// WhatsApp Setup (shown when WhatsApp is enabled)
    pub whatsapp_field: WhatsAppField,
    pub whatsapp_qr_text: Option<String>,
    pub whatsapp_connecting: bool,
    pub whatsapp_connected: bool,
    pub whatsapp_error: Option<String>,
    pub whatsapp_phone_input: String,

    /// Slack Setup (shown when Slack is enabled)
    pub slack_field: SlackField,
    pub slack_bot_token_input: String,
    pub slack_app_token_input: String,
    pub slack_channel_id_input: String,
    pub slack_allowed_list_input: String,

    /// Trello Setup (shown when Trello is enabled)
    pub trello_field: TrelloField,
    pub trello_api_key_input: String,
    pub trello_api_token_input: String,
    pub trello_board_id_input: String,
    pub trello_allowed_users_input: String,

    /// Cursor position for whichever channel text field is currently active
    pub channel_input_cursor: usize,

    /// Channel test connection status
    pub channel_test_status: ChannelTestStatus,

    /// Step 6: Voice Setup
    pub voice_field: VoiceField,
    /// Which STT provider is selected
    pub stt_provider: SttProvider,
    pub groq_api_key_input: String,
    /// Index into LOCAL_MODEL_PRESETS (0=Tiny, 1=Base, 2=Small, 3=Medium)
    pub selected_local_stt_model: usize,
    /// Download progress (0.0 - 1.0), None if not downloading
    pub stt_model_download_progress: Option<f64>,
    /// Download status message
    pub stt_model_download_error: Option<String>,
    /// Whether the selected model is downloaded and ready
    pub stt_model_downloaded: bool,
    pub tts_enabled: bool,
    /// Which TTS provider is selected
    pub tts_provider: TtsProvider,
    /// Index into PIPER_VOICES (0=ryan, 1=amy, etc.)
    pub selected_tts_voice: usize,
    /// Download progress for Piper voice (0.0 - 1.0), None if not downloading
    pub tts_voice_download_progress: Option<f64>,
    /// Download error message
    pub tts_voice_download_error: Option<String>,
    /// Whether the selected Piper voice is downloaded and ready
    pub tts_voice_downloaded: bool,

    // OpenAI-compatible STT
    pub stt_openai_compat_base_url: String,
    pub stt_openai_compat_model: String,
    pub stt_openai_compat_key_input: String,
    // Voicebox STT
    pub stt_voicebox_base_url: String,
    // Built-in OpenAI TTS
    pub tts_api_voice: String,
    pub tts_api_key_input: String,
    // OpenAI-compatible TTS
    pub tts_openai_compat_base_url: String,
    pub tts_openai_compat_model: String,
    pub tts_openai_compat_voice: String,
    pub tts_openai_compat_key_input: String,
    // Voicebox TTS
    pub tts_voicebox_base_url: String,
    pub tts_voicebox_profile_id: String,
    pub tts_voicebox_engine: String,

    /// Step 7: Image Setup
    pub image_field: ImageField,
    pub image_vision_enabled: bool,
    pub image_generation_enabled: bool,
    /// Free-text override for the generation model name. Empty keeps the
    /// hardcoded default (`gemini-3.1-flash-image-preview`); a populated
    /// value writes through to `image.generation.model`.
    pub image_generation_model_input: String,
    pub image_api_key_input: String,

    /// Step 8: Daemon
    pub install_daemon: bool,

    /// Step 7: Health check
    pub health_results: Vec<(String, HealthStatus)>,
    pub health_running: bool,
    pub health_complete: bool,

    /// Step 8: Brain Setup
    pub brain_field: BrainField,
    pub about_me: String,
    pub about_opencrabs: String,
    /// Original values loaded from workspace brain files (for change detection)
    pub original_about_me: String,
    pub original_about_opencrabs: String,
    /// Whether the user has started editing each field (arrow key or char input).
    /// First backspace on an untouched template clears the entire field.
    pub brain_me_edited: bool,
    pub brain_agent_edited: bool,
    pub brain_generating: bool,
    pub brain_generated: bool,
    pub brain_error: Option<String>,
    /// Auto-format preview state
    pub preview_shown: bool,
    pub formatted_about_me: String,
    pub formatted_about_agent: String,
    pub generated_soul: Option<String>,
    pub generated_user: Option<String>,
    pub generated_agents: Option<String>,
    pub generated_tools: Option<String>,
    pub generated_memory: Option<String>,

    /// GitHub Copilot device flow state
    pub github_user_code: Option<String>,
    pub github_device_flow_status: GitHubDeviceFlowStatus,

    /// Codex OAuth device flow state lives in ps (ProviderSelectorState)

    /// Navigation
    pub focused_field: usize,
    pub error_message: Option<String>,

    /// Opened from chat via slash command (e.g. /doctor, /models).
    /// Shows only the target step: no progress dots, no navigation, Enter/Esc exit.
    pub quick_jump: bool,
    /// Set by `next_step()` when `quick_jump` is true — signals the step is done
    /// and `handle_key` should return `WizardAction::Cancel`.
    pub quick_jump_done: bool,
    /// True only after this run actually went through the voice step (#1233).
    /// Gates voice-flag writes so a Complete reached without touching voice
    /// preserves on-disk `[providers.stt.*]` / `[providers.tts.*]` truth
    /// instead of rewriting it from page state.
    pub voice_step_touched: bool,
    /// Whether onboarding auto-triggered because no config exists (true) vs
    /// user manually re-ran it via /onboard (false). Used to decide whether
    /// to send the first-time welcome message on completion.
    pub is_first_time: bool,

    /// Set when the wizard reopened partway through a previous run, naming
    /// the steps still outstanding. Shown once so a user who left halfway is
    /// told what remains rather than dropped back in without context (#919).
    pub resume_notice: Option<String>,

    /// Extra vertical scroll the user requested manually via Page Up /
    /// Page Down. Added on top of the focus-driven scroll computed each
    /// frame in `render_onboarding` so the user can peek at fields below
    /// the current focus without having to Tab through them — important
    /// when the terminal is small enough that the form's last 1-3 fields
    /// fall below the visible viewport. Cleared on step changes so each
    /// new screen starts at the top.
    pub user_scroll_offset: u16,
}

impl Default for OnboardingWizard {
    fn default() -> Self {
        Self::new()
    }
}

impl OnboardingWizard {
    /// Create a new wizard with default state
    /// Loads existing config if available to pre-fill settings
    pub fn new() -> Self {
        let default_workspace = crate::config::opencrabs_home();

        // config_models loaded on demand per provider via reload_config_models()
        let config_models = Vec::new();

        // Try to load existing config to pre-fill settings
        let existing_config = crate::config::Config::load().ok();

        // Detect existing enabled provider
        let mut custom_provider_name_init: Option<String> = None;
        let (selected_provider, api_key_input, custom_base_url, custom_model) =
            if let Some(ref config) = existing_config {
                // Find first enabled provider
                if config
                    .providers
                    .anthropic
                    .as_ref()
                    .is_some_and(|p| p.enabled)
                {
                    (
                        0,
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if config.providers.openai.as_ref().is_some_and(|p| p.enabled) {
                    (
                        1,
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if config.providers.github.as_ref().is_some_and(|p| p.enabled) {
                    (
                        2,
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if config.providers.gemini.as_ref().is_some_and(|p| p.enabled) {
                    (
                        3,
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if config
                    .providers
                    .openrouter
                    .as_ref()
                    .is_some_and(|p| p.enabled)
                {
                    (
                        4,
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if config.providers.minimax.as_ref().is_some_and(|p| p.enabled) {
                    (
                        5,
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if config.providers.zai.as_ref().is_some_and(|p| p.enabled) {
                    (
                        6,
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if config
                    .providers
                    .moonshot
                    .as_ref()
                    .is_some_and(|p| p.enabled)
                {
                    (
                        crate::tui::provider_selector::index_of_provider("moonshot").unwrap_or(7),
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if config
                    .providers
                    .claude_cli
                    .as_ref()
                    .is_some_and(|p| p.enabled)
                {
                    (
                        crate::tui::provider_selector::index_of_provider("claude-cli").unwrap_or(0),
                        String::new(),
                        String::new(),
                        String::new(),
                    )
                } else if config
                    .providers
                    .opencode_cli
                    .as_ref()
                    .is_some_and(|p| p.enabled)
                {
                    (
                        crate::tui::provider_selector::index_of_provider("opencode-cli")
                            .unwrap_or(0),
                        String::new(),
                        String::new(),
                        String::new(),
                    )
                } else if config
                    .providers
                    .codex_cli
                    .as_ref()
                    .is_some_and(|p| p.enabled)
                {
                    (
                        crate::tui::provider_selector::index_of_provider("codex-cli").unwrap_or(0),
                        String::new(),
                        String::new(),
                        String::new(),
                    )
                } else if config
                    .providers
                    .command_code_cli
                    .as_ref()
                    .is_some_and(|p| p.enabled)
                {
                    (
                        crate::tui::provider_selector::index_of_provider("command-code-cli")
                            .unwrap_or(0),
                        String::new(),
                        String::new(),
                        String::new(),
                    )
                } else if config
                    .providers
                    .opencrabs
                    .as_ref()
                    .is_some_and(|p| p.enabled)
                {
                    // The built-in provider is enabled: open on it (#2060).
                    (
                        crate::tui::provider_selector::index_of_provider("opencrabs").unwrap_or(0),
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if config.providers.qwen.as_ref().is_some_and(|p| p.enabled) {
                    (
                        crate::tui::provider_selector::index_of_provider("qwen").unwrap_or(0),
                        EXISTING_KEY_SENTINEL.to_string(),
                        String::new(),
                        String::new(),
                    )
                } else if let Some((name, c)) = config.providers.active_custom() {
                    // Only an enabled custom provider is selected. A disabled
                    // one used to win here as the first entry in the map (#2060).
                    let base = c.base_url.clone().unwrap_or_default();
                    let model = c.default_model.clone().unwrap_or_default();
                    custom_provider_name_init = Some(name.to_string());
                    // context_window is set after wizard construction below
                    // Map to CUSTOM_INSTANCES_START+ for existing custom providers
                    use crate::tui::provider_selector::{
                        CUSTOM_INSTANCES_START, CUSTOM_PROVIDER_IDX,
                    };
                    let idx = config
                        .providers
                        .custom
                        .as_ref()
                        .and_then(|m| {
                            m.keys()
                                .position(|k| k == name)
                                .map(|pos| CUSTOM_INSTANCES_START + pos)
                        })
                        .unwrap_or(CUSTOM_PROVIDER_IDX);
                    (idx, EXISTING_KEY_SENTINEL.to_string(), base, model)
                } else {
                    (0, String::new(), String::new(), String::new())
                }
            } else {
                (0, String::new(), String::new(), String::new())
            };

        let ps = crate::tui::provider_selector::ProviderSelectorState {
            selected_provider,
            api_key_input,
            api_key_cursor: 0,
            selected_model: 0,
            custom_name: custom_provider_name_init.unwrap_or_default(),
            base_url: custom_base_url,
            custom_model,
            context_window: String::new(),
            models: Vec::new(),
            models_fetching: false,
            config_models,
            custom_names: existing_config
                .as_ref()
                .and_then(|c| c.providers.custom.as_ref())
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default(),
            zhipu_endpoint_type: 0,    // default to API mode
            xiaomi_endpoint_type: 0,   // default to API mode
            moonshot_endpoint_type: 0, // default to API plan
            opencode_endpoint_type: 0, // default to Go plan (#2040)
            model_filter: String::new(),
            ..Default::default()
        };

        let mut wizard = Self {
            step: OnboardingStep::ModeSelect,
            mode: WizardMode::QuickStart,

            ps,
            auth_field: AuthField::Provider,

            workspace_path: default_workspace.to_string_lossy().to_string(),
            seed_templates: true,

            channel_toggles: CHANNEL_NAMES
                .iter()
                .map(|(name, _desc)| (name.to_string(), false))
                .collect(),

            telegram_field: TelegramField::BotToken,
            telegram_token_input: String::new(),
            telegram_user_id_input: String::new(),

            discord_field: DiscordField::BotToken,
            discord_token_input: String::new(),
            discord_channel_id_input: String::new(),
            discord_allowed_list_input: String::new(),

            telegram_rich_text: true, // on by default (#425); older clients opt out
            telegram_respond_to: 0,   // all
            discord_respond_to: 2,    // mention
            slack_respond_to: 2,      // mention

            whatsapp_field: WhatsAppField::Connection,
            whatsapp_qr_text: None,
            whatsapp_connecting: false,
            whatsapp_connected: false,
            whatsapp_error: None,
            whatsapp_phone_input: String::new(),

            slack_field: SlackField::BotToken,
            slack_bot_token_input: String::new(),
            slack_app_token_input: String::new(),
            slack_channel_id_input: String::new(),
            slack_allowed_list_input: String::new(),

            trello_field: TrelloField::ApiKey,
            trello_api_key_input: String::new(),
            trello_api_token_input: String::new(),
            trello_board_id_input: String::new(),
            trello_allowed_users_input: String::new(),

            channel_input_cursor: 0,
            channel_test_status: ChannelTestStatus::Idle,

            voice_field: VoiceField::SttModeSelect,
            stt_provider: SttProvider::Off,
            groq_api_key_input: String::new(),
            selected_local_stt_model: 0,
            stt_model_download_progress: None,
            stt_model_download_error: None,
            stt_model_downloaded: false,
            tts_enabled: false,
            tts_provider: TtsProvider::Off,
            selected_tts_voice: 0,
            tts_voice_download_progress: None,
            tts_voice_download_error: None,
            tts_voice_downloaded: false,
            // OpenAI-compatible STT
            stt_openai_compat_base_url: String::new(),
            stt_openai_compat_model: "whisper-1".to_string(),
            stt_openai_compat_key_input: String::new(),
            // Voicebox STT
            stt_voicebox_base_url: "http://localhost:8000".to_string(),
            // Built-in OpenAI TTS
            tts_api_voice: "echo".to_string(),
            tts_api_key_input: String::new(),
            // OpenAI-compatible TTS
            tts_openai_compat_base_url: String::new(),
            tts_openai_compat_model: "tts-1".to_string(),
            tts_openai_compat_voice: "alloy".to_string(),
            tts_openai_compat_key_input: String::new(),
            // Voicebox TTS
            tts_voicebox_base_url: String::new(),
            tts_voicebox_profile_id: String::new(),
            tts_voicebox_engine: String::new(),

            image_field: ImageField::VisionToggle,
            image_vision_enabled: false,
            image_generation_enabled: false,
            image_generation_model_input: String::new(),
            image_api_key_input: String::new(),

            install_daemon: false,

            health_results: Vec::new(),
            health_running: false,
            health_complete: false,

            brain_field: BrainField::AboutMe,
            about_me: String::new(),
            about_opencrabs: String::new(),
            original_about_me: String::new(),
            original_about_opencrabs: String::new(),
            brain_me_edited: false,
            brain_agent_edited: false,
            brain_generating: false,
            brain_generated: false,
            brain_error: None,
            preview_shown: false,
            formatted_about_me: String::new(),
            formatted_about_agent: String::new(),
            generated_soul: None,
            generated_user: None,
            generated_agents: None,
            generated_tools: None,
            generated_memory: None,

            github_user_code: None,
            github_device_flow_status: GitHubDeviceFlowStatus::Idle,

            // codex device flow state lives in ps (ProviderSelectorState)
            focused_field: 0,
            error_message: None,
            quick_jump: false,
            quick_jump_done: false,
            voice_step_touched: false,
            is_first_time: false,
            resume_notice: None,
            user_scroll_offset: 0,
        };

        // Load existing brain files from workspace if available
        let workspace = std::path::Path::new(&wizard.workspace_path);
        if let Ok(content) = std::fs::read_to_string(workspace.join("USER.md")) {
            let truncated = Self::truncate_preview(&content, 200);
            wizard.about_me = truncated.clone();
            wizard.original_about_me = truncated;
        }

        wizard
    }

    /// A wizard pre-filled from the config on disk, falling back to defaults
    /// when there is no readable config yet (genuine first run).
    ///
    /// Every wizard that can reach a save has to start here. `apply_config`
    /// writes each section from wizard state, and the fields it writes are
    /// plain values, not "unset" markers: a wizard built by `new()` carries
    /// `SttProvider::Off` and `TtsProvider::Off` because those are the struct
    /// defaults, not because the user chose them. Saving from such a wizard
    /// writes `enabled = false` over every `providers.stt.*` and
    /// `providers.tts.*` section, so working voice config was wiped by a
    /// wizard the user opened for an unrelated step and then left.
    fn seeded_from_disk() -> Self {
        match crate::config::Config::load() {
            Ok(config) => Self::from_config(&config),
            Err(e) => {
                // Not fatal: a first run has no config yet. Logged because the
                // other reason to land here is an unparseable config, and that
                // silently downgrades the wizard to defaults.
                tracing::info!("[onboarding] no config to seed the wizard from: {e}");
                Self::new()
            }
        }
    }

    /// Reopen the wizard where the user left it.
    ///
    /// Falls back to a fresh wizard when there is nothing to resume, when the
    /// recorded step is from a build that does not have it, or when it is
    /// `Complete` — resuming onto the finish screen would let a half-finished
    /// setup be marked done by pressing Enter.
    pub fn resumed() -> Self {
        let mut wizard = Self::seeded_from_disk();
        let saved = super::state::OnboardingState::load();
        let Some(step) = saved
            .last_step
            .as_deref()
            .and_then(OnboardingStep::from_key)
            .filter(|s| !matches!(s, OnboardingStep::ModeSelect | OnboardingStep::Complete))
        else {
            return wizard;
        };

        if saved.mode.as_deref() == Some(WizardMode::Advanced.as_key()) {
            wizard.mode = WizardMode::Advanced;
        }
        wizard.step = step;
        let remaining = step.remaining_titles(wizard.mode);
        wizard.resume_notice = Some(if remaining.is_empty() {
            format!("Picking up where you left off, at {}.", step.title())
        } else {
            format!(
                "Picking up where you left off, at {}. Still to do: {}.",
                step.title(),
                remaining.join(", ")
            )
        });
        tracing::info!(
            "[onboarding] resuming at {} ({} step(s) outstanding)",
            step.as_key(),
            remaining.len()
        );
        wizard
    }

    /// Put the provider and model a session is using into the dialog (#2060).
    ///
    /// Returns false, leaving the dialog as it was, when the provider cannot be
    /// found: a session can outlive the provider entry it was created with.
    pub fn select_session_pair(
        &mut self,
        provider_name: &str,
        model: &str,
        config: &crate::config::Config,
    ) -> bool {
        use crate::tui::provider_selector::{CUSTOM_INSTANCES_START, index_of_provider};

        let custom_name = provider_name.strip_prefix("custom:");
        let idx = if let Some(name) = custom_name {
            let customs = config.providers.custom.as_ref();
            let Some(pos) = customs.and_then(|m| m.keys().position(|k| k == name)) else {
                return false;
            };
            CUSTOM_INSTANCES_START + pos
        } else {
            match index_of_provider(provider_name) {
                Some(i) => i,
                None => return false,
            }
        };

        // The custom names the index above points into come from this config,
        // and the key/fields probe runs first: it overwrites the custom fields
        // from the live config, so the session's own values are set after it.
        self.ps.custom_names = config
            .providers
            .custom
            .as_ref()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        self.ps.selected_provider = idx;
        self.ps.detect_existing_key();
        if let Some(name) = custom_name {
            self.ps.custom_name = name.to_string();
            self.ps.base_url = config
                .providers
                .custom
                .as_ref()
                .and_then(|m| m.get(name))
                .and_then(|c| c.base_url.clone())
                .unwrap_or_default();
        }
        self.ps.reload_config_models();

        // A model the provider does not list is still the session's model, so it
        // is added to the list rather than silently replaced by the first entry.
        let listed = self.ps.all_model_names().contains(&model);
        if !listed {
            self.ps.config_models.push(model.to_string());
        }
        self.ps.custom_model = model.to_string();
        self.ps.resolve_selected_model_index();
        true
    }

    /// Create a wizard with existing config.toml values as defaults
    pub fn from_config(config: &crate::config::Config) -> Self {
        let mut wizard = Self::new();

        // The selection below is derived from `config` alone. `new()` pre-fills
        // it from the live config on disk, and inheriting that made the answer
        // depend on the machine running it: on a CI runner with an enabled
        // `claude-cli` in its own config.toml, that pre-fill survived every arm
        // here, so `/models` opened on provider 8 while the fixture asked for
        // the built-in one at 17 (#2077). Reset to the no-config default first,
        // so the arms are the only thing that can set the selection.
        wizard.ps.selected_provider = 0;
        wizard.ps.api_key_input.clear();
        wizard.ps.custom_model.clear();

        // Determine which provider is configured and set selected_provider.
        // Routes by id via `index_of_provider` so reordering PROVIDERS never
        // requires bumping numeric literals in this block.
        use crate::tui::provider_selector::index_of_provider;
        let resolve = |id: &str| index_of_provider(id).unwrap_or(0);
        if config
            .providers
            .anthropic
            .as_ref()
            .is_some_and(|p| p.enabled)
        {
            wizard.ps.selected_provider = resolve("anthropic");
            if let Some(model) = &config
                .providers
                .anthropic
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        } else if config.providers.openai.as_ref().is_some_and(|p| p.enabled) {
            wizard.ps.selected_provider = resolve("openai");
            if let Some(base_url) = &config
                .providers
                .openai
                .as_ref()
                .and_then(|p| p.base_url.clone())
            {
                wizard.ps.base_url = base_url.clone();
            }
            if let Some(model) = &config
                .providers
                .openai
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        } else if config.providers.github.as_ref().is_some_and(|p| p.enabled) {
            wizard.ps.selected_provider = resolve("github");
            if let Some(model) = &config
                .providers
                .github
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        } else if config.providers.gemini.as_ref().is_some_and(|p| p.enabled) {
            wizard.ps.selected_provider = resolve("gemini");
        } else if config
            .providers
            .openrouter
            .as_ref()
            .is_some_and(|p| p.enabled)
        {
            wizard.ps.selected_provider = resolve("openrouter");
            if let Some(model) = &config
                .providers
                .openrouter
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        } else if config.providers.minimax.as_ref().is_some_and(|p| p.enabled) {
            wizard.ps.selected_provider = resolve("minimax");
            if let Some(model) = &config
                .providers
                .minimax
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        } else if config.providers.zai.as_ref().is_some_and(|p| p.enabled) {
            wizard.ps.selected_provider = resolve("zai");
            if let Some(p) = config.providers.zai.as_ref() {
                // Seed the picker with the saved endpoint type (#1596) so
                // /models opens on the real value and a no-change Confirm
                // writes it back instead of the General API default.
                wizard.ps.zhipu_endpoint_type = if p.endpoint_type.as_deref() == Some("coding") {
                    1
                } else {
                    0
                };
                if let Some(model) = &p.default_model {
                    wizard.ps.custom_model = model.clone();
                }
            }
        } else if config
            .providers
            .moonshot
            .as_ref()
            .is_some_and(|p| p.enabled)
        {
            wizard.ps.selected_provider = resolve("moonshot");
            if let Some(p) = config.providers.opencode.as_ref() {
                wizard.ps.opencode_endpoint_type = if p.endpoint_type.as_deref() == Some("zen") {
                    1
                } else {
                    0
                };
            }
            if let Some(p) = config.providers.moonshot.as_ref() {
                wizard.ps.moonshot_endpoint_type = if p.endpoint_type.as_deref() == Some("coding") {
                    1
                } else {
                    0
                };
                wizard.ps.moonshot_plan = p
                    .plan
                    .as_deref()
                    .and_then(|plan| {
                        crate::brain::provider::kimi_plan::PLAN_TIERS
                            .iter()
                            .position(|t| t.eq_ignore_ascii_case(plan.trim()))
                    })
                    .unwrap_or(0);
                if let Some(model) = &p.default_model {
                    wizard.ps.custom_model = model.clone();
                }
            }
        } else if config.providers.xiaomi.as_ref().is_some_and(|p| p.enabled) {
            wizard.ps.selected_provider = resolve("xiaomi");
            if let Some(p) = config.providers.xiaomi.as_ref() {
                // Seed the picker with the saved endpoint type (#1596), same
                // gap as z.ai: without this, /models resets to General API
                // and Confirm persists the reset over the saved choice.
                wizard.ps.xiaomi_endpoint_type = if p.endpoint_type.as_deref() == Some("token-plan")
                {
                    1
                } else {
                    0
                };
                if let Some(model) = &p.default_model {
                    wizard.ps.custom_model = model.clone();
                }
            }
        } else if config
            .providers
            .claude_cli
            .as_ref()
            .is_some_and(|p| p.enabled)
        {
            wizard.ps.selected_provider = resolve("claude-cli");
            if let Some(model) = &config
                .providers
                .claude_cli
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        } else if config
            .providers
            .opencode_cli
            .as_ref()
            .is_some_and(|p| p.enabled)
        {
            wizard.ps.selected_provider = resolve("opencode-cli");
            if let Some(model) = &config
                .providers
                .opencode_cli
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        } else if config
            .providers
            .codex_cli
            .as_ref()
            .is_some_and(|p| p.enabled)
        {
            wizard.ps.selected_provider = resolve("codex-cli");
            if let Some(model) = &config
                .providers
                .codex_cli
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        } else if config
            .providers
            .command_code_cli
            .as_ref()
            .is_some_and(|p| p.enabled)
        {
            wizard.ps.selected_provider = resolve("command-code-cli");
            if let Some(model) = &config
                .providers
                .command_code_cli
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        } else if config
            .providers
            .opencrabs
            .as_ref()
            .is_some_and(|p| p.enabled)
        {
            // The built-in provider, same as `new()` handles it. This arm was
            // the one missing from the chain, so an enabled OpenCrabs section
            // fell through to the custom-provider fallback and `/models`
            // opened on the wrong entry (#2079).
            wizard.ps.selected_provider = resolve("opencrabs");
            wizard.ps.api_key_input = EXISTING_KEY_SENTINEL.to_string();
            if let Some(model) = &config
                .providers
                .opencrabs
                .as_ref()
                .and_then(|p| p.default_model.clone())
            {
                wizard.ps.custom_model = model.clone();
            }
        }

        // Key presence comes from the config we were handed, never from the
        // live config.toml on this machine (#2077).
        wizard.ps.detect_existing_key_from(config);
        wizard.ps.reload_config_models();
        wizard.ps.resolve_selected_model_index();

        // Load channel toggles (indices match CHANNEL_NAMES order)
        wizard.channel_toggles[0].1 = config.channels.telegram.enabled; // Telegram
        wizard.channel_toggles[1].1 = config.channels.discord.enabled; // Discord
        wizard.channel_toggles[2].1 = config.channels.whatsapp.enabled; // WhatsApp
        wizard.channel_toggles[3].1 = config.channels.slack.enabled; // Slack
        wizard.channel_toggles[4].1 = config.channels.trello.enabled; // Trello

        wizard.telegram_rich_text = config.channels.telegram.rich_messages;

        // Load respond_to per channel
        use crate::config::RespondTo;
        wizard.telegram_respond_to = match config.channels.telegram.respond_to {
            RespondTo::All => 0,
            RespondTo::DmOnly => 1,
            RespondTo::Mention => 2,
            RespondTo::Auto => 3,
        };
        wizard.discord_respond_to = match config.channels.discord.respond_to {
            RespondTo::All => 0,
            RespondTo::DmOnly => 1,
            RespondTo::Mention => 2,
            RespondTo::Auto => 3,
        };
        wizard.slack_respond_to = match config.channels.slack.respond_to {
            RespondTo::All => 0,
            RespondTo::DmOnly => 1,
            RespondTo::Mention => 2,
            RespondTo::Auto => 3,
        };

        // Load voice settings
        let vc = config.voice_config();
        // Initial mapping from legacy VoiceConfig (Groq/Local only)
        wizard.stt_provider = if !vc.stt_enabled {
            SttProvider::Off
        } else {
            match vc.stt_mode {
                crate::config::SttMode::Api => SttProvider::Groq,
                crate::config::SttMode::Local => SttProvider::Local,
            }
        };
        wizard.tts_enabled = vc.tts_enabled;
        wizard.tts_provider = if !vc.tts_enabled {
            TtsProvider::Off
        } else {
            match vc.tts_mode {
                crate::config::TtsMode::Api => TtsProvider::OpenAi,
                crate::config::TtsMode::Local => TtsProvider::Local,
            }
        };

        // If Local was saved but the capability isn't available, reset to Off
        if wizard.stt_provider == SttProvider::Local
            && !crate::channels::voice::local_stt_available()
        {
            wizard.stt_provider = SttProvider::Off;
        }
        if wizard.tts_provider == TtsProvider::Local
            && !crate::channels::voice::local_tts_available()
        {
            wizard.tts_provider = TtsProvider::Off;
            wizard.tts_enabled = false;
        }

        wizard.detect_existing_groq_key();

        // Load STT provider from config
        if let Some(stt) = config.providers.stt.as_ref() {
            if stt.voicebox.as_ref().is_some_and(|v| v.enabled) {
                wizard.stt_provider = SttProvider::Voicebox;
                wizard.stt_voicebox_base_url = stt.voicebox.as_ref().unwrap().base_url.clone();
            } else if stt.openai_compatible.as_ref().is_some_and(|v| v.enabled) {
                wizard.stt_provider = SttProvider::OpenAiCompatible;
                let oc = stt.openai_compatible.as_ref().unwrap();
                wizard.stt_openai_compat_base_url = oc.base_url.clone().unwrap_or_default();
                wizard.stt_openai_compat_model =
                    oc.model.clone().unwrap_or_else(|| "whisper-1".to_string());
                wizard.stt_openai_compat_key_input =
                    super::types::EXISTING_KEY_SENTINEL.to_string();
            } else if stt.local.as_ref().is_some_and(|l| l.enabled) {
                wizard.stt_provider = SttProvider::Local;
            } else if stt.groq.as_ref().is_some_and(|g| g.enabled) {
                wizard.stt_provider = SttProvider::Groq;
            } else {
                wizard.stt_provider = SttProvider::Off;
            }
        }

        // Load TTS provider from config
        if let Some(tts) = config.providers.tts.as_ref() {
            if tts.voicebox.as_ref().is_some_and(|v| v.enabled) {
                wizard.tts_provider = TtsProvider::Voicebox;
                wizard.tts_enabled = true;
                let vb = tts.voicebox.as_ref().unwrap();
                wizard.tts_voicebox_base_url = vb.base_url.clone();
                wizard.tts_voicebox_profile_id = vb.profile_id.clone();
                wizard.tts_voicebox_engine = vb.engine.clone();
                wizard.tts_voicebox_engine = vb.engine.clone();
            } else if tts.openai_compatible.as_ref().is_some_and(|v| v.enabled) {
                wizard.tts_provider = TtsProvider::OpenAiCompatible;
                wizard.tts_enabled = true;
                let oc = tts.openai_compatible.as_ref().unwrap();
                wizard.tts_openai_compat_base_url = oc.base_url.clone().unwrap_or_default();
                wizard.tts_openai_compat_model =
                    oc.model.clone().unwrap_or_else(|| "tts-1".to_string());
                wizard.tts_openai_compat_voice =
                    oc.voice.clone().unwrap_or_else(|| "alloy".to_string());
                wizard.tts_openai_compat_key_input =
                    super::types::EXISTING_KEY_SENTINEL.to_string();
            } else if tts.local.as_ref().is_some_and(|l| l.enabled) {
                wizard.tts_provider = TtsProvider::Local;
                wizard.tts_enabled = true;
            } else if tts.openai.as_ref().is_some_and(|o| o.enabled) {
                wizard.tts_provider = TtsProvider::OpenAi;
                wizard.tts_enabled = true;
                // Seed voice from [voice] tts_voice
                wizard.tts_api_voice = vc.tts_voice.clone();
                // Seed key sentinel if a key exists in providers.tts.openai or providers.openai
                let has_tts_key = tts
                    .openai
                    .as_ref()
                    .and_then(|o| o.api_key.as_ref())
                    .is_some_and(|k| !k.is_empty());
                let has_chat_key = config
                    .providers
                    .openai
                    .as_ref()
                    .and_then(|o| o.api_key.as_ref())
                    .is_some_and(|k| !k.is_empty());
                if has_tts_key || has_chat_key {
                    wizard.tts_api_key_input = super::types::EXISTING_KEY_SENTINEL.to_string();
                }
            } else {
                wizard.tts_provider = TtsProvider::Off;
                wizard.tts_enabled = false;
            }
        }

        // Reset Local STT/TTS to Off if capability isn't available (after config loading)
        if wizard.stt_provider == SttProvider::Local
            && !crate::channels::voice::local_stt_available()
        {
            wizard.stt_provider = SttProvider::Off;
        }
        if wizard.tts_provider == TtsProvider::Local
            && !crate::channels::voice::local_tts_available()
        {
            wizard.tts_provider = TtsProvider::Off;
            wizard.tts_enabled = false;
        }

        // Resolve selected Piper voice index from config
        #[cfg(feature = "local-tts")]
        {
            use crate::channels::voice::local_tts::{PIPER_VOICES, piper_voice_exists};
            if let Some(idx) = PIPER_VOICES.iter().position(|v| v.id == vc.local_tts_voice) {
                wizard.selected_tts_voice = idx;
                wizard.tts_voice_downloaded = piper_voice_exists(PIPER_VOICES[idx].id);
            }
        }

        // Resolve selected local model index from config
        #[cfg(feature = "local-stt")]
        {
            use crate::channels::voice::local_whisper::{LOCAL_MODEL_PRESETS, is_model_downloaded};
            if let Some(idx) = LOCAL_MODEL_PRESETS
                .iter()
                .position(|p| p.id == vc.local_stt_model)
            {
                wizard.selected_local_stt_model = idx;
                wizard.stt_model_downloaded = is_model_downloaded(&LOCAL_MODEL_PRESETS[idx]);
            }
        }

        // Load image settings
        wizard.image_vision_enabled = config.image.vision.enabled;
        wizard.image_generation_enabled = config.image.generation.enabled;
        // Prefill the model input from existing config so re-entering
        // the wizard shows what's actually wired today. Empty string =
        // default sentinel; the write path leaves it untouched when blank.
        wizard.image_generation_model_input =
            if config.image.generation.model == crate::config::default_image_model() {
                String::new()
            } else {
                config.image.generation.model.clone()
            };
        wizard.detect_existing_image_key();

        // Mark existing channel data with sentinels so apply_config() won't overwrite.
        // When the user enters a channel sub-step, detect_existing_* re-checks and
        // keeps the sentinel; if they clear + re-enter data the sentinel is replaced.
        use super::types::EXISTING_KEY_SENTINEL;
        let sentinel = || EXISTING_KEY_SENTINEL.to_string();

        if config
            .channels
            .telegram
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            wizard.telegram_token_input = sentinel();
        }
        // Telegram user ID is NOT a secret — show the real stored value so the
        // user can see what's configured instead of a misleading "exists"
        // sentinel that makes them think an ID materialised from nowhere.
        if let Some(first) = config.channels.telegram.allowed_users.first() {
            wizard.telegram_user_id_input = first.clone();
        }
        if config
            .channels
            .discord
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            wizard.discord_token_input = sentinel();
        }
        if !config.channels.discord.allowed_channels.is_empty() {
            wizard.discord_channel_id_input = sentinel();
        }
        if !config.channels.discord.allowed_users.is_empty() {
            wizard.discord_allowed_list_input = sentinel();
        }
        if config
            .channels
            .slack
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            wizard.slack_bot_token_input = sentinel();
        }
        if config
            .channels
            .slack
            .app_token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            wizard.slack_app_token_input = sentinel();
        }
        if !config.channels.slack.allowed_channels.is_empty() {
            wizard.slack_channel_id_input = sentinel();
        }
        if !config.channels.slack.allowed_users.is_empty() {
            wizard.slack_allowed_list_input = sentinel();
        }
        // Trello
        if config
            .channels
            .trello
            .app_token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            wizard.trello_api_key_input = sentinel();
        }
        if config
            .channels
            .trello
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            wizard.trello_api_token_input = sentinel();
        }
        if !config.channels.trello.board_ids.is_empty() {
            wizard.trello_board_id_input = sentinel();
        }
        if !config.channels.trello.allowed_users.is_empty() {
            wizard.trello_allowed_users_input = sentinel();
        }
        // WhatsApp
        if !config.channels.whatsapp.allowed_phones.is_empty() {
            wizard.whatsapp_phone_input = sentinel();
        }
        // WhatsApp: check if session.db exists (means it's paired)
        let wa_session = crate::config::opencrabs_home()
            .join("whatsapp")
            .join("session.db");
        wizard.whatsapp_connected = wa_session.exists();

        // Jump directly to provider auth step since config exists
        wizard.step = OnboardingStep::ProviderAuth;
        wizard.auth_field = AuthField::Provider;

        wizard
    }
}
