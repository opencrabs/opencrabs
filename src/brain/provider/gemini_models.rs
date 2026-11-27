//! The one live Gemini catalogue fetch, shared by every surface (#3176).
//!
//! Three code paths used to talk to Google's models endpoint with three
//! different filters and no pagination: the picker read a bare `/models` URL
//! and never saw page 2 (Google returns 50 entries per response, and 8 chat
//! models were hiding on the next page), the channels menu offered embedding
//! and speech families as if they were chat models, and neither path agreed
//! with the other on order. The TUI picker, the GUI picker and the channels
//! `/models` menu all go through this one function now, so they cannot drift
//! apart again.
//!
//! `brain::provider` is the home, not `ui_core`: the provider trait's
//! `fetch_models()` (channels) and the picker discovery (TUI/GUI) both call
//! it, and the ui_core -> brain edge is the direction this repo already uses
//! for provider live fetches (copilot).

use std::time::Duration;

/// Google defaults to 50 entries per response and hands back a
/// `nextPageToken`; asking for 100 usually covers the whole catalogue in one
/// request and the loop below catches the rest.
const PAGE_SIZE: &str = "100";

/// Hard ceiling on pages, so a misbehaving endpoint that keeps handing back a
/// `nextPageToken` can never loop forever.
const MAX_PAGES: usize = 10;

/// Model discovery is a small JSON GET. An endpoint that does not answer
/// within this window is treated as offline and the caller falls back, so the
/// surface opens instead of waiting on a dead host. Matches the picker's
/// `MODELS_TIMEOUT`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// One entry of Google's models list response. `pub(crate)` fields so tests
/// can build entries without a network.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiModelEntry {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) supported_generation_methods: Vec<String>,
}

/// One page of Google's models list response.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiModelsPage {
    #[serde(default)]
    models: Vec<GeminiModelEntry>,
    #[serde(default)]
    next_page_token: Option<String>,
}

/// Reduce raw API entries to chat model ids: keep entries whose method list
/// carries `generateContent`, strip the `models/` prefix, drop families that
/// can generate but are not general chat (image, speech, embedding, ...),
/// dedupe, order newest-first. Pure, so the catalogue shape is unit-tested
/// without a network.
pub(crate) fn chat_models_from_entries(entries: Vec<GeminiModelEntry>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        let generative = entry
            .supported_generation_methods
            .iter()
            .any(|m| m == "generateContent");
        if !generative {
            continue;
        }
        let id = entry.name.strip_prefix("models/").unwrap_or(&entry.name);
        if !super::gemini::is_chat_model(id) {
            continue;
        }
        if !out.iter().any(|m| m == id) {
            out.push(id.to_string());
        }
    }
    let mut refs: Vec<&str> = out.iter().map(String::as_str).collect();
    crate::utils::model_order::sort_newest_first(&mut refs);
    refs.into_iter().map(str::to_string).collect()
}

/// Fetch Google's live model catalogue: every page, chat models only,
/// newest-first. Empty on any failure; callers keep their own fallbacks
/// (the picker falls back to its static catalogue, the provider to
/// `FALLBACK_MODELS`).
pub(crate) async fn fetch_live_chat_models(api_key: &str) -> Vec<String> {
    if api_key.is_empty() {
        return Vec::new();
    }
    let client = crate::utils::http::shared();
    let mut entries: Vec<GeminiModelEntry> = Vec::new();
    let mut page_token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut request = client
            .get(super::gemini::GEMINI_BASE_URL.to_string() + "/models")
            .query(&[("pageSize", PAGE_SIZE)])
            .header("x-goog-api-key", api_key)
            .timeout(REQUEST_TIMEOUT);
        if let Some(token) = &page_token {
            request = request.query(&[("pageToken", token)]);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!("[gemini_models] catalogue fetch failed: {}", e);
                return Vec::new();
            }
        };
        if !response.status().is_success() {
            tracing::warn!("[gemini_models] catalogue returned {}", response.status());
            return Vec::new();
        }
        let page = match response.json::<GeminiModelsPage>().await {
            Ok(page) => page,
            Err(e) => {
                tracing::warn!("[gemini_models] catalogue parse error: {}", e);
                return Vec::new();
            }
        };
        entries.extend(page.models);
        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => {
                tracing::debug!(
                    "[gemini_models] catalogue complete: {} entries",
                    entries.len()
                );
                break;
            }
        }
    }
    let models = chat_models_from_entries(entries);
    tracing::info!("[gemini_models] {} chat models live", models.len());
    models
}
