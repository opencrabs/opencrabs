//! Compatibility exports for provider and model selection.
//!
//! State and live request behavior live in `ui_core::model_selector`.

pub use crate::ui_core::model_selector::{
    CUSTOM_INSTANCES_START, CUSTOM_PROVIDER_IDX, EXISTING_KEY_SENTINEL, PROVIDERS, ProviderInfo,
    ProviderSelectorState, index_of_provider,
};

/// Map an API model id to a human-readable display label.
/// Returns the id itself when no special label is defined.
/// Used by /models, onboarding, and the footer to show friendly names
/// for models whose API id is an opaque alias (e.g. qwen-oauth's `coder-model`).
pub fn model_display_label(model_id: &str) -> &str {
    match model_id {
        "qwen-3.7-max" | "qwen3.7-max" | "qwen-latest-series" | "qwen-latest-series-invite" => {
            "Qwen 3.7 Max"
        }
        "qwen-3.7-plus" | "qwen3.7-plus" | "qwen-3.7-plus-preview" => "Qwen 3.7 Plus",
        "qwen-3.6-max-preview" | "qwen3.6-max-preview" => "Qwen 3.6 Max Preview",
        "coder-model" | "qwen-3.6-plus" | "qwen3.6-plus" => "Qwen 3.6 Plus",
        "qwen-3.5-plus" | "qwen3.5-plus" => "Qwen 3.5 Plus",
        "minimax-m2.5" => "Minimax M2.5",
        "minimax-m2.7" => "Minimax M2.7",
        "minimax-m3" => "Minimax M3",
        "mimo-v2-omni" | "mimo-v2-omni-free" => "Mimo V2 Omni",
        "mimo-v2-pro" | "mimo-v2-pro-free" => "Mimo V2 Pro",
        "kimi-k2.6" => "Kimi K2.6",
        "kimi-k2.5" | "kimi-k2-5" => "Kimi K2.5",
        "glm-5.1" => "GLM 5.1",
        "glm-5-turbo" => "GLM 5 Turbo",
        // Bare Claude CLI aliases are a MOVING pointer to the newest release of
        // that family (the CLI documents them as "an alias for the latest
        // model"), so they must not be labelled with a fixed version — `fable`
        // and `fable-5` both rendering "Fable 5" put the same label on two rows
        // of the picker (#754). Label them as the pointer they are.
        "fable" => "Fable (latest)",
        "opus" => "Opus (latest)",
        "sonnet" => "Sonnet (latest)",
        "haiku" => "Haiku (latest)",
        "fable-5" => "Fable 5",
        "opus-4-8" => "Opus 4.8",
        "opus-4-7" => "Opus 4.7",
        "opus-4-6" => "Opus 4.6",
        "sonnet-4-6" => "Sonnet 4.6",
        "haiku-4-5" => "Haiku 4.5",
        other => prettify_claude_cli_model(other).unwrap_or(other),
    }
}

/// Fallback prettifier for Claude CLI shorthand models we haven't
/// hardcoded yet. Matches `opus-X-Y` / `sonnet-X-Y` / `haiku-X-Y` and
/// returns "Opus X.Y", "Sonnet X.Y", "Haiku X.Y". The `&'static str`
/// return matches the main match arm; per-id strings are leaked into
/// a process-wide cache (one slot per distinct model id ever observed)
/// so the same id never re-leaks.
fn prettify_claude_cli_model(model: &str) -> Option<&'static str> {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    static PRETTIFIED: LazyLock<Mutex<HashMap<String, &'static str>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    let (family, rest) = if let Some(r) = model.strip_prefix("opus-") {
        ("Opus", r)
    } else if let Some(r) = model.strip_prefix("sonnet-") {
        ("Sonnet", r)
    } else if let Some(r) = model.strip_prefix("fable-") {
        ("Fable", r)
    } else {
        let r = model.strip_prefix("haiku-")?;
        ("Haiku", r)
    };
    // Versions come in two shapes: `4-8` (major-minor) and `5` (major only).
    // Requiring the `-` meant a major-only release rendered as the raw id —
    // "opus-5" sat un-formatted right below "Opus 4.8" (#754).
    let version = match rest.split_once('-') {
        Some((major, minor)) => {
            if major.is_empty()
                || minor.is_empty()
                || !major.chars().all(|c| c.is_ascii_digit())
                || !minor.chars().all(|c| c.is_ascii_digit())
            {
                return None;
            }
            format!("{major}.{minor}")
        }
        None => {
            if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            rest.to_string()
        }
    };

    let mut cache = PRETTIFIED.lock().ok()?;
    if let Some(existing) = cache.get(model) {
        return Some(existing);
    }
    let pretty: &'static str = Box::leak(format!("{family} {version}").into_boxed_str());
    cache.insert(model.to_string(), pretty);
    Some(pretty)
}

pub use crate::ui_core::model_selector::load_default_models;
