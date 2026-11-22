//! Generic model fetching from OpenAI-compatible endpoints.
//!
//! Provides a reusable function to fetch model lists from any
//! OpenAI-compatible `/v1/models` endpoint or Ollama `/api/tags`.
//! Used by custom providers, Ollama, onboarding, and the /models dialog.

use std::time::Duration;

/// Response shape for OpenAI-compatible `/v1/models` endpoint.
#[derive(serde::Deserialize)]
pub(crate) struct OpenAIModelsResponse {
    pub data: Vec<OpenAIModelEntry>,
}

#[derive(serde::Deserialize)]
pub(crate) struct OpenAIModelEntry {
    pub id: String,
    #[serde(default)]
    pub created: i64,
}

/// Response shape for Ollama `/api/tags` endpoint.
#[derive(serde::Deserialize)]
pub(crate) struct OllamaModelsResponse {
    pub models: Vec<OllamaModelEntry>,
}

#[derive(serde::Deserialize)]
pub(crate) struct OllamaModelEntry {
    pub name: String,
}

/// Natural version sort: extracts numeric segments and compares numerically.
/// So "3.7" > "3.6" > "3.5", "32B" > "14B" > "8B", "max" > "plus" by position.
/// Returns newest-first ordering (descending).
fn version_sort_key(name: &str) -> Vec<(isize, String)> {
    let lower = name.to_lowercase();
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_number = false;

    for ch in lower.chars() {
        if ch.is_ascii_digit() {
            if !in_number && !current.is_empty() {
                segments.push((-1, std::mem::take(&mut current)));
            }
            in_number = true;
            current.push(ch);
        } else {
            if in_number && !current.is_empty() {
                let n: isize = current.parse().unwrap_or(0);
                segments.push((n, String::new()));
                current.clear();
            }
            in_number = false;
            current.push(ch);
        }
    }
    if !current.is_empty() {
        if in_number {
            let n: isize = current.parse().unwrap_or(0);
            segments.push((n, String::new()));
        } else {
            segments.push((-1, current));
        }
    }

    // Negate numeric segments for descending order (newest first)
    segments
        .into_iter()
        .map(|(n, s)| if n >= 0 { (-n, s) } else { (n, s) })
        .collect()
}

/// Normalize a base URL by stripping trailing slashes and common API path suffixes.
pub(crate) fn normalize_base_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    base.strip_suffix("/v1/chat/completions")
        .or_else(|| base.strip_suffix("/chat/completions"))
        .or_else(|| base.strip_suffix("/v1"))
        .unwrap_or(base)
        .to_string()
}

/// Fetch model names from an OpenAI-compatible endpoint.
///
/// Strategy:
/// 1. Try `{base_url}/v1/models` (OpenAI standard)
/// 2. Fall back to `{base_url}/api/tags` (Ollama native)
///
/// Returns sorted model names (newest first by `created` timestamp for OpenAI,
/// alphabetical for Ollama). Returns empty vec on failure.
pub async fn fetch_models_from_endpoint(base_url: &str, api_key: Option<&str>) -> Vec<String> {
    let base = normalize_base_url(base_url);

    // Two attempts, one client. The ceilings are client-level settings, so the
    // slot builds it once instead of per discovery call.
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = crate::utils::http::cached_client(&CLIENT, |b| {
        b.timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
    });

    // Attempt 1: OpenAI-compatible /v1/models
    let models_url = format!("{}/v1/models", base);
    tracing::debug!("[model_fetch] trying OpenAI-compatible: {}", models_url);

    let mut req = client.get(&models_url);
    if let Some(key) = api_key
        && !key.is_empty()
    {
        req = req.header("Authorization", format!("Bearer {}", key));
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<OpenAIModelsResponse>().await {
            Ok(body) if !body.data.is_empty() => {
                let mut entries = body.data;
                // Use created timestamps if meaningful (not all zero/identical), else version sort
                let timestamps_useful = entries.iter().any(|e| e.created > 0)
                    && entries.windows(2).any(|w| w[0].created != w[1].created);
                if timestamps_useful {
                    entries.sort_by_key(|e| std::cmp::Reverse(e.created));
                    let models: Vec<String> = entries.into_iter().map(|m| m.id).collect();
                    return models;
                }
                let mut models: Vec<String> = entries.into_iter().map(|m| m.id).collect();
                models.sort_by_key(|a| version_sort_key(a));
                tracing::info!(
                    "[model_fetch] fetched {} models from {}",
                    models.len(),
                    models_url
                );
                return models;
            }
            Ok(_) => {
                tracing::debug!("[model_fetch] /v1/models returned empty list");
            }
            Err(e) => {
                tracing::debug!("[model_fetch] /v1/models parse error: {}", e);
            }
        },
        Ok(resp) => {
            tracing::debug!(
                "[model_fetch] /v1/models returned {} — trying Ollama",
                resp.status()
            );
        }
        Err(e) => {
            tracing::debug!(
                "[model_fetch] /v1/models fetch failed: {} — trying Ollama",
                e
            );
        }
    }

    // Attempt 2: Ollama /api/tags
    let ollama_url = format!("{}/api/tags", base);
    tracing::debug!("[model_fetch] trying Ollama: {}", ollama_url);

    let mut req = client.get(&ollama_url);
    if let Some(key) = api_key
        && !key.is_empty()
    {
        req = req.header("Authorization", format!("Bearer {}", key));
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<OllamaModelsResponse>().await {
            Ok(body) if !body.models.is_empty() => {
                let mut models: Vec<String> = body.models.into_iter().map(|m| m.name).collect();
                models.sort_by_key(|a| version_sort_key(a));
                tracing::info!(
                    "[model_fetch] fetched {} models from {}",
                    models.len(),
                    ollama_url
                );
                return models;
            }
            Ok(_) => {
                tracing::debug!("[model_fetch] /api/tags returned empty list");
            }
            Err(e) => {
                tracing::debug!("[model_fetch] /api/tags parse error: {}", e);
            }
        },
        Ok(resp) => {
            tracing::debug!("[model_fetch] /api/tags returned {}", resp.status());
        }
        Err(e) => {
            tracing::debug!("[model_fetch] /api/tags fetch failed: {}", e);
        }
    }

    Vec::new()
}
