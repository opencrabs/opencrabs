//! Provider model discovery shared by the TUI and GUI.
//!
//! This is the existing provider endpoint behavior moved out of the terminal.
//! Frontends supply a request snapshot and consume the same normalized result.

use super::{FetchOutcome, FetchRequest};

/// The `/models` URL for an OpenCode plan (#2040). Go is the default; `zen`
/// selects the Zen catalog. Each plan serves its own model list.
pub(crate) fn opencode_models_url(plan: Option<&str>) -> &'static str {
    if plan == Some("zen") {
        "https://opencode.ai/zen/v1/models"
    } else {
        "https://opencode.ai/zen/go/v1/models"
    }
}

pub async fn fetch_provider_models(
    provider_index: usize,
    api_key: Option<&str>,
    zhipu_endpoint_type: Option<&str>,
    xiaomi_endpoint_type: Option<&str>,
    moonshot_endpoint_type: Option<&str>,
    base_url: Option<&str>,
    opencode_endpoint_type: Option<&str>,
) -> Vec<String> {
    use crate::ui_core::model_selector::PROVIDERS;
    let provider_id = PROVIDERS.get(provider_index).map(|p| p.id).unwrap_or("");
    tracing::info!(
        "[fetch_provider_models] provider_index={}, provider_id={}, has_api_key={}",
        provider_index,
        provider_id,
        api_key.is_some(),
    );
    #[derive(serde::Deserialize)]
    struct ModelEntry {
        id: String,
        #[serde(default)]
        created: i64,
    }
    #[derive(serde::Deserialize)]
    struct ModelsResponse {
        data: Vec<ModelEntry>,
    }

    // Claude CLI - no /v1/models endpoint, so the list is DISCOVERED from the
    // installed binary (its `--help` aliases plus the versions it was seen to
    // resolve), with the built-in const as a floor (#753). Reading the const
    // directly here made this a third parallel list: a model released after
    // build time (Opus 5) never showed up in this dialog even after the menu
    // and provider were switched to discovery.
    if provider_id == "claude-cli" {
        return crate::brain::provider::claude_cli::available_models();
    }

    // OpenCode CLI - fetch models via `opencode models` command
    if provider_id == "opencode-cli" {
        return fetch_opencode_models().await;
    }

    // Command Code CLI - curated list mirroring `command-code --list-models`.
    if provider_id == "command-code-cli" {
        return crate::brain::provider::command_code_cli::SUPPORTED_MODELS
            .iter()
            .map(|s| s.to_string())
            .collect();
    }

    // Codex CLI & Codex OAuth - model list is curated; no /v1/models endpoint.
    if provider_id == "codex-cli" || provider_id == "codex" {
        let config_key = if provider_id == "codex" {
            "codex"
        } else {
            "codex-cli"
        };
        let models = crate::ui_core::model_selector::load_default_models(config_key);
        if !models.is_empty() {
            return models;
        }
        return vec![
            "gpt-5.5".to_string(),
            "gpt-5.4".to_string(),
            "gpt-5.4-mini".to_string(),
            "gpt-5.3-codex".to_string(),
            "gpt-5.3-codex-spark".to_string(),
            "gpt-5.2".to_string(),
        ];
    }

    // Qwen (DashScope): no /v1/models endpoint on the OpenAI-compat path,
    // so we read the curated list from config.toml.example. Users can
    // override via `models = [...]` in their own config.toml.
    if provider_id == "qwen" {
        let models = crate::ui_core::model_selector::load_default_models("qwen");
        if !models.is_empty() {
            return models;
        }
        return vec![
            "qwen3.6-plus".to_string(),
            "qwen3-max".to_string(),
            "qwen3-coder-plus".to_string(),
            "qwen3.5-plus".to_string(),
            "qwen-max".to_string(),
            "qwen-plus".to_string(),
            "qwen-flash".to_string(),
        ];
    }

    // MiniMax: fetch models live from {base_url}/models (api.minimax.io/v1
    // by default), merge with config models. Falls back to the binary's
    // baseline list when the request fails so the picker is never empty.
    if provider_id == "minimax" {
        let base_url = crate::config::Config::load()
            .ok()
            .and_then(|c| c.providers.minimax.clone())
            .and_then(|p| p.base_url)
            .unwrap_or_else(|| "https://api.minimax.io/v1".to_string());
        let models_url = format!("{}/models", base_url.trim_end_matches('/'));

        let client = reqwest::Client::new();
        let mut req = client.get(&models_url);
        if let Some(key) = api_key
            && !key.is_empty()
        {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let api_models: Vec<String> = match req.send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<ModelsResponse>().await {
                Ok(body) => {
                    let mut entries = body.data;
                    entries.sort_by_key(|e| std::cmp::Reverse(e.created));
                    entries.into_iter().map(|m| m.id).collect()
                }
                Err(e) => {
                    tracing::warn!("[fetch_provider_models] minimax parse error: {}", e);
                    Vec::new()
                }
            },
            Ok(resp) => {
                tracing::warn!(
                    "[fetch_provider_models] minimax /models HTTP {}",
                    resp.status()
                );
                Vec::new()
            }
            Err(e) => {
                tracing::warn!("[fetch_provider_models] minimax request failed: {}", e);
                Vec::new()
            }
        };

        let user_models = user_minimax_models();
        if api_models.is_empty() {
            // Live fetch failed - fall back to the binary baseline so
            // users on stale configs still see current releases.
            return merge_minimax_baseline(minimax_baseline_models(), user_models);
        }
        return merge_minimax_baseline(api_models, user_models);
    }

    // Xiaomi MiMo: fetch models live from /v1/models, merge with config models.
    // Two endpoints: api (default) and token-plan. Mirrors zhipu's live-fetch-then-merge.
    if provider_id == "xiaomi" {
        let endpoint_type = xiaomi_endpoint_type
            .map(|s| s.to_string())
            .or_else(|| {
                crate::config::Config::load()
                    .ok()
                    .and_then(|c| c.providers.xiaomi.clone())
                    .and_then(|p| p.endpoint_type)
            })
            .unwrap_or_else(|| "api".to_string());

        let base_url = match endpoint_type.as_str() {
            "token-plan" => "https://token-plan-ams.xiaomimimo.com/v1/models",
            _ => "https://api.xiaomimimo.com/v1/models",
        };

        let client = reqwest::Client::new();
        let mut req = client.get(base_url);
        if let Some(key) = api_key
            && !key.is_empty()
        {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let api_models: Vec<String> = match req.send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<ModelsResponse>().await {
                Ok(body) => {
                    let mut entries = body.data;
                    entries.sort_by_key(|e| std::cmp::Reverse(e.created));
                    entries.into_iter().map(|m| m.id).collect()
                }
                Err(e) => {
                    tracing::warn!("[fetch_provider_models] xiaomi parse error: {}", e);
                    Vec::new()
                }
            },
            Ok(resp) => {
                tracing::warn!(
                    "[fetch_provider_models] xiaomi /models HTTP {}",
                    resp.status()
                );
                Vec::new()
            }
            Err(e) => {
                tracing::warn!("[fetch_provider_models] xiaomi request failed: {}", e);
                Vec::new()
            }
        };

        let user_models = crate::config::Config::load()
            .ok()
            .and_then(|c| c.providers.xiaomi.clone())
            .map(|p| p.models)
            .unwrap_or_default();
        if api_models.is_empty() {
            // Live fetch failed (401 / offline) - fall back to the binary
            // baseline so the picker is never empty (#1419). Mirrors the
            // MiniMax fallback above.
            return merge_minimax_baseline(xiaomi_baseline_models(), user_models);
        }
        return merge_minimax_baseline(api_models, user_models);
    }

    let client = reqwest::Client::new();

    let result = match provider_id {
        "anthropic" => {
            // Anthropic - /v1/models is public
            let mut req = client
                .get("https://api.anthropic.com/v1/models")
                .header("anthropic-version", "2023-06-01");

            // Include key if available (may show more models)
            if let Some(key) = api_key {
                if key.starts_with("sk-ant-oat") {
                    req = req
                        .header("Authorization", format!("Bearer {}", key))
                        .header("anthropic-beta", "oauth-2025-04-20");
                } else if !key.is_empty() {
                    req = req.header("x-api-key", key);
                }
            }

            req.send().await
        }
        "openai" => {
            // OpenAI - /v1/models
            let mut req = client.get("https://api.openai.com/v1/models");
            if let Some(key) = api_key
                && !key.is_empty()
            {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
            req.send().await
        }
        "github" => {
            // GitHub Copilot - fetch from Copilot API using OAuth token
            if let Some(key) = api_key
                && !key.is_empty()
            {
                match crate::brain::provider::copilot::fetch_copilot_models(key).await {
                    Ok(models) if !models.is_empty() => return models,
                    Ok(_) => tracing::debug!("Copilot models endpoint returned empty list"),
                    Err(e) => tracing::debug!("Copilot models fetch failed: {}", e),
                }
            }
            // Fall back to config or defaults
            if let Ok(config) = crate::config::Config::load()
                && let Some(p) = &config.providers.github
            {
                if !p.models.is_empty() {
                    return p.models.clone();
                }
                if let Some(model) = &p.default_model {
                    return vec![model.clone()];
                }
            }
            return crate::ui_core::model_selector::load_default_models("github");
        }
        "gemini" => {
            // Google Gemini - list models via generativelanguage API
            let key = match api_key {
                Some(k) if !k.is_empty() => k,
                _ => {
                    tracing::warn!(
                        "[fetch_provider_models] Gemini: no API key provided, returning empty"
                    );
                    return Vec::new();
                }
            };
            tracing::info!("[fetch_provider_models] Gemini: fetching models (key present)");
            let url = "https://generativelanguage.googleapis.com/v1beta/models";
            // Gemini uses a different response shape: { models: [{ name: "models/gemini-..." }] }
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
            match client.get(url).header("x-goog-api-key", key).send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<GeminiModelsResponse>().await {
                        Ok(body) => {
                            let mut models: Vec<String> = body
                                .models
                                .into_iter()
                                .filter(|m| {
                                    m.supported_generation_methods
                                        .iter()
                                        .any(|g| g == "generateContent")
                                })
                                .map(|m| {
                                    m.name
                                        .strip_prefix("models/")
                                        .unwrap_or(&m.name)
                                        .to_string()
                                })
                                .collect();
                            models.sort();
                            models.reverse(); // Newest model versions first
                            tracing::info!(
                                "[fetch_provider_models] Gemini: fetched {} models",
                                models.len()
                            );
                            return models;
                        }
                        Err(e) => {
                            tracing::warn!("Gemini models parse error: {}", e);
                            return Vec::new();
                        }
                    }
                }
                Ok(resp) => {
                    tracing::warn!("Gemini models API returned {}", resp.status());
                    return Vec::new();
                }
                Err(e) => {
                    tracing::warn!("Gemini models fetch failed: {}", e);
                    return Vec::new();
                }
            }
        }
        "openrouter" => {
            // OpenRouter - /api/v1/models
            let mut req = client.get("https://openrouter.ai/api/v1/models");
            if let Some(key) = api_key
                && !key.is_empty()
            {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
            req.send().await
        }
        "opencode" => {
            // OpenCode API: each plan serves its own catalog (#2040).
            let mut req = client.get(opencode_models_url(opencode_endpoint_type));
            if let Some(key) = api_key
                && !key.is_empty()
            {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
            req.send().await
        }
        "zai" => {
            // z.ai: list from the same host and channel the chat URL
            // resolves to (#1350). Wizard state wins, then the saved
            // [providers.zai] (its base_url override included), then the
            // "api" default.
            let saved = crate::config::Config::load()
                .ok()
                .and_then(|c| c.providers.zai.clone());
            let endpoint_type = zhipu_endpoint_type
                .map(|s| s.to_string())
                .or_else(|| saved.as_ref().and_then(|p| p.endpoint_type.clone()))
                .unwrap_or_else(|| "api".to_string());
            let configured_base = base_url
                .map(str::to_string)
                .or_else(|| saved.as_ref().and_then(|p| p.base_url.clone()));

            let base = crate::brain::provider::zhipu_endpoint::models_url(
                configured_base.as_deref(),
                Some(endpoint_type.as_str()),
            );

            let mut req = client.get(&base);
            if let Some(key) = api_key
                && !key.is_empty()
            {
                req = req.header("Authorization", format!("Bearer {}", key));
            }

            // z.ai's /models is entitlement-gated: it returns only the models
            // the key can access, so a brand-new GLM the account hasn't been
            // granted (or one that lives on the other endpoint_type) is simply
            // absent. Merge in the user's config.toml [providers.zhipu].models
            // so a model can be pinned by exact id even when /models omits it -
            // the same fallback MiniMax already has. (merge_minimax_baseline is
            // a generic first-wins dedup merge despite the name.)
            let api_models: Vec<String> = match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<ModelsResponse>().await {
                        Ok(body) => {
                            // Newest first (created desc), matching the common
                            // fetch path. The earlier merge bypassed this sort and
                            // surfaced z.ai's raw oldest-first order, pushing new
                            // GLMs to the bottom (and out of the truncated list).
                            let mut entries = body.data;
                            entries.sort_by_key(|e| std::cmp::Reverse(e.created));
                            entries.into_iter().map(|m| m.id).collect()
                        }
                        Err(e) => {
                            tracing::warn!("[fetch_provider_models] zhipu parse error: {}", e);
                            Vec::new()
                        }
                    }
                }
                Ok(resp) => {
                    tracing::warn!(
                        "[fetch_provider_models] zhipu /models HTTP {}",
                        resp.status()
                    );
                    Vec::new()
                }
                Err(e) => {
                    tracing::warn!("[fetch_provider_models] zhipu request failed: {}", e);
                    Vec::new()
                }
            };
            let user_models = crate::config::Config::load()
                .ok()
                .and_then(|c| c.providers.zai.clone())
                .map(|p| p.models)
                .unwrap_or_default();
            return merge_minimax_baseline(api_models, user_models);
        }
        "moonshot" => {
            // Moonshot AI - /v1/models on api.moonshot.ai (API plan,
            // pay-per-token) or api.kimi.com/coding/v1 (Coding plan, token
            // subscription). Use passed endpoint_type (from wizard state),
            // fall back to config, then default "api".
            let endpoint_type = moonshot_endpoint_type
                .map(|s| s.to_string())
                .or_else(|| {
                    crate::config::Config::load()
                        .ok()
                        .and_then(|c| c.providers.moonshot.clone())
                        .and_then(|p| p.endpoint_type)
                })
                .unwrap_or_else(|| "api".to_string());

            let base = match endpoint_type.as_str() {
                "coding" => "https://api.kimi.com/coding/v1/models",
                _ => "https://api.moonshot.ai/v1/models",
            };

            let mut req = client.get(base);
            if let Some(key) = api_key
                && !key.is_empty()
            {
                req = req.header("Authorization", format!("Bearer {}", key));
            }

            // Same entitlement-gating caveat as z.ai: /models may omit models
            // the key cannot access, so merge the user's config.toml
            // [providers.moonshot].models as a pin-by-exact-id fallback.
            let api_models: Vec<String> = match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<ModelsResponse>().await {
                        Ok(body) => {
                            let mut entries = body.data;
                            entries.sort_by_key(|e| std::cmp::Reverse(e.created));
                            entries.into_iter().map(|m| m.id).collect()
                        }
                        Err(e) => {
                            tracing::warn!("[fetch_provider_models] moonshot parse error: {}", e);
                            Vec::new()
                        }
                    }
                }
                Ok(resp) => {
                    tracing::warn!(
                        "[fetch_provider_models] moonshot /models HTTP {}",
                        resp.status()
                    );
                    Vec::new()
                }
                Err(e) => {
                    tracing::warn!("[fetch_provider_models] moonshot request failed: {}", e);
                    Vec::new()
                }
            };
            let user_models = crate::config::Config::load()
                .ok()
                .and_then(|c| c.providers.moonshot.clone())
                .map(|p| p.models)
                .unwrap_or_default();
            return merge_minimax_baseline(api_models, user_models);
        }
        "ollama" => {
            // Ollama - fetch from /api/tags (local or cloud)
            let base = if let Some(url) = base_url
                && !url.is_empty()
            {
                url.to_string()
            } else {
                "http://localhost:11434".to_string()
            };
            let base = base.trim_end_matches('/');
            #[derive(serde::Deserialize)]
            struct OllamaModel {
                name: String,
            }
            #[derive(serde::Deserialize)]
            struct OllamaModelsResponse {
                models: Vec<OllamaModel>,
            }
            let mut req = client.get(format!("{}/api/tags", base));
            if let Some(key) = api_key
                && !key.is_empty()
            {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<OllamaModelsResponse>().await {
                        Ok(body) => {
                            let mut models: Vec<String> =
                                body.models.into_iter().map(|m| m.name).collect();
                            models.sort();
                            models.reverse();
                            tracing::info!(
                                "[fetch_provider_models] Ollama: fetched {} models",
                                models.len()
                            );
                            return models;
                        }
                        Err(e) => {
                            tracing::warn!("Ollama models parse error: {}", e);
                            return Vec::new();
                        }
                    }
                }
                Ok(resp) => {
                    tracing::warn!("Ollama models API returned {}", resp.status());
                    return Vec::new();
                }
                Err(e) => {
                    tracing::warn!("Ollama models fetch failed: {}", e);
                    return Vec::new();
                }
            }
        }
        _ => {
            // Custom provider: try fetching from base_url if provided
            if let Some(url) = base_url
                && !url.is_empty()
            {
                return crate::brain::provider::model_fetch::fetch_models_from_endpoint(
                    url, api_key,
                )
                .await;
            }
            return Vec::new();
        }
    };

    match result {
        Ok(resp) if resp.status().is_success() => match resp.json::<ModelsResponse>().await {
            Ok(body) => {
                let mut entries = body.data;
                // Sort newest first (by created timestamp descending)
                entries.sort_by_key(|e| std::cmp::Reverse(e.created));
                entries.into_iter().map(|m| m.id).collect()
            }
            Err(_) => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Binary's known MiniMax models, used as fallback when the live
/// /models fetch fails. Newest first so the picker highlights the
/// current model. Live fetch is the primary source - this only
/// covers offline / unreachable-API scenarios.
fn minimax_baseline_models() -> Vec<String> {
    vec![
        "MiniMax-M3".to_string(),
        "MiniMax-M2.7".to_string(),
        "MiniMax-M2.7-highspeed".to_string(),
        "MiniMax-M2.5".to_string(),
        "MiniMax-M2.5-highspeed".to_string(),
        "MiniMax-M2.1".to_string(),
        "MiniMax-M2.1-highspeed".to_string(),
        "MiniMax-M2".to_string(),
    ]
}

/// Binary's known Xiaomi MiMo models, used as fallback when the live
/// /models fetch fails - the endpoint is keyless but geo/credential
/// sensitive (401 on some CI runners, #1419). Newest first so the
/// picker highlights the current model. Live fetch is the primary
/// source - this only covers offline / unreachable-API scenarios.
pub(crate) fn xiaomi_baseline_models() -> Vec<String> {
    vec![
        "mimo-v2.5-pro".to_string(),
        "mimo-v2-pro".to_string(),
        "mimo-v2-omni".to_string(),
        "mimo-v2-flash".to_string(),
        "mimo-v2-pro-free".to_string(),
        "mimo-v2-omni-free".to_string(),
    ]
}

/// User's saved MiniMax models from config.toml, plus the
/// default_model fallback when no list was saved. Empty when no
/// MiniMax provider is configured.
fn user_minimax_models() -> Vec<String> {
    let Ok(config) = crate::config::Config::load() else {
        return Vec::new();
    };
    let Some(p) = &config.providers.minimax else {
        return Vec::new();
    };
    if !p.models.is_empty() {
        return p.models.clone();
    }
    if let Some(model) = &p.default_model {
        return vec![model.clone()];
    }
    Vec::new()
}

/// Merge MiniMax baseline + user models. Baseline order preserved
/// at the front (so a fresh release like MiniMax-M3 lands at the
/// top of the picker on every binary upgrade); user-only entries
/// appended at the end (so private variants / Text-01 / etc. stay
/// available). Case-insensitive dedup so `MiniMax-M3` and
/// `minimax-m3` don't both appear.
pub(crate) fn merge_minimax_baseline(baseline: Vec<String>, user: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(baseline.len() + user.len());
    let mut seen = std::collections::HashSet::<String>::new();
    for m in baseline {
        let key = m.to_lowercase();
        if seen.insert(key) {
            out.push(m);
        }
    }
    for m in user {
        let key = m.to_lowercase();
        if seen.insert(key) {
            out.push(m);
        }
    }
    out
}

/// Fetch available models from the opencode CLI binary.
async fn fetch_opencode_models() -> Vec<String> {
    // Resolve binary path
    let home = dirs::home_dir().unwrap_or_default();
    let candidates = [
        std::env::var("OPENCODE_PATH").unwrap_or_default(),
        home.join(".opencode/bin/opencode")
            .to_string_lossy()
            .to_string(),
        "/opt/homebrew/bin/opencode".to_string(),
        "/usr/local/bin/opencode".to_string(),
    ];

    let binary = candidates
        .iter()
        .find(|p| !p.is_empty() && std::path::Path::new(p).exists());

    let Some(binary) = binary else {
        // Try `which` as fallback
        if let Ok(output) = tokio::process::Command::new("which")
            .arg("opencode")
            .output()
            .await
            && output.status.success()
        {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return run_opencode_models(&path).await;
            }
        }
        return Vec::new();
    };

    run_opencode_models(binary).await
}

async fn run_opencode_models(binary: &str) -> Vec<String> {
    let output = match tokio::process::Command::new(binary)
        .arg("models")
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut models: Vec<String> = stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('{'))
        .map(|l| l.to_string())
        .collect();
    models.sort();
    models
}

/// Execute one immutable request and preserve its identity for stale checks.
pub async fn fetch(request: FetchRequest) -> FetchOutcome {
    let provider_index = request.provider_index;
    let models = fetch_provider_models(
        request.provider_index,
        request.api_key.as_deref(),
        request.zhipu_endpoint.as_deref(),
        request.xiaomi_endpoint.as_deref(),
        request.moonshot_endpoint.as_deref(),
        request.base_url.as_deref(),
        request.opencode_endpoint.as_deref(),
    )
    .await;
    let error = models.is_empty().then(|| {
        "the provider returned no live models; check the key and endpoint, then retry".to_string()
    });
    FetchOutcome {
        provider_index,
        provider_id: request.provider_id,
        provider_name: request.provider_name,
        models,
        error,
    }
}
