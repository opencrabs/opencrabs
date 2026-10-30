use crate::brain::Provider;
use crate::brain::provider::custom_openai_compatible::{
    OpenAIProvider, is_token_field_mismatch, uses_max_completion_tokens,
};

// ── Provider creation ────────────────────────────────────────────

#[test]
fn openai_provider_creation() {
    let provider = OpenAIProvider::new("test-key".to_string());
    assert_eq!(provider.name(), "openai");
}

#[test]
fn local_provider_creation() {
    let provider = OpenAIProvider::local("http://localhost:1234/v1/chat/completions".to_string());
    assert_eq!(provider.name(), "openai-compatible");
}

#[test]
fn supported_models_include_gpt4() {
    let provider = OpenAIProvider::new("test-key".to_string());
    let models = provider.supported_models();
    assert!(models.contains(&"gpt-4".to_string()));
    assert!(models.contains(&"gpt-3.5-turbo".to_string()));
}

#[test]
fn context_window_known_models() {
    let provider = OpenAIProvider::new("test-key".to_string());
    assert_eq!(provider.context_window("gpt-4"), Some(8_192));
    assert_eq!(
        provider.context_window("gpt-4-turbo-preview"),
        Some(128_000)
    );
    assert_eq!(provider.context_window("gpt-4o"), Some(128_000));
    assert_eq!(provider.context_window("gpt-4o-mini"), Some(128_000));
    assert_eq!(provider.context_window("gpt-4.1"), Some(1_047_576));
    assert_eq!(provider.context_window("gpt-4.1-mini"), Some(1_047_576));
    assert_eq!(provider.context_window("gpt-5"), Some(1_047_576));
    assert_eq!(provider.context_window("gpt-5-mini"), Some(1_047_576));
    assert_eq!(provider.context_window("gpt-6"), Some(1_047_576));
    assert_eq!(provider.context_window("gpt-6-mini"), Some(1_047_576));
    assert_eq!(provider.context_window("o1-mini"), Some(200_000));
    assert_eq!(provider.context_window("o3-mini"), Some(200_000));
    assert_eq!(provider.context_window("o4-mini"), Some(200_000));
}

#[test]
fn context_window_unknown_returns_none() {
    let provider = OpenAIProvider::new("test-key".to_string());
    assert_eq!(provider.context_window("unknown"), None);
}

#[test]
fn calculate_cost_gpt5_nano() {
    let provider = OpenAIProvider::new("test-key".to_string());
    let cost = provider.calculate_cost("gpt-5-nano", 1000, 1000);
    assert!(
        (cost - 0.0005).abs() < 0.0001,
        "expected ~0.0005 but got {cost}"
    );
}

// ── max_tokens vs max_completion_tokens routing ──────────────────

#[test]
fn gpt41_uses_max_completion_tokens() {
    assert!(uses_max_completion_tokens("gpt-4.1-mini"));
    assert!(uses_max_completion_tokens("gpt-4.1-nano"));
    assert!(uses_max_completion_tokens("gpt-4.1"));
}

#[test]
fn gpt5_uses_max_completion_tokens() {
    assert!(uses_max_completion_tokens("gpt-5-mini"));
    assert!(uses_max_completion_tokens("gpt-5"));
}

#[test]
fn gpt6_uses_max_completion_tokens() {
    assert!(uses_max_completion_tokens("gpt-6"));
    assert!(uses_max_completion_tokens("gpt-6-mini"));
}

#[test]
fn o_series_uses_max_completion_tokens() {
    assert!(uses_max_completion_tokens("o1-mini"));
    assert!(uses_max_completion_tokens("o1-preview"));
    assert!(uses_max_completion_tokens("o3-mini"));
    assert!(uses_max_completion_tokens("o4-mini"));
}

#[test]
fn older_models_use_max_tokens() {
    assert!(!uses_max_completion_tokens("gpt-4o"));
    assert!(!uses_max_completion_tokens("gpt-4o-mini"));
    assert!(!uses_max_completion_tokens("gpt-3.5-turbo"));
    assert!(!uses_max_completion_tokens("gpt-4-turbo"));
}

// ── Token field mismatch detection (fallback retry) ──────────────

#[test]
fn detects_max_tokens_unsupported_error() {
    assert!(is_token_field_mismatch(
        "Unsupported parameter: 'max_tokens' is not supported with this model"
    ));
}

#[test]
fn detects_max_completion_tokens_unsupported_error() {
    assert!(is_token_field_mismatch(
        "Unsupported parameter: 'max_completion_tokens' is not supported"
    ));
}

#[test]
fn detects_scaleway_style_cap_wording() {
    // #1059: names the field but never says "unsupported", so the original
    // matcher missed it and the swap-and-retry never fired.
    assert!(is_token_field_mismatch(
        "payload validation: max_completion_tokens is limited to 16384 for qwen3.5-397b-a17b"
    ));
}

#[test]
fn detects_an_explicit_instruction_to_use_the_other_field() {
    assert!(is_token_field_mismatch(
        "'max_tokens' is not supported; use max_completion_tokens instead"
    ));
}

#[test]
fn a_rejection_signal_alone_is_not_enough() {
    // The field name has to be present too, or any capped parameter would
    // trigger a pointless token-field swap.
    assert!(!is_token_field_mismatch(
        "temperature is limited to 2.0 for this model"
    ));
    assert!(!is_token_field_mismatch("top_p is not supported"));
}

#[test]
fn the_field_name_alone_is_not_enough() {
    // A message that merely mentions the field, with no sign of rejection,
    // must not send us into a retry.
    assert!(!is_token_field_mismatch(
        "request accepted with max_tokens=4096"
    ));
}

#[test]
fn ignores_unrelated_errors() {
    assert!(!is_token_field_mismatch("Rate limit exceeded"));
    assert!(!is_token_field_mismatch("Invalid API key"));
    assert!(!is_token_field_mismatch("model not found"));
}

// ── Extra headers (GitHub Copilot uses this) ──────────────────────

#[test]
fn extra_headers_builder() {
    let provider = OpenAIProvider::new("key".to_string())
        .with_extra_headers(vec![("X-Custom".to_string(), "value".to_string())]);
    assert_eq!(provider.extra_headers.len(), 1);
    assert_eq!(provider.extra_headers[0].0, "X-Custom");
}

#[test]
fn with_name_changes_provider_name() {
    let provider = OpenAIProvider::new("key".to_string()).with_name("GitHub Copilot");
    assert_eq!(provider.name(), "GitHub Copilot");
}

#[test]
fn with_base_url_changes_provider() {
    let provider =
        OpenAIProvider::with_base_url("key".to_string(), "https://example.com/v1/chat".to_string());
    assert_eq!(provider.name(), "openai-compatible");
}
