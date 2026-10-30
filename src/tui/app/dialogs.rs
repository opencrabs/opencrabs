//! Dialogs — onboarding wizard, file/directory pickers, usage dashboard.

use super::events::{AppMode, TuiEvent};
use super::onboarding::{OnboardingStep, WizardAction, WizardMode, welcome_message_for};
use super::*;
use crate::brain::provider::{ContentBlock, LLMRequest};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Shown after the first Ctrl+C inside the wizard; a second one quits.
const QUIT_HINT: &str = "Press Ctrl+C again to quit";

impl App {
    /// Zero-setup first run (#2079).
    ///
    /// A fresh install goes straight to chat on the built-in OpenCrabs
    /// provider: its section is turned on, the proxy issues this install's
    /// token, the agent is rebuilt onto it, and the agent opens the
    /// conversation with the welcome prompt. The wizard is never shown here.
    /// `/onboard` and `--onboard` are the opt-in paths to Quick start and
    /// Advanced.
    ///
    /// Nothing in here blocks the session. An unreachable proxy still lands
    /// the user in a working chat, with a line saying what happened and where
    /// to go next; a dead screen would be the one outcome worse than a failed
    /// fetch.
    pub(crate) async fn provision_first_run(&mut self) {
        // Clear the flag first: every path below ends the provisioning, and a
        // second launch must not re-run it from a stale flag.
        self.first_run = false;

        // The documented files first. `write_key` below needs the provider
        // sections the example carries, and a genuinely fresh home has
        // neither config.toml nor keys.toml on disk.
        crate::config::seed::ensure_config_seeded();
        crate::config::seed::ensure_keys_seeded();

        // The example ships every provider disabled, the built-in one
        // included, so it has to be turned on explicitly. Without this the
        // section exists but `active_provider_and_model` skips it.
        if let Err(e) = crate::config::Config::write_key("providers.opencrabs", "enabled", "true") {
            tracing::warn!("First run: could not enable the built-in provider: {e}");
        }

        // The token the proxy issues is the only credential this provider
        // takes. `ensure_install` writes it to keys.toml under
        // `[providers.opencrabs]`, and it is reused as-is on later launches.
        if let Err(e) = crate::brain::provider::opencrabs_acquire::ensure_install().await {
            tracing::warn!("First run: built-in provider token request failed: {e:?}");
            self.push_system_message(first_run_offline_message(&e));
            return;
        }

        // Only now is the install actually set up, so only now is onboarding
        // recorded as done. A failed fetch above leaves it unrecorded, and the
        // next launch tries again instead of leaving a chat with no provider.
        crate::tui::onboarding::state::OnboardingState::mark_completed();

        if let Err(e) = self.rebuild_agent_service().await {
            tracing::warn!("First run: could not build the built-in provider: {e}");
            self.push_system_message(format!(
                "The built-in brain did not come up: {e}\n\
                 Type /onboard and pick Advanced to choose another provider."
            ));
            return;
        }

        self.sync_session_to_provider().await;

        let (provider, model) = self.welcome_provider_and_model().await;
        self.push_system_message(format!(
            "Setup complete! Provider: {provider} | Model: {model}"
        ));
        self.send_onboarding_welcome(true, WizardMode::QuickStart)
            .await;
    }

    /// Send the hidden welcome prompt once the wizard has saved its config.
    /// Runs for every completion, quick or advanced, first run or `/onboard`.
    /// A failed save sends nothing, so the agent never greets an unconfigured
    /// install (#2022).
    pub(crate) async fn send_onboarding_welcome(&mut self, saved: bool, mode: WizardMode) {
        if !saved {
            return;
        }
        let (provider, model) = self.welcome_provider_and_model().await;
        let gaps = crate::tui::onboarding::setup_gaps::read_setup_gaps();
        let prompt = welcome_message_for(mode, &provider, &model, &gaps);
        if let Err(e) = self.send_message(prompt).await {
            tracing::warn!(error = %e, "failed to send welcome message");
        }
    }

    /// Provider and model serving the current session, read from its row in
    /// the database. Falls back to the agent's view when the row is missing
    /// or has no value recorded, so the greeting always names a model.
    pub(crate) async fn welcome_provider_and_model(&self) -> (String, String) {
        let row = match self.current_session.as_ref() {
            Some(session) => match self.session_service.get_session(session.id).await {
                Ok(found) => found,
                Err(e) => {
                    tracing::warn!(error = %e, "welcome: could not read session row");
                    None
                }
            },
            None => None,
        };
        let provider = row
            .as_ref()
            .and_then(|s| s.provider_name.clone())
            .unwrap_or_else(|| self.provider_name_for_current_session());
        let model = row
            .as_ref()
            .and_then(|s| s.model.clone())
            .unwrap_or_else(|| self.provider_model_for_current_session());
        (provider, model)
    }

    /// Leave the app from inside the wizard. Whatever was typed is saved
    /// first, like a cancel; an unfinished first run reopens on next launch
    /// because completion is only recorded on `Complete`.
    fn quit_from_onboarding(&mut self) {
        if let Some(ref wizard) = self.onboarding
            && let Err(e) = wizard.apply_config()
        {
            tracing::warn!("Wizard quit: partial save failed: {}", e);
        }
        self.should_quit = true;
    }

    /// Request this install's built-in OpenCrabs token. On failure the cause is
    /// shown on the wizard and nothing is saved, so the user can pick another
    /// provider (#2047).
    async fn request_builtin_token(&mut self) -> bool {
        match crate::brain::provider::opencrabs_acquire::ensure_install().await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("Built-in provider token request failed: {e:?}");
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.error_message = Some(
                        crate::brain::provider::opencrabs_install::install_error_message(&e)
                            .to_string(),
                    );
                }
                false
            }
        }
    }

    /// Save the wizard's config, enter chat, and greet the user. Shared by a
    /// finished wizard and the quick start that skips its steps (#2048).
    pub(crate) async fn finish_onboarding(&mut self) -> Result<()> {
        // Apply wizard config before transitioning
        let mut saved = false;
        if let Some(ref wizard) = self.onboarding {
            match wizard.apply_config() {
                Ok(()) => {
                    saved = true;
                    let (provider_name, model_name) = if wizard.ps.is_custom() {
                        (
                            format!("Custom ({})", wizard.ps.custom_name),
                            wizard.ps.custom_model.clone(),
                        )
                    } else {
                        (
                            super::onboarding::PROVIDERS[wizard.ps.selected_provider]
                                .name
                                .to_string(),
                            wizard.ps.selected_model_name().to_string(),
                        )
                    };
                    // Record completion only where the config was
                    // actually written. The Err arm below saved
                    // nothing, so onboarding must run again rather
                    // than be marked done (#919).
                    crate::tui::onboarding::state::OnboardingState::mark_completed();
                    self.push_system_message(format!(
                        "Setup complete! Provider: {} | Model: {}",
                        provider_name, model_name
                    ));
                    // Rebuild agent service with new provider
                    if let Err(e) = self.rebuild_agent_service().await {
                        tracing::warn!("Failed to rebuild agent service: {}", e);
                        self.push_system_message(format!(
                            "Warning: Failed to reload provider: {}",
                            e
                        ));
                    }
                }
                Err(e) => {
                    // "finished with warnings" reads as success.
                    // apply_config returning Err means the config
                    // was NOT written (#914).
                    self.push_system_message(format!("⚠️ Setup did NOT save: {e}"));
                }
            }
        }
        let mode = self
            .onboarding
            .as_ref()
            .map_or(WizardMode::QuickStart, |w| w.mode);
        self.onboarding = None;
        self.sync_session_to_provider().await;
        self.switch_mode(AppMode::Chat).await?;

        // Hidden system prompt so the agent can check its environment
        // and greet the user. The `[SYSTEM:` prefix hides it from display.
        self.send_onboarding_welcome(saved, mode).await;
        Ok(())
    }

    /// Handle keys in onboarding wizard mode
    pub(crate) async fn handle_onboarding_key(
        &mut self,
        event: crossterm::event::KeyEvent,
    ) -> Result<()> {
        // Ctrl+C twice quits from any wizard step, matching chat. The wizard
        // owns the whole screen, so without this there was no way out of a
        // first run short of killing the terminal.
        if super::events::keys::is_quit(&event) {
            if self
                .ctrl_c_pending_at
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3))
            {
                self.quit_from_onboarding();
                return Ok(());
            }
            self.ctrl_c_pending_at = Some(std::time::Instant::now());
            if let Some(ref mut wizard) = self.onboarding {
                wizard.error_message = Some(QUIT_HINT.to_string());
            }
            return Ok(());
        }
        if self.ctrl_c_pending_at.take().is_some()
            && let Some(ref mut wizard) = self.onboarding
            && wizard.error_message.as_deref() == Some(QUIT_HINT)
        {
            wizard.error_message = None;
        }

        if let Some(ref mut wizard) = self.onboarding {
            let action = wizard.handle_key(event);
            match action {
                WizardAction::Quit => {
                    self.quit_from_onboarding();
                }
                WizardAction::Cancel => {
                    // Persist whatever the user entered before dropping the wizard.
                    // Without this, going back from channel setup loses all typed values.
                    if let Some(ref wizard) = self.onboarding
                        && let Err(e) = wizard.apply_config()
                    {
                        // Surface it, do not just log (#914). Leaving the
                        // wizard is exactly when the user stops looking, so a
                        // save that failed here is the one most likely to be
                        // discovered days later as "it never saved anything".
                        tracing::warn!("Wizard cancel: partial save failed: {}", e);
                        self.push_system_message(format!("⚠️ Settings not saved: {e}"));
                    }
                    self.onboarding = None;
                    self.switch_mode(AppMode::Chat).await?;
                }
                WizardAction::QuickJumpDone => {
                    // Quick-jump completed a step — save config then close
                    let mut needs_rebuild = false;
                    if let Some(ref wizard) = self.onboarding {
                        if let Err(e) = wizard.apply_config() {
                            // An Err means the write did NOT happen. Calling it
                            // "saved with warnings" told the user their settings
                            // were stored when nothing was (#914).
                            self.push_system_message(format!("⚠️ Settings NOT saved: {e}"));
                        } else {
                            // Show what changed based on the step
                            let msg = match wizard.step {
                                OnboardingStep::ProviderAuth => {
                                    needs_rebuild = true;
                                    let (pname, mname) = if wizard.ps.is_custom() {
                                        (
                                            format!("Custom ({})", wizard.ps.custom_name),
                                            wizard.ps.custom_model.clone(),
                                        )
                                    } else {
                                        (
                                            super::onboarding::PROVIDERS
                                                [wizard.ps.selected_provider]
                                                .name
                                                .to_string(),
                                            wizard.ps.selected_model_name().to_string(),
                                        )
                                    };
                                    format!("[Model changed to {}/{}]", pname, mname)
                                }
                                OnboardingStep::VoiceSetup => {
                                    let stt_name = match wizard.stt_provider {
                                        super::onboarding::SttProvider::Off => "Off",
                                        super::onboarding::SttProvider::Groq => "Groq",
                                        super::onboarding::SttProvider::Local => "Local Whisper",
                                        super::onboarding::SttProvider::OpenAiCompatible => {
                                            "OpenAI-compatible"
                                        }
                                        super::onboarding::SttProvider::Voicebox => "Voicebox",
                                    };
                                    let tts_name = match wizard.tts_provider {
                                        super::onboarding::TtsProvider::Off => "Off",
                                        super::onboarding::TtsProvider::OpenAi => "OpenAI",
                                        super::onboarding::TtsProvider::Local => "Local Piper",
                                        super::onboarding::TtsProvider::OpenAiCompatible => {
                                            "OpenAI-compatible"
                                        }
                                        super::onboarding::TtsProvider::Voicebox => "Voicebox",
                                    };
                                    let mut parts = vec![
                                        format!("STT: {}", stt_name),
                                        format!("TTS: {}", tts_name),
                                    ];
                                    if wizard.tts_provider
                                        == super::onboarding::TtsProvider::Voicebox
                                        && !wizard.tts_voicebox_profile_id.is_empty()
                                    {
                                        parts.push(format!(
                                            "Profile: {}",
                                            &wizard.tts_voicebox_profile_id
                                                [..8.min(wizard.tts_voicebox_profile_id.len())]
                                        ));
                                    }
                                    if wizard.tts_provider
                                        == super::onboarding::TtsProvider::Voicebox
                                        && !wizard.tts_voicebox_engine.is_empty()
                                    {
                                        parts.push(format!(
                                            "Engine: {}",
                                            wizard.tts_voicebox_engine
                                        ));
                                    }
                                    format!("Voice settings saved — {}", parts.join(" | "))
                                }
                                OnboardingStep::ImageSetup => {
                                    let mut parts = vec![];
                                    if wizard.image_vision_enabled {
                                        parts.push("Vision: ON".to_string());
                                    }
                                    if wizard.image_generation_enabled {
                                        parts.push("Generation: ON".to_string());
                                    }
                                    if parts.is_empty() {
                                        parts.push("Image: OFF".to_string());
                                    }
                                    format!("Image settings saved — {}", parts.join(" | "))
                                }
                                OnboardingStep::Channels => {
                                    let mut parts = vec![];
                                    if wizard.is_telegram_enabled() {
                                        parts.push("Telegram".to_string());
                                    }
                                    if wizard.is_discord_enabled() {
                                        parts.push("Discord".to_string());
                                    }
                                    if wizard.channel_toggles.get(2).is_some_and(|t| t.1) {
                                        parts.push("WhatsApp".to_string());
                                    }
                                    if wizard.is_slack_enabled() {
                                        parts.push("Slack".to_string());
                                    }
                                    if wizard.is_trello_enabled() {
                                        parts.push("Trello".to_string());
                                    }
                                    if parts.is_empty() {
                                        parts.push("All channels OFF".to_string());
                                    }
                                    format!("Channels saved — {}", parts.join(", "))
                                }
                                _ => "Settings saved.".to_string(),
                            };
                            self.push_system_message(msg);
                        }
                    }
                    self.onboarding = None;
                    if needs_rebuild && let Err(e) = self.rebuild_agent_service().await {
                        tracing::warn!("Failed to rebuild agent service: {}", e);
                        self.push_system_message(format!(
                            "Warning: Failed to reload provider: {}",
                            e
                        ));
                    }
                    if needs_rebuild {
                        // Swap the CURRENT session's per-session provider to
                        // the newly-rebuilt global. Without this, the agent
                        // service still serves the old per-session pin (set
                        // by an earlier swap_provider_for_session, e.g. from
                        // a previous /models switch), so the footer keeps
                        // showing the OLD provider while the banner reads
                        // the new GLOBAL provider — half-changed state.
                        // 2026-05-28 user report: "[Model changed to
                        // OpenRouter/qwen]" banner appeared, footer still
                        // said dialagram. Both now reflect the same change.
                        if let Some(ref session) = self.current_session {
                            let session_id = session.id;
                            let new_provider = self.agent_service.provider();
                            // Pair with the newly-configured global model.
                            let model = self.agent_service.provider_model();
                            self.agent_service.swap_provider_for_session(
                                session_id,
                                new_provider,
                                model,
                            );
                        }
                        self.sync_session_to_provider().await;
                    }
                    self.switch_mode(AppMode::Chat).await?;
                }
                WizardAction::Complete => {
                    self.finish_onboarding().await?;
                }
                WizardAction::AcquireBuiltinToken => {
                    if self.request_builtin_token().await
                        && let Some(ref mut wizard) = self.onboarding
                    {
                        wizard.next_step();
                    }
                }
                WizardAction::QuickStartBuiltin => {
                    let prepared = match self.onboarding.as_mut() {
                        Some(wizard) => wizard.prepare_quick_start(),
                        None => Ok(()),
                    };
                    match prepared {
                        Err(e) => {
                            if let Some(ref mut wizard) = self.onboarding {
                                wizard.error_message = Some(e);
                            }
                        }
                        Ok(()) => {
                            if self.request_builtin_token().await {
                                self.finish_onboarding().await?;
                            }
                        }
                    }
                }
                WizardAction::FetchModels => {
                    let provider_idx = wizard.ps.selected_provider;
                    // Resolve the real API key: a freshly typed one, else the
                    // saved key from config (keys.toml) for ANY provider,
                    // including custom. Never the EXISTING_KEY_SENTINEL, which
                    // 401'd custom endpoints like Model Studio (#656).
                    let api_key = wizard.ps.effective_api_key();
                    wizard.ps.models_fetching = true;

                    // Capture custom base_url so custom providers can hit <base_url>/v1/models
                    let base_url = if wizard.ps.base_url.trim().is_empty() {
                        None
                    } else {
                        Some(wizard.ps.base_url.clone())
                    };

                    // OpenCode plan (Zen or Go) picks the /models catalog (#2040)
                    let opencode_et = (wizard.ps.provider_id() == "opencode").then(|| {
                        if wizard.ps.opencode_endpoint_type == 1 {
                            "zen".to_string()
                        } else {
                            "go".to_string()
                        }
                    });

                    // Capture zhipu endpoint type from wizard state (not yet saved to config)
                    let zhipu_et = if wizard.ps.provider_id() == "zai" {
                        Some(if wizard.ps.zhipu_endpoint_type == 1 {
                            "coding".to_string()
                        } else {
                            "api".to_string()
                        })
                    } else {
                        None
                    };

                    // Capture xiaomi endpoint type from wizard state (not yet saved to config)
                    let xiaomi_et = if wizard.ps.provider_id() == "xiaomi" {
                        Some(if wizard.ps.xiaomi_endpoint_type == 1 {
                            "token-plan".to_string()
                        } else {
                            "api".to_string()
                        })
                    } else {
                        None
                    };

                    // Capture moonshot endpoint type from wizard state (not yet saved to config)
                    let moonshot_et = if wizard.ps.provider_id() == "moonshot" {
                        Some(if wizard.ps.moonshot_endpoint_type == 1 {
                            "coding".to_string()
                        } else {
                            "api".to_string()
                        })
                    } else {
                        None
                    };

                    let sender = self.event_sender();
                    tokio::spawn(async move {
                        let models = super::onboarding::fetch_provider_models(
                            provider_idx,
                            api_key.as_deref(),
                            zhipu_et.as_deref(),
                            xiaomi_et.as_deref(),
                            moonshot_et.as_deref(),
                            base_url.as_deref(),
                            opencode_et.as_deref(),
                        )
                        .await;
                        let _ = sender.send(TuiEvent::OnboardingModelsFetched(models));
                    });
                }
                WizardAction::GitHubDeviceFlow => {
                    wizard.github_device_flow_status =
                        super::onboarding::GitHubDeviceFlowStatus::WaitingForUser;
                    let sender = self.event_sender();
                    tokio::spawn(async move {
                        // Step 1: Request device code
                        let device =
                            match crate::brain::provider::copilot::start_device_flow().await {
                                Ok(d) => d,
                                Err(e) => {
                                    let _ = sender.send(TuiEvent::GitHubOAuthError(e.to_string()));
                                    return;
                                }
                            };

                        // Send the user code for display
                        let _ = sender.send(TuiEvent::GitHubDeviceCode(device.user_code.clone()));

                        // Step 2-3: Poll until user authorizes
                        match crate::brain::provider::copilot::poll_for_oauth_token(
                            &device.device_code,
                            device.interval,
                        )
                        .await
                        {
                            Ok(token) => {
                                let _ = sender.send(TuiEvent::GitHubOAuthComplete(token));
                            }
                            Err(e) => {
                                let _ = sender.send(TuiEvent::GitHubOAuthError(e.to_string()));
                            }
                        }
                    });
                }
                WizardAction::CodexDeviceFlow => {
                    wizard.ps.codex_device_flow_status =
                        super::onboarding::CodexDeviceFlowStatus::WaitingForUser;
                    let sender = self.event_sender();
                    tokio::spawn(async move {
                        // Step 1: Request device code
                        let device =
                            match crate::brain::provider::codex_oauth::start_device_flow().await {
                                Ok(d) => d,
                                Err(e) => {
                                    let _ = sender.send(TuiEvent::CodexOAuthError(e.to_string()));
                                    return;
                                }
                            };

                        // Send the user code for display
                        let _ = sender.send(TuiEvent::CodexDeviceCode(device.user_code.clone()));

                        // Step 2: Poll until user authorizes (returns intermediate PKCE code)
                        let device_code =
                            match crate::brain::provider::codex_oauth::poll_for_device_code(
                                &device.device_auth_id,
                                &device.user_code,
                                device.interval,
                            )
                            .await
                            {
                                Ok(dc) => dc,
                                Err(e) => {
                                    let _ = sender.send(TuiEvent::CodexOAuthError(e.to_string()));
                                    return;
                                }
                            };

                        // Step 3: Exchange PKCE code for final tokens at /oauth/token
                        match crate::brain::provider::codex_oauth::exchange_device_code_for_tokens(
                            &device_code,
                        )
                        .await
                        {
                            Ok(token_resp) => {
                                // Save tokens to disk
                                let expires_at = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs()
                                    + token_resp.expires_in;
                                let tokens = crate::brain::provider::codex_oauth::CodexTokens {
                                    access_token: token_resp.access_token,
                                    refresh_token: token_resp.refresh_token,
                                    id_token: token_resp.id_token,
                                    account_id: token_resp.account_id,
                                    expires_at,
                                };
                                if let Err(e) = tokens.save() {
                                    let _ = sender.send(TuiEvent::CodexOAuthError(format!(
                                        "Failed to save tokens: {}",
                                        e
                                    )));
                                    return;
                                }
                                let _ = sender.send(TuiEvent::CodexOAuthComplete);
                            }
                            Err(e) => {
                                let _ = sender.send(TuiEvent::CodexOAuthError(e.to_string()));
                            }
                        }
                    });
                }
                WizardAction::WhatsAppConnect => {
                    // Wipe session.db (old auth on disk), then request a restart
                    // so reconcile aborts the live agent and starts a fresh one
                    // against the wiped session. Without the restart the running
                    // agent keeps its in-memory auth and never re-pairs, so the
                    // QR never refreshes after a reset.
                    #[cfg(feature = "whatsapp")]
                    {
                        let wa_dir = crate::config::opencrabs_home().join("whatsapp");
                        let _ = std::fs::remove_file(wa_dir.join("session.db"));
                        let _ = std::fs::remove_file(wa_dir.join("session.db-wal"));
                        let _ = std::fs::remove_file(wa_dir.join("session.db-shm"));
                        self.whatsapp_state.request_restart();
                        let _ = crate::config::Config::write_key(
                            "channels.whatsapp",
                            "enabled",
                            "true",
                        );
                    }

                    // Subscribe to QR/connected events from the agent bot
                    #[cfg(feature = "whatsapp")]
                    let wa_state = self.whatsapp_state.clone();
                    let sender = self.event_sender();
                    tokio::spawn(async move {
                        #[cfg(feature = "whatsapp")]
                        {
                            let handle =
                                crate::brain::tools::whatsapp_connect::subscribe_whatsapp_pairing(
                                    &wa_state, false,
                                );
                            // Replay the current QR immediately so a connect that
                            // subscribes after the agent already emitted still
                            // shows it, instead of waiting for the next refresh
                            // (the "press Enter twice" race).
                            if let Some(qr) = wa_state.current_qr() {
                                let _ = sender.send(TuiEvent::WhatsAppQrCode(qr));
                            }
                            // Forward QR codes to the TUI
                            let qr_sender = sender.clone();
                            let mut qr_rx = handle.qr_rx;
                            tokio::spawn(async move {
                                while let Ok(qr) = qr_rx.recv().await {
                                    let _ = qr_sender.send(TuiEvent::WhatsAppQrCode(qr));
                                }
                            });
                            // Forward agent errors to the TUI
                            let err_sender = sender.clone();
                            let mut error_rx = handle.error_rx;
                            tokio::spawn(async move {
                                if let Ok(err) = error_rx.recv().await {
                                    let _ = err_sender.send(TuiEvent::WhatsAppError(err));
                                }
                            });
                            // Wait for connection (2 minute timeout)
                            let mut connected_rx = handle.connected_rx;
                            match tokio::time::timeout(
                                std::time::Duration::from_secs(120),
                                connected_rx.recv(),
                            )
                            .await
                            {
                                Ok(Ok(())) => {
                                    let _ = sender.send(TuiEvent::WhatsAppConnected);
                                }
                                Ok(Err(e)) => {
                                    // Broadcast channel closed — agent crashed or failed to start
                                    let msg = format!(
                                        "WhatsApp agent stopped unexpectedly: {}. Check logs at ~/.opencrabs/logs/",
                                        e
                                    );
                                    tracing::error!("{}", msg);
                                    let _ = sender.send(TuiEvent::WhatsAppError(msg));
                                }
                                Err(_) => {
                                    let _ = sender.send(TuiEvent::WhatsAppError(
                                        "QR scan timed out (2 minutes). Press R to retry.".into(),
                                    ));
                                }
                            }
                        }
                    });
                }
                WizardAction::TestWhatsApp => {
                    wizard.channel_test_status = super::onboarding::ChannelTestStatus::Testing;
                    let phone = if wizard.has_existing_whatsapp_phone() {
                        crate::config::Config::load()
                            .ok()
                            .and_then(|c| c.channels.whatsapp.allowed_phones.first().cloned())
                            .unwrap_or_default()
                    } else {
                        wizard.whatsapp_phone_input.clone()
                    };
                    #[cfg(feature = "whatsapp")]
                    let wa_state = self.whatsapp_state.clone();
                    let sender = self.event_sender();
                    let agent = self.agent_service.clone();
                    tokio::spawn(async move {
                        #[cfg(feature = "whatsapp")]
                        let result = test_whatsapp_connection(wa_state, &phone, agent).await;
                        #[cfg(not(feature = "whatsapp"))]
                        let result: Result<(), String> =
                            Err("WhatsApp feature not enabled".to_string());
                        let _ = sender.send(TuiEvent::ChannelTestResult {
                            channel: "whatsapp".to_string(),
                            success: result.is_ok(),
                            error: result.err(),
                            detected_telegram_user_id: None,
                        });
                    });
                }
                WizardAction::TestTelegram => {
                    wizard.channel_test_status = super::onboarding::ChannelTestStatus::Testing;
                    let token = if wizard.has_existing_telegram_token() {
                        crate::config::Config::load()
                            .ok()
                            .and_then(|c| c.channels.telegram.token.clone())
                            .unwrap_or_default()
                    } else {
                        wizard.telegram_token_input.clone()
                    };
                    // telegram_user_id_input is never a sentinel — always the real value.
                    let user_id_str = wizard.telegram_user_id_input.clone();
                    let sender = self.event_sender();
                    let agent = self.agent_service.clone();
                    tokio::spawn(async move {
                        let result = test_telegram_connection(&token, &user_id_str, agent).await;
                        let detected_uid = result
                            .as_ref()
                            .ok()
                            .and_then(|r| r.detected_user_id.clone());
                        let _ = sender.send(TuiEvent::ChannelTestResult {
                            channel: "telegram".to_string(),
                            success: result.is_ok(),
                            error: result.err(),
                            detected_telegram_user_id: detected_uid,
                        });
                    });
                }
                WizardAction::TestDiscord => {
                    wizard.channel_test_status = super::onboarding::ChannelTestStatus::Testing;
                    let token = if wizard.has_existing_discord_token() {
                        crate::config::Config::load()
                            .ok()
                            .and_then(|c| c.channels.discord.token.clone())
                            .unwrap_or_default()
                    } else {
                        wizard.discord_token_input.clone()
                    };
                    let channel_id = if wizard.has_existing_discord_channel_id() {
                        crate::config::Config::load()
                            .ok()
                            .and_then(|c| c.channels.discord.allowed_channels.first().cloned())
                            .unwrap_or_default()
                    } else {
                        wizard.discord_channel_id_input.clone()
                    };
                    let sender = self.event_sender();
                    let agent = self.agent_service.clone();
                    tokio::spawn(async move {
                        let result = test_discord_connection(&token, &channel_id, agent).await;
                        let _ = sender.send(TuiEvent::ChannelTestResult {
                            channel: "discord".to_string(),
                            success: result.is_ok(),
                            error: result.err(),
                            detected_telegram_user_id: None,
                        });
                    });
                }
                WizardAction::TestSlack => {
                    wizard.channel_test_status = super::onboarding::ChannelTestStatus::Testing;
                    let token = if wizard.has_existing_slack_bot_token() {
                        crate::config::Config::load()
                            .ok()
                            .and_then(|c| c.channels.slack.token.clone())
                            .unwrap_or_default()
                    } else {
                        wizard.slack_bot_token_input.clone()
                    };
                    let channel_id = if wizard.has_existing_slack_channel_id() {
                        crate::config::Config::load()
                            .ok()
                            .and_then(|c| c.channels.slack.allowed_channels.first().cloned())
                            .unwrap_or_default()
                    } else {
                        wizard.slack_channel_id_input.clone()
                    };
                    let sender = self.event_sender();
                    let agent = self.agent_service.clone();
                    tokio::spawn(async move {
                        let result = test_slack_connection(&token, &channel_id, agent).await;
                        let _ = sender.send(TuiEvent::ChannelTestResult {
                            channel: "slack".to_string(),
                            success: result.is_ok(),
                            error: result.err(),
                            detected_telegram_user_id: None,
                        });
                    });
                }
                WizardAction::TestTrello => {
                    wizard.channel_test_status = super::onboarding::ChannelTestStatus::Testing;
                    let api_key = if wizard.has_existing_trello_api_key() {
                        crate::config::Config::load()
                            .ok()
                            .and_then(|c| c.channels.trello.app_token.clone())
                            .unwrap_or_default()
                    } else {
                        wizard.trello_api_key_input.clone()
                    };
                    let api_token = if wizard.has_existing_trello_api_token() {
                        crate::config::Config::load()
                            .ok()
                            .and_then(|c| c.channels.trello.token.clone())
                            .unwrap_or_default()
                    } else {
                        wizard.trello_api_token_input.clone()
                    };
                    let sender = self.event_sender();
                    tokio::spawn(async move {
                        let result = test_trello_connection(&api_key, &api_token).await;
                        let _ = sender.send(TuiEvent::ChannelTestResult {
                            channel: "trello".to_string(),
                            success: result.is_ok(),
                            error: result.err(),
                            detected_telegram_user_id: None,
                        });
                    });
                }
                WizardAction::GenerateBrain => {
                    // Extract prompt and workspace path before dropping wizard.
                    // Brain generation runs in the background after entering chat.
                    let brain_context = self.onboarding.as_mut().map(|wizard| {
                        wizard.normalize_brain_inputs();
                        let prompt = wizard.build_brain_prompt();
                        let workspace = wizard.workspace_path.clone();
                        (prompt, workspace)
                    });

                    // Ensure provider is available (fresh install may still
                    // have PlaceholderProvider at this point).
                    if self.agent_service.provider_name() == "none" {
                        if let Some(ref wizard) = self.onboarding
                            && let Err(e) = wizard.apply_config()
                        {
                            tracing::warn!("Brain gen: apply_config before generation: {}", e);
                            self.push_system_message(format!(
                                "⚠️ Settings NOT saved before brain generation: {e}"
                            ));
                        }
                        if let Err(e) = self.rebuild_agent_service().await {
                            tracing::warn!("Brain gen: rebuild_agent_service failed: {}", e);
                        }
                    }

                    // Complete onboarding — go straight to chat
                    let mut saved = false;
                    if let Some(ref wizard) = self.onboarding {
                        match wizard.apply_config() {
                            Ok(()) => {
                                saved = true;
                                let (provider_name, model_name) = if wizard.ps.is_custom() {
                                    (
                                        format!("Custom ({})", wizard.ps.custom_name),
                                        wizard.ps.custom_model.clone(),
                                    )
                                } else {
                                    (
                                        super::onboarding::PROVIDERS[wizard.ps.selected_provider]
                                            .name
                                            .to_string(),
                                        wizard.ps.selected_model_name().to_string(),
                                    )
                                };
                                // Record completion only where the config was
                                // actually written. The Err arm below saved
                                // nothing, so onboarding must run again rather
                                // than be marked done (#919).
                                crate::tui::onboarding::state::OnboardingState::mark_completed();
                                self.push_system_message(format!(
                                    "Setup complete! Provider: {} | Model: {}",
                                    provider_name, model_name
                                ));
                                if let Err(e) = self.rebuild_agent_service().await {
                                    tracing::warn!("Failed to rebuild agent service: {}", e);
                                }
                            }
                            Err(e) => {
                                // "finished with warnings" reads as success.
                                // apply_config returning Err means the config
                                // was NOT written (#914).
                                self.push_system_message(format!("⚠️ Setup did NOT save: {e}"));
                            }
                        }
                    }
                    let mode = self
                        .onboarding
                        .as_ref()
                        .map_or(WizardMode::QuickStart, |w| w.mode);
                    self.onboarding = None;
                    self.sync_session_to_provider().await;
                    self.switch_mode(AppMode::Chat).await?;

                    // Hidden system prompt so the agent can check its environment
                    // and greet the user. Sent on every saved completion (#2022).
                    self.send_onboarding_welcome(saved, mode).await;

                    // Fire brain generation in the background
                    if let Some((prompt, workspace)) = brain_context {
                        self.push_system_message(
                            "Generating personalized brain files in the background...".to_string(),
                        );
                        self.generate_brain_files_background(prompt, workspace);
                    }
                }
                WizardAction::DownloadWhisperModel => {
                    #[cfg(feature = "local-stt")]
                    {
                        use crate::channels::voice::local_whisper::{
                            DownloadProgress, LOCAL_MODEL_PRESETS,
                        };
                        let tui_sender = self.event_sender();
                        if let Some(ref mut wizard) = self.onboarding {
                            let idx = wizard.selected_local_stt_model;
                            if idx < LOCAL_MODEL_PRESETS.len() {
                                let preset = &LOCAL_MODEL_PRESETS[idx];
                                wizard.stt_model_download_progress = Some(0.0);
                                let (progress_tx, mut progress_rx) =
                                    tokio::sync::mpsc::unbounded_channel::<DownloadProgress>();
                                let fwd_sender = tui_sender.clone();
                                tokio::spawn(async move {
                                    while let Some(p) = progress_rx.recv().await {
                                        let frac = match p.total {
                                            Some(t) if t > 0 => p.downloaded as f64 / t as f64,
                                            _ => 0.0,
                                        };
                                        let _ = fwd_sender
                                            .send(TuiEvent::WhisperDownloadProgress(frac));
                                    }
                                });
                                tokio::spawn(async move {
                                    let result =
                                        crate::channels::voice::local_whisper::download_model(
                                            preset,
                                            progress_tx,
                                        )
                                        .await;
                                    let _ = tui_sender.send(TuiEvent::WhisperDownloadComplete(
                                        result.map(|_| ()).map_err(|e| e.to_string()),
                                    ));
                                });
                            }
                        }
                    }
                }
                WizardAction::DownloadPiperVoice => {
                    #[cfg(feature = "local-tts")]
                    {
                        use crate::channels::voice::local_tts::{
                            DownloadProgress, PIPER_VOICES, delete_other_voices,
                        };
                        let tui_sender = self.event_sender();
                        if let Some(ref mut wizard) = self.onboarding {
                            let idx = wizard.selected_tts_voice;
                            if idx < PIPER_VOICES.len() {
                                let voice_id = PIPER_VOICES[idx].id.to_string();
                                delete_other_voices(&voice_id);
                                wizard.tts_voice_download_progress = Some(0.0);
                                let (progress_tx, mut progress_rx) =
                                    tokio::sync::mpsc::unbounded_channel::<DownloadProgress>();
                                let fwd_sender = tui_sender.clone();
                                tokio::spawn(async move {
                                    while let Some(p) = progress_rx.recv().await {
                                        let frac = match p.total {
                                            Some(t) if t > 0 => p.downloaded as f64 / t as f64,
                                            _ => 0.0,
                                        };
                                        let _ =
                                            fwd_sender.send(TuiEvent::PiperDownloadProgress(frac));
                                    }
                                });
                                tokio::spawn(async move {
                                    // Install Piper venv if not present
                                    if !crate::channels::voice::local_tts::piper_venv_exists() {
                                        let (setup_tx, mut setup_rx) =
                                            tokio::sync::mpsc::unbounded_channel::<
                                                crate::channels::voice::local_tts::SetupProgress,
                                            >();
                                        let setup_fwd = tui_sender.clone();
                                        tokio::spawn(async move {
                                            while let Some(p) = setup_rx.recv().await {
                                                tracing::info!("Piper setup: {}", p.stage);
                                                let _ = setup_fwd
                                                    .send(TuiEvent::PiperDownloadProgress(0.0));
                                            }
                                        });
                                        if let Err(e) =
                                            crate::channels::voice::local_tts::setup_piper_venv(
                                                setup_tx,
                                            )
                                            .await
                                        {
                                            let _ = tui_sender.send(
                                                TuiEvent::PiperDownloadComplete(Err(e.to_string())),
                                            );
                                            return;
                                        }
                                    }
                                    // Download voice model
                                    let result = crate::channels::voice::local_tts::download_voice(
                                        &voice_id,
                                        progress_tx,
                                    )
                                    .await;
                                    let _ = tui_sender.send(TuiEvent::PiperDownloadComplete(
                                        result.map(|_| voice_id.clone()).map_err(|e| e.to_string()),
                                    ));
                                });
                            }
                        }
                    }
                }
                WizardAction::None => {
                    // Stay in onboarding
                }
            }
        }
        Ok(())
    }

    /// Fire brain generation in the background. Onboarding is already done —
    /// the user is in chat. On success, writes brain files directly to workspace
    /// and notifies via `TuiEvent::BrainGenerationResult`.
    fn generate_brain_files_background(&self, prompt: String, workspace: String) {
        let provider = self.agent_service.provider().clone();
        let model = self.agent_service.provider_model().to_string();
        let sender = self.event_sender();

        let request = LLMRequest::new(model, vec![crate::brain::provider::Message::user(prompt)])
            .with_max_tokens(65536);

        tokio::spawn(async move {
            let result: Result<String, String> = match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                provider.complete(request),
            )
            .await
            {
                Ok(Ok(response)) => {
                    let text: String = response
                        .content
                        .iter()
                        .filter_map(|block| {
                            if let ContentBlock::Text { text } = block {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect();

                    // Parse and write directly to workspace
                    let parsed = crate::tui::onboarding::parse_brain_sections(&text);

                    let names = ["SOUL", "USER", "AGENTS", "TOOLS", "MEMORY"];
                    let found: Vec<&str> = names
                        .iter()
                        .zip(parsed.iter())
                        .filter_map(|(n, p)| p.as_ref().map(|_| *n))
                        .collect();
                    let missing: Vec<&str> = names
                        .iter()
                        .zip(parsed.iter())
                        .filter_map(|(n, p)| if p.is_none() { Some(*n) } else { None })
                        .collect();
                    tracing::info!(
                        "Brain gen parsed: found=[{}], missing=[{}]",
                        found.join(", "),
                        missing.join(", ")
                    );

                    // Need at least SOUL + USER
                    if parsed[0].is_none() || parsed[0].is_none() || parsed[1].is_none() {
                        tracing::warn!(
                            "Brain gen: couldn't parse response (first 500 chars): {}",
                            crate::utils::string::truncate_str(&text, 500)
                        );
                        Err(
                            "Couldn't parse brain files from AI response — using defaults"
                                .to_string(),
                        )
                    } else {
                        let ws = std::path::Path::new(&workspace);
                        let file_map = [
                            ("SOUL.md", &parsed[0]),
                            ("USER.md", &parsed[1]),
                            ("AGENTS.md", &parsed[2]),
                            ("TOOLS.md", &parsed[3]),
                            ("MEMORY.md", &parsed[4]),
                        ];
                        let mut written = 0;
                        for (filename, content) in &file_map {
                            if let Some(text) = content {
                                if let Err(e) = std::fs::write(ws.join(filename), text) {
                                    tracing::warn!(
                                        "Brain gen: failed to write {}: {}",
                                        filename,
                                        e
                                    );
                                } else {
                                    written += 1;
                                }
                            }
                        }
                        Ok(format!("{written} brain files personalized"))
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("Brain generation failed: {}", e);
                    Err(format!("Brain generation failed: {}", e))
                }
                Err(_) => {
                    tracing::warn!("Brain generation timed out after 120s");
                    Err("Brain generation timed out — using defaults".to_string())
                }
            };
            let _ = sender.send(TuiEvent::BrainGenerationResult { result });
        });
    }

    /// Open file picker and populate file list.
    ///
    /// Starts at the session's working directory (not the app startup cwd).
    /// Call `refresh_file_picker()` to reload entries without resetting the dir.
    pub(crate) async fn open_file_picker(&mut self) -> Result<()> {
        // Start at the session's working directory
        self.file_picker_current_dir = self.working_directory.clone();
        self.file_picker_search.clear();
        self.file_picker_recursive = false;
        self.refresh_file_picker().await
    }

    /// Reload file list for the current directory and apply search filter.
    pub(crate) async fn refresh_file_picker(&mut self) -> Result<()> {
        self.load_flat_picker_files();
        self.file_picker_recursive = false;
        self.apply_file_picker_filter();
        self.switch_mode(AppMode::FilePicker).await?;
        Ok(())
    }

    /// Populate `file_picker_files` with a flat listing of
    /// `file_picker_current_dir` (plus a `..` entry when applicable).
    fn load_flat_picker_files(&mut self) {
        let mut files = Vec::new();

        if self.file_picker_current_dir.parent().is_some() {
            files.push(self.file_picker_current_dir.join(".."));
        }

        if let Ok(entries) = std::fs::read_dir(&self.file_picker_current_dir) {
            for entry in entries.flatten() {
                files.push(entry.path());
            }
        }

        files.sort_by(|a, b| {
            let a_is_dir = a.is_dir();
            let b_is_dir = b.is_dir();
            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.file_name().cmp(&b.file_name()),
            }
        });

        self.file_picker_files = files;
    }

    /// Recursively walk the session working directory using ripgrep's
    /// `ignore` walker (respects `.gitignore`, `.ignore`, hidden file rules).
    /// Caps the result at `MAX_RECURSIVE_RESULTS` to keep huge repos snappy.
    fn load_recursive_picker_files(&mut self) {
        const MAX_RECURSIVE_RESULTS: usize = 20_000;

        let mut files = Vec::with_capacity(256);
        let walker = ignore::WalkBuilder::new(&self.working_directory)
            .standard_filters(true)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .max_depth(Some(20))
            // `.hidden(false)` keeps dotfiles like `.env` visible, but without
            // this filter we also descend into VCS metadata trees — `.git/`
            // alone can hold thousands of pack/ref files and silently eat the
            // result cap before legitimate source dirs are reached.
            .filter_entry(|e| !matches!(e.file_name().to_str(), Some(".git" | ".hg" | ".svn")))
            .build();

        for entry in walker.flatten() {
            if entry.depth() == 0 {
                continue;
            }
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                continue;
            }
            files.push(entry.into_path());
            if files.len() >= MAX_RECURSIVE_RESULTS {
                break;
            }
        }

        files.sort();
        self.file_picker_files = files;
    }

    /// Switch the underlying file source based on the current search length:
    /// flat dir listing for `< 2` chars, recursive walk for `>= 2`. Only
    /// rebuilds when the source actually needs to change so per-keystroke
    /// filtering stays cheap.
    fn sync_file_picker_source(&mut self) {
        let wants_recursive = self.file_picker_search.chars().count() >= 2;
        if wants_recursive && !self.file_picker_recursive {
            self.load_recursive_picker_files();
            self.file_picker_recursive = true;
        } else if !wants_recursive && self.file_picker_recursive {
            self.load_flat_picker_files();
            self.file_picker_recursive = false;
        }
    }

    /// Filter the file list based on the current search query.
    /// `".."` shows when the query is empty so the user can still navigate
    /// up a directory, but drops OUT of the filtered list as soon as the
    /// user starts typing — otherwise `file_picker_selected = 0` lands on
    /// `..` instead of the first real match, and hitting Enter navigates
    /// up instead of picking the file the user was filtering for.
    ///
    /// In recursive mode the query is matched against the path **relative to
    /// the working directory** so users can filter by directory segments
    /// (e.g. `tui/render` matches `src/tui/render/dialogs.rs`).
    fn apply_file_picker_filter(&mut self) {
        let query = self.file_picker_search.to_lowercase();
        let recursive = self.file_picker_recursive;
        let working_dir = self.working_directory.clone();
        self.file_picker_filtered = if query.is_empty() {
            (0..self.file_picker_files.len()).collect()
        } else {
            self.file_picker_files
                .iter()
                .enumerate()
                .filter(|(_, path)| {
                    if path.ends_with("..") {
                        return false;
                    }
                    let haystack = if recursive {
                        path.strip_prefix(&working_dir)
                            .unwrap_or(path)
                            .to_string_lossy()
                            .to_lowercase()
                    } else {
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| s.to_lowercase())
                            .unwrap_or_default()
                    };
                    haystack.contains(&query)
                })
                .map(|(i, _)| i)
                .collect()
        };
        self.file_picker_selected = 0;
        self.file_picker_scroll_offset = 0;
    }

    /// Handle keys in file picker mode
    pub(crate) async fn handle_file_picker_key(
        &mut self,
        event: crossterm::event::KeyEvent,
    ) -> Result<()> {
        use super::events::keys;
        use crossterm::event::KeyCode;

        let filtered_len = self.file_picker_filtered.len();

        if keys::is_cancel(&event) {
            self.file_picker_search.clear();
            self.switch_mode(AppMode::Chat).await?;
        } else if keys::is_up(&event) {
            self.file_picker_selected = self.file_picker_selected.saturating_sub(1);
            if self.file_picker_selected < self.file_picker_scroll_offset {
                self.file_picker_scroll_offset = self.file_picker_selected;
            }
        } else if keys::is_down(&event) {
            if self.file_picker_selected + 1 < filtered_len {
                self.file_picker_selected += 1;
                let visible_items = 20;
                if self.file_picker_selected >= self.file_picker_scroll_offset + visible_items {
                    self.file_picker_scroll_offset = self.file_picker_selected - visible_items + 1;
                }
            }
        } else if keys::is_enter(&event) || keys::is_tab(&event) {
            // Resolve filtered index to actual file index
            if let Some(&file_idx) = self.file_picker_filtered.get(self.file_picker_selected)
                && let Some(selected_path) = self.file_picker_files.get(file_idx).cloned()
            {
                if selected_path.is_dir() {
                    if selected_path.ends_with("..") {
                        if let Some(parent) = self.file_picker_current_dir.parent() {
                            self.file_picker_current_dir = parent.to_path_buf();
                        }
                    } else {
                        self.file_picker_current_dir = selected_path;
                    }
                    self.file_picker_search.clear();
                    self.refresh_file_picker().await?;
                } else {
                    let path_str = selected_path.to_string_lossy().to_string();
                    self.input_buffer
                        .insert_str(self.cursor_position, &path_str);
                    self.cursor_position += path_str.len();
                    self.file_picker_search.clear();
                    self.switch_mode(AppMode::Chat).await?;
                }
            }
        } else if event.code == KeyCode::Backspace {
            if self.file_picker_search.pop().is_some() {
                self.sync_file_picker_source();
                self.apply_file_picker_filter();
            }
        } else if let KeyCode::Char(c) = event.code {
            self.file_picker_search.push(c);
            self.sync_file_picker_source();
            self.apply_file_picker_filter();
        }

        Ok(())
    }

    /// Open directory picker (reuses file picker state, dirs only)
    /// Whether a path's final component is a dotfile (hidden entry). The
    /// navigate-up `..` entry is never treated as hidden.
    fn is_hidden_entry(path: &std::path::Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.') && n != ".." && n != ".")
            .unwrap_or(false)
    }

    pub(crate) async fn open_directory_picker(&mut self) -> Result<()> {
        let mut files = Vec::new();

        // Add parent directory option if not at root
        if self.file_picker_current_dir.parent().is_some() {
            files.push(self.file_picker_current_dir.join(".."));
        }

        // Read directory entries — directories only. Dotfile dirs are hidden
        // unless the user toggled them on (`.` key), matching Finder's
        // cmd/ctrl+shift+. behaviour (a terminal app can't capture that combo).
        if let Ok(entries) = std::fs::read_dir(&self.file_picker_current_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if !self.file_picker_show_hidden && Self::is_hidden_entry(&path) {
                    continue;
                }
                files.push(path);
            }
        }

        // Sort alphabetically
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        self.file_picker_files = files;
        self.file_picker_selected = 0;
        self.file_picker_scroll_offset = 0;
        self.switch_mode(AppMode::DirectoryPicker).await?;

        Ok(())
    }

    /// Handle keys in directory picker mode
    pub(crate) async fn handle_directory_picker_key(
        &mut self,
        event: crossterm::event::KeyEvent,
    ) -> Result<()> {
        use super::events::keys;
        use crossterm::event::KeyCode;

        if keys::is_cancel(&event) {
            self.switch_mode(AppMode::Chat).await?;
        } else if keys::is_up(&event) {
            self.file_picker_selected = self.file_picker_selected.saturating_sub(1);
            if self.file_picker_selected < self.file_picker_scroll_offset {
                self.file_picker_scroll_offset = self.file_picker_selected;
            }
        } else if keys::is_down(&event) {
            if self.file_picker_selected + 1 < self.file_picker_files.len() {
                self.file_picker_selected += 1;
                let visible_items = 20;
                if self.file_picker_selected >= self.file_picker_scroll_offset + visible_items {
                    self.file_picker_scroll_offset = self.file_picker_selected - visible_items + 1;
                }
            }
        } else if keys::is_enter(&event) {
            // Enter navigates into directory
            if let Some(selected_path) = self
                .file_picker_files
                .get(self.file_picker_selected)
                .cloned()
            {
                if selected_path.ends_with("..") {
                    if let Some(parent) = self.file_picker_current_dir.parent() {
                        self.file_picker_current_dir = parent.to_path_buf();
                    }
                } else {
                    self.file_picker_current_dir = selected_path;
                }
                self.open_directory_picker().await?;
            }
        } else if matches!(event.code, KeyCode::Char('.') | KeyCode::Char('>')) {
            // Toggle hidden (dotfile) directories. The Finder shortcut
            // cmd/ctrl+shift+. can't reach a terminal app, so `.` is the
            // reliable mnemonic; `>` covers shift+. on US layouts.
            self.file_picker_show_hidden = !self.file_picker_show_hidden;
            self.open_directory_picker().await?;
        } else if event.code == KeyCode::Tab || event.code == KeyCode::Char(' ') {
            // Tab/Space selects the current directory as working dir
            let selected_dir = self.file_picker_current_dir.clone();
            self.apply_working_directory_change(&selected_dir).await;

            self.switch_mode(AppMode::Chat).await?;
        }

        Ok(())
    }

    /// Open the usage dashboard — fetch data and populate state
    pub(crate) async fn open_usage_dashboard(&mut self) {
        use crate::usage::dashboard::DashboardState;
        use crate::usage::data::{DashboardData, Period};

        let period = self
            .dashboard_state
            .as_ref()
            .map(|s| s.period)
            .unwrap_or(Period::AllTime);

        let data = if let Some(pool) = crate::db::global_pool() {
            DashboardData::fetch(pool, period).await.unwrap_or_default()
        } else {
            DashboardData::default()
        };

        self.dashboard_state = Some(DashboardState {
            period,
            focused_card: 0,
            data,
            cache_detail_open: false,
        });
    }

    /// Update dashboard period and re-fetch data
    pub(crate) async fn set_dashboard_period(&mut self, period: crate::usage::data::Period) {
        if let Some(ds) = &mut self.dashboard_state
            && ds.set_period(period)
            && let Some(pool) = crate::db::global_pool()
            && let Ok(data) = crate::usage::data::DashboardData::fetch(pool, period).await
        {
            ds.data = data;
        }
    }
}

/// Directory for downloaded helper binaries (whispercrabs, …). Deliberately
/// NOT the brain dir (`~/.opencrabs`), which is for agent state — executables
/// don't belong there. Uses `~/.local/bin` on Unix (PATH-friendly and
/// user-writable) and the platform data dir on Windows.
pub(crate) fn helper_bin_dir() -> PathBuf {
    #[cfg(windows)]
    {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("opencrabs")
            .join("bin")
    }
    #[cfg(not(windows))]
    {
        dirs::home_dir()
            .map(|h| h.join(".local").join("bin"))
            .unwrap_or_else(std::env::temp_dir)
    }
}

/// File name of the helper binary on this platform.
pub(crate) const WHISPERCRABS_BIN: &str = if cfg!(windows) {
    "whispercrabs.exe"
} else {
    "whispercrabs"
};

/// Sidecar recording which release is installed, next to the binary (#2114).
///
/// The binary carries no version string of its own (`strings` returns none),
/// so the installed tag is unrecoverable after the fact without this file. Its
/// mtime doubles as the last-check time for the re-check TTL.
const WHISPERCRABS_TAG_FILE: &str = "whispercrabs.version";

/// How long a positive "you are current" answer is trusted before asking
/// GitHub again.
///
/// `/whisper` runs from a dialog, so a network round trip must not sit in
/// front of every launch (#2114 triage).
const WHISPERCRABS_RECHECK_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Timeout for the release lookup. A slow or blocked API must not stall the
/// dialog; the cached binary is used instead.
const WHISPERCRABS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Should the cached binary be replaced? Pure: no network, no filesystem.
///
/// `path_ok` is [`helper_binary_is_usable`]'s answer, so a missing, empty or
/// non-executable file always refreshes even when the tag matches.
pub(crate) fn helper_needs_refresh(
    cached_tag: Option<&str>,
    latest_tag: &str,
    path_ok: bool,
) -> bool {
    if !path_ok {
        return true;
    }
    match cached_tag {
        // No sidecar means the installed release is unknown, so it cannot be
        // trusted to be the latest one.
        None => true,
        Some(cached) => cached.trim() != latest_tag.trim(),
    }
}

/// Is there a usable executable at `path`?
///
/// `exists()` is not enough: a killed download leaves a zero-byte file, and a
/// file without the exec bit fails at `Command::new` with a cryptic error and
/// no self-heal (#2114).
pub(crate) fn helper_binary_is_usable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

/// Is a network re-check due? Pure.
///
/// `last_check` is the sidecar's mtime: the last time the installed tag was
/// confirmed against GitHub.
pub(crate) fn helper_refresh_is_due(
    last_check: Option<SystemTime>,
    now: SystemTime,
    ttl: Duration,
) -> bool {
    let Some(last) = last_check else {
        return true;
    };
    match now.duration_since(last) {
        // A future timestamp (clock skew, a restored backup) is not a reason
        // to skip the check.
        Err(_) => true,
        Ok(age) => age >= ttl,
    }
}

/// The tag recorded beside the binary, if any.
pub(crate) fn read_installed_tag(tag_path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(tag_path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Record the installed tag beside the binary.
///
/// Rewriting the same content is also how the re-check TTL is stamped, since
/// the sidecar's mtime is the last-check time.
pub(crate) fn write_installed_tag(tag_path: &Path, tag: &str) -> std::io::Result<()> {
    let tmp = tag_path.with_extension("version.tmp");
    std::fs::write(&tmp, format!("{}\n", tag.trim()))?;
    #[cfg(windows)]
    {
        // Windows refuses to rename onto an existing path.
        if tag_path.exists() {
            let _ = std::fs::remove_file(tag_path);
        }
    }
    std::fs::rename(&tmp, tag_path)
}

/// Bring a binary left in the brain dir (`~/.opencrabs/bin`) into the install
/// dir, once (#2114).
///
/// The old code returned the legacy path in place, so two install locations
/// lived forever and neither was versioned. The legacy file is copied, never
/// deleted: it belongs to the user. The tag is deliberately cleared, because
/// the legacy file's release is unknown and the next run must re-download.
pub(crate) fn migrate_legacy_whispercrabs(
    legacy: &Path,
    binary_path: &Path,
    tag_path: &Path,
) -> Result<()> {
    if !legacy.exists() || helper_binary_is_usable(binary_path) {
        return Ok(());
    }
    if !helper_binary_is_usable(legacy) {
        // A zero-byte or non-executable legacy file is not worth migrating.
        return Ok(());
    }
    std::fs::copy(legacy, binary_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(binary_path, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = std::fs::remove_file(tag_path);
    tracing::info!(
        from = %legacy.display(),
        to = %binary_path.display(),
        "WhisperCrabs: migrated the legacy helper binary"
    );
    Ok(())
}

/// Extract a downloaded archive into `staging`.
///
/// The format comes from the bytes, not the platform, so the decision is
/// testable without threading the OS through. Everything in the archive lands
/// in `staging`: the Windows zip ships DLLs beside the exe and moving only the
/// exe would leave it unable to start.
///
/// Windows zips were never extracted at all before #2114; only the tar.gz
/// branch ran, so a Windows install could never produce a binary.
pub(crate) fn extract_whispercrabs_archive(bytes: &[u8], staging: &Path) -> Result<()> {
    std::fs::create_dir_all(staging)?;
    if bytes.starts_with(b"PK\x03\x04") {
        extract_zip(bytes, staging)?;
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        extract_tar_gz(bytes, staging)?;
    } else {
        anyhow::bail!("download is neither a zip nor a tar.gz archive");
    }
    if !staging.join(WHISPERCRABS_BIN).is_file() {
        anyhow::bail!(
            "archive did not contain {} at its root, so the release layout changed",
            WHISPERCRABS_BIN
        );
    }
    Ok(())
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| anyhow::anyhow!("cannot read zip: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| anyhow::anyhow!("zip entry {i}: {e}"))?;
        // The entry name comes from the archive, so it is untrusted: this
        // rejects absolute paths and `..` escapes.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut file)?;
    }
    Ok(())
}

fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    archive
        .unpack(dest)
        .map_err(|e| anyhow::anyhow!("cannot extract tar.gz: {e}"))?;
    Ok(())
}

/// Move an extracted staging tree into the install directory.
///
/// The binary moves LAST, so an interrupted install can leave new DLLs beside
/// an old exe (harmless, the next run re-downloads) but never a new exe beside
/// missing ones.
pub(crate) fn install_staged_helpers(staging: &Path, bin_dir: &Path) -> Result<()> {
    let binary = staging.join(WHISPERCRABS_BIN);
    let mut others = Vec::new();
    for entry in std::fs::read_dir(staging)? {
        let path = entry?.path();
        if path.is_file() && path != binary {
            others.push(path);
        }
    }
    for src in others {
        if let Some(name) = src.file_name() {
            replace_file(&src, &bin_dir.join(name))?;
        }
    }
    replace_file(&binary, &bin_dir.join(WHISPERCRABS_BIN))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            bin_dir.join(WHISPERCRABS_BIN),
            std::fs::Permissions::from_mode(0o755),
        )?;
    }
    Ok(())
}

/// Rename over the target, removing it first where the platform refuses to
/// replace an existing file.
fn replace_file(src: &Path, dst: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        if dst.exists() {
            std::fs::remove_file(dst)?;
        }
    }
    std::fs::rename(src, dst)?;
    Ok(())
}

/// The OS component of a WhisperCrabs release asset name.
fn whispercrabs_platform_os() -> Result<&'static str> {
    match std::env::consts::OS {
        "linux" => Ok("linux"),
        "macos" => Ok("macos"),
        "windows" => Ok("windows"),
        other => anyhow::bail!("Unsupported OS: {other}"),
    }
}

/// Latest WhisperCrabs release, with the dialog-friendly timeout.
async fn fetch_latest_whispercrabs_release(client: &reqwest::Client) -> Result<serde_json::Value> {
    let release: serde_json::Value = client
        .get("https://api.github.com/repos/adolfousier/whispercrabs/releases/latest")
        .header("User-Agent", "opencrabs")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(release)
}

/// Return the path to a current WhisperCrabs binary, downloading it when the
/// cached copy is missing, unusable or out of date (#2114).
///
/// The old version returned on `path.exists()` alone, so a cached binary was
/// never refreshed: the first release after the one a user installed would
/// have stranded them forever, with no API call and no message.
pub(crate) async fn ensure_whispercrabs() -> Result<PathBuf> {
    let bin_dir = helper_bin_dir();
    std::fs::create_dir_all(&bin_dir)?;

    let binary_path = bin_dir.join(WHISPERCRABS_BIN);
    let tag_path = bin_dir.join(WHISPERCRABS_TAG_FILE);

    // An older version may have a copy in the brain dir (`~/.opencrabs/bin`).
    // It is copied in once, so one install location remains.
    let legacy = crate::config::opencrabs_home()
        .join("bin")
        .join(WHISPERCRABS_BIN);
    migrate_legacy_whispercrabs(&legacy, &binary_path, &tag_path)?;

    let cached_tag = read_installed_tag(&tag_path);
    let path_ok = helper_binary_is_usable(&binary_path);
    let last_check = std::fs::metadata(&tag_path).and_then(|m| m.modified()).ok();

    // A recent positive answer is trusted: /whisper must feel local.
    if path_ok
        && cached_tag.is_some()
        && !helper_refresh_is_due(last_check, SystemTime::now(), WHISPERCRABS_RECHECK_AFTER)
    {
        return Ok(binary_path);
    }

    let client = reqwest::Client::builder()
        .timeout(WHISPERCRABS_LOOKUP_TIMEOUT)
        .build()?;

    let release = match fetch_latest_whispercrabs_release(&client).await {
        Ok(release) => release,
        Err(e) => {
            // Offline is not an install failure: a usable cache still works.
            // Only a machine with no working binary has to hear about it.
            if path_ok {
                tracing::warn!(
                    error = %e,
                    "WhisperCrabs: release API unreachable, using the cached binary"
                );
                return Ok(binary_path);
            }
            return Err(
                e.context("WhisperCrabs release lookup failed and no usable binary is cached")
            );
        }
    };

    let latest_tag = release["tag_name"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if latest_tag.is_empty() {
        anyhow::bail!("WhisperCrabs release API returned no tag name");
    }

    if !helper_needs_refresh(cached_tag.as_deref(), &latest_tag, path_ok) {
        // Stamp the check time; the sidecar's mtime is the TTL anchor.
        let _ = write_installed_tag(&tag_path, &latest_tag);
        return Ok(binary_path);
    }

    let os_name = whispercrabs_platform_os()?;
    let pattern = format!("whispercrabs-{}-{}", os_name, std::env::consts::ARCH);
    let download_url = release["assets"]
        .as_array()
        .and_then(|assets| {
            assets
                .iter()
                .find(|a| a["name"].as_str().is_some_and(|n| n.contains(&pattern)))
        })
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or_else(|| anyhow::anyhow!("No WhisperCrabs release asset matching '{pattern}'"))?
        .to_string();

    let bytes = client
        .get(&download_url)
        .header("User-Agent", "opencrabs")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    // Staging lives inside `bin_dir` so the final move is a same-filesystem
    // rename, and a failed extract never touches the working binary.
    let staging = bin_dir.join(format!(".whispercrabs-staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let extracted = extract_whispercrabs_archive(&bytes, &staging);
    if let Err(e) = extracted {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let installed = install_staged_helpers(&staging, &bin_dir);
    let _ = std::fs::remove_dir_all(&staging);
    installed?;

    write_installed_tag(&tag_path, &latest_tag)?;

    if !helper_binary_is_usable(&binary_path) {
        anyhow::bail!(
            "WhisperCrabs was downloaded but {} is not a usable executable",
            binary_path.display()
        );
    }
    tracing::info!(tag = %latest_tag, path = %binary_path.display(), "WhisperCrabs: installed");
    Ok(binary_path)
}

struct TelegramTestResult {
    /// Auto-detected user ID from getUpdates (set when user_id was empty)
    detected_user_id: Option<String>,
}

/// Test Telegram connection: validate token, auto-detect user ID, send greeting.
#[cfg(feature = "telegram")]
async fn test_telegram_connection(
    token: &str,
    user_id_str: &str,
    agent: std::sync::Arc<crate::brain::agent::AgentService>,
) -> Result<TelegramTestResult, String> {
    use teloxide::prelude::Requester;

    // Step 1: Validate the bot token with getMe
    let bot = teloxide::Bot::new(token);
    let me = bot.get_me().await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("Unauthorized") || msg.contains("401") {
            "Invalid bot token. Make sure you copied the full token from @BotFather.".to_string()
        } else if msg.contains("Forbidden") || msg.contains("403") {
            "Telegram rejected this token (Forbidden). Check you copied it correctly.".to_string()
        } else if msg.contains("Not Found") || msg.contains("404") {
            "Token not recognized by Telegram. It should look like 123456789:ABCdef...".to_string()
        } else {
            format!("Failed to verify bot token: {}", msg)
        }
    })?;

    tracing::info!(
        "Telegram bot token validated: @{}",
        me.username.as_deref().unwrap_or_default()
    );

    // Step 2: Resolve user ID — auto-detect via getUpdates if empty
    let trimmed = user_id_str.trim();
    let user_id: i64 = if trimmed.is_empty() {
        // Auto-detect: call getUpdates to find the most recent user who messaged the bot
        match bot.get_updates().await {
            Ok(updates) => {
                // Find the most recent message from a non-bot user
                let detected = updates.iter().rev().find_map(|u| {
                    if let teloxide::types::UpdateKind::Message(ref m) = u.kind {
                        if !m.from.as_ref().is_some_and(|f| f.is_bot) {
                            Some(m.chat.id.0)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });
                match detected {
                    Some(id) => {
                        tracing::info!("Telegram: auto-detected user ID {} from getUpdates", id);
                        id
                    }
                    None => {
                        return Err(
                            "No messages found for this bot yet. Message your bot on                              Telegram first (send any text), then retry. Your chat ID                              will be auto-detected."
                                .to_string(),
                        );
                    }
                }
            }
            Err(e) => {
                return Err(format!(
                    "Could not check for messages (getUpdates failed: {}).                      Paste your numeric chat ID manually.                      Message @userinfobot on Telegram to get it.",
                    e
                ));
            }
        }
    } else {
        trimmed
            .parse()
            .map_err(|_| format!("Invalid chat ID '{}': must be a numeric ID.", trimmed))?
    };

    // Reject the bot's own numeric ID
    if me.id.0 as i64 == user_id {
        return Err(
            "That's the bot's own ID, not yours. Open Telegram, message              @userinfobot, and paste the numeric ID it replies with."
                .to_string(),
        );
    }

    // Step 3: Send greeting
    let greeting = crate::channels::generate_connection_greeting(&agent, "Telegram").await;
    // Review F10: the onboarding greeting was the audit's named MISSING
    // site — no tracing on any path. Errors go to the wizard UI; the
    // telemetry line makes the send visible in logs too.
    let greeting_len = greeting.len();
    let greeting_hash8 = crate::channels::telegram::telemetry::content_hash8(greeting.as_str());
    bot.send_message(teloxide::types::ChatId(user_id), greeting)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("chat not found") {
                format!(
                    "Chat ID {} not found. You must message your bot first                      so it can reply to you. Open Telegram, find @{},                      send it any message, then retry.",
                    user_id,
                    me.username
                        .as_deref()
                        .unwrap_or("your_bot")
                )
            } else if msg.contains("bot was blocked") {
                "You blocked the bot. Unblock it in Telegram and retry.".to_string()
            } else {
                format!("Telegram API error: {}", msg)
            }
        })?;
    crate::channels::telegram::telemetry::log_send_success(
        "system",
        "onboarding_greeting",
        "-",
        "owner_dm",
        "send",
        user_id,
        None,
        0, // message id not captured by the wizard; chat correlation suffices
        greeting_len,
        &greeting_hash8,
    );

    // Return detected user ID if we auto-detected it
    let detected_user_id = if trimmed.is_empty() {
        Some(user_id.to_string())
    } else {
        None
    };

    Ok(TelegramTestResult { detected_user_id })
}

#[cfg(not(feature = "telegram"))]
async fn test_telegram_connection(
    _token: &str,
    _user_id_str: &str,
    _agent: std::sync::Arc<crate::brain::agent::AgentService>,
) -> Result<(), String> {
    Err("Telegram feature not enabled".to_string())
}

/// Test Discord connection by sending a message to a channel.
#[cfg(feature = "discord")]
async fn test_discord_connection(
    token: &str,
    channel_id_str: &str,
    agent: std::sync::Arc<crate::brain::agent::AgentService>,
) -> Result<(), String> {
    let channel_id: u64 = channel_id_str
        .parse()
        .map_err(|_| format!("Invalid channel ID: {}", channel_id_str))?;
    let greeting = crate::channels::generate_connection_greeting(&agent, "Discord").await;
    let http = serenity::http::Http::new(token);
    let channel = serenity::model::id::ChannelId::new(channel_id);
    channel
        .say(&http, greeting)
        .await
        .map_err(|e| format!("Discord API error: {}", e))?;
    Ok(())
}

#[cfg(not(feature = "discord"))]
async fn test_discord_connection(
    _token: &str,
    _channel_id_str: &str,
    _agent: std::sync::Arc<crate::brain::agent::AgentService>,
) -> Result<(), String> {
    Err("Discord feature not enabled".to_string())
}

/// Test Slack connection by posting a message to a channel.
#[cfg(feature = "slack")]
async fn test_slack_connection(
    token: &str,
    channel_id: &str,
    agent: std::sync::Arc<crate::brain::agent::AgentService>,
) -> Result<(), String> {
    use slack_morphism::prelude::*;

    let greeting = crate::channels::generate_connection_greeting(&agent, "Slack").await;
    let client = SlackClient::new(
        SlackClientHyperConnector::new().map_err(|e| format!("Slack client error: {}", e))?,
    );
    let api_token = SlackApiToken::new(SlackApiTokenValue::from(token.to_string()));
    let session = client.open_session(&api_token);
    let request = SlackApiChatPostMessageRequest::new(
        SlackChannelId::new(channel_id.to_string()),
        SlackMessageContent::new().with_text(greeting),
    );
    session
        .chat_post_message(&request)
        .await
        .map_err(|e| format!("Slack API error: {}", e))?;
    Ok(())
}

#[cfg(not(feature = "slack"))]
async fn test_slack_connection(
    _token: &str,
    _channel_id: &str,
    _agent: std::sync::Arc<crate::brain::agent::AgentService>,
) -> Result<(), String> {
    Err("Slack feature not enabled".to_string())
}

/// Test WhatsApp connection by sending a message using the paired bot's client.
#[cfg(feature = "whatsapp")]
async fn test_whatsapp_connection(
    wa_state: std::sync::Arc<crate::channels::whatsapp::WhatsAppState>,
    phone: &str,
    agent: std::sync::Arc<crate::brain::agent::AgentService>,
) -> Result<(), String> {
    // Wait until WhatsApp is actually CONNECTED, not just until a client object
    // exists. The client is stored on Event::Connected but the socket can drop
    // right after (keepalive churn), so a stored-but-disconnected client makes
    // send_message fail with "client is not connected". Gate on is_connected()
    // so the test only sends on a live link.
    let client = {
        let mut found = None;
        for _ in 0..40 {
            if wa_state.is_connected().await
                && let Some(c) = wa_state.client().await
            {
                found = Some(c);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        found.ok_or_else(|| {
            "WhatsApp did not reach a connected state (20s). Scan the QR code and \
             wait for the link to finish, then retry."
                .to_string()
        })?
    };

    // Send the round-trip greeting to the PAIRED account's OWN self-chat — the
    // only target a delivery is guaranteed for. The typed `phone` (an allowed
    // contact) may be a third party the bot must not greet unsolicited, so it is
    // only a fallback when the paired number isn't known yet. The paired number
    // is captured at PairSuccess into owner_jid.
    let target = match wa_state.owner_jid().await {
        Some(jid) => jid
            .split('@')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .trim_start_matches('+')
            .to_string(),
        None => phone.trim_start_matches('+').to_string(),
    };
    if target.is_empty() {
        return Err(
            "Could not determine the paired WhatsApp number to test against. \
             Re-scan the QR code."
                .to_string(),
        );
    }

    let jid_str = format!("{}@s.whatsapp.net", target);
    let jid: wacore_binary::jid::Jid = jid_str
        .parse()
        .map_err(|e| format!("Invalid phone number format: {}", e))?;

    let greeting = crate::channels::generate_connection_greeting(&agent, "WhatsApp").await;
    let wa_msg = waproto::whatsapp::Message {
        conversation: Some(format!(
            "{}\n\n{}",
            crate::channels::whatsapp::handler::MSG_HEADER,
            greeting
        )),
        ..Default::default()
    };

    // Subscribe BEFORE sending so the delivery receipt can't be missed.
    let mut delivered_rx = wa_state.subscribe_delivered();
    // Send, retrying once if the socket briefly dropped between connect and send
    // (the client can still report "not connected" mid-reconnect). On retry,
    // re-fetch the client because a reconnect installs a fresh one.
    let mut send_res = client.send_message(jid.clone(), wa_msg.clone()).await;
    if send_res
        .as_ref()
        .err()
        .is_some_and(|e| e.to_string().contains("not connected"))
    {
        let mut reconnected = None;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if wa_state.is_connected().await {
                reconnected = wa_state.client().await;
                break;
            }
        }
        if let Some(c) = reconnected {
            send_res = c.send_message(jid, wa_msg).await;
        }
    }
    let sent = send_res.map_err(|e| format!("WhatsApp send error: {}", e))?;

    // `send_message` returning Ok only means the stanza was transmitted — the
    // server can still reject it asynchronously (error 400, e.g. when a
    // recipient device session can't be established). Confirm the message was
    // actually DELIVERED by waiting for its delivery receipt; otherwise report
    // failure so onboarding never shows success for a message that never landed.
    let target_id = sent.message_id;
    let wait_for_delivery = async {
        loop {
            match delivered_rx.recv().await {
                Ok(id) if id == target_id => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(20), wait_for_delivery).await {
        Ok(true) => Ok(()),
        _ => Err(
            "Message was sent but never confirmed delivered by WhatsApp \
                  (no delivery receipt). It may have been rejected — check that \
                  the device is linked and reachable."
                .to_string(),
        ),
    }
}

#[cfg(feature = "trello")]
async fn test_trello_connection(api_key: &str, api_token: &str) -> Result<(), String> {
    let client = crate::channels::trello::TrelloClient::new(api_key, api_token);
    client
        .get_member_me()
        .await
        .map(|_me| ())
        .map_err(|e| format!("Trello API error: {}", e))
}

#[cfg(not(feature = "trello"))]
async fn test_trello_connection(_api_key: &str, _api_token: &str) -> Result<(), String> {
    Err("Trello feature not enabled".to_string())
}

/// What a first run says when the built-in provider could not come online.
///
/// The reason is the proxy's own, so the user gets "rate limited" or
/// "unreachable" instead of a shrug. The second line is the way out: the
/// wizard is one command away, and the session is alive either way.
fn first_run_offline_message(
    err: &crate::brain::provider::opencrabs_install::InstallError,
) -> String {
    use crate::brain::provider::opencrabs_install::install_error_message;
    format!(
        "Could not bring the built-in brain online: {}\n\
         No drama, the crab is fine. Type /onboard to pick another provider, or fix \
         the connection and restart.",
        install_error_message(err)
    )
}
