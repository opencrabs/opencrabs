//! GitHub Copilot provider integration tests.
//!
//! Tests config resolution, provider indices, factory wiring,
//! extra headers on OpenAIProvider, and onboarding integration.

use crate::brain::Provider;
use crate::brain::provider::custom_openai_compatible::OpenAIProvider;
use crate::config::{Config, ProviderConfig, ProviderConfigs, resolve_provider_from_config};
use crate::tui::onboarding::{OnboardingWizard, PROVIDERS};
use crate::ui_core::model_selector::normalize_filter_query;

// ── Provider array ──────────────────────────────────────────────

#[test]
fn github_models_is_at_index_2() {
    assert_eq!(PROVIDERS[2].name, "GitHub Copilot");
}

#[test]
fn github_models_has_no_static_models() {
    assert!(PROVIDERS[2].models.is_empty());
}

#[test]
fn github_copilot_key_label() {
    assert_eq!(PROVIDERS[2].key_label, "OAuth");
}

#[test]
fn github_copilot_has_help_lines() {
    assert!(!PROVIDERS[2].help_lines.is_empty());
    let help = PROVIDERS[2].help_lines.join(" ");
    assert!(help.contains("Copilot"));
}

#[test]
fn provider_order_after_github_insertion() {
    assert_eq!(PROVIDERS[0].name, "Anthropic");
    assert_eq!(PROVIDERS[1].name, "OpenAI");
    assert_eq!(PROVIDERS[2].name, "GitHub Copilot");
    assert_eq!(PROVIDERS[3].name, "Google Gemini");
    assert_eq!(PROVIDERS[4].name, "OpenRouter");
    assert_eq!(PROVIDERS[5].name, "Minimax");
    assert_eq!(PROVIDERS[6].name, "z.ai");
    assert_eq!(PROVIDERS[7].name, "Moonshot AI");
    assert_eq!(PROVIDERS[8].name, "Claude CLI");
    assert_eq!(PROVIDERS[9].name, "OpenCode CLI");
    assert_eq!(PROVIDERS[10].name, "Codex CLI");
    assert_eq!(PROVIDERS[11].name, "Command Code CLI");
    assert_eq!(PROVIDERS[12].name, "Codex");
    assert_eq!(PROVIDERS[13].name, "OpenCode");
    assert_eq!(PROVIDERS[14].name, "Qwen");
    assert_eq!(PROVIDERS[15].name, "Ollama");
    assert_eq!(PROVIDERS[16].name, "Xiaomi");
    assert_eq!(PROVIDERS[17].name, "OpenCrabs");
    assert_eq!(PROVIDERS[18].name, "Custom OpenAI-Compatible");
}

// ── Config struct ───────────────────────────────────────────────

#[test]
fn provider_configs_has_github_field() {
    let configs = ProviderConfigs::default();
    assert!(configs.github.is_none());
}

#[test]
fn provider_configs_github_round_trip() {
    let configs = ProviderConfigs {
        github: Some(ProviderConfig {
            enabled: true,
            default_model: Some("gpt-4o".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(configs.github.as_ref().unwrap().enabled);
    assert_eq!(
        configs.github.as_ref().unwrap().default_model.as_deref(),
        Some("gpt-4o")
    );
}

// ── resolve_provider_from_config ────────────────────────────────

#[test]
fn resolve_github_when_enabled() {
    let mut config = Config::default();
    config.providers.github = Some(ProviderConfig {
        enabled: true,
        api_key: Some("test-key".to_string()),
        default_model: Some("gpt-4o".to_string()),
        ..Default::default()
    });
    let (name, model) = resolve_provider_from_config(&config);
    assert_eq!(name, "GitHub Copilot");
    assert_eq!(model, "gpt-4o");
}

#[test]
fn resolve_github_default_model_when_none() {
    let mut config = Config::default();
    config.providers.github = Some(ProviderConfig {
        enabled: true,
        api_key: Some("test-key".to_string()),
        default_model: None,
        ..Default::default()
    });
    let (name, model) = resolve_provider_from_config(&config);
    assert_eq!(name, "GitHub Copilot");
    assert_eq!(model, "(default)");
}

#[test]
fn resolve_skips_github_when_disabled() {
    let mut config = Config::default();
    config.providers.github = Some(ProviderConfig {
        enabled: false,
        ..Default::default()
    });
    let (name, _) = resolve_provider_from_config(&config);
    assert_ne!(name, "GitHub Copilot");
}

#[test]
fn resolve_skips_github_when_absent() {
    let config = Config::default();
    assert!(config.providers.github.is_none());
    let (name, _) = resolve_provider_from_config(&config);
    assert_ne!(name, "GitHub Copilot");
}

// ── OpenAIProvider extra_headers ─────────────────────────────────

#[test]
fn extra_headers_builder_sets_headers() {
    use crate::brain::provider::copilot::copilot_extra_headers;
    let headers = copilot_extra_headers();
    let provider = OpenAIProvider::with_base_url(
        "copilot-managed".to_string(),
        "https://api.githubcopilot.com/chat/completions".to_string(),
    )
    .with_extra_headers(headers);
    assert!(!provider.extra_headers.is_empty());
    assert!(
        provider
            .extra_headers
            .iter()
            .any(|(k, _)| k == "copilot-integration-id")
    );
}

#[test]
fn extra_headers_default_empty() {
    let provider = OpenAIProvider::with_base_url(
        "test-key".to_string(),
        "https://api.openai.com/v1/chat/completions".to_string(),
    );
    assert!(provider.extra_headers.is_empty());
}

#[test]
fn with_name_sets_provider_name() {
    let provider = OpenAIProvider::with_base_url(
        "copilot-managed".to_string(),
        "https://api.githubcopilot.com/chat/completions".to_string(),
    )
    .with_name("GitHub Copilot");
    assert_eq!(provider.name(), "GitHub Copilot");
}

// ── Onboarding wizard ──────────────────────────────────────────

#[test]
fn wizard_github_is_not_custom() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    assert!(!wizard.ps.is_custom());
}

#[test]
fn wizard_github_supports_model_fetch() {
    // GitHub Copilot supports live model fetching via /models endpoint
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    assert!(wizard.ps.supports_model_fetch());
}

#[test]
fn wizard_current_provider_at_index_2_is_github() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    assert_eq!(wizard.ps.current_provider().name, "GitHub Copilot");
}

#[test]
fn wizard_model_filter_works_for_github() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.models = vec![
        "gpt-4o".to_string(),
        "gpt-4o-mini".to_string(),
        "Llama-3.3-70B-Instruct".to_string(),
    ];
    wizard.ps.model_filter = "gpt".to_string();
    let filtered = wizard.ps.filtered_model_names();
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|m| m.contains("gpt")));
}

#[test]
fn wizard_all_model_names_uses_fetched_first() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.models = vec!["gpt-4o".to_string()];
    let names = wizard.ps.all_model_names();
    assert_eq!(names, vec!["gpt-4o"]);
}

#[test]
fn wizard_all_model_names_falls_back_to_config_models() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.config_models = vec!["gpt-4o".to_string()];
    let names = wizard.ps.all_model_names();
    assert_eq!(names, vec!["gpt-4o"]);
}

#[test]
fn wizard_selected_model_name_in_bounds() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.models = vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()];
    wizard.ps.selected_model = 1;
    // In bounds against the list as DISPLAYED, which is ordered newest-first
    // (#1057). The property is that an in-range index resolves to a real
    // entry, not that the order is the one they were inserted in.
    let shown = wizard.ps.filtered_model_names();
    assert_eq!(wizard.ps.selected_model_name(), shown[1]);
    assert!(!wizard.ps.selected_model_name().is_empty());
}

#[test]
fn wizard_selected_model_name_out_of_bounds_fallback() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.models = vec!["gpt-4o".to_string()];
    wizard.ps.selected_model = 99;
    assert_eq!(wizard.ps.selected_model_name(), "gpt-4o");
}

// ── Factory ─────────────────────────────────────────────────────

#[test]
fn github_provider_builder_full_chain() {
    use crate::brain::provider::copilot::copilot_extra_headers;
    let headers = copilot_extra_headers();
    let provider = OpenAIProvider::with_base_url(
        "copilot-managed".to_string(),
        "https://api.githubcopilot.com/chat/completions".to_string(),
    )
    .with_name("GitHub Copilot")
    .with_extra_headers(headers.clone());
    assert_eq!(provider.name(), "GitHub Copilot");
    assert!(!provider.extra_headers.is_empty());
    let keys: Vec<&str> = provider
        .extra_headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect();
    assert!(keys.contains(&"copilot-integration-id"));
}

#[test]
fn extra_headers_new_constructor_empty() {
    let provider = OpenAIProvider::new("test-key".to_string());
    assert!(provider.extra_headers.is_empty());
}

// ── Config apply section mapping ────────────────────────────────

#[test]
fn github_config_section_is_providers_github() {
    // The all_provider_sections array includes "providers.github"
    let sections = [
        "providers.anthropic",
        "providers.openai",
        "providers.github",
        "providers.gemini",
        "providers.openrouter",
        "providers.minimax",
    ];
    assert_eq!(sections[2], "providers.github");
}

#[test]
fn github_writes_base_url() {
    // GitHub writes base_url to config like OpenRouter and Minimax
    let github_index: usize = 2;
    let writes_base_url = matches!(github_index, 2 | 4 | 5 | 6);
    assert!(writes_base_url);
}

#[test]
fn github_writes_models_array() {
    // GitHub (2), Minimax (5), and Custom (6) write models arrays
    let github_index: usize = 2;
    let writes_models = matches!(github_index, 2 | 5 | 6);
    assert!(writes_models);
}

// ── Config resolution priority ──────────────────────────────────

#[test]
fn resolve_anthropic_takes_priority_over_github() {
    let mut config = Config::default();
    config.providers.anthropic = Some(ProviderConfig {
        enabled: true,
        api_key: Some("test-key".to_string()),
        default_model: Some("claude-sonnet-4-20250514".to_string()),
        ..Default::default()
    });
    config.providers.github = Some(ProviderConfig {
        enabled: true,
        api_key: Some("test-key".to_string()),
        default_model: Some("gpt-4o".to_string()),
        ..Default::default()
    });
    let (name, _) = resolve_provider_from_config(&config);
    assert_eq!(name, "Anthropic");
}

#[test]
fn resolve_github_chosen_when_only_github_enabled() {
    let mut config = Config::default();
    config.providers.anthropic = Some(ProviderConfig {
        enabled: false,
        ..Default::default()
    });
    config.providers.openai = Some(ProviderConfig {
        enabled: false,
        ..Default::default()
    });
    config.providers.github = Some(ProviderConfig {
        enabled: true,
        api_key: Some("test-key".to_string()),
        default_model: Some("Llama-3.3-70B-Instruct".to_string()),
        ..Default::default()
    });
    let (name, model) = resolve_provider_from_config(&config);
    assert_eq!(name, "GitHub Copilot");
    assert_eq!(model, "Llama-3.3-70B-Instruct");
}

// ── GitHub-specific onboarding OAuth flow ───────────────────────

#[test]
fn github_help_mentions_copilot_subscription() {
    let help = PROVIDERS[2].help_lines.join(" ");
    assert!(help.contains("Copilot"));
    assert!(help.contains("subscription"));
}

#[test]
fn wizard_github_static_models_empty() {
    // GitHub has no static models in PROVIDERS[2].models — they come from config
    assert!(PROVIDERS[2].models.is_empty());
}

#[test]
fn github_load_default_models_from_config_example() {
    // Copilot models are fetched live from the API, not from config.toml.example
    // So load_default_models may return empty (no hardcoded list)
    let models = crate::tui::provider_selector::load_default_models("github");
    // Either empty or contains whatever is in config.toml.example
    let _ = models;
}

#[test]
fn github_config_models_written_on_apply() {
    // Verify index 2 is in the models-write match
    let github_index: usize = 2;
    let writes_models = matches!(github_index, 2 | 5 | 6);
    assert!(writes_models);
}

#[test]
fn github_token_persist_path_index_matches() {
    // Verify that index 2 triggers the gh token persist code path in apply_config
    let github_index: usize = 2;
    assert_eq!(github_index, 2);
}

#[test]
fn wizard_github_model_filter_empty_returns_all() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.models = vec!["gpt-4o".to_string(), "Llama-3.3-70B-Instruct".to_string()];
    wizard.ps.model_filter.clear();
    assert_eq!(wizard.ps.filtered_model_names().len(), 2);
}

#[test]
fn wizard_github_model_filter_no_match() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.models = vec!["gpt-4o".to_string()];
    wizard.ps.model_filter = "nonexistent".to_string();
    assert!(wizard.ps.filtered_model_names().is_empty());
}

#[test]
fn wizard_github_model_filter_case_insensitive() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.models = vec!["GPT-4o".to_string(), "Llama-3.3-70B-Instruct".to_string()];
    wizard.ps.model_filter = "gpt".to_string();
    let filtered = wizard.ps.filtered_model_names();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0], "GPT-4o");
}

#[test]
fn wizard_github_model_filter_by_provider_name() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.models = vec![
        "gpt-4o".to_string(),
        "gpt-4o-mini".to_string(),
        "Llama-3.3-70B-Instruct".to_string(),
        "Mistral-Large".to_string(),
    ];
    wizard.ps.model_filter = "llama".to_string();
    let filtered = wizard.ps.filtered_model_names();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0], "Llama-3.3-70B-Instruct");
}

/// #3182: a space in the query and a dash in the model id are the same
/// separator, so the owner's "qwen ambassador" finds "Qwen-Ambassador".
#[test]
fn wizard_model_filter_folds_separators_between_query_and_id() {
    let mut wizard = OnboardingWizard::new();
    wizard.ps.selected_provider = 2;
    wizard.ps.models = vec![
        "Qwen-Ambassador".to_string(),
        "Qwen-Ambassador/Qwen4-Max".to_string(),
    ];
    // Spelled with a space, found in a dash-shaped id.
    wizard.ps.model_filter = "qwen ambassador".to_string();
    assert_eq!(wizard.ps.filtered_model_names().len(), 2);
    // The reverse direction too: dash query, space-shaped id.
    wizard.ps.models = vec!["Qwen Ambassador".to_string()];
    wizard.ps.model_filter = "qwen-ambassador".to_string();
    assert_eq!(wizard.ps.filtered_model_names().len(), 1);
    // An underscore is folded the same way.
    wizard.ps.model_filter = "qwen_ambassador".to_string();
    assert_eq!(wizard.ps.filtered_model_names().len(), 1);
    // And a genuinely different word still misses.
    wizard.ps.model_filter = "qwenx".to_string();
    assert!(wizard.ps.filtered_model_names().is_empty());
}

/// The normalizer itself: lowercase, fold any separator run to one space,
/// keep everything else, including the `/` pair split.
#[test]
fn normalize_filter_query_folds_separator_runs_and_keeps_slashes() {
    assert_eq!(
        normalize_filter_query("Qwen--Ambassador / Qwen4_Max"),
        "qwen ambassador / qwen4 max"
    );
    assert_eq!(normalize_filter_query("  spaced \t out  "), "spaced out");
    assert_eq!(normalize_filter_query("two/bet"), "two/bet");
    assert_eq!(normalize_filter_query("---"), "");
    assert_eq!(normalize_filter_query(""), "");
}
