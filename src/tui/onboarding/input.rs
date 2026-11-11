use crossterm::event::{KeyCode, KeyEvent};

use super::helpers::{handle_text_paste, is_clear_field};
use super::types::*;
use super::wizard::OnboardingWizard;

impl OnboardingWizard {
    /// Handle key events for the current step
    /// Returns `WizardAction` indicating what the app should do
    pub fn handle_key(&mut self, event: KeyEvent) -> WizardAction {
        // Global: Escape goes back (but if model filter is active, clear it first)
        if event.code == KeyCode::Esc {
            if self.quick_jump {
                return WizardAction::Cancel;
            }
            if !self.ps.model_filter.is_empty() {
                self.ps.model_filter.clear();
                self.ps.selected_model = 0;
                return WizardAction::None;
            }
            if self.prev_step() {
                // A first run has nothing to fall back to: dropping into chat
                // would leave the user talking to no provider. Quit instead;
                // `is_first_time()` reopens the wizard on the next launch.
                if self.is_first_time {
                    return WizardAction::Quit;
                }
                return WizardAction::Cancel;
            }
            return WizardAction::None;
        }

        // Global: Page Up / Page Down let the user scroll the wizard
        // body independently of focus. Important on small terminals
        // where the form's last fields (API Key, Model, Context for
        // custom providers) fall below the viewport even though the
        // user hasn't Tabbed to them yet. Without this they had to
        // guess fields existed below.
        if event.code == KeyCode::PageDown {
            self.user_scroll_offset = self.user_scroll_offset.saturating_add(5);
            return WizardAction::None;
        }
        if event.code == KeyCode::PageUp {
            self.user_scroll_offset = self.user_scroll_offset.saturating_sub(5);
            return WizardAction::None;
        }

        let action = match self.step {
            OnboardingStep::ModeSelect => self.handle_mode_select_key(event),
            OnboardingStep::ProviderAuth => self.handle_provider_auth_key(event),
            OnboardingStep::Workspace => self.handle_workspace_key(event),
            OnboardingStep::Channels => self.handle_channels_key(event),
            OnboardingStep::TelegramSetup => self.handle_telegram_setup_key(event),
            OnboardingStep::DiscordSetup => self.handle_discord_setup_key(event),
            OnboardingStep::WhatsAppSetup => self.handle_whatsapp_setup_key(event),
            OnboardingStep::SlackSetup => self.handle_slack_setup_key(event),
            OnboardingStep::TrelloSetup => self.handle_trello_setup_key(event),
            OnboardingStep::VoiceSetup => self.handle_voice_setup_key(event),
            OnboardingStep::ImageSetup => self.handle_image_setup_key(event),
            OnboardingStep::Daemon => self.handle_daemon_key(event),
            OnboardingStep::HealthCheck => self.handle_health_check_key(event),
            OnboardingStep::BrainSetup => self.handle_brain_setup_key(event),
            OnboardingStep::Complete => WizardAction::Complete,
        };
        if self.quick_jump_done {
            self.quick_jump_done = false;
            return WizardAction::QuickJumpDone;
        }
        action
    }

    /// Handle paste event - inserts text at current cursor position
    pub fn handle_paste(&mut self, text: &str) -> WizardAction {
        // Brain setup accepts full multi-line markdown paste — handle it
        // before the single-line sanitizer so the user can dump a whole
        // markdown document (or raw prose) into the About fields.
        if self.step == OnboardingStep::BrainSetup {
            // Normalize CRLF → LF so Windows clipboards don't leave stray \r.
            let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            let trimmed = normalized.trim();
            if trimmed.is_empty() {
                return WizardAction::None;
            }
            // Paste replaces untouched template, appends to edited field
            let field = match self.brain_field {
                BrainField::AboutMe => &mut self.about_me,
                BrainField::AboutAgent => &mut self.about_opencrabs,
            };
            let edited = match self.brain_field {
                BrainField::AboutMe => self.brain_me_edited,
                BrainField::AboutAgent => self.brain_agent_edited,
            };
            if !edited || field.is_empty() {
                // Replace template entirely with pasted content
                *field = trimmed.to_string();
            } else {
                field.push('\n');
                field.push_str(trimmed);
            }
            // Mark as edited after paste
            match self.brain_field {
                BrainField::AboutMe => self.brain_me_edited = true,
                BrainField::AboutAgent => self.brain_agent_edited = true,
            }
            return WizardAction::None;
        }

        // All other steps expect single-line input (API keys, channel IDs,
        // etc.). Collapse the paste to its first non-empty line and strip
        // surrounding whitespace to defend against trailing newlines.
        let clean = text.split(['\r', '\n']).next().unwrap_or("").trim();
        if clean.is_empty() {
            return WizardAction::None;
        }

        // Dispatch paste based on current step first, then auth_field
        match self.step {
            OnboardingStep::TelegramSetup => {
                tracing::debug!(
                    "[paste] Telegram pasted ({} chars) field={:?}",
                    clean.len(),
                    self.telegram_field
                );
                let field = self.telegram_field;
                let existing_token = self.has_existing_telegram_token();
                let existing_uid = self.has_existing_telegram_user_id();
                match field {
                    TelegramField::BotToken => {
                        handle_text_paste(
                            clean,
                            &mut self.telegram_token_input,
                            &mut self.channel_input_cursor,
                            existing_token,
                            None,
                        );
                    }
                    TelegramField::UserID => {
                        handle_text_paste(
                            clean,
                            &mut self.telegram_user_id_input,
                            &mut self.channel_input_cursor,
                            existing_uid,
                            Some(|c: char| c.is_ascii_digit()),
                        );
                    }
                    // Checkbox and selector: paste is a no-op.
                    TelegramField::RichText | TelegramField::RespondTo => {}
                }
            }
            OnboardingStep::DiscordSetup => {
                tracing::debug!(
                    "[paste] Discord pasted ({} chars) field={:?}",
                    clean.len(),
                    self.discord_field
                );
                let field = self.discord_field;
                let existing_token = self.has_existing_discord_token();
                let existing_ch = self.has_existing_discord_channel_id();
                let existing_al = self.has_existing_discord_allowed_list();
                match field {
                    DiscordField::BotToken => {
                        handle_text_paste(
                            clean,
                            &mut self.discord_token_input,
                            &mut self.channel_input_cursor,
                            existing_token,
                            None,
                        );
                    }
                    DiscordField::ChannelID => {
                        handle_text_paste(
                            clean,
                            &mut self.discord_channel_id_input,
                            &mut self.channel_input_cursor,
                            existing_ch,
                            None,
                        );
                    }
                    DiscordField::AllowedList => {
                        handle_text_paste(
                            clean,
                            &mut self.discord_allowed_list_input,
                            &mut self.channel_input_cursor,
                            existing_al,
                            Some(|c: char| c.is_ascii_digit()),
                        );
                    }
                    DiscordField::RespondTo => {} // selector, paste is no-op
                }
            }
            OnboardingStep::SlackSetup => {
                tracing::debug!(
                    "[paste] Slack pasted ({} chars) field={:?}",
                    clean.len(),
                    self.slack_field
                );
                let field = self.slack_field;
                let existing_bot = self.has_existing_slack_bot_token();
                let existing_app = self.has_existing_slack_app_token();
                let existing_ch = self.has_existing_slack_channel_id();
                let existing_al = self.has_existing_slack_allowed_list();
                match field {
                    SlackField::BotToken => {
                        handle_text_paste(
                            clean,
                            &mut self.slack_bot_token_input,
                            &mut self.channel_input_cursor,
                            existing_bot,
                            None,
                        );
                    }
                    SlackField::AppToken => {
                        handle_text_paste(
                            clean,
                            &mut self.slack_app_token_input,
                            &mut self.channel_input_cursor,
                            existing_app,
                            None,
                        );
                    }
                    SlackField::ChannelID => {
                        handle_text_paste(
                            clean,
                            &mut self.slack_channel_id_input,
                            &mut self.channel_input_cursor,
                            existing_ch,
                            None,
                        );
                    }
                    SlackField::AllowedList => {
                        handle_text_paste(
                            clean,
                            &mut self.slack_allowed_list_input,
                            &mut self.channel_input_cursor,
                            existing_al,
                            None,
                        );
                    }
                    SlackField::RespondTo => {} // selector, paste is no-op
                }
            }
            OnboardingStep::TrelloSetup => {
                tracing::debug!(
                    "[paste] Trello pasted ({} chars) field={:?}",
                    clean.len(),
                    self.trello_field
                );
                let field = self.trello_field;
                let existing_ak = self.has_existing_trello_api_key();
                let existing_at = self.has_existing_trello_api_token();
                let existing_bd = self.has_existing_trello_board_id();
                let existing_au = self.has_existing_trello_allowed_users();
                match field {
                    TrelloField::ApiKey => {
                        handle_text_paste(
                            clean,
                            &mut self.trello_api_key_input,
                            &mut self.channel_input_cursor,
                            existing_ak,
                            None,
                        );
                    }
                    TrelloField::ApiToken => {
                        handle_text_paste(
                            clean,
                            &mut self.trello_api_token_input,
                            &mut self.channel_input_cursor,
                            existing_at,
                            None,
                        );
                    }
                    TrelloField::BoardId => {
                        handle_text_paste(
                            clean,
                            &mut self.trello_board_id_input,
                            &mut self.channel_input_cursor,
                            existing_bd,
                            None,
                        );
                    }
                    TrelloField::AllowedUsers => {
                        handle_text_paste(
                            clean,
                            &mut self.trello_allowed_users_input,
                            &mut self.channel_input_cursor,
                            existing_au,
                            None,
                        );
                    }
                }
            }
            OnboardingStep::WhatsAppSetup
                if self.whatsapp_field == WhatsAppField::PhoneAllowlist =>
            {
                let existing = self.has_existing_whatsapp_phone();
                handle_text_paste(
                    clean,
                    &mut self.whatsapp_phone_input,
                    &mut self.channel_input_cursor,
                    existing,
                    Some(|c: char| c.is_ascii_digit() || c == '+' || c == '-'),
                );
            }
            OnboardingStep::VoiceSetup => match self.voice_field {
                VoiceField::GroqApiKey => {
                    tracing::debug!("[paste] Groq API key pasted ({} chars)", clean.len());
                    if self.has_existing_groq_key() {
                        self.groq_api_key_input.clear();
                    }
                    self.groq_api_key_input.push_str(clean);
                }
                VoiceField::SttOpenaiCompatUrl => {
                    self.stt_openai_compat_base_url.push_str(clean);
                }
                VoiceField::SttOpenaiCompatModel => {
                    self.stt_openai_compat_model.clear();
                    self.stt_openai_compat_model.push_str(clean);
                }
                VoiceField::SttOpenaiCompatKey => {
                    // Pasting over a seeded marker replaces the stored key; it
                    // does not append to the marker (#1075).
                    super::key_field::clear_marker_before_edit(
                        &mut self.stt_openai_compat_key_input,
                    );
                    self.stt_openai_compat_key_input.push_str(clean);
                }
                VoiceField::SttVoiceboxUrl => {
                    self.stt_voicebox_base_url.push_str(clean);
                }
                VoiceField::TtsOpenaiCompatUrl => {
                    self.tts_openai_compat_base_url.push_str(clean);
                }
                VoiceField::TtsOpenaiCompatModel => {
                    self.tts_openai_compat_model.clear();
                    self.tts_openai_compat_model.push_str(clean);
                }
                VoiceField::TtsOpenaiCompatVoice => {
                    self.tts_openai_compat_voice.clear();
                    self.tts_openai_compat_voice.push_str(clean);
                }
                VoiceField::TtsOpenaiCompatKey => {
                    super::key_field::clear_marker_before_edit(
                        &mut self.tts_openai_compat_key_input,
                    );
                    self.tts_openai_compat_key_input.push_str(clean);
                }
                VoiceField::TtsApiKey => {
                    // The field that produced the corrupted key on disk: seeded
                    // with the marker by `from_config`, pasted into, saved whole.
                    super::key_field::clear_marker_before_edit(&mut self.tts_api_key_input);
                    self.tts_api_key_input.push_str(clean);
                }
                VoiceField::TtsVoiceboxUrl => {
                    self.tts_voicebox_base_url.push_str(clean);
                }
                VoiceField::TtsVoiceboxProfileId => {
                    self.tts_voicebox_profile_id.clear();
                    self.tts_voicebox_profile_id.push_str(clean);
                }
                VoiceField::TtsVoiceboxEngine => {
                    self.tts_voicebox_engine.clear();
                    self.tts_voicebox_engine.push_str(clean);
                }
                _ => {
                    tracing::debug!(
                        "[paste] Voice field {:?} does not accept text input, ignoring paste",
                        self.voice_field
                    );
                }
            },
            OnboardingStep::ImageSetup if self.image_field == ImageField::ApiKey => {
                tracing::debug!("[paste] Google API key pasted ({} chars)", clean.len());
                if self.has_existing_image_key() {
                    self.image_api_key_input.clear();
                }
                self.image_api_key_input.push_str(clean);
            }
            OnboardingStep::ProviderAuth => match self.auth_field {
                AuthField::ApiKey | AuthField::CustomApiKey => {
                    if self.ps.has_existing_key_sentinel() {
                        self.ps.api_key_input.clear();
                    }
                    self.ps.api_key_input.push_str(clean);
                    self.ps.api_key_cursor = self.ps.api_key_input.len();
                    // Custom provider: if base_url is already set, fetch models after key paste
                    if self.ps.is_custom() && !self.ps.base_url.is_empty() {
                        self.ps.models.clear();
                        self.ps.selected_model = 0;
                        return WizardAction::FetchModels;
                    }
                }
                AuthField::CustomName => {
                    self.ps.custom_name.push_str(clean);
                }
                AuthField::CustomBaseUrl => {
                    self.ps.base_url.push_str(clean);
                    // Normalize pasted URL: strip trailing slashes and
                    // /chat/completions suffix. Keep /v1 intact so routers
                    // like Dialagram (/router/v1) get /v1/chat/completions.
                    let normalized = self.ps.base_url.trim_end_matches('/');
                    let normalized = normalized
                        .strip_suffix("/chat/completions")
                        .unwrap_or(normalized)
                        .to_string();
                    self.ps.base_url = normalized;
                    if !self.ps.base_url.is_empty() {
                        self.ps.models.clear();
                        self.ps.selected_model = 0;
                        return WizardAction::FetchModels;
                    }
                }
                AuthField::CustomModel => {
                    self.ps.custom_model.push_str(clean);
                }
                AuthField::ZhipuEndpointType => {
                    // User pasted on endpoint type — they meant to paste API key
                    self.auth_field = AuthField::ApiKey;
                    if self.ps.has_existing_key_sentinel() {
                        self.ps.api_key_input.clear();
                    }
                    self.ps.api_key_input.push_str(clean);
                    self.ps.api_key_cursor = self.ps.api_key_input.len();
                }
                AuthField::XiaomiEndpointType => {
                    // User pasted on endpoint type — they meant to paste API key
                    self.auth_field = AuthField::ApiKey;
                    if self.ps.has_existing_key_sentinel() {
                        self.ps.api_key_input.clear();
                    }
                    self.ps.api_key_input.push_str(clean);
                    self.ps.api_key_cursor = self.ps.api_key_input.len();
                }
                AuthField::OpenCodeEndpointType => {
                    // User pasted on endpoint type — they meant to paste API key
                    self.auth_field = AuthField::ApiKey;
                    if self.ps.has_existing_key_sentinel() {
                        self.ps.api_key_input.clear();
                    }
                    self.ps.api_key_input.push_str(clean);
                    self.ps.api_key_cursor = self.ps.api_key_input.len();
                }
                AuthField::MoonshotEndpointType => {
                    // User pasted on endpoint type — they meant to paste API key
                    self.auth_field = AuthField::ApiKey;
                    if self.ps.has_existing_key_sentinel() {
                        self.ps.api_key_input.clear();
                    }
                    self.ps.api_key_input.push_str(clean);
                    self.ps.api_key_cursor = self.ps.api_key_input.len();
                }
                _ => {}
            },
            _ => {}
        }
        WizardAction::None
    }

    // --- Step-specific key handlers ---

    pub(super) fn handle_mode_select_key(&mut self, event: KeyEvent) -> WizardAction {
        match event.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = WizardMode::QuickStart;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = WizardMode::Advanced;
            }
            KeyCode::Char('1') => {
                self.mode = WizardMode::QuickStart;
            }
            KeyCode::Char('2') => {
                self.mode = WizardMode::Advanced;
            }
            KeyCode::Enter => {
                // A first run in quick start goes straight to chat on the built-in
                // provider, so it skips the steps below (#2048).
                if self.mode == WizardMode::QuickStart && self.is_first_time {
                    return WizardAction::QuickStartBuiltin;
                }
                self.next_step();
                // If entering ProviderAuth with existing key detected, pre-fetch models
                if self.step == OnboardingStep::ProviderAuth
                    && self.ps.has_existing_key_sentinel()
                    && self.ps.supports_model_fetch()
                {
                    return WizardAction::FetchModels;
                }
            }
            _ => {}
        }
        WizardAction::None
    }

    pub(super) fn handle_provider_auth_key(&mut self, event: KeyEvent) -> WizardAction {
        match self.auth_field {
            AuthField::Provider => match event.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    let order = self.ps.provider_display_order();
                    let pos = order
                        .iter()
                        .position(|&i| i == self.ps.selected_provider)
                        .unwrap_or(0);
                    if pos > 0 {
                        self.ps.selected_provider = order[pos - 1];
                    }
                    self.model_fetch_lifecycle.invalidate();
                    self.ps.models_fetching = false;
                    self.ps.selected_model = 0;
                    self.ps.model_filter.clear();
                    self.ps.api_key_input.clear();
                    self.ps.models.clear();
                    self.ps.config_models.clear();
                    self.ps.load_custom_fields();
                    self.ps.reload_config_models();
                    self.ps.detect_existing_key();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let order = self.ps.provider_display_order();
                    let pos = order
                        .iter()
                        .position(|&i| i == self.ps.selected_provider)
                        .unwrap_or(0);
                    if pos + 1 < order.len() {
                        self.ps.selected_provider = order[pos + 1];
                    }
                    self.model_fetch_lifecycle.invalidate();
                    self.ps.models_fetching = false;
                    self.ps.selected_model = 0;
                    self.ps.model_filter.clear();
                    self.ps.api_key_input.clear();
                    self.ps.models.clear();
                    self.ps.config_models.clear();
                    self.ps.load_custom_fields();
                    self.ps.reload_config_models();
                    self.ps.detect_existing_key();
                }
                KeyCode::Enter | KeyCode::Tab => {
                    self.ps.detect_existing_key();
                    if self.ps.provider_id() == "github" {
                        // GitHub Copilot: if already authenticated, go to model select
                        if self.ps.has_existing_key_sentinel() {
                            self.auth_field = AuthField::Model;
                            self.ps.models.clear();
                            self.ps.selected_model = 0;
                            return WizardAction::FetchModels;
                        }
                        // Not yet authenticated — start device flow
                        return WizardAction::GitHubDeviceFlow;
                    } else if self.ps.provider_id() == "codex" {
                        // Codex OAuth: if already authenticated, go to model select
                        if self.ps.has_existing_key_sentinel() {
                            self.auth_field = AuthField::Model;
                            self.ps.models.clear();
                            self.ps.selected_model = 0;
                            return WizardAction::FetchModels;
                        }
                        // Not yet authenticated — start device flow
                        return WizardAction::CodexDeviceFlow;
                    } else if self.ps.is_custom() {
                        self.auth_field = AuthField::CustomName;
                    } else if self.ps.is_keyless() {
                        // Keyless providers — CLI subprocesses AND any API
                        // provider with an empty key_label. No API key, so skip
                        // the key field and go straight to model selection.
                        self.auth_field = AuthField::Model;
                        self.ps.models.clear();
                        self.ps.selected_model = 0;
                        if self.ps.supports_model_fetch() {
                            return WizardAction::FetchModels;
                        }
                        // No live fetch — load static models from the example config
                        self.ps.config_models = crate::tui::provider_selector::load_default_models(
                            self.ps.provider_id(),
                        );
                        // A provider whose list holds one model has no decision
                        // on this screen, so Enter leaves it on its own (#2782).
                        // Tab only moves focus and must not commit config.
                        if matches!(event.code, KeyCode::Enter)
                            && let Some(action) = self.advance_past_sole_model()
                        {
                            return action;
                        }
                    } else if self.ps.provider_id() == "opencode" {
                        // OpenCode: Go or Zen plan first, then API key (#2040)
                        self.auth_field = AuthField::OpenCodeEndpointType;
                    } else if self.ps.provider_id() == "zai" {
                        // z.ai GLM: endpoint type first, then API key
                        self.auth_field = AuthField::ZhipuEndpointType;
                    } else if self.ps.provider_id() == "xiaomi" {
                        // Xiaomi: endpoint type first, then API key
                        self.auth_field = AuthField::XiaomiEndpointType;
                    } else if self.ps.provider_id() == "moonshot" {
                        // Moonshot AI: plan picker first, then API key
                        self.auth_field = AuthField::MoonshotEndpointType;
                    } else {
                        self.auth_field = AuthField::ApiKey;
                    }
                }
                _ => {}
            },
            AuthField::ApiKey => match event.code {
                KeyCode::Char(c) => {
                    // If existing key is loaded and user starts typing, clear it (replace mode)
                    if self.ps.has_existing_key_sentinel() {
                        self.ps.api_key_input.clear();
                    }
                    self.ps.api_key_input.push(c);
                    self.ps.api_key_cursor = self.ps.api_key_input.len();
                }
                KeyCode::Backspace if is_clear_field(&event) => {
                    self.ps.api_key_input.clear();
                    self.ps.api_key_cursor = 0;
                }
                KeyCode::Backspace => {
                    // If existing key sentinel, clear entirely on backspace
                    if self.ps.has_existing_key_sentinel() {
                        self.ps.api_key_input.clear();
                    } else {
                        self.ps.api_key_input.pop();
                    }
                    self.ps.api_key_cursor = self.ps.api_key_input.len();
                }
                KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                    self.auth_field = AuthField::Model;
                    // Fetch live models when provider supports it
                    // Custom providers: always try (local endpoints like Ollama/LM Studio need no key)
                    // Built-in providers: require a key
                    let should_fetch = if self.ps.is_custom() {
                        self.ps.supports_model_fetch()
                    } else {
                        self.ps.supports_model_fetch()
                            && (!self.ps.api_key_input.is_empty()
                                || self.ps.has_existing_key_sentinel())
                    };
                    if should_fetch {
                        self.ps.models.clear();
                        self.ps.selected_model = 0;
                        return WizardAction::FetchModels;
                    }
                    // For providers without live fetch, load defaults from config.toml.example
                    if self.ps.config_models.is_empty() && self.ps.models.is_empty() {
                        self.ps.config_models = crate::tui::provider_selector::load_default_models(
                            self.ps.provider_id(),
                        );
                        self.ps.selected_model = 0;
                    }
                    // Enter is a commit, so a provider whose catalogue holds a
                    // single model skips the picker it has no decision on
                    // (#2782). Tab and Down only move focus and never commit.
                    if matches!(event.code, KeyCode::Enter)
                        && let Some(action) = self.advance_past_sole_model()
                    {
                        return action;
                    }
                }
                KeyCode::BackTab | KeyCode::Up => {
                    if self.ps.provider_id() == "zai" {
                        self.auth_field = AuthField::ZhipuEndpointType;
                    } else if self.ps.provider_id() == "xiaomi" {
                        self.auth_field = AuthField::XiaomiEndpointType;
                    } else {
                        self.auth_field = AuthField::Provider;
                    }
                }
                _ => {}
            },
            AuthField::Model => match event.code {
                KeyCode::Up => {
                    self.ps.selected_model = self.ps.selected_model.saturating_sub(1);
                }
                KeyCode::Down => {
                    let count = self.ps.model_count();
                    if count > 0 {
                        self.ps.selected_model = (self.ps.selected_model + 1).min(count - 1);
                    }
                }
                KeyCode::Char(c) if event.modifiers.is_empty() => {
                    self.ps.model_filter.push(c);
                    self.ps.selected_model = 0; // reset selection on filter change
                }
                KeyCode::Backspace => {
                    if self.ps.model_filter.is_empty() {
                        // Keyless providers (CLI or empty key_label) have no API
                        // key field — go straight back to Provider.
                        if self.ps.is_keyless() {
                            self.auth_field = AuthField::Provider;
                        } else {
                            self.auth_field = AuthField::ApiKey;
                        }
                    } else {
                        self.ps.model_filter.pop();
                        self.ps.selected_model = 0;
                    }
                }
                KeyCode::Enter => {
                    return self.commit_model_step();
                }
                KeyCode::BackTab => {
                    if self.ps.is_keyless() {
                        self.auth_field = AuthField::Provider;
                    } else {
                        self.auth_field = AuthField::ApiKey;
                    }
                    self.ps.model_filter.clear();
                    self.ps.selected_model = 0;
                }
                // Tab: deliberately a no-op on Model. Was `self.next_step()`
                // which silently committed config (disabled every other
                // provider, enabled this one, rebuilt the agent) when the
                // user pressed Tab to navigate. 2026-05-28 bug: a single
                // unintended Tab while inspecting models in /models
                // switched the active provider with no confirmation prompt.
                // Only Enter commits now.
                _ => {}
            },
            AuthField::CustomName => match event.code {
                KeyCode::Char(c) => {
                    self.ps.custom_name.push(c);
                }
                KeyCode::Backspace if is_clear_field(&event) => {
                    self.ps.custom_name.clear();
                }
                KeyCode::Backspace => {
                    self.ps.custom_name.pop();
                }
                KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                    if self.ps.custom_name.is_empty() {
                        self.error_message =
                            Some("Enter a name identifier for this provider".to_string());
                        return WizardAction::None;
                    }
                    self.ps.custom_name = crate::config::normalize_toml_key(&self.ps.custom_name);
                    self.auth_field = AuthField::CustomBaseUrl;
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.auth_field = AuthField::Provider;
                }
                _ => {}
            },
            AuthField::CustomBaseUrl => match event.code {
                KeyCode::Char(c) => {
                    self.ps.base_url.push(c);
                }
                KeyCode::Backspace if is_clear_field(&event) => {
                    self.ps.base_url.clear();
                }
                KeyCode::Backspace => {
                    self.ps.base_url.pop();
                }
                KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                    // Normalize base URL: strip trailing slashes and
                    // /chat/completions suffix. Keep /v1 intact so routers
                    // like Dialagram (/router/v1) get /v1/chat/completions.
                    let normalized = self.ps.base_url.trim_end_matches('/');
                    let normalized = normalized
                        .strip_suffix("/chat/completions")
                        .unwrap_or(normalized)
                        .to_string();
                    self.ps.base_url = normalized;
                    self.auth_field = AuthField::CustomApiKey;
                    if !self.ps.base_url.is_empty() {
                        self.ps.models.clear();
                        self.ps.selected_model = 0;
                        return WizardAction::FetchModels;
                    }
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.auth_field = AuthField::CustomName;
                }
                _ => {}
            },
            AuthField::CustomApiKey => match event.code {
                KeyCode::Char(c) => {
                    if self.ps.has_existing_key_sentinel() {
                        self.ps.api_key_input.clear();
                    }
                    self.ps.api_key_input.push(c);
                }
                KeyCode::Backspace if is_clear_field(&event) => {
                    self.ps.api_key_input.clear();
                }
                KeyCode::Backspace => {
                    if self.ps.has_existing_key_sentinel() {
                        self.ps.api_key_input.clear();
                    } else {
                        self.ps.api_key_input.pop();
                    }
                }
                KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                    self.auth_field = AuthField::CustomModel;
                    if !self.ps.base_url.is_empty() {
                        self.ps.models.clear();
                        self.ps.selected_model = 0;
                        return WizardAction::FetchModels;
                    }
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.auth_field = AuthField::CustomBaseUrl;
                }
                _ => {}
            },
            AuthField::CustomModel => {
                // If models were fetched, treat as list picker; otherwise free-text
                if self.ps.models.is_empty() {
                    match event.code {
                        KeyCode::Char(c) => {
                            self.ps.custom_model.push(c);
                        }
                        KeyCode::Backspace if is_clear_field(&event) => {
                            self.ps.custom_model.clear();
                        }
                        KeyCode::Backspace => {
                            self.ps.custom_model.pop();
                        }
                        KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                            // Empty model + base_url that supports it: refetch the live
                            // catalogue so the user can pick instead of typing a name.
                            if self.ps.custom_model.trim().is_empty()
                                && self.ps.supports_model_fetch()
                            {
                                return WizardAction::FetchModels;
                            }
                            self.auth_field = AuthField::CustomContextWindow;
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            self.auth_field = AuthField::CustomApiKey;
                        }
                        _ => {}
                    }
                } else {
                    // Models fetched — filterable list picker. Navigate with the
                    // ARROW keys only (matching AuthField::Model); every character
                    // — including `j`/`k` — types into the filter, so a model ID
                    // that isn't in the fetched list (and may contain j/k, e.g.
                    // `kimi-k2`) can be typed and committed as a custom model on
                    // Enter (#676). The old j/k vim-nav stole those letters and
                    // made such IDs impossible to enter.
                    match event.code {
                        KeyCode::Char(c) => {
                            self.ps.model_filter.push(c);
                            self.ps.selected_model = 0;
                        }
                        KeyCode::Backspace if is_clear_field(&event) => {
                            self.ps.model_filter.clear();
                            self.ps.selected_model = 0;
                        }
                        KeyCode::Backspace => {
                            self.ps.model_filter.pop();
                            let filter = self.ps.model_filter.to_lowercase();
                            let count = self
                                .ps
                                .models
                                .iter()
                                .filter(|m| m.to_lowercase().contains(&filter))
                                .count();
                            if self.ps.selected_model >= count && count > 0 {
                                self.ps.selected_model = count - 1;
                            }
                        }
                        KeyCode::Esc => {
                            self.ps.model_filter.clear();
                            self.ps.selected_model = 0;
                        }
                        KeyCode::Up => {
                            self.ps.selected_model = self.ps.selected_model.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            let filter = self.ps.model_filter.to_lowercase();
                            let count = self
                                .ps
                                .models
                                .iter()
                                .filter(|m| m.to_lowercase().contains(&filter))
                                .count();
                            if count > 0 {
                                self.ps.selected_model =
                                    (self.ps.selected_model + 1).min(count - 1);
                            }
                        }
                        KeyCode::Enter | KeyCode::Tab => {
                            // Resolve the user's pick before advancing.
                            // If the filter matches at least one fetched
                            // model, take filtered[selected_model]. If
                            // the filter is non-empty but matches NO
                            // fetched model, treat the typed text as a
                            // manual model name and push it into the
                            // models list so the save flow persists it
                            // alongside the other entries — common when
                            // a provider exposes a model that
                            // `/v1/models` doesn't advertise.
                            let filter = self.ps.model_filter.to_lowercase();
                            let filtered: Vec<&String> = self
                                .ps
                                .models
                                .iter()
                                .filter(|m| m.to_lowercase().contains(&filter))
                                .collect();
                            // An EXACT typed match wins over a substring hit
                            // (#873). Filtering is `contains`, so against a
                            // large catalogue a typed id is usually also a
                            // substring of something else, and taking the
                            // substring hit silently committed a DIFFERENT
                            // model than the one typed — with no indication.
                            // Only fall through to the highlighted row when the
                            // typed text is not itself a catalogue entry.
                            let typed_raw = self.ps.model_filter.trim();
                            let typed_is_exact = !typed_raw.is_empty()
                                && self.ps.models.iter().any(|m| m == typed_raw);
                            if typed_is_exact {
                                self.ps.custom_model = typed_raw.to_string();
                            } else if let Some(m) = filtered.get(self.ps.selected_model) {
                                self.ps.custom_model = (*m).clone();
                            } else if !self.ps.model_filter.trim().is_empty() {
                                let typed = self.ps.model_filter.trim().to_string();
                                self.ps.custom_model = typed.clone();
                                if !self.ps.models.iter().any(|m| m == &typed) {
                                    self.ps.models.push(typed);
                                }
                            }
                            self.ps.model_filter.clear();
                            self.ps.selected_model = 0;
                            self.auth_field = AuthField::CustomContextWindow;
                        }
                        KeyCode::BackTab => {
                            self.auth_field = AuthField::CustomApiKey;
                        }
                        _ => {}
                    }
                }
            }
            AuthField::ZhipuEndpointType => match event.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
                    // Toggle between 0 (api) and 1 (coding)
                    self.ps.zhipu_endpoint_type = 1 - self.ps.zhipu_endpoint_type;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    // Endpoint type selected → now enter API key
                    self.auth_field = AuthField::ApiKey;
                }
                KeyCode::BackTab => {
                    self.auth_field = AuthField::Provider;
                }
                _ => {}
            },
            AuthField::XiaomiEndpointType => match event.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
                    // Toggle between 0 (api) and 1 (token-plan)
                    self.ps.xiaomi_endpoint_type = 1 - self.ps.xiaomi_endpoint_type;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    // Endpoint type selected → now enter API key
                    self.auth_field = AuthField::ApiKey;
                }
                KeyCode::BackTab => {
                    self.auth_field = AuthField::Provider;
                }
                _ => {}
            },
            AuthField::OpenCodeEndpointType => match event.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
                    // Toggle between 0 (Go) and 1 (Zen)
                    self.ps.opencode_endpoint_type = 1 - self.ps.opencode_endpoint_type;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    self.auth_field = AuthField::ApiKey;
                }
                KeyCode::BackTab => {
                    self.auth_field = AuthField::Provider;
                }
                _ => {}
            },
            AuthField::MoonshotEndpointType => match event.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
                    // Toggle between 0 (api plan) and 1 (coding plan)
                    self.ps.moonshot_endpoint_type = 1 - self.ps.moonshot_endpoint_type;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    // Coding plan → pick the subscription tier next; API plan
                    // has no tier, so go straight to the API key.
                    if self.ps.moonshot_endpoint_type == 1 {
                        self.auth_field = AuthField::MoonshotPlan;
                    } else {
                        self.auth_field = AuthField::ApiKey;
                    }
                }
                KeyCode::BackTab => {
                    self.auth_field = AuthField::Provider;
                }
                _ => {}
            },
            AuthField::MoonshotPlan => match event.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    let n = crate::brain::provider::kimi_plan::PLAN_TIERS.len();
                    self.ps.moonshot_plan = (self.ps.moonshot_plan + n - 1) % n;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let n = crate::brain::provider::kimi_plan::PLAN_TIERS.len();
                    self.ps.moonshot_plan = (self.ps.moonshot_plan + 1) % n;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    // Tier selected → now enter API key
                    self.auth_field = AuthField::ApiKey;
                }
                KeyCode::BackTab => {
                    self.auth_field = AuthField::MoonshotEndpointType;
                }
                _ => {}
            },
            AuthField::CustomContextWindow => match event.code {
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    self.ps.context_window.push(c);
                }
                KeyCode::Backspace if is_clear_field(&event) => {
                    self.ps.context_window.clear();
                }
                KeyCode::Backspace => {
                    self.ps.context_window.pop();
                }
                KeyCode::Enter => {
                    // CustomContextWindow is the LAST step in the custom-
                    // provider flow. Only Enter commits. Tab and Down used
                    // to call next_step() and auto-saved without the user
                    // confirming — same class of bug as Tab on the Model
                    // field for built-in providers (2026-05-28).
                    self.next_step();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.auth_field = AuthField::CustomModel;
                }
                // Tab / Down: deliberately no-op. See comment on Enter.
                _ => {}
            },
        }
        WizardAction::None
    }

    pub(super) fn handle_workspace_key(&mut self, event: KeyEvent) -> WizardAction {
        // Workspace step has exactly 2 focusable fields:
        //   0 = path input
        //   1 = seed templates toggle
        // Tab/BackTab cycle between them; previously the handler let
        // focused_field reach a phantom 3rd "Next button" that wasn't
        // rendered, leaving the user stuck (#52).
        if self.focused_field > 1 {
            self.focused_field = 0;
        }

        match self.focused_field {
            0 => {
                // Editing workspace path
                match event.code {
                    KeyCode::Char(c) => {
                        self.workspace_path.push(c);
                    }
                    KeyCode::Backspace => {
                        self.workspace_path.pop();
                    }
                    KeyCode::Tab => {
                        self.focused_field = 1;
                    }
                    KeyCode::BackTab => {
                        self.focused_field = 1;
                    }
                    KeyCode::Enter => {
                        self.workspace_path = self.workspace_path.trim().to_string();
                        self.next_step();
                        return self.maybe_fetch_models();
                    }
                    _ => {}
                }
            }
            _ => {
                // Seed templates toggle (field 1)
                match event.code {
                    KeyCode::Char(' ') => {
                        self.seed_templates = !self.seed_templates;
                    }
                    KeyCode::Enter => {
                        self.workspace_path = self.workspace_path.trim().to_string();
                        self.next_step();
                        return self.maybe_fetch_models();
                    }
                    KeyCode::Tab | KeyCode::BackTab => {
                        self.focused_field = 0;
                    }
                    _ => {}
                }
            }
        }
        WizardAction::None
    }

    /// If we just entered ProviderAuth with an existing key (or custom with base_url), trigger model fetch
    pub(super) fn maybe_fetch_models(&self) -> WizardAction {
        if self.step != OnboardingStep::ProviderAuth || !self.ps.supports_model_fetch() {
            return WizardAction::None;
        }
        // Custom providers: fetch even without key (local endpoints need none)
        // Built-in providers: require existing key sentinel
        if self.ps.is_custom() || self.ps.has_existing_key_sentinel() {
            WizardAction::FetchModels
        } else {
            WizardAction::None
        }
    }
}
