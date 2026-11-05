//! The onboarding wizard, painted by the window instead of the terminal.
//!
//! The step machine, the validation, and the config writes are the terminal's
//! own (`OnboardingWizard`): a step confirmed in the window writes exactly the
//! section `/onboard` in the terminal writes, through `apply_step_config`, so
//! the two cannot drift. This module only adds what a window needs on top:
//! which field owns the keyboard, and which rows the rail and the lists show.

use opencrabs::tui::onboarding::{
    CHANNEL_NAMES, EXISTING_KEY_SENTINEL, HealthStatus, OnboardingWizard, PROVIDERS, SttProvider,
    TtsProvider,
};
pub use opencrabs::tui::onboarding::{OnboardingStep, WizardMode};

/// The step names `/onboard` opens into, so a slash command can jump straight
/// at the screen the user asked for instead of the first one.
pub fn step_for_command(command: &str) -> Option<OnboardingStep> {
    match command {
        "/onboard" => Some(OnboardingStep::ModeSelect),
        // `/workspace` is the wizard's own workspace step in the terminal
        // (`tui::onboarding::deep_link`), so the window jumps to the same one
        // instead of answering "terminal only" (#2469).
        "/workspace" => Some(OnboardingStep::Workspace),
        "/channels" => Some(OnboardingStep::Channels),
        "/voice" => Some(OnboardingStep::VoiceSetup),
        "/image" => Some(OnboardingStep::ImageSetup),
        "/daemon" => Some(OnboardingStep::Daemon),
        "/brain" => Some(OnboardingStep::BrainSetup),
        "/doctor" => Some(OnboardingStep::HealthCheck),
        _ => None,
    }
}

/// What confirming a step did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Advance {
    /// The step was accepted and the wizard moved on.
    Moved,
    /// The wizard refused: the reason is in `error()`.
    Blocked,
    /// There is no step after this one (a quick-jump step, or Complete).
    Done,
    /// The brain step has prose worth writing, so the wizard is waiting on the
    /// generation thread before it can move. The window starts that thread.
    GenerateBrain,
}

/// Which image switch a click flipped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageToggle {
    Vision,
    Generation,
}

/// A row the keyboard can stand on and flip: the mode radios, the voice
/// engines, the image switches and the daemon switch.
///
/// The terminal picks these with Up/Down on their own step; the window used to
/// leave them reachable only by mouse, so the arrows walked text fields and
/// nothing else (#2432). They are counted in `field_count`, so Tab reaches them
/// and the caret has a row to stand on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChoiceRow {
    Mode(WizardMode),
    Stt(SttProvider),
    Tts(TtsProvider),
    Image(ImageToggle),
    Daemon,
}

/// What happened at the end of the flow. `Failed` carries the reason the
/// terminal would print, so a window that cannot save says why instead of
/// closing as if it had.
#[derive(Debug)]
pub enum Finish {
    Saved { brain_prompt: Option<String> },
    Failed(String),
}

/// Every string the wizard edits from this window. One enum, one accessor
/// (`text_mut`), so a row's label and the field it edits cannot drift.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextTarget {
    WorkspacePath,
    TelegramToken,
    TelegramOwner,
    DiscordToken,
    DiscordChannel,
    DiscordAllowed,
    WhatsappPhone,
    SlackBot,
    SlackApp,
    SlackChannel,
    SlackAllowed,
    TrelloKey,
    TrelloToken,
    TrelloBoard,
    TrelloUsers,
    GroqKey,
    SttBaseUrl,
    SttModel,
    SttKey,
    TtsBaseUrl,
    TtsModel,
    TtsVoiceField,
    TtsKey,
    TtsApiKey,
    SttVoiceboxUrl,
    TtsVoiceboxUrl,
    TtsVoiceboxProfile,
    TtsVoiceboxEngine,
    ImageKey,
    ImageModel,
    BrainMe,
    BrainAgent,
}

pub struct Onboard {
    pub wizard: OnboardingWizard,
    /// Which editable field owns the keyboard on the current step. Reset on
    /// every step change, the same way the terminal resets `focused_field`.
    pub field: usize,
}

impl Onboard {
    pub fn open() -> Self {
        // Bare `/onboard` in the terminal is not the resume path. The TUI
        // resolves the deep link to `ModeSelect` and seeds the wizard from the
        // live config, so the first thing asked is "Pick Your Vibe". Resuming
        // instead landed the window on whatever step a previous run had
        // recorded, which is how it opened straight on ProviderAuth.
        let mut wizard = match opencrabs::config::Config::load() {
            Ok(config) => OnboardingWizard::from_config(&config),
            Err(_) => OnboardingWizard::new(),
        };
        wizard.step = OnboardingStep::ModeSelect;
        wizard.is_first_time = opencrabs::tui::onboarding::is_first_time();
        Self { wizard, field: 0 }
    }

    /// Open the wizard at one step, for a slash command that names it.
    ///
    /// The terminal builds this the same way (`from_config`), so the step shows
    /// what is configured today, and runs the same per-step seeding: the image
    /// step probes the saved key, the doctor step has its checks queued. A
    /// window that skipped it would show a blank form over a working setup.
    pub fn open_at(step: OnboardingStep) -> Self {
        let mut wizard = match opencrabs::config::Config::load() {
            Ok(config) => OnboardingWizard::from_config(&config),
            Err(_) => OnboardingWizard::new(),
        };
        wizard.step = step;
        wizard.quick_jump = true;
        if step == OnboardingStep::ImageSetup {
            wizard.detect_existing_image_key();
        }
        if step == OnboardingStep::HealthCheck {
            // The terminal starts the checks on arrival and resolves them on
            // the next tick, so the step never opens on a list of dead rows.
            // The window has no tick loop, so it resolves them here: the
            // checks are local (a binary lookup and file stats), nothing that
            // can stall the frame.
            wizard.start_health_check();
            wizard.tick_health_check();
        }
        Self { wizard, field: 0 }
    }

    /// Point the provider step at the pair the session is answering with, the
    /// way `/models` does in the terminal (#2060). Returns false when the pair
    /// is not one the provider list can show.
    pub fn seed_session_pair(&mut self, provider: &str, model: &str) -> bool {
        match opencrabs::config::Config::load() {
            Ok(config) => self.wizard.select_session_pair(provider, model, &config),
            Err(_) => false,
        }
    }

    /// `/channels whatsapp` lands on that channel's dialog instead of the menu.
    /// An unknown name leaves the menu in place, exactly like the terminal.
    pub fn open_channel(&mut self, channel: &str) -> bool {
        self.wizard.open_channel_setup(channel)
    }

    pub fn step(&self) -> OnboardingStep {
        self.wizard.step
    }

    pub fn mode(&self) -> WizardMode {
        self.wizard.mode
    }

    pub fn set_mode(&mut self, mode: WizardMode) {
        self.wizard.mode = mode;
    }

    pub fn title(&self) -> &'static str {
        self.wizard.step.title()
    }

    pub fn error(&self) -> Option<&str> {
        self.wizard.error_message.as_deref()
    }

    /// The rail: every step the current flow walks, with the one the user is
    /// standing on marked. Channel sub-steps are not stops and never appear.
    pub fn rail(&self) -> Vec<(usize, &'static str, bool)> {
        OnboardingStep::flow_steps(self.wizard.mode)
            .iter()
            .map(|step| {
                (
                    step.flow_number(self.wizard.mode),
                    step.title(),
                    *step == self.wizard.step,
                )
            })
            .collect()
    }

    /// Position and total for the header: `(4, 9)` in the advanced flow.
    pub fn progress(&self) -> (usize, usize) {
        let total = match self.wizard.mode {
            WizardMode::QuickStart => OnboardingStep::quick_total(),
            WizardMode::Advanced => OnboardingStep::total(),
        };
        (self.wizard.step.flow_number(self.wizard.mode), total)
    }

    /// Every provider the terminal offers, in the terminal's own order.
    /// Saved custom instances have runtime names rather than static metadata.
    pub fn providers(&self) -> Vec<(usize, String)> {
        self.wizard
            .ps
            .provider_display_order()
            .into_iter()
            .filter_map(|index| {
                if index >= opencrabs::ui_core::model_selector::CUSTOM_INSTANCES_START {
                    self.wizard
                        .ps
                        .custom_names
                        .get(index - opencrabs::ui_core::model_selector::CUSTOM_INSTANCES_START)
                        .cloned()
                        .map(|name| (index, name))
                } else {
                    PROVIDERS
                        .get(index)
                        .map(|info| (index, info.name.to_string()))
                }
            })
            .collect()
    }

    /// Select a provider by its shared selector index, and re-probe the key and
    /// the config models the way the terminal does when the row is walked.
    pub fn select_provider(&mut self, index: usize) {
        if !self.wizard.ps.provider_display_order().contains(&index) {
            return;
        }
        self.wizard.ps.selected_provider = index;
        self.wizard.ps.detect_existing_key();
        self.wizard.ps.reload_config_models();
        if self.wizard.ps.is_custom() {
            self.wizard.ps.load_custom_fields();
        }
        self.field = 0;
    }

    /// Select a row from the shared all-provider picker without saving yet.
    /// Unknown model ids are retained as real wizard selections.
    pub fn select_pair(&mut self, provider: &str, model: &str) -> bool {
        if model.trim().is_empty() {
            return false;
        }
        let current = if self.wizard.ps.is_custom() {
            self.wizard.ps.custom_name.as_str()
        } else {
            self.provider_id()
        };
        if current == provider {
            if !self.wizard.ps.all_model_names().contains(&model) {
                self.wizard.ps.config_models.push(model.to_string());
            }
            self.wizard.ps.custom_model = model.to_string();
            self.wizard.ps.resolve_selected_model_index();
            self.wizard.ps.model_filter.clear();
            self.field = self.search_field();
            return true;
        }
        let Ok(config) = opencrabs::config::Config::load() else {
            return false;
        };
        let custom = config
            .providers
            .custom
            .as_ref()
            .is_some_and(|providers| providers.contains_key(provider));
        let provider = if custom {
            format!("custom:{provider}")
        } else {
            provider.to_string()
        };
        if !self.wizard.select_session_pair(&provider, model, &config) {
            return false;
        }
        self.wizard.ps.model_filter.clear();
        self.field = self.search_field();
        true
    }

    /// True when this provider takes a pasted key. OpenCrabs mints its own and
    /// the CLI providers sign in elsewhere, so neither shows a key field.
    /// The provider id the step is pointed at, so a test and the screen agree
    /// about which one is chosen without reading the wizard's internals.
    pub fn provider_id(&self) -> &'static str {
        self.wizard.ps.current_provider().id
    }

    pub fn key_needed(&self) -> bool {
        let ps = &self.wizard.ps;
        !ps.is_keyless() && !ps.is_cli() && !ps.is_oauth()
    }

    pub fn key_label(&self) -> &'static str {
        self.wizard.ps.current_provider().key_label
    }

    pub fn help_lines(&self) -> &'static [&'static str] {
        self.wizard.ps.current_provider().help_lines
    }

    pub fn model(&self) -> String {
        self.wizard.ps.selected_model_name().to_string()
    }

    /// The slice of the provider list the window paints, with the counts it
    /// has to hide above and below. The terminal windows this list the same
    /// way (`visible_window`), so a short window keeps the fields under it
    /// reachable instead of pushing them off screen, and the arrows can still
    /// walk every provider.
    pub fn provider_rows(&self, list_rows: usize) -> (Vec<(usize, String)>, usize, usize) {
        let all = self.providers();
        let total = all.len();
        let selected = all
            .iter()
            .position(|(index, _)| *index == self.wizard.ps.selected_provider)
            .unwrap_or(0);
        let (start, end) =
            opencrabs::tui::onboarding_render::visible_window(total, selected, list_rows);
        (all[start..end].to_vec(), start, total.saturating_sub(end))
    }

    /// How many rows the keyboard can stand on for this step. On the provider
    /// step field 0 is the provider list and the rest are text rows; on every
    /// other step it is one row per choice switch, then one per text field, in
    /// paint order.
    pub fn field_count(&self) -> usize {
        if self.wizard.step == OnboardingStep::ProviderAuth {
            return 1 + self.text_field_count();
        }
        self.channel_row_count() + self.choice_row_count() + self.slots().len()
    }

    /// The channel toggles sit in front of the text fields on a channel step,
    /// so one Tab walk reaches both. Other steps have no rows of their own.
    pub fn channel_row_count(&self) -> usize {
        if self.is_channel_step() {
            CHANNEL_NAMES.len()
        } else {
            0
        }
    }

    /// The choices this step paints as rows of their own, in paint order. The
    /// window used to count only text fields here, which is why the arrows and
    /// Tab skipped every switch on the mode, voice, image and daemon steps
    /// (#2432).
    pub fn choice_rows(&self) -> Vec<ChoiceRow> {
        let w = &self.wizard;
        match w.step {
            OnboardingStep::ModeSelect => {
                vec![
                    ChoiceRow::Mode(WizardMode::QuickStart),
                    ChoiceRow::Mode(WizardMode::Advanced),
                ]
            }
            OnboardingStep::VoiceSetup => {
                let mut rows = Vec::new();
                for (provider, _, _) in self.stt_options() {
                    rows.push(ChoiceRow::Stt(provider));
                }
                for (provider, _, _) in self.tts_options() {
                    rows.push(ChoiceRow::Tts(provider));
                }
                rows
            }
            OnboardingStep::ImageSetup => vec![
                ChoiceRow::Image(ImageToggle::Vision),
                ChoiceRow::Image(ImageToggle::Generation),
            ],
            OnboardingStep::Daemon => vec![ChoiceRow::Daemon],
            _ => Vec::new(),
        }
    }

    pub fn choice_row_count(&self) -> usize {
        self.choice_rows().len()
    }

    /// The choice under the keyboard, when it stands on one.
    pub fn focused_choice(&self) -> Option<ChoiceRow> {
        let index = self.field.checked_sub(self.channel_row_count())?;
        self.choice_rows().get(index).copied()
    }

    /// Flip the choice under the keyboard. False when the keyboard is on a text
    /// field, so Space knows it is an ordinary character there.
    pub fn activate_focused_choice(&mut self) -> bool {
        let Some(choice) = self.focused_choice() else {
            return false;
        };
        self.apply_choice(choice);
        true
    }

    /// Channels first, then the choices: the one key the window uses for "turn
    /// this row on" on every step that has rows.
    pub fn activate_focused_row(&mut self) -> bool {
        self.activate_focused_choice() || self.toggle_focused_channel()
    }

    /// Apply one choice. Single picks are set, switches are flipped.
    fn apply_choice(&mut self, choice: ChoiceRow) {
        match choice {
            ChoiceRow::Mode(mode) => self.set_mode(mode),
            ChoiceRow::Stt(provider) => self.set_stt_provider(provider),
            ChoiceRow::Tts(provider) => self.set_tts_provider(provider),
            ChoiceRow::Image(which) => self.toggle_image(which),
            ChoiceRow::Daemon => self.toggle_daemon(),
        }
    }

    /// The channel the keyboard stands on, when it stands on one. Space flips
    /// it, which is the terminal's own key for this list.
    pub fn focused_channel(&self) -> Option<usize> {
        (self.field < self.channel_row_count()).then_some(self.field)
    }

    /// Flip the channel under the keyboard. False when the keyboard is on a
    /// text field, so the caller knows the key was not a toggle.
    pub fn toggle_focused_channel(&mut self) -> bool {
        match self.focused_channel() {
            Some(index) => {
                self.toggle_channel(index);
                true
            }
            None => false,
        }
    }

    fn text_field_count(&self) -> usize {
        let mut count = 1; // model search
        if self.key_needed() {
            count += 1;
        }
        if self.wizard.ps.is_custom() {
            count += 3;
        }
        count
    }

    /// The channel rows the Channels step paints: name, description, and
    /// whether it is switched on. The terminal's own list, so the two front
    /// ends never disagree about which channels exist.
    pub fn channels(&self) -> Vec<(&'static str, &'static str, bool)> {
        CHANNEL_NAMES
            .iter()
            .enumerate()
            .map(|(index, (name, description))| {
                let on = self
                    .wizard
                    .channel_toggles
                    .get(index)
                    .is_some_and(|(_, enabled)| *enabled);
                (*name, *description, on)
            })
            .collect()
    }

    /// Flip one channel on or off, by its index in the terminal's list.
    pub fn toggle_channel(&mut self, index: usize) {
        if let Some((_, enabled)) = self.wizard.channel_toggles.get_mut(index) {
            *enabled = !*enabled;
        }
        // The field list changes with the toggles, so the focus cannot be left
        // pointing past the end of it.
        let count = self.field_count();
        if count > 0 && self.field >= count {
            self.field = count - 1;
        }
    }

    /// The channel sub-steps are not stops in a window: their fields are drawn
    /// under the Channels list, so confirming one loops straight to the next.
    pub fn is_channel_step(&self) -> bool {
        matches!(
            self.wizard.step,
            OnboardingStep::Channels
                | OnboardingStep::TelegramSetup
                | OnboardingStep::DiscordSetup
                | OnboardingStep::WhatsAppSetup
                | OnboardingStep::SlackSetup
                | OnboardingStep::TrelloSetup
        )
    }

    /// The setup steps the terminal prints above each channel's fields: where
    /// to create the bot and which scopes to tick. They are the same lines, so
    /// a user in the window is not sent to a different page than the terminal.
    pub fn channel_help(&self) -> &'static [&'static str] {
        match self.wizard.step {
            OnboardingStep::TelegramSetup | OnboardingStep::Channels => &[
                "open Telegram, search @BotFather",
                "send /newbot, follow the prompts",
                "copy the bot token and paste it below",
            ],
            OnboardingStep::DiscordSetup => &[
                "go to discord.com/developers/applications",
                "create app > Bot > copy the token",
                "enable Message Content Intent",
            ],
            OnboardingStep::WhatsAppSetup => &[
                "scan the QR with WhatsApp on the phone that will answer",
                "the phone number is the one that sends the messages",
            ],
            OnboardingStep::SlackSetup => &[
                "go to api.slack.com/apps > Create App",
                "OAuth > bot scopes: chat:write, channels:history, users:read, files:read",
                "enable Socket Mode, copy the App Token (xapp-...)",
                "install to workspace, copy the Bot Token (xoxb-...)",
            ],
            OnboardingStep::TrelloSetup => &[
                "go to trello.com/power-ups/admin > Create Power-Up",
                "API Key tab > copy your key",
                "Token link > authorize > copy the token",
            ],
            _ => &[],
        }
    }

    /// The text rows of the current step: `(label, target)` in paint order.
    /// The provider step keeps its own layout (two lists plus fields), so it
    /// reports its rows through `active_text_mut` instead.
    pub fn slots(&self) -> Vec<(&'static str, TextTarget)> {
        use TextTarget::*;
        let w = &self.wizard;
        match w.step {
            OnboardingStep::Workspace => vec![("workspace", WorkspacePath)],
            OnboardingStep::Channels
            | OnboardingStep::TelegramSetup
            | OnboardingStep::DiscordSetup
            | OnboardingStep::WhatsAppSetup
            | OnboardingStep::SlackSetup
            | OnboardingStep::TrelloSetup => {
                let mut rows = Vec::new();
                if w.is_telegram_enabled() {
                    rows.push(("bot token", TelegramToken));
                    rows.push(("owner user id", TelegramOwner));
                }
                if w.is_discord_enabled() {
                    rows.push(("bot token", DiscordToken));
                    rows.push(("channel id", DiscordChannel));
                    rows.push(("allowed user ids", DiscordAllowed));
                }
                if w.is_whatsapp_enabled() {
                    rows.push(("phone number", WhatsappPhone));
                }
                if w.is_slack_enabled() {
                    rows.push(("bot token", SlackBot));
                    rows.push(("app token", SlackApp));
                    rows.push(("channel id", SlackChannel));
                    rows.push(("allowed user ids", SlackAllowed));
                }
                if w.is_trello_enabled() {
                    rows.push(("api key", TrelloKey));
                    rows.push(("token", TrelloToken));
                    rows.push(("board id", TrelloBoard));
                    rows.push(("allowed users", TrelloUsers));
                }
                rows
            }
            OnboardingStep::VoiceSetup => self.voice_slots(),
            OnboardingStep::ImageSetup => self.image_slots(),
            OnboardingStep::BrainSetup => {
                vec![("about me", BrainMe), ("about opencrabs", BrainAgent)]
            }
            _ => Vec::new(),
        }
    }

    /// The rows the picked voice engines actually ask for. The terminal paints
    /// only the fields of the provider that is on, so a Groq key never shows up
    /// while Whisper-on-device is selected; a fixed list of eight would ask for
    /// keys nothing is going to read.
    pub fn voice_slots(&self) -> Vec<(&'static str, TextTarget)> {
        use TextTarget::*;
        let w = &self.wizard;
        let mut rows = Vec::new();
        match w.stt_provider {
            SttProvider::Off | SttProvider::Local => {}
            SttProvider::Groq => rows.push(("groq api key", GroqKey)),
            SttProvider::OpenAiCompatible => {
                rows.push(("base url", SttBaseUrl));
                rows.push(("model", SttModel));
                rows.push(("api key (optional)", SttKey));
            }
            SttProvider::Voicebox => rows.push(("base url", SttVoiceboxUrl)),
        }
        match w.tts_provider {
            TtsProvider::Off | TtsProvider::Local => {}
            TtsProvider::OpenAi => rows.push(("api key (optional)", TtsApiKey)),
            TtsProvider::OpenAiCompatible => {
                rows.push(("tts base url", TtsBaseUrl));
                rows.push(("tts model", TtsModel));
                rows.push(("tts voice", TtsVoiceField));
                rows.push(("tts api key (optional)", TtsKey));
            }
            TtsProvider::Voicebox => {
                rows.push(("tts base url", TtsVoiceboxUrl));
                rows.push(("voicebox profile id", TtsVoiceboxProfile));
                rows.push(("voicebox engine", TtsVoiceboxEngine));
            }
        }
        rows
    }

    /// The rows the image step shows, in the terminal's own order: the generation
    /// model has nothing to say while generation is off, and the key is only
    /// asked for once at least one of the two switches is on.
    pub fn image_slots(&self) -> Vec<(&'static str, TextTarget)> {
        use TextTarget::*;
        let w = &self.wizard;
        let mut rows = Vec::new();
        if w.image_generation_enabled {
            rows.push(("generation model", ImageModel));
        }
        if w.image_vision_enabled || w.image_generation_enabled {
            rows.push(("api key", ImageKey));
        }
        rows
    }

    /// The generation model the step falls back to when the row is left empty,
    /// so the window can name it instead of showing a blank row. It comes from
    /// the config's own default, never a copy, so the two cannot drift.
    pub fn default_image_model() -> String {
        opencrabs::config::default_image_model()
    }

    /// What the picked engines do when they need no field of their own: the
    /// on-device models run with nothing to type, and saying so beats an empty
    /// stretch of screen that looks broken.
    pub fn voice_notes(&self) -> Vec<&'static str> {
        let w = &self.wizard;
        let mut notes = Vec::new();
        if w.stt_provider == SttProvider::Local {
            notes.push("whisper runs on this machine, no key and no endpoint to type");
        }
        if w.tts_provider == TtsProvider::Local {
            notes.push("piper speaks on this machine, free and offline");
        }
        if w.tts_provider == TtsProvider::OpenAi {
            notes.push("openai voices: the key falls back to your openai chat key");
        }
        notes
    }

    /// The OpenAI voice names, with the one currently picked marked. The
    /// terminal walks these with the arrows; the window paints them as rows.
    pub fn tts_voice_options(&self) -> Vec<(&'static str, bool)> {
        opencrabs::tui::onboarding::voice::OPENAI_TTS_VOICES
            .iter()
            .map(|voice| (*voice, *voice == self.wizard.tts_api_voice))
            .collect()
    }

    pub fn set_tts_voice(&mut self, voice: &str) {
        self.wizard.tts_api_voice = voice.to_string();
    }

    /// The string behind a text row. One place, so the paint and the keyboard
    /// can never edit different fields.
    pub fn text_mut(&mut self, target: TextTarget) -> &mut String {
        use TextTarget::*;
        let w = &mut self.wizard;
        match target {
            WorkspacePath => &mut w.workspace_path,
            TelegramToken => &mut w.telegram_token_input,
            TelegramOwner => &mut w.telegram_user_id_input,
            DiscordToken => &mut w.discord_token_input,
            DiscordChannel => &mut w.discord_channel_id_input,
            DiscordAllowed => &mut w.discord_allowed_list_input,
            WhatsappPhone => &mut w.whatsapp_phone_input,
            SlackBot => &mut w.slack_bot_token_input,
            SlackApp => &mut w.slack_app_token_input,
            SlackChannel => &mut w.slack_channel_id_input,
            SlackAllowed => &mut w.slack_allowed_list_input,
            TrelloKey => &mut w.trello_api_key_input,
            TrelloToken => &mut w.trello_api_token_input,
            TrelloBoard => &mut w.trello_board_id_input,
            TrelloUsers => &mut w.trello_allowed_users_input,
            GroqKey => &mut w.groq_api_key_input,
            SttBaseUrl => &mut w.stt_openai_compat_base_url,
            SttModel => &mut w.stt_openai_compat_model,
            SttKey => &mut w.stt_openai_compat_key_input,
            TtsBaseUrl => &mut w.tts_openai_compat_base_url,
            TtsModel => &mut w.tts_openai_compat_model,
            TtsVoiceField => &mut w.tts_openai_compat_voice,
            TtsKey => &mut w.tts_openai_compat_key_input,
            TtsApiKey => &mut w.tts_api_key_input,
            SttVoiceboxUrl => &mut w.stt_voicebox_base_url,
            TtsVoiceboxUrl => &mut w.tts_voicebox_base_url,
            TtsVoiceboxProfile => &mut w.tts_voicebox_profile_id,
            TtsVoiceboxEngine => &mut w.tts_voicebox_engine,
            ImageKey => &mut w.image_api_key_input,
            ImageModel => &mut w.image_generation_model_input,
            BrainMe => &mut w.about_me,
            BrainAgent => &mut w.about_opencrabs,
        }
    }

    /// The text row the keyboard is on, when it is on one.
    pub fn focused_text(&self) -> Option<TextTarget> {
        if self.wizard.step == OnboardingStep::ProviderAuth {
            return None; // the provider step answers through `active_text_mut`
        }
        let base = self.channel_row_count() + self.choice_row_count();
        self.field
            .checked_sub(base)
            .and_then(|index| self.slots().get(index).map(|(_, target)| *target))
    }

    /// The search row sits first behind the provider list: row 0 is the list
    /// itself, so the arrows walk the list while the search stays reachable by
    /// Tab and by click.
    pub fn search_field(&self) -> usize {
        1
    }

    /// Where the custom-provider fields start. The row order is the terminal's
    /// own: list, search, key (when the provider needs one), then name, base
    /// url and model.
    fn custom_field_base(&self) -> usize {
        2 + usize::from(self.key_needed())
    }

    /// The key row, when this provider is one that takes a key.
    pub fn key_field(&self) -> Option<usize> {
        self.key_needed().then_some(2)
    }

    /// The custom endpoint row. Confirming it is another trigger for the live
    /// fetch, just as it is in the terminal's provider selector.
    pub fn custom_base_url_field(&self) -> Option<usize> {
        self.wizard
            .ps
            .is_custom()
            .then(|| self.custom_field_base() + 1)
    }

    /// The string the keyboard edits right now, if the focused row is a text
    /// field. The provider list and every list row return None, and the arrows
    /// move those instead.
    pub fn active_text_mut(&mut self) -> Option<&mut String> {
        if self.wizard.step != OnboardingStep::ProviderAuth {
            return self.focused_text().map(|target| self.text_mut(target));
        }
        if self.field == 0 {
            return None;
        }
        let custom_base = self.custom_field_base();
        let custom = self.wizard.ps.is_custom();
        let key_field = self.key_field();
        let ps = &mut self.wizard.ps;
        match self.field {
            f if f == custom_base && custom => Some(&mut ps.custom_name),
            f if f == custom_base + 1 && custom => Some(&mut ps.base_url),
            f if f == custom_base + 2 && custom => Some(&mut ps.custom_model),
            f if Some(f) == key_field => Some(&mut ps.api_key_input),
            _ => Some(&mut ps.model_filter),
        }
    }

    /// The end of the flow: the one write that covers the whole config, the
    /// workspace templates and the optional daemon install. Every step already
    /// saved its own section, so this is the completion pass the terminal runs
    /// from its dialog.
    ///
    /// A filled-in Brain step comes back as `brain_prompt`: the personalized
    /// brain files are generated after the save, through the provider the save
    /// has just selected, which is the order the terminal uses too.
    pub fn finish(&mut self) -> Finish {
        // A brain the wizard already wrote on its own step is not written a
        // second time here. When generation failed, the prompt survives, so
        // finishing retries the writer instead of leaving the prose unused.
        let brain_prompt = if self.wizard.brain_generated {
            None
        } else {
            self.brain_prompt()
        };
        match self.wizard.apply_config() {
            Ok(()) => {
                // A quick jump saves one section; it is not a pass through the
                // wizard, so it must not tell the gate the setup is done (#919).
                if !self.wizard.quick_jump {
                    opencrabs::tui::onboarding::state::OnboardingState::mark_completed();
                }
                Finish::Saved { brain_prompt }
            }
            Err(e) => Finish::Failed(e),
        }
    }

    /// The workspace the generated brain files belong in.
    pub fn workspace(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.wizard.workspace_path)
    }

    /// The transcription choices the terminal offers, with its labels and the
    /// one currently selected. Local is offered only when this build carries it.
    pub fn stt_options(&self) -> Vec<(SttProvider, &'static str, bool)> {
        SttProvider::available(cfg!(feature = "local-stt"))
            .iter()
            .map(|provider| {
                (
                    *provider,
                    provider.label(),
                    *provider == self.wizard.stt_provider,
                )
            })
            .collect()
    }

    pub fn set_stt_provider(&mut self, provider: SttProvider) {
        self.wizard.stt_provider = provider;
        self.clamp_field();
    }

    /// Switching an engine swaps the rows under the keyboard, so a field index
    /// that pointed past the new end would leave the caret nowhere.
    fn clamp_field(&mut self) {
        let end = self.field_count();
        if self.field >= end {
            self.field = end.saturating_sub(1);
        }
    }

    /// The speech choices, same shape as the transcription ones.
    pub fn tts_options(&self) -> Vec<(TtsProvider, &'static str, bool)> {
        TtsProvider::available(cfg!(feature = "local-tts"))
            .iter()
            .map(|provider| {
                (
                    *provider,
                    provider.label(),
                    *provider == self.wizard.tts_provider,
                )
            })
            .collect()
    }

    pub fn set_tts_provider(&mut self, provider: TtsProvider) {
        self.wizard.tts_provider = provider;
        self.clamp_field();
    }

    /// The two image switches on the image step.
    pub fn image_vision(&self) -> bool {
        self.wizard.image_vision_enabled
    }

    pub fn image_generation(&self) -> bool {
        self.wizard.image_generation_enabled
    }

    pub fn toggle_image(&mut self, which: ImageToggle) {
        match which {
            ImageToggle::Vision => {
                self.wizard.image_vision_enabled = !self.wizard.image_vision_enabled
            }
            ImageToggle::Generation => {
                self.wizard.image_generation_enabled = !self.wizard.image_generation_enabled
            }
        }
        self.clamp_field();
    }

    /// Put the row that edits this string under the keyboard. Clicking a field
    /// and typing into it must send the keystrokes to the same string the row
    /// paints; this is the only thing that decides which.
    pub fn focus_text(&mut self, target: TextTarget) {
        let base = self.channel_row_count() + self.choice_row_count();
        if let Some(index) = self.slots().iter().position(|(_, slot)| *slot == target) {
            self.field = base + index;
        }
    }

    /// The health checks, in the terminal's own order and with its own labels.
    /// Empty until the check has been started.
    pub fn health(&self) -> &[(String, HealthStatus)] {
        &self.wizard.health_results
    }

    /// Run the checks now. The terminal spreads them over ticks so the Pending
    /// row is visible for one frame; the window has no such loop, so it starts
    /// and resolves them together and paints the outcome, which is the part the
    /// user came for.
    pub fn run_health(&mut self) {
        self.wizard.start_health_check();
        self.wizard.tick_health_check();
    }

    pub fn health_complete(&self) -> bool {
        self.wizard.health_complete
    }

    /// Every check passed and the run is over, so the step can say so in words
    /// instead of leaving the user to read six marks. The terminal's own
    /// verdict, from the same predicate.
    pub fn all_health_passed(&self) -> bool {
        self.wizard.all_health_passed()
    }

    /// What the daemon step offers to install, named the way the platform
    /// names it: the terminal says "LaunchAgent" here, not "background
    /// service".
    pub fn daemon_platform(&self) -> &'static str {
        if cfg!(target_os = "linux") {
            "systemd user unit"
        } else if cfg!(target_os = "macos") {
            "LaunchAgent"
        } else {
            "background service"
        }
    }

    /// Whether Daemon will install the service when the flow is saved. Off
    /// until the user says so: a system service is never a side effect of
    /// clicking continue.
    pub fn install_daemon(&self) -> bool {
        self.wizard.install_daemon
    }

    pub fn toggle_daemon(&mut self) {
        self.wizard.install_daemon = !self.wizard.install_daemon;
    }

    /// Confirm the step. The wizard validates, writes this step's own config
    /// section, and moves on; a rejected step stays put and carries the reason.
    pub fn advance(&mut self) -> Advance {
        // The brain step is the one step that does work before it can move.
        // The terminal decides there between skipping (nothing typed, or text
        // unchanged from the files on disk) and handing the prose to the
        // writer. The window asks the same question, and reports the third
        // answer so the caller can start the thread.
        if self.wizard.step == OnboardingStep::BrainSetup {
            let nothing = self.wizard.about_me.trim().is_empty()
                && self.wizard.about_opencrabs.trim().is_empty();
            let unchanged =
                !self.wizard.brain_inputs_changed() && !self.wizard.original_about_me.is_empty();
            if nothing || unchanged {
                self.wizard.step = OnboardingStep::Complete;
                return Advance::Done;
            }
            if !self.wizard.brain_generated
                && self.wizard.brain_error.is_none()
                && !self.wizard.brain_generating
            {
                return Advance::GenerateBrain;
            }
        }
        let before = self.wizard.step;
        self.wizard.next_step();
        if self.wizard.error_message.is_some() {
            return Advance::Blocked;
        }
        if self.wizard.quick_jump && self.wizard.quick_jump_done {
            return Advance::Done;
        }
        if self.wizard.step == OnboardingStep::Complete {
            return Advance::Done;
        }
        if self.wizard.step == before {
            return Advance::Blocked;
        }
        self.field = 0;
        Advance::Moved
    }

    /// The prose to hand the brain writer, normalized the way the terminal
    /// normalizes it. `None` when there is nothing to say.
    pub fn brain_prompt(&mut self) -> Option<String> {
        if self.wizard.about_me.trim().is_empty() && self.wizard.about_opencrabs.trim().is_empty() {
            return None;
        }
        self.wizard.normalize_brain_inputs();
        Some(self.wizard.build_brain_prompt())
    }

    /// The generation thread is running: the step waits instead of asking
    /// again, so a second Enter cannot start a second writer.
    pub fn mark_brain_generating(&mut self) {
        self.wizard.brain_generating = true;
    }

    /// The files are written. The step may now finish.
    pub fn mark_brain_generated(&mut self) {
        self.wizard.brain_generating = false;
        self.wizard.brain_generated = true;
        self.wizard.brain_error = None;
    }

    /// The writer failed. The reason is kept so the step says why, and the
    /// step is still allowed to finish: a saved setup without a personalized
    /// brain is better than a wizard nobody can get out of.
    pub fn mark_brain_failed(&mut self, reason: String) {
        self.wizard.brain_generating = false;
        self.wizard.brain_error = Some(reason);
    }

    pub fn brain_generating(&self) -> bool {
        self.wizard.brain_generating
    }

    pub fn brain_generated(&self) -> bool {
        self.wizard.brain_generated
    }

    pub fn brain_error(&self) -> Option<&str> {
        self.wizard.brain_error.as_deref()
    }

    /// Back one step. False means the wizard is on its first step and the
    /// caller closes it: the terminal's `prev_step` returns true to signal
    /// cancel (ModeSelect only), which becomes this door's close signal.
    pub fn back(&mut self) -> bool {
        let cancelled = self.wizard.prev_step();
        if cancelled {
            return false;
        }
        self.field = 0;
        true
    }

    /// Esc: clear an active model search, otherwise step back. False means
    /// there was nowhere to go and the caller closes the screen. The terminal's
    /// order exactly: a search is cleared before the step is abandoned.
    pub fn escape(&mut self) -> bool {
        if self.wizard.step == OnboardingStep::ProviderAuth
            && !self.wizard.ps.model_filter.is_empty()
        {
            self.wizard.ps.model_filter.clear();
            self.wizard.ps.selected_model = 0;
            return true;
        }
        self.back()
    }

    /// An arrow key: it walks whichever list the focus is on, and moves the
    /// focus when it is on a text field.
    pub fn walk(&mut self, delta: isize) {
        if self.wizard.step == OnboardingStep::ProviderAuth && self.field == 0 {
            self.provider_step(delta);
            return;
        }
        // On a step with switches the arrows travel them, and the rows that
        // are a single pick apply what the arrows land on. That is the
        // terminal's own Up/Down on the mode and voice steps, and it is why the
        // window's arrows used to look dead here: the rows were not counted at
        // all (#2432). Tab still walks past them into the text rows.
        let base = self.channel_row_count();
        let choices = self.choice_row_count();
        if choices > 0 && self.field >= base && self.field < base + choices {
            let next =
                base + ((self.field - base) as isize + delta).rem_euclid(choices as isize) as usize;
            self.field = next;
            match self.choice_rows()[next - base] {
                ChoiceRow::Mode(mode) => self.set_mode(mode),
                ChoiceRow::Stt(provider) => self.set_stt_provider(provider),
                ChoiceRow::Tts(provider) => self.set_tts_provider(provider),
                // A switch flips on Space or Enter, never by passing over it:
                // an arrow that toggles on its way makes the list unusable.
                ChoiceRow::Image(_) | ChoiceRow::Daemon => {}
            }
            return;
        }
        if delta < 0 {
            self.focus_prev();
        } else {
            self.focus_next();
        }
    }

    /// Walk the provider list by one row, wrapping at both ends. Selecting
    /// re-probes the key and the config models, the same work the terminal
    /// does when its arrow moves.
    pub fn provider_step(&mut self, delta: isize) {
        let order = self.wizard.ps.provider_display_order();
        if order.is_empty() {
            return;
        }
        let current = order
            .iter()
            .position(|index| *index == self.wizard.ps.selected_provider)
            .unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(order.len() as isize) as usize;
        self.select_provider(order[next]);
        self.field = 0;
    }

    /// Tab: the next row the keyboard can stand on, wrapping.
    pub fn focus_next(&mut self) {
        let count = self.field_count().max(1);
        self.field = (self.field + 1) % count;
    }

    pub fn focus_prev(&mut self) {
        let count = self.field_count().max(1);
        self.field = (self.field + count - 1) % count;
    }

    /// A printable character, into the focused text field. Returns false when
    /// the keyboard stands on a list row, so the caller can decide. A key that
    /// is already saved shows as a sentinel, and the first character typed
    /// replaces it rather than appending to it (the terminal's own rule).
    pub fn type_char(&mut self, ch: char) -> bool {
        let replacing = self.key_field() == Some(self.field)
            && self.wizard.ps.api_key_input == EXISTING_KEY_SENTINEL;
        if let Some(field) = self.active_text_mut() {
            if replacing {
                field.clear();
            }
            field.push(ch);
            return true;
        }
        false
    }

    pub fn backspace(&mut self) -> bool {
        if let Some(field) = self.active_text_mut() {
            if field == EXISTING_KEY_SENTINEL {
                return true;
            }
            field.pop();
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::config::profile::with_home_override;

    /// Every test runs against a throwaway home: the wizard reads config on
    /// construction, and a test must never see or touch the live one.
    fn home() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp home")
    }

    #[test]
    fn a_first_run_lands_on_the_built_in_provider() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open();
            // A first run walks Workspace into the provider step, which is
            // where the terminal preselects the free built-in provider (#2047).
            onboard.wizard.step = OnboardingStep::Workspace;
            onboard.advance();
            assert_eq!(
                onboard.step(),
                OnboardingStep::ProviderAuth,
                "the workspace step leads to the provider step"
            );
            assert_eq!(
                onboard.provider_id(),
                "opencrabs",
                "a fresh install starts on the free built-in provider"
            );
        });
    }

    #[test]
    fn built_in_provider_asks_for_no_key() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open();
            onboard.wizard.step = OnboardingStep::ProviderAuth;
            let built_in = PROVIDERS
                .iter()
                .position(|p| p.id == "opencrabs")
                .expect("built-in provider is defined");
            onboard.select_provider(built_in);
            assert!(!onboard.key_needed(), "OpenCrabs mints its own token");
            assert_eq!(onboard.field_count(), 2, "the list plus the model search");
        });
    }

    #[test]
    fn an_api_provider_adds_the_key_field() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open();
            onboard.wizard.step = OnboardingStep::ProviderAuth;
            let anthropic = PROVIDERS
                .iter()
                .position(|p| p.id == "anthropic")
                .expect("anthropic is defined");
            onboard.select_provider(anthropic);
            assert!(onboard.key_needed());
            assert_eq!(onboard.field_count(), 3, "list, search, key");
        });
    }

    #[test]
    fn every_slash_command_named_screen_opens_at_its_own_step() {
        assert_eq!(
            step_for_command("/channels"),
            Some(OnboardingStep::Channels)
        );
        assert_eq!(step_for_command("/voice"), Some(OnboardingStep::VoiceSetup));
        assert_eq!(step_for_command("/image"), Some(OnboardingStep::ImageSetup));
        assert_eq!(step_for_command("/daemon"), Some(OnboardingStep::Daemon));
        assert_eq!(step_for_command("/brain"), Some(OnboardingStep::BrainSetup));
        assert_eq!(
            step_for_command("/doctor"),
            Some(OnboardingStep::HealthCheck)
        );
        // #2469: `/workspace` was answered "terminal only" while the wizard it
        // jumps to was already in the window. The terminal maps it to the same
        // step (`tui::onboarding::deep_link`), so the two cannot disagree.
        assert_eq!(
            step_for_command("/workspace"),
            Some(OnboardingStep::Workspace)
        );
        assert_eq!(step_for_command("/nope"), None);
    }

    /// #2469: every name `step_for_command` answers must also be a command the
    /// window claims to run, or the palette badges a screen it refuses.
    #[test]
    fn every_workspace_command_is_whitelisted_for_the_window() {
        for name in [
            "/onboard",
            "/workspace",
            "/channels",
            "/voice",
            "/image",
            "/daemon",
            "/brain",
            "/doctor",
        ] {
            assert!(
                step_for_command(name).is_some(),
                "{name} has no wizard step"
            );
            assert!(
                opencrabs::ui_core::commands::GUI_SUPPORTED.contains(&name),
                "{name} opens a step but is missing from GUI_SUPPORTED"
            );
        }
    }

    #[test]
    fn the_channels_door_lands_on_the_named_channel() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::Channels);
            assert!(onboard.open_channel("whatsapp"));
            assert_eq!(onboard.step(), OnboardingStep::WhatsAppSetup);
            assert!(onboard.is_channel_step(), "sub-steps wear the channel UI");
            // The unknown name leaves the menu up, the terminal's own choice.
            assert!(!onboard.open_channel("myspace"));
            assert_eq!(onboard.step(), OnboardingStep::WhatsAppSetup);
        });
    }

    #[test]
    fn a_channel_toggle_decides_which_fields_appear() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::Channels);
            // A fresh home has nothing enabled, so there is nothing to fill in.
            let before = onboard.slots();
            assert!(before.is_empty(), "no channel means no fields: {before:?}");
            let telegram = onboard
                .channels()
                .iter()
                .position(|(name, _, _)| *name == "Telegram")
                .expect("Telegram is in the terminal's channel list");
            onboard.toggle_channel(telegram);
            let after = onboard.slots();
            let labels: Vec<_> = after.iter().map(|(label, _)| *label).collect();
            assert_eq!(labels, vec!["bot token", "owner user id"]);
            // Every row is reachable by the keyboard: the toggles first, then
            // the fields, so a row cannot paint and swallow the key.
            assert_eq!(onboard.field_count(), 5 + after.len());
            // The keyboard starts on the first channel, not in a text field.
            assert_eq!(onboard.focused_channel(), Some(0));
            assert_eq!(onboard.focused_text(), None);
            // Tab walking reaches the fields behind the toggles.
            for _ in 0..5 {
                onboard.focus_next();
            }
            assert_eq!(onboard.focused_channel(), None);
            assert_eq!(
                onboard.focused_text(),
                Some(TextTarget::TelegramToken),
                "after the toggles the walk enters the channel's own fields"
            );
        });
    }

    #[test]
    fn space_flips_the_channel_under_the_keyboard() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::TelegramSetup);
            let before = onboard.channels()[0].2;
            assert!(onboard.toggle_focused_channel(), "the keyboard is on a row");
            assert_ne!(onboard.channels()[0].2, before);
            // Moving onto the fields turns Space back into an ordinary key.
            onboard.field = onboard.channel_row_count();
            assert!(!onboard.toggle_focused_channel());
        });
    }

    #[test]
    fn every_channel_step_carries_its_setup_instructions() {
        for step in [
            OnboardingStep::Channels,
            OnboardingStep::TelegramSetup,
            OnboardingStep::DiscordSetup,
            OnboardingStep::WhatsAppSetup,
            OnboardingStep::SlackSetup,
            OnboardingStep::TrelloSetup,
        ] {
            let dir = home();
            with_home_override(dir.path().to_path_buf(), || {
                let onboard = Onboard::open_at(step);
                assert!(
                    !onboard.channel_help().is_empty(),
                    "{step:?} must tell the user where to create the bot"
                );
            });
        }
    }

    #[test]
    fn the_voice_step_asks_only_what_the_picked_engine_needs() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::VoiceSetup);
            // Both engines off is the terminal's default, so there is nothing
            // to type and the step must not paint eight dead rows.
            onboard.set_stt_provider(SttProvider::Off);
            onboard.set_tts_provider(TtsProvider::Off);
            assert!(onboard.slots().is_empty(), "off asks for nothing");

            onboard.set_stt_provider(SttProvider::Groq);
            assert_eq!(labels(&onboard), vec!["groq api key"]);

            onboard.set_stt_provider(SttProvider::OpenAiCompatible);
            assert_eq!(
                labels(&onboard),
                vec!["base url", "model", "api key (optional)"],
                "the custom endpoint asks for its url, its model and a key"
            );

            onboard.set_stt_provider(SttProvider::Voicebox);
            assert_eq!(labels(&onboard), vec!["base url"]);

            // Speech is a second, independent choice on the same step.
            onboard.set_stt_provider(SttProvider::Off);
            onboard.set_tts_provider(TtsProvider::OpenAi);
            assert_eq!(labels(&onboard), vec!["api key (optional)"]);
            onboard.set_tts_provider(TtsProvider::Voicebox);
            assert_eq!(
                labels(&onboard),
                vec!["tts base url", "voicebox profile id", "voicebox engine"]
            );
        });
    }

    #[test]
    fn switching_the_engine_pulls_the_old_fields_off_the_screen() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::VoiceSetup);
            onboard.set_stt_provider(SttProvider::Groq);
            let groq = labels(&onboard);
            onboard.set_stt_provider(SttProvider::Voicebox);
            let voicebox = labels(&onboard);
            assert_ne!(groq, voicebox, "each engine brings its own rows");
            assert!(
                !voicebox.contains(&"groq api key"),
                "a groq key stays off the screen once groq is gone: {voicebox:?}"
            );
        });
    }

    #[test]
    fn the_caret_never_lands_past_the_last_row() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::VoiceSetup);
            onboard.set_stt_provider(SttProvider::OpenAiCompatible);
            onboard.set_tts_provider(TtsProvider::OpenAiCompatible);
            // Stand on the last row of the long layout, then pick engines that
            // ask for one field each.
            onboard.field = onboard.field_count() - 1;
            onboard.set_stt_provider(SttProvider::Groq);
            onboard.set_tts_provider(TtsProvider::Off);
            assert_eq!(onboard.slots().len(), 1, "one text row left to stand on");
            assert!(
                onboard.field < onboard.field_count(),
                "the caret stayed inside the rows that are still there"
            );
            assert_eq!(
                onboard.focused_text(),
                Some(TextTarget::GroqKey),
                "and it stands on the row that is actually there"
            );
        });
    }

    #[test]
    fn openai_speech_offers_the_terminals_voices() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::VoiceSetup);
            onboard.set_tts_provider(TtsProvider::OpenAi);
            let voices = onboard.tts_voice_options();
            assert_eq!(voices.len(), 10, "the terminal's own voice list");
            assert_eq!(
                voices.iter().filter(|(_, here)| *here).count(),
                1,
                "exactly one voice is picked"
            );
            onboard.set_tts_voice("nova");
            let picked: Vec<_> = onboard
                .tts_voice_options()
                .into_iter()
                .filter(|(_, here)| *here)
                .map(|(name, _)| name)
                .collect();
            assert_eq!(picked, vec!["nova"]);
        });
    }

    #[test]
    fn the_image_step_asks_for_a_key_only_once_a_switch_is_on() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::ImageSetup);
            onboard.wizard.image_vision_enabled = false;
            onboard.wizard.image_generation_enabled = false;
            assert!(
                onboard.slots().is_empty(),
                "nothing on means nothing to ask"
            );

            onboard.toggle_image(ImageToggle::Vision);
            assert_eq!(labels(&onboard), vec!["api key"]);

            // The generation model row belongs to generation alone: a vision
            // only setup has no model to pick.
            onboard.toggle_image(ImageToggle::Generation);
            assert_eq!(
                labels(&onboard),
                vec!["generation model", "api key"],
                "generation adds the model row above the key"
            );

            onboard.toggle_image(ImageToggle::Generation);
            assert_eq!(
                labels(&onboard),
                vec!["api key"],
                "turning generation off pulls its model row back off"
            );
        });
    }

    #[test]
    fn turning_generation_off_never_strands_the_caret() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::ImageSetup);
            onboard.wizard.image_vision_enabled = false;
            onboard.wizard.image_generation_enabled = true;
            onboard.field = onboard.field_count() - 1; // the last row: the key
            onboard.toggle_image(ImageToggle::Generation);
            assert!(
                onboard.slots().is_empty(),
                "with both switches off nothing is asked for"
            );
            assert_eq!(onboard.field_count(), 2, "only the two switches are left");
            assert_eq!(
                onboard.focused_choice(),
                Some(ChoiceRow::Image(ImageToggle::Generation)),
                "the caret cannot sit past the end, it lands on a row still there"
            );
        });
    }

    fn labels(onboard: &Onboard) -> Vec<&'static str> {
        onboard.slots().iter().map(|(label, _)| *label).collect()
    }

    #[test]
    fn the_rail_lists_the_flow_with_the_current_step_marked() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open();
            onboard.set_mode(WizardMode::Advanced);
            onboard.wizard.step = OnboardingStep::Channels;
            let rail = onboard.rail();
            assert_eq!(rail.len(), OnboardingStep::total());
            let marked: Vec<_> = rail.iter().filter(|(_, _, here)| *here).collect();
            assert_eq!(marked.len(), 1);
            assert_eq!(marked[0].1, "Chat Me Anywhere");
        });
    }

    #[test]
    fn the_brain_step_writes_before_it_moves() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::BrainSetup);
            onboard.wizard.quick_jump = false;
            onboard.wizard.about_me = "I run two companies and I hate fluff.".into();
            assert_eq!(
                onboard.advance(),
                Advance::GenerateBrain,
                "prose means the writer has to run first"
            );
            assert_eq!(
                onboard.step(),
                OnboardingStep::BrainSetup,
                "the step waits for the writer instead of moving on"
            );
            // A second Enter while the thread runs starts nothing new.
            onboard.mark_brain_generating();
            assert_eq!(onboard.advance(), Advance::Blocked);
            onboard.mark_brain_generated();
            assert_eq!(onboard.advance(), Advance::Done);
            assert_eq!(onboard.step(), OnboardingStep::Complete);
            let finish = onboard.finish();
            assert!(
                matches!(finish, Finish::Saved { brain_prompt: None }),
                "a brain already written is not written again: {finish:?}"
            );
        });
    }

    #[test]
    fn an_empty_brain_step_finishes_without_writing() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::BrainSetup);
            onboard.wizard.quick_jump = false;
            onboard.wizard.about_me.clear();
            onboard.wizard.about_opencrabs.clear();
            assert_eq!(onboard.advance(), Advance::Done);
            assert_eq!(onboard.step(), OnboardingStep::Complete);
            assert!(onboard.brain_prompt().is_none(), "nothing to write");
        });
    }

    #[test]
    fn an_unchanged_brain_step_skips_the_writer() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::BrainSetup);
            onboard.wizard.quick_jump = false;
            // Text already on disk, unchanged: regenerating it would spend a
            // model call to produce what is already there.
            onboard.wizard.about_me = "already written".into();
            onboard.wizard.original_about_me = "already written".into();
            onboard.wizard.about_opencrabs = onboard.wizard.original_about_opencrabs.clone();
            assert_eq!(onboard.advance(), Advance::Done);
            assert_eq!(onboard.step(), OnboardingStep::Complete);
        });
    }

    #[test]
    fn a_failed_writer_still_lets_the_step_finish() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::BrainSetup);
            onboard.wizard.quick_jump = false;
            onboard.wizard.about_me = "something worth keeping".into();
            assert_eq!(onboard.advance(), Advance::GenerateBrain);
            onboard.mark_brain_failed("the model was rate limited".into());
            assert_eq!(
                onboard.brain_error(),
                Some("the model was rate limited"),
                "the step keeps the reason"
            );
            assert_eq!(
                onboard.advance(),
                Advance::Done,
                "a failed writer never traps the user in the wizard"
            );
            let finish = onboard.finish();
            assert!(
                matches!(
                    finish,
                    Finish::Saved {
                        brain_prompt: Some(_)
                    }
                ),
                "the prose is retried on save: {finish:?}"
            );
        });
    }

    /// A bare `/onboard` is the whole flow, not one screen: every step the
    /// terminal walks, confirmed the way Enter confirms it here, ending in the
    /// same writes. This is the test that would have caught a step that paints
    /// but cannot be left.
    #[test]
    fn a_full_run_walks_every_step_and_writes_the_completion_state() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open();
            onboard.wizard.mode = WizardMode::Advanced;
            let workspace = dir.path().join("workspace");
            onboard.wizard.workspace_path = workspace.display().to_string();

            // Bare `/onboard` opens where the terminal opens it: the TUI
            // resolves the deep link to `ModeSelect` and seeds from the live
            // config, so the first question is "Pick Your Vibe". Landing on a
            // later step (the resume path) skipped that choice.
            assert_eq!(
                onboard.step(),
                OnboardingStep::ModeSelect,
                "the window opens on the same step the terminal does"
            );
            // A fresh home has nothing enabled, so the step starts on the
            // config's default provider. Pick the free built-in one, the way a
            // user reading the list would: it mints its own token, so the walk
            // needs no key and no network.
            let built_in = PROVIDERS
                .iter()
                .position(|p| p.id == "opencrabs")
                .expect("built-in provider is defined");
            onboard.select_provider(built_in);

            let mut walked = vec![onboard.step()];
            for _ in 0..24 {
                // The doctor step runs its checks on Enter in the terminal, so
                // it runs them here before the same key confirms it.
                if onboard.step() == OnboardingStep::HealthCheck {
                    onboard.run_health();
                    assert!(onboard.health_complete(), "the checks resolve in one pass");
                }
                match onboard.advance() {
                    Advance::Moved => walked.push(onboard.step()),
                    Advance::Done => break,
                    Advance::GenerateBrain => {
                        panic!("an empty brain step must not ask for a writer")
                    }
                    Advance::Blocked => {
                        panic!(
                            "step blocked: {:?} (on {:?}, provider {:?}, walked {walked:?})",
                            onboard.error(),
                            onboard.step(),
                            onboard.provider_id()
                        )
                    }
                }
            }

            assert_eq!(
                onboard.step(),
                OnboardingStep::Complete,
                "the flow reaches the end: {walked:?}"
            );
            assert_eq!(
                walked,
                vec![
                    OnboardingStep::ModeSelect,
                    OnboardingStep::Workspace,
                    OnboardingStep::ProviderAuth,
                    OnboardingStep::Channels,
                    OnboardingStep::VoiceSetup,
                    OnboardingStep::ImageSetup,
                    OnboardingStep::Daemon,
                    OnboardingStep::HealthCheck,
                    OnboardingStep::BrainSetup,
                ],
                "the window walks the terminal's own advanced flow"
            );

            match onboard.finish() {
                Finish::Saved { brain_prompt } => assert!(
                    brain_prompt.is_none(),
                    "nothing typed into the brain step means nothing to write"
                ),
                Finish::Failed(e) => panic!("the flow did not save: {e}"),
            }
            assert!(
                dir.path().join("config.toml").exists(),
                "the flow writes the config the terminal writes"
            );
            assert!(
                opencrabs::tui::onboarding::state::OnboardingState::load().completed,
                "finishing a full run records the completion the gate reads"
            );
            assert!(
                !opencrabs::tui::onboarding::is_first_time(),
                "a finished run stops looking like a first run"
            );
        });
    }

    #[test]
    fn the_provider_window_follows_the_selection_through_the_whole_list() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open();
            onboard.wizard.step = OnboardingStep::ProviderAuth;
            onboard.wizard.mode = WizardMode::Advanced;
            let all = onboard.providers();
            assert!(
                all.len() > 6,
                "the catalogue is longer than one window: {}",
                all.len()
            );

            // Standing on the first row: the window starts there and reports
            // the rest below it. The window used to be a fixed pixel height, so
            // five rows showed and the rest were simply invisible.
            let (rows, above, below) = onboard.provider_rows(6);
            assert_eq!(above, 0, "nothing is hidden above the first row");
            assert_eq!(below, all.len() - 6, "the rest is reported below");
            assert_eq!(rows.first().map(|(i, _)| *i), Some(all[0].0));

            // Standing on the last row: the window has to move with it, or the
            // amber dot walks off the painted rows and the list looks frozen.
            let last = all.last().expect("a provider").0;
            onboard.select_provider(last);
            let (rows, above, below) = onboard.provider_rows(6);
            assert_eq!(below, 0, "nothing is hidden below the last row");
            assert_eq!(above, all.len() - 6, "the rest is reported above");
            assert_eq!(
                rows.last().map(|(i, _)| *i),
                Some(last),
                "the row the keyboard stands on is painted"
            );
        });
    }

    #[test]
    fn saved_custom_providers_are_visible_and_selectable() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::ProviderAuth);
            onboard.wizard.ps.custom_names = vec!["local-test".into()];
            let index = opencrabs::ui_core::model_selector::CUSTOM_INSTANCES_START;
            assert!(
                onboard
                    .providers()
                    .iter()
                    .any(|(candidate, name)| *candidate == index && name == "local-test")
            );
            onboard.select_provider(index);
            assert_eq!(onboard.wizard.ps.selected_provider, index);
            assert!(onboard.wizard.ps.is_custom());
        });
    }

    #[test]
    fn onboard_model_filter_keeps_arbitrary_model_ids() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::ProviderAuth);
            onboard.wizard.ps.models = vec!["existing-model".into()];
            onboard.wizard.ps.model_filter = "custom-future-model".into();
            let provider = onboard.provider_id().to_string();
            assert!(onboard.select_pair(&provider, "custom-future-model"));
            assert_eq!(onboard.model(), "custom-future-model");
            assert!(onboard.wizard.ps.model_filter.is_empty());
        });
    }

    #[test]
    fn esc_walks_back_one_step_until_the_first() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::VoiceSetup);
            // The terminal contract: prev_step() returning false means the
            // step moved, so Esc walks back and the wizard stays open.
            assert!(onboard.escape(), "Esc on VoiceSetup walks back");
            assert_eq!(onboard.step(), OnboardingStep::Channels);
            assert!(onboard.escape(), "Esc on Channels walks back");
            assert_eq!(onboard.step(), OnboardingStep::ProviderAuth);
        });
    }

    #[test]
    fn esc_on_the_first_step_cancels_the_wizard() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::ModeSelect);
            // The terminal contract: prev_step() returning true means cancel,
            // so Esc hands the caller the close signal instead of staying.
            assert!(!onboard.escape(), "Esc on ModeSelect says close me");
        });
    }

    #[test]
    fn arrows_move_the_mode_row_and_pick_it() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::ModeSelect);
            // The terminal's own keys: Down picks Advanced, Up picks
            // QuickStart. The window used to have one row here, so both arrows
            // were a no-op on the first screen of the flow (#2432).
            assert_eq!(onboard.field_count(), 2, "two modes, two rows");
            assert_eq!(onboard.field, 0);
            assert_eq!(onboard.mode(), WizardMode::QuickStart);
            onboard.walk(1);
            assert_eq!(onboard.field, 1, "the caret moved to the second row");
            assert_eq!(onboard.mode(), WizardMode::Advanced, "and picked it");
            onboard.walk(-1);
            assert_eq!(onboard.field, 0);
            assert_eq!(onboard.mode(), WizardMode::QuickStart);
        });
    }

    #[test]
    fn arrows_move_the_voice_engine_rows() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            let mut onboard = Onboard::open_at(OnboardingStep::VoiceSetup);
            let engines = onboard.stt_options();
            assert!(engines.len() > 1, "the step offers more than one engine");
            assert_eq!(
                onboard.choice_row_count(),
                engines.len() + onboard.tts_options().len(),
                "every engine is a row the caret can stand on"
            );
            // Row 0 is the first transcription engine; one step down picks the
            // second, which is what the terminal's Down does on this step.
            assert_eq!(onboard.field, 0);
            onboard.walk(1);
            assert_eq!(onboard.field, 1);
            assert_eq!(
                onboard.wizard.stt_provider, engines[1].0,
                "the arrow picked the engine it landed on"
            );
            assert_ne!(engines[0].0, engines[1].0);
        });
    }

    #[test]
    fn space_flips_the_row_under_the_keyboard() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            // The daemon switch is one row: before, the caret could not reach it
            // and only a mouse click could change it.
            let mut onboard = Onboard::open_at(OnboardingStep::Daemon);
            assert_eq!(onboard.field_count(), 1);
            assert_eq!(onboard.focused_choice(), Some(ChoiceRow::Daemon));
            let before = onboard.install_daemon();
            assert!(onboard.activate_focused_row(), "space has a row to flip");
            assert_ne!(onboard.install_daemon(), before, "and it flipped it");

            // On an image switch the flip adds the model row, and the caret has
            // to stay on a row that exists afterwards.
            let mut image = Onboard::open_at(OnboardingStep::ImageSetup);
            image.field = 1; // the generation switch
            assert!(image.activate_focused_row());
            assert!(image.image_generation(), "the generation switch turned on");
            assert!(
                image.field < image.field_count(),
                "the caret is still on a row: field {} of {}",
                image.field,
                image.field_count()
            );

            // A text row is not a switch: space must type there, not flip.
            let mut brain = Onboard::open_at(OnboardingStep::BrainSetup);
            brain.focus_text(TextTarget::BrainMe);
            assert!(
                !brain.activate_focused_row(),
                "a text field is not a row to flip"
            );
        });
    }

    #[test]
    fn every_switch_step_has_rows_the_keyboard_can_reach() {
        let dir = home();
        with_home_override(dir.path().to_path_buf(), || {
            for step in [
                OnboardingStep::ModeSelect,
                OnboardingStep::VoiceSetup,
                OnboardingStep::ImageSetup,
                OnboardingStep::Daemon,
            ] {
                let onboard = Onboard::open_at(step);
                assert!(
                    onboard.choice_row_count() > 0,
                    "{:?} paints switches, so it must count them as rows",
                    step
                );
                assert!(
                    onboard.field_count() >= onboard.choice_row_count(),
                    "{:?} rows fit inside the walk",
                    step
                );
            }
        });
    }
}
