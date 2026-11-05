use crossterm::event::{KeyCode, KeyEvent};

use super::types::*;
use super::wizard::OnboardingWizard;

impl OnboardingWizard {
    pub(super) fn handle_voice_setup_key(&mut self, event: KeyEvent) -> WizardAction {
        super::voice::handle_key(self, event)
    }

    pub(super) fn handle_image_setup_key(&mut self, event: KeyEvent) -> WizardAction {
        let either_enabled = self.image_vision_enabled || self.image_generation_enabled;

        match self.image_field {
            ImageField::VisionToggle => match event.code {
                KeyCode::Char(' ') | KeyCode::Up | KeyCode::Down => {
                    self.image_vision_enabled = !self.image_vision_enabled;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    self.image_field = ImageField::GenerationToggle;
                }
                _ => {}
            },
            ImageField::GenerationToggle => match event.code {
                KeyCode::Char(' ') | KeyCode::Up | KeyCode::Down => {
                    self.image_generation_enabled = !self.image_generation_enabled;
                }
                KeyCode::BackTab => {
                    self.image_field = ImageField::VisionToggle;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    if self.image_generation_enabled {
                        self.image_field = ImageField::GenerationModel;
                    } else if either_enabled {
                        self.image_field = ImageField::ApiKey;
                    } else {
                        self.next_step();
                    }
                }
                _ => {}
            },
            ImageField::GenerationModel => match event.code {
                KeyCode::Char(c) => {
                    self.image_generation_model_input.push(c);
                }
                KeyCode::Backspace => {
                    self.image_generation_model_input.pop();
                }
                KeyCode::BackTab => {
                    self.image_field = ImageField::GenerationToggle;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    self.image_field = ImageField::ApiKey;
                }
                _ => {}
            },
            ImageField::ApiKey => match event.code {
                KeyCode::Char(c) => {
                    if self.has_existing_image_key() {
                        self.image_api_key_input.clear();
                    }
                    self.image_api_key_input.push(c);
                }
                KeyCode::Backspace => {
                    if self.has_existing_image_key() {
                        self.image_api_key_input.clear();
                    } else {
                        self.image_api_key_input.pop();
                    }
                }
                KeyCode::BackTab => {
                    // Skip back over GenerationModel only when generation
                    // is enabled — otherwise it never got navigated to.
                    self.image_field = if self.image_generation_enabled {
                        ImageField::GenerationModel
                    } else {
                        ImageField::GenerationToggle
                    };
                }
                KeyCode::Enter => {
                    self.next_step();
                }
                _ => {}
            },
        }
        WizardAction::None
    }

    pub(super) fn handle_daemon_key(&mut self, event: KeyEvent) -> WizardAction {
        match event.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Char(' ') => {
                self.install_daemon = !self.install_daemon;
            }
            KeyCode::Enter => {
                self.next_step();
            }
            _ => {}
        }
        WizardAction::None
    }

    pub(super) fn handle_health_check_key(&mut self, event: KeyEvent) -> WizardAction {
        match event.code {
            KeyCode::Enter if self.quick_jump && self.health_complete => {
                // Re-run checks on Enter after complete
                self.start_health_check();
            }
            KeyCode::Enter if self.health_complete => {
                self.next_step();
                return WizardAction::None;
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.start_health_check();
            }
            _ => {}
        }
        WizardAction::None
    }
}

/// Whether onboarding still needs to run.
///
/// The answer comes from a recorded fact — did the user reach the end of the
/// wizard — not from inspecting the config. Those are different questions, and
/// answering the second one hid the first: a CLI provider needs no API key, so
/// a single `enabled = true` left by a partial run made onboarding disappear
/// while nothing else had been set, dropping a first-time user into a chat
/// with no usable pair (#919).
///
/// To re-run the wizard after finishing, use `opencrabs onboard`, `--onboard`,
/// or `/onboard`.
pub fn is_first_time() -> bool {
    let state = super::state::OnboardingState::load();
    if state.completed {
        tracing::debug!("[is_first_time] onboarding recorded as completed");
        return false;
    }

    // Installs that predate the progress file have no marker, and nagging
    // someone who genuinely finished would be its own bug. Decide once, from
    // the config, then record it so this never runs again.
    if !super::state::OnboardingState::path().exists() && setup_looks_finished() {
        tracing::info!(
            "[is_first_time] no progress file but setup is complete — recording it and skipping onboarding"
        );
        super::state::OnboardingState::mark_completed();
        return false;
    }

    tracing::debug!("[is_first_time] onboarding not completed, wizard will run");
    true
}

/// One-time migration test for installs that predate the progress file.
///
/// Deliberately stricter than the check it replaces. "A provider is enabled"
/// was the old bar and is exactly what let an unconfigured CLI provider pass;
/// a finished setup also has a model chosen for that provider. Anything short
/// of that gets the wizard, which is the recoverable direction to be wrong in.
fn setup_looks_finished() -> bool {
    let config_path = crate::config::opencrabs_home().join("config.toml");
    if !config_path.exists() {
        return false;
    }
    let config = match crate::config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("[is_first_time] config did not load ({e}) — treating as unfinished");
            return false;
        }
    };
    // active_provider_and_model walks provider_registry + customs, so it can't
    // drift out of sync with the provider list the way a hardcoded OR-chain
    // did (an enabled Xiaomi used to loop back into onboarding forever).
    let (provider, model) = config.providers.active_provider_and_model();
    let finished = provider != "none" && model != "none" && model != "(default)";
    tracing::debug!("[is_first_time] migration check: {provider}/{model} finished={finished}");
    finished
}
