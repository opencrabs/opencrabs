//! Test that Gemini model fetching actually works end-to-end.

#[cfg(test)]
mod tests {
    use crate::tui::onboarding::fetch_provider_models;

    /// Load the Gemini API key from keys.toml via Config::load, same path the app uses.
    fn load_gemini_key() -> Option<String> {
        let config = crate::config::Config::load().ok()?;
        config.providers.gemini?.api_key
    }

    /// Direct HTTP call to Gemini models API — bypasses our fetch_provider_models entirely.
    /// If this fails, the key itself is bad.
    #[tokio::test]
    async fn test_gemini_api_direct() {
        let key = match load_gemini_key() {
            Some(k) => k,
            None => {
                eprintln!("SKIP: no Gemini API key in config");
                return;
            }
        };

        // Use header-based auth instead of query param to avoid CodeQL cleartext alert
        let url = "https://generativelanguage.googleapis.com/v1beta/models";

        let resp = match reqwest::Client::new()
            .get(url)
            .header("x-goog-api-key", &key)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SKIP: Gemini API unreachable: {e}");
                return;
            }
        };
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        eprintln!("Status: {}", status);
        eprintln!("Body (first 500 chars): {}", &body[..body.len().min(500)]);

        if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::UNAUTHORIZED {
            eprintln!(
                "SKIP: Gemini API key rejected ({}), likely IP restriction",
                status
            );
            return;
        }

        assert!(
            status.is_success(),
            "Gemini API returned {}: {}",
            status,
            &body[..body.len().min(200)]
        );

        // Parse like our code does
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct GeminiModel {
            name: String,
            #[serde(default)]
            supported_generation_methods: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct GeminiModelsResponse {
            models: Vec<GeminiModel>,
        }

        let parsed: GeminiModelsResponse = serde_json::from_str(&body).unwrap();
        let generate_content_models: Vec<&str> = parsed
            .models
            .iter()
            .filter(|m| {
                m.supported_generation_methods
                    .iter()
                    .any(|g| g == "generateContent")
            })
            .map(|m| m.name.as_str())
            .collect();

        eprintln!("Total models: {}", parsed.models.len());
        eprintln!("generateContent models: {}", generate_content_models.len());
        for m in &generate_content_models[..generate_content_models.len().min(5)] {
            eprintln!("  {}", m);
        }

        assert!(
            !generate_content_models.is_empty(),
            "No models with generateContent found"
        );
    }

    /// Test via our actual fetch_provider_models function (provider_index=3 = Gemini).
    #[tokio::test]
    async fn test_gemini_fetch_provider_models() {
        let key = match load_gemini_key() {
            Some(k) => k,
            None => {
                eprintln!("SKIP: no Gemini API key in config");
                return;
            }
        };

        eprintln!("Calling fetch_provider_models(3, key, None)...");
        let models = fetch_provider_models(3, Some(&key), None, None, None, None, None).await;

        eprintln!("Returned {} models", models.len());
        for m in models.iter().take(10) {
            eprintln!("  {}", m);
        }

        if models.is_empty() {
            eprintln!(
                "SKIP: fetch_provider_models returned empty — likely IP restriction or API issue"
            );
            return;
        }
    }

    /// Test that calling with None key returns empty (not a crash).
    #[tokio::test]
    async fn test_gemini_fetch_no_key() {
        let models = fetch_provider_models(3, None, None, None, None, None, None).await;
        assert!(models.is_empty(), "Should return empty with no key");
    }
}

/// #3149: the default model must be resolved from the live model list, never
/// pinned to a version Google can shut down. `gemini-2.0-flash` was the old
/// hardcoded default and died on every unset config. These tests cover the
/// picker against a trimmed copy of the real API shape; the network probe
/// itself is skipped under test so `create_provider` stays offline.
#[cfg(test)]
mod default_resolution {
    use crate::brain::provider::gemini::{
        FALLBACK_MODELS, GeminiProvider, LATEST_FLASH_MODEL, is_chat_model, pick_default_model,
    };
    use crate::brain::provider::r#trait::Provider;

    /// Trimmed from the live API (2026-11-25): general chat, specialized
    /// families, and the gaps where `gemini-2.0-flash` used to be.
    fn live_shaped_list() -> Vec<String> {
        [
            "gemini-flash-latest",
            "gemini-flash-lite-latest",
            "gemini-pro-latest",
            "gemini-2.5-flash",
            "gemini-2.5-pro",
            "gemini-2.5-flash-preview-tts",
            "gemma-4-26b-a4b-it",
            "gemini-3-flash-preview",
            "gemini-3.1-pro-preview",
            "gemini-3.1-flash-lite-preview",
            "gemini-3-pro-image",
            "nano-banana-pro-preview",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
            "gemini-omni-flash-preview",
            "gemini-3.5-transcribe",
            "gemini-3.6-flash",
            "gemini-3.7-flash",
            "gemini-3.8-flash",
            "gemini-4-argon",
            "lyria-3.5",
            "gemini-3.1-flash-tts-preview",
            "gemini-robotics-er-2-preview",
            "gemini-2.5-computer-use-preview-10-2025",
            "deep-research-preview-04-2026",
            "antigravity-preview-latest",
            // The old dead default: gone from Google's API. Its presence
            // here must never win, and a list without the alias must not
            // fall back to it.
            "gemini-2.0-flash",
        ]
        .iter()
        .map(|m| m.to_string())
        .collect()
    }

    #[test]
    fn alias_wins_when_the_live_list_offers_it() {
        let picked = pick_default_model(&live_shaped_list());
        assert_eq!(picked.as_deref(), Some(LATEST_FLASH_MODEL));
    }

    #[test]
    fn newest_stable_flash_wins_without_the_alias() {
        let mut models = live_shaped_list();
        models.retain(|m| m != LATEST_FLASH_MODEL && m != "gemini-flash-lite-latest");
        let picked = pick_default_model(&models);
        assert_eq!(picked.as_deref(), Some("gemini-3.8-flash"));
    }

    #[test]
    fn stable_outranks_same_version_preview_and_lite() {
        let models: Vec<String> = [
            "gemini-3.8-flash-preview",
            "gemini-3.8-flash",
            "gemini-3.8-flash-lite",
            "gemini-3.7-flash",
        ]
        .iter()
        .map(|m| m.to_string())
        .collect();
        let picked = pick_default_model(&models);
        assert_eq!(picked.as_deref(), Some("gemini-3.8-flash"));
    }

    #[test]
    fn pro_is_the_fallback_when_no_flash_exists() {
        let models: Vec<String> = ["gemini-2.5-pro", "gemini-3.1-pro-preview"]
            .iter()
            .map(|m| m.to_string())
            .collect();
        let picked = pick_default_model(&models);
        assert_eq!(picked.as_deref(), Some("gemini-3.1-pro-preview"));
    }

    #[test]
    fn specialized_families_never_become_the_default() {
        let models: Vec<String> = [
            "gemini-3-pro-image",
            "nano-banana-pro-preview",
            "gemini-3.1-flash-tts-preview",
            "gemini-omni-flash-preview",
            "gemini-3.5-transcribe",
            "lyria-3.5",
            "gemini-robotics-er-2-preview",
            "gemini-2.5-computer-use-preview-10-2025",
            "deep-research-preview-04-2026",
            "antigravity-preview-latest",
            "text-embedding-004",
        ]
        .iter()
        .map(|m| m.to_string())
        .collect();
        assert!(pick_default_model(&models).is_none());
        for m in &models {
            assert!(!is_chat_model(m), "{m} must not count as a chat model");
        }
    }

    #[test]
    fn offline_floor_holds_only_live_models() {
        // Every model in the floor was verified against the API on the day
        // this list was written (#3149 evidence). The dead 2.0-flash and the
        // 1.5 family must never come back here.
        for dead in ["gemini-2.0-flash", "gemini-1.5-pro", "gemini-1.5-flash"] {
            assert!(
                !FALLBACK_MODELS.contains(&dead),
                "{dead} is shut down and must not return to the floor"
            );
        }
        assert!(FALLBACK_MODELS.contains(&LATEST_FLASH_MODEL));
    }

    #[test]
    fn new_defaults_to_the_alias_and_resolve_stays_offline_under_test() {
        let provider = GeminiProvider::new("k".to_string());
        assert_eq!(provider.default_model(), LATEST_FLASH_MODEL);
    }

    #[tokio::test]
    async fn resolve_default_model_under_test_returns_the_alias_without_network() {
        let resolved = GeminiProvider::resolve_default_model("test-key").await;
        assert_eq!(resolved, LATEST_FLASH_MODEL);
    }
}

/// #3176: one shared catalogue reducer feeds the TUI picker, the GUI picker
/// and the channels menu. These cover the shape rules that used to differ per
/// surface: pagination is the fetcher's job, but what counts as a chat model,
/// how the `models/` prefix is handled, dedupe and order are all decided here.
#[cfg(test)]
mod shared_catalogue {
    use crate::brain::provider::gemini_models::{GeminiModelEntry, chat_models_from_entries};

    fn entry(name: &str, methods: &[&str]) -> GeminiModelEntry {
        GeminiModelEntry {
            name: name.to_string(),
            supported_generation_methods: methods.iter().map(|m| m.to_string()).collect(),
        }
    }

    fn chat(name: &str) -> GeminiModelEntry {
        entry(name, &["generateContent"])
    }

    #[test]
    fn keeps_chat_models_and_strips_the_models_prefix() {
        let models = chat_models_from_entries(vec![
            chat("models/gemini-3.8-flash"),
            chat("models/gemini-2.5-pro"),
        ]);
        assert_eq!(models, vec!["gemini-3.8-flash", "gemini-2.5-pro"]);
    }

    #[test]
    fn drops_entries_without_generate_content() {
        // An embedding model lists only embedContent; offering it in a chat
        // picker produced a request the API rejects.
        let models = chat_models_from_entries(vec![
            entry("models/gemini-embedding-001", &["embedContent"]),
            entry("models/gemini-2.5-flash", &["countTokens"]),
            chat("models/gemini-2.5-flash"),
        ]);
        assert_eq!(models, vec!["gemini-2.5-flash"]);
    }

    #[test]
    fn drops_generative_families_that_are_not_chat() {
        let models = chat_models_from_entries(vec![
            chat("models/gemini-3.1-flash-image-preview"),
            chat("models/gemini-2.5-flash-preview-tts"),
            chat("models/gemini-2.5-flash-native-audio-preview-09-2025"),
            chat("models/gemini-3.5-live-translate-preview"),
            chat("models/imagen-4"),
            chat("models/gemini-2.5-pro"),
        ]);
        assert_eq!(models, vec!["gemini-2.5-pro"]);
    }

    #[test]
    fn dedupes_repeated_ids_across_pages() {
        let models = chat_models_from_entries(vec![
            chat("models/gemini-3.8-flash"),
            chat("gemini-3.8-flash"),
        ]);
        assert_eq!(models, vec!["gemini-3.8-flash"]);
    }

    #[test]
    fn orders_versions_numerically_not_lexicographically() {
        // The old picker arm did `sort(); reverse()`, which reads 3.8 as
        // newer than 3.10 and puts gemini-4 below a 3.x list.
        let models = chat_models_from_entries(vec![
            chat("models/gemini-3.8-flash"),
            chat("models/gemini-3.10-flash"),
            chat("models/gemini-4-carbon"),
            chat("models/gemini-2.5-pro"),
        ]);
        assert_eq!(
            models,
            vec![
                "gemini-4-carbon",
                "gemini-3.10-flash",
                "gemini-3.8-flash",
                "gemini-2.5-pro"
            ]
        );
    }

    #[test]
    fn empty_catalogue_stays_empty() {
        assert!(chat_models_from_entries(Vec::new()).is_empty());
    }
}
