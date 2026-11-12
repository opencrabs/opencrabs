//! Onboarding Wizard
//!
//! A TUI-based onboarding wizard for first-time OpenCrabs users.
//! Handles mode selection, provider/auth setup, workspace, channels,
//! voice, image, daemon installation, health check and brain setup.

mod brain;
mod channels;
pub mod config;
pub mod deep_link;
mod fetch;
pub(crate) mod helpers;
mod input;
pub(crate) mod key_field;
mod keys;
mod models;
mod navigation;
pub mod setup_gaps;
pub mod state;
mod types;
pub mod voice;
pub(crate) mod voice_chain;
mod welcome;
mod wizard;

// Re-export all public types
pub use types::{
    AuthField, BrainField, CHANNEL_NAMES, ChannelTestStatus, CodexDeviceFlowStatus, DiscordField,
    EXISTING_KEY_SENTINEL, GitHubDeviceFlowStatus, HealthStatus, ImageField, OnboardingStep,
    PROVIDERS, ProviderInfo, SlackField, SttProvider, TEMPLATE_FILES, TelegramField, TrelloField,
    TtsProvider, VoiceField, WhatsAppField, WizardAction, WizardMode,
};

pub use setup_gaps::SetupGap;
pub use welcome::{
    WELCOME_MESSAGE, WELCOME_QUICK_START_MESSAGE, quick_start_welcome, welcome_message_for,
};
pub use wizard::OnboardingWizard;

pub use crate::ui_core::model_selector::fetch_provider_models;
#[cfg(test)]
pub(crate) use crate::ui_core::model_selector::{
    merge_minimax_baseline, opencode_models_url, xiaomi_baseline_models,
};
pub use brain::parse_brain_sections;
pub use fetch::is_first_time;
// build_onboarding_systemd_unit is used by tests via
// `crate::tui::onboarding::build_onboarding_systemd_unit`; `config` is private,
// so it needs the same cfg(test) bridge as `fetch` above. The path is `self::`
// because `config` is also an external crate dependency, which would make a
// bare `use config::…` ambiguous.
#[cfg(test)]
pub(crate) use self::config::build_onboarding_systemd_unit;
