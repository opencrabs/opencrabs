use super::types::*;
use super::wizard::OnboardingWizard;

impl OnboardingWizard {
    /// Write down where the user is, so leaving halfway is resumable (#919).
    ///
    /// Only for a genuine first-time run. `quick_jump` opens a single step from
    /// a slash command and is not a pass through the wizard, and a user
    /// re-running `/onboard` after finishing has nothing to resume — the gate
    /// reads the completion marker and never looks at the step.
    fn persist_progress(&self) {
        if self.quick_jump || !self.is_first_time {
            return;
        }
        super::state::OnboardingState::record_step(self.step.as_key(), self.mode.as_key());
    }

    /// Advance to the next step
    pub fn next_step(&mut self) {
        // Wrapped so every path out of the step machine below — including the
        // early returns on validation errors — records where the user ended up.
        let prev_step = self.step;
        self.next_step_inner();
        // Step-scoped save (#926): a successful transition (the step actually
        // changed, no validation error) commits the config section owned by
        // the step just completed, so an interrupted wizard keeps everything
        // already confirmed instead of losing it to the single end-of-wizard
        // write. On failure, surface it and stay put: nothing was persisted,
        // so the user retries the same step rather than moving on with an
        // unsaved foundation.
        if self.step != prev_step
            && self.error_message.is_none()
            && let Err(e) = self.apply_step_config(prev_step)
        {
            self.error_message = Some(e);
            self.step = prev_step;
        }
        self.persist_progress();
    }

    /// Commit the model step and advance.
    ///
    /// The one path for both an Enter keypress on the model list and the
    /// automatic advance of a one-model provider (#2782), so the two can never
    /// disagree about which model gets committed.
    pub fn commit_model_step(&mut self) -> WizardAction {
        // Commit the user's pick before advancing. If the filter matches at
        // least one entry, `selected_model_name()` resolves
        // `filtered[selected_model]`. If the filter matched NOTHING but is
        // non-empty, the typed text is a manual model name (the user typed
        // `MiniMax-M3` on a build that still ships M2.7 / M2.5 / M2.1 as
        // suggestions), so push it into `models` and point the selection at it.
        // Mirrors the custom-provider populated-model branch at input.rs:818.
        let filter = self.ps.model_filter.trim().to_string();
        let filtered_count = self.ps.filtered_model_names().len();
        if filtered_count == 0 && !filter.is_empty() {
            if !self.ps.models.iter().any(|m| m == &filter) {
                self.ps.models.push(filter.clone());
            }
            if let Some(idx) = self.ps.models.iter().position(|m| m == &filter) {
                self.ps.selected_model = idx;
            }
            self.ps.model_filter.clear();
        }
        // The built-in provider has no key to type. Its install token is
        // requested first, and the app advances once it is stored (#2047).
        if self.ps.provider_id() == "opencrabs"
            && crate::brain::provider::opencrabs_store::load().is_none()
        {
            return WizardAction::AcquireBuiltinToken;
        }
        self.next_step();
        WizardAction::None
    }

    /// Leave the model step on its own when the provider offers exactly one
    /// model, so a one-model provider never parks the user on a screen whose
    /// only choice is already made (#2782).
    ///
    /// Returns `None` when the screen has to stay: zero models, several models,
    /// a typed filter, a provider still owing an API key, or a step other than
    /// the model list. `Some(action)` means the commit ran; dispatch the action,
    /// which is [`WizardAction::AcquireBuiltinToken`] when the built-in
    /// provider still needs its token. The caller in the event loop cannot
    /// dispatch that action, and there the step deliberately stays up.
    pub fn advance_past_sole_model(&mut self) -> Option<WizardAction> {
        if self.auth_field != AuthField::Model {
            return None;
        }
        self.ps.sole_model_choice()?;
        // One model, empty filter: the rendered order has a single entry, so
        // the index the commit resolves through is 0.
        self.ps.selected_model = 0;
        Some(self.commit_model_step())
    }

    fn next_step_inner(&mut self) {
        self.error_message = None;
        self.focused_field = 0;
        // Reset manual scroll so each new step starts at the top —
        // a Page Down on the previous step shouldn't leave the user
        // mid-scroll on a step they haven't seen yet.
        self.user_scroll_offset = 0;

        // In quick_jump mode, completing a step exits back to chat
        // instead of advancing through the wizard flow.
        if self.quick_jump {
            self.quick_jump_done = true;
            return;
        }

        match self.step {
            OnboardingStep::ModeSelect => {
                self.step = OnboardingStep::Workspace;
            }
            OnboardingStep::Workspace => {
                // Create config files in the workspace directory
                if let Err(e) = self.ensure_config_files() {
                    self.error_message = Some(format!("Failed to create config files: {}", e));
                    return;
                }
                self.step = OnboardingStep::ProviderAuth;
                self.auth_field = AuthField::Provider;
                if self.is_first_time {
                    self.preselect_built_in_provider();
                }
                self.ps.detect_existing_key();
            }
            OnboardingStep::ProviderAuth => {
                // Keyless providers need no API key: CLI subprocesses (Claude
                // CLI, OpenCode CLI) AND any API provider whose onboarding entry
                // has an empty key_label. Only demand a key when the provider
                // actually has a key field.
                let keyless = self.ps.is_keyless();
                if self.ps.api_key_input.is_empty() && !self.ps.is_custom() && !keyless {
                    self.error_message = Some("API key is required".to_string());
                    return;
                }
                if self.ps.is_custom()
                    && (self.ps.base_url.is_empty()
                        || self.ps.custom_model.is_empty()
                        || self.ps.custom_name.is_empty())
                {
                    self.error_message = Some(
                        "Base URL, model name, and provider name are required for custom provider"
                            .to_string(),
                    );
                    return;
                }
                // A provider without a model can't answer anything, and an
                // empty fetched list used to let this step pass with "" (the
                // custom branch above already demands one).
                if !self.ps.is_custom() && self.ps.selected_model_name().trim().is_empty() {
                    self.error_message =
                        Some("Select a model, or type its name in the Model field".to_string());
                    return;
                }
                // This run went through the provider page, so its exclusive
                // enable sweep is legitimate (#2265).
                self.provider_step_touched = true;
                // QuickStart: skip channels, go straight to daemon
                if self.mode == WizardMode::QuickStart {
                    self.step = OnboardingStep::Daemon;
                } else {
                    tracing::debug!("[next_step] ProviderAuth → Channels");
                    self.step = OnboardingStep::Channels;
                    self.focused_field = 0;
                }
            }
            OnboardingStep::Channels => {
                // Handled by handle_channels_key — Enter on focused channel or Continue
                self.step = OnboardingStep::VoiceSetup;
                self.voice_field = VoiceField::SttModeSelect;
                self.detect_existing_groq_key();
            }
            OnboardingStep::TelegramSetup
            | OnboardingStep::DiscordSetup
            | OnboardingStep::WhatsAppSetup
            | OnboardingStep::SlackSetup
            | OnboardingStep::TrelloSetup => {
                // Return to channel list after completing a channel setup
                self.step = OnboardingStep::Channels;
            }
            OnboardingStep::VoiceSetup => {
                // This run touched voice, so its writes are legitimate (#1233)
                self.voice_step_touched = true;
                self.step = OnboardingStep::ImageSetup;
                self.image_field = ImageField::VisionToggle;
                self.detect_existing_image_key();
            }
            OnboardingStep::ImageSetup => {
                self.step = OnboardingStep::Daemon;
            }
            OnboardingStep::Daemon => {
                self.step = OnboardingStep::HealthCheck;
                self.start_health_check();
            }
            OnboardingStep::HealthCheck => {
                self.step = OnboardingStep::BrainSetup;
                self.brain_field = BrainField::AboutMe;
            }
            OnboardingStep::BrainSetup => {
                if self.brain_generated || self.brain_error.is_some() {
                    self.step = OnboardingStep::Complete;
                }
                // Otherwise wait for generation to finish or user to trigger it
            }
            OnboardingStep::Complete => {
                // Already complete
            }
        }
    }

    /// Go back to the previous step
    pub fn prev_step(&mut self) -> bool {
        let cancelled = self.prev_step_inner();
        self.persist_progress();
        cancelled
    }

    fn prev_step_inner(&mut self) -> bool {
        self.error_message = None;
        self.focused_field = 0;
        self.user_scroll_offset = 0;

        match self.step {
            OnboardingStep::ModeSelect => {
                // Can't go back further — return true to signal "cancel wizard"
                return true;
            }
            OnboardingStep::Workspace => {
                self.step = OnboardingStep::ModeSelect;
            }
            OnboardingStep::ProviderAuth => {
                self.step = OnboardingStep::Workspace;
            }
            OnboardingStep::Channels => {
                self.step = OnboardingStep::ProviderAuth;
                self.auth_field = AuthField::Provider;
            }
            OnboardingStep::TelegramSetup => {
                self.step = OnboardingStep::Channels;
            }
            OnboardingStep::DiscordSetup
            | OnboardingStep::WhatsAppSetup
            | OnboardingStep::SlackSetup
            | OnboardingStep::TrelloSetup => {
                self.step = OnboardingStep::Channels;
            }
            OnboardingStep::VoiceSetup => {
                self.step = OnboardingStep::Channels;
            }
            OnboardingStep::ImageSetup => {
                self.step = OnboardingStep::VoiceSetup;
                self.voice_field = VoiceField::SttModeSelect;
            }
            OnboardingStep::Daemon => {
                // QuickStart: go back to ProviderAuth, Advanced: go back to ImageSetup
                if self.mode == WizardMode::QuickStart {
                    self.step = OnboardingStep::ProviderAuth;
                    self.auth_field = AuthField::Provider;
                } else {
                    self.step = OnboardingStep::ImageSetup;
                    self.image_field = ImageField::VisionToggle;
                }
            }
            OnboardingStep::HealthCheck => {
                self.step = OnboardingStep::Daemon;
            }
            OnboardingStep::BrainSetup => {
                self.step = OnboardingStep::HealthCheck;
                self.brain_generating = false;
                self.brain_error = None;
            }
            OnboardingStep::Complete => {
                self.step = OnboardingStep::BrainSetup;
                self.brain_field = BrainField::AboutMe;
            }
        }
        false
    }
}

impl OnboardingWizard {
    /// Point the LLM step at the built-in OpenCrabs provider and its one model.
    /// Only a first run gets this; an existing setup keeps its provider (#2047).
    pub(super) fn preselect_built_in_provider(&mut self) {
        if let Some(idx) = PROVIDERS.iter().position(|p| p.id == "opencrabs") {
            self.ps.selected_provider = idx;
            self.ps.selected_model = 0;
        }
    }

    /// Everything a quick start needs before it can enter chat: the config
    /// files, and the built-in provider selected (#2048).
    pub fn prepare_quick_start(&mut self) -> Result<(), String> {
        self.ensure_config_files()?;
        self.preselect_built_in_provider();
        Ok(())
    }
}
