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
mod wizard;

// Re-export all public types
pub use types::{
    AuthField, BrainField, CHANNEL_NAMES, ChannelTestStatus, CodexDeviceFlowStatus, DiscordField,
    EXISTING_KEY_SENTINEL, GitHubDeviceFlowStatus, HealthStatus, ImageField, OnboardingStep,
    PROVIDERS, ProviderInfo, SlackField, SttProvider, TEMPLATE_FILES, TelegramField, TrelloField,
    TtsProvider, VoiceField, WhatsAppField, WizardAction, WizardMode,
};

pub use setup_gaps::SetupGap;
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

/// System prompt sent once after the user completes first-time onboarding.
/// Starts with `[SYSTEM:` so it's hidden from the user display but processed
/// by the agent. The agent uses its personality (SOUL.md) and user knowledge
/// (USER.md) to craft a personalized surprise instead of a hardcoded greeting.
pub const WELCOME_MESSAGE: &str = "[SYSTEM: Onboarding complete. Check your environment, use load_config to see what's configured and tool_search to discover what you have. You know who your user is (USER.md) and you have your personality (SOUL.md). Now do something that'll make them go 'holy shit'. Not a boring greeting. Not 'hey how can I help'. Something only YOU would say, like your best friend just walked in. Be creative, be unexpected. Then ask if they want daily check-ins, task reminders, or heartbeat, you can set those up with cron_manage right now.]";

/// Hidden system prompt for a quick start. The LLM is already set by the time
/// this runs, so it is never asked about. `{provider}` and `{model}` are filled
/// per send from the session row in the database, so the greeting names the
/// model that is actually serving this session. `{gaps}` lists what the first
/// chat still has to ask about (see `setup_gaps`). SOUL.md stays as shipped
/// unless the user asks to change it.
pub const WELCOME_QUICK_START_MESSAGE: &str = "[SYSTEM: Setup just finished. This session runs on the {provider} provider with model {model}, so the LLM is already set and you never ask about it. Quick start kept the defaults: workspace ~/.opencrabs, SOUL.md as seeded, daemon off. Still missing: {gaps}. Greet them in your own voice from SOUL.md, short and a little playful, and say the brain is online on {model}. Then ask only about what is missing. For the profile fields, ask one or two questions at a time, never a form, and save each answer to USER.md with write_opencrabs_file using operation=append. Keep entries short and factual. For speech to text, text to speech and image generation, ask if they want one set up now, and configure only what they say yes to. If nothing is missing, skip the questions and ask what they want to work on. Do not rewrite SOUL.md unless they explicitly ask you to change your personality. If they want another provider, tell them to type /models, or run /onboard and pick Advanced.]";

/// Build the quick-start welcome for the provider and model serving this session.
pub fn quick_start_welcome(provider: &str, model: &str, gaps: &[SetupGap]) -> String {
    WELCOME_QUICK_START_MESSAGE
        .replace("{provider}", provider)
        .replace("{model}", model)
        .replace("{gaps}", &setup_gaps::render_gaps(gaps))
}

/// Pick the hidden welcome prompt for the flow the user just finished. The
/// quick-start prompt is filled with the provider, model and remaining gaps.
pub fn welcome_message_for(
    mode: WizardMode,
    provider: &str,
    model: &str,
    gaps: &[SetupGap],
) -> String {
    match mode {
        WizardMode::QuickStart => quick_start_welcome(provider, model, gaps),
        WizardMode::Advanced => WELCOME_MESSAGE.to_string(),
    }
}
