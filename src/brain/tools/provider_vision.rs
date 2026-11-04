//! Provider Vision Tool
//!
//! Analyzes images by rolling through every provider vision candidate
//! (vision_model + usable key) via the OpenAI-compatible API, then the
//! fallback chain entries, with Gemini `[image.vision]` strictly last
//! (#430). Registered as `analyze_image` whenever any candidate exists.

use super::analyze_image::{AnalyzeImageTool, base64_encode, detect_mime_type};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use crate::brain::provider::factory::VisionCandidate;
use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

/// What to tell the user when no vision backend resolves at all.
///
/// Shared so the request-time path and [`VisionSetupHintTool`] cannot drift.
pub(crate) const VISION_SETUP_HINT: &str = "Image analysis isn't set up yet. To enable it, either: (1) set a \
     multimodal `vision_model` on your active provider via the \
     `config_manager` tool (works for OpenAI-compatible providers), \
     or (2) add a Google Gemini vision key via the `/image` \
     wizard (or an `[image.vision]` section). It hot-reloads, so no \
     restart is needed. Tell the user this.";

/// Image vision/analysis tool rolling through provider vision candidates.
///
/// Holds NO candidate list. It used to be built once at startup and baked in,
/// which meant a `config.toml` edit changed nothing until restart, and meant
/// the list could not know which provider the session was actually running on
/// (#1318). Resolution happens per request instead, from `Config::current()`,
/// the same way the Telegram governors read their knobs live.
pub struct ProviderVisionTool {
    /// An explicit candidate list, when the caller already resolved one.
    ///
    /// `analyze_video` extracts frames locally and hands them to a vision
    /// backend it has already picked, so it pins that rather than re-deriving
    /// it. `None` is the ordinary case: resolve from config per request.
    pinned: Option<Vec<VisionCandidate>>,
    /// Gemini fallback, tried only after EVERY candidate fails. Gemini is
    /// never primary while any provider can serve vision (owner contract).
    gemini_fallback: Option<AnalyzeImageTool>,
}

impl Default for ProviderVisionTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderVisionTool {
    pub fn new() -> Self {
        Self {
            pinned: None,
            gemini_fallback: None,
        }
    }

    /// Pin an explicit candidate list, skipping config resolution.
    ///
    /// The tuple shape carries no per-provider cap, so a pinned candidate
    /// sends no `max_tokens` and lets the endpoint decide (#2120), which is
    /// what `analyze_video`'s frame calls want.
    pub fn with_candidates(candidates: Vec<(String, String, String)>) -> Self {
        Self {
            pinned: Some(
                candidates
                    .into_iter()
                    .map(|(api_key, base_url, model)| VisionCandidate {
                        api_key,
                        base_url,
                        model,
                        max_tokens: None,
                    })
                    .collect(),
            ),
            gemini_fallback: None,
        }
    }

    /// Candidates for THIS request: current provider, then the configured
    /// chain, read from config now so an edit needs no restart (#1318).
    fn candidates_for(session_provider: Option<&str>) -> Vec<VisionCandidate> {
        crate::brain::provider::factory::vision_candidates_for(
            &crate::config::Config::current(),
            session_provider,
        )
    }

    /// Attach a Gemini fallback (`image.vision` key + model). Tried only if the
    /// provider's own vision call fails.
    pub fn with_gemini_fallback(mut self, api_key: String, model: String) -> Self {
        self.gemini_fallback = Some(AnalyzeImageTool::new(api_key, model));
        self
    }

    /// Run the Gemini fallback if present; otherwise return `primary`.
    async fn fallback_or(
        &self,
        input: &Value,
        context: &ToolExecutionContext,
        primary: ToolResult,
        reason: &str,
    ) -> super::error::Result<ToolResult> {
        if let Some(fb) = &self.gemini_fallback {
            tracing::warn!(
                "analyze_image: provider vision failed ({reason}); falling back to Gemini"
            );
            return fb.execute(input.clone(), context).await;
        }
        Ok(primary)
    }
}

/// Longest single candidate reason kept in the surfaced failure message
/// (#1792). An API error body can be a full HTML page; the model, the endpoint
/// and the head of the reason are what identify a misconfigured candidate.
pub(crate) const VISION_FAILURE_REASON_LIMIT: usize = 240;

/// Char-safe clip with a marker, so one huge error body cannot drown the rest
/// of the list.
pub(crate) fn clip_failure_reason(reason: &str) -> String {
    if reason.chars().count() <= VISION_FAILURE_REASON_LIMIT {
        return reason.to_string();
    }
    let mut clipped: String = reason.chars().take(VISION_FAILURE_REASON_LIMIT).collect();
    clipped.push_str("... [truncated]");
    clipped
}

/// Aggregate per-candidate failures into the surfaced error (#1792): every
/// tried model and endpoint with its own reason, numbered. The message used to
/// carry only the LAST candidate's error, so one endpoint's 400 (an
/// opencode-go MissingSessionID) read like a blanket failure of the whole
/// chain and hid which entry was actually wrong.
pub(crate) fn format_vision_failures(failures: &[String]) -> String {
    let mut out = format!("All {} provider vision candidates failed:", failures.len());
    for (i, entry) in failures.iter().enumerate() {
        out.push_str(&format!("\n  [{}] {}", i + 1, entry));
    }
    out.push_str(
        "\nEach line above is one tried candidate; correct or drop the offending \
         [providers.*].vision_model or [fallback.vision] entry. Full reasons are \
         in the log under 'analyze_image: vision candidate'.",
    );
    out
}

#[async_trait]
impl Tool for ProviderVisionTool {
    fn name(&self) -> &str {
        "analyze_image"
    }

    fn description(&self) -> &str {
        "Analyze an image file (local path) or URL using the provider's vision model. \
         Use when: the current model doesn't support vision, you need to analyze a saved file, \
         or the user sends an image. The vision model describes the image so you can understand it."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "image": {
                    "type": "string",
                    "description": "Local file path (e.g. /home/user/photo.png) or HTTPS URL to the image"
                },
                "question": {
                    "type": "string",
                    "description": "What to ask about the image. Defaults to 'Describe this image in detail.'"
                }
            },
            "required": ["image"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network, ToolCapability::ReadFiles]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        input: Value,
        context: &ToolExecutionContext,
    ) -> super::error::Result<ToolResult> {
        let image_src = match input["image"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return Ok(ToolResult::error(
                    "Missing required parameter: image".to_string(),
                ));
            }
        };

        let question = input["question"]
            .as_str()
            .unwrap_or("Describe this image in detail.")
            .to_string();

        // Build image_url content part
        let image_url = if image_src.starts_with("http://") || image_src.starts_with("https://") {
            image_src.clone()
        } else {
            // Local file — resolve the path (expand ~, join relative to the
            // working dir) BEFORE reading (#720). The system prompt renders paths
            // tilde-collapsed, so the model passes ~/…; reading it raw fails on a
            // literal ~ dir. analyze_image already did this; the provider-vision
            // path (which runs before the Gemini fallback) had missed it.
            let resolved = super::error::resolve_tool_path(&image_src, &context.working_dir());
            let resolved_str = resolved.to_string_lossy().to_string();
            let bytes = tokio::fs::read(&resolved).await.map_err(|e| {
                super::error::ToolError::Execution(format!(
                    "Failed to read image file '{}': {}",
                    resolved_str, e
                ))
            })?;
            let mime = detect_mime_type(&resolved_str);
            let b64 = base64_encode(&bytes);
            format!("data:{};base64,{}", mime, b64)
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        // Roll through candidates in order (#430): a failure logs and tries
        // the next; Gemini runs only after every candidate failed. #1792: every
        // failure is kept, not just the last, so the surfaced error names the
        // offending candidate instead of blaming the whole chain for it.
        let mut failures: Vec<String> = Vec::new();
        let candidates = match &self.pinned {
            Some(pinned) => pinned.clone(),
            None => Self::candidates_for(context.session_provider.as_deref()),
        };
        // Nothing configured anywhere: say how to fix it rather than failing
        // opaquely. Emitted HERE, at request time, so it follows a config
        // edit — the old startup gate could register this permanently over a
        // config that had since gained vision (#1318).
        if candidates.is_empty() && self.gemini_fallback.is_none() {
            return Ok(ToolResult::error(VISION_SETUP_HINT.to_string()));
        }
        for candidate in &candidates {
            match try_vision_candidate(
                &client,
                candidate,
                &question,
                &image_url,
                // The conversation id, so a gateway that asks to be told which
                // conversation a call belongs to gets the same one the chat
                // path sends for it (#1792).
                Some(context.session_id),
            )
            .await
            {
                Ok(text) => return Ok(ToolResult::success(text)),
                Err(reason) => {
                    tracing::warn!(
                        "analyze_image: vision candidate model={} url={} failed: {reason}, \
                         trying next candidate",
                        candidate.model,
                        candidate.base_url
                    );
                    failures.push(format!(
                        "{} @ {}: {}",
                        candidate.model,
                        candidate.base_url,
                        clip_failure_reason(&reason)
                    ));
                }
            }
        }
        let summary = if failures.is_empty() {
            "no vision candidates configured".to_string()
        } else {
            format_vision_failures(&failures)
        };
        if !failures.is_empty() {
            tracing::error!("analyze_image: {summary}");
        }
        self.fallback_or(
            &input,
            context,
            ToolResult::error(summary.clone()),
            &summary,
        )
        .await
    }
}

/// The complete header set for one vision call: auth, then the gateway's
/// client-identification contract.
///
/// Split out so the contract is testable without a live endpoint. The identity
/// half is [`crate::brain::provider::identity::headers_for`], the very same
/// call the chat path makes (`custom_openai_compatible.rs`), on the same
/// per-conversation id. That parity is the fix for #1792: `analyze_image`
/// hand-rolled its request with `Content-Type` and `Authorization` only, so it
/// was the one path that reached a gateway anonymously. OpenCode Go was the
/// gateway that acted on it, answering the image call with
/// `400 Request is missing x-opencode-session` while chat on the same provider
/// worked, because chat went through `headers_for` and vision did not.
///
/// Hosts with no identity contract get nothing extra, which is `headers_for`'s
/// deliberate opt-in (see its module docs on gateway fingerprinting); this
/// must not become a blanket header dump.
pub(crate) fn vision_headers(
    base_url: &str,
    api_key: &str,
    session: Option<Uuid>,
) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {api_key}")),
    ];
    headers.extend(crate::brain::provider::identity::headers_for(
        base_url, session,
    ));
    headers
}

/// Output cap used when an endpoint REFUSES a request that omits
/// `max_tokens` (#2120).
///
/// Only reachable on that specific refusal: the default request carries no cap
/// at all, because a reasoning vision model shares the completion budget with
/// its reasoning tokens and a fixed 1024 left the answer truncated or empty.
/// Generous on purpose, since this fires only where the endpoint demands a
/// number and a small one would reproduce the bug.
pub(crate) const VISION_RETRY_MAX_TOKENS: u32 = 65000;

/// Build the request body for one vision call.
///
/// `max_tokens` is OMITTED entirely when no cap is set, rather than sent as
/// `null`: some gateways reject an explicit null, and the whole point is to
/// let the endpoint apply its own default (#2120).
pub(crate) fn vision_request_body(
    model: &str,
    question: &str,
    image_url: &str,
    max_tokens: Option<u32>,
) -> Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": question },
                { "type": "image_url", "image_url": { "url": image_url } }
            ]
        }]
    });
    if let Some(cap) = max_tokens {
        body["max_tokens"] = Value::from(cap);
    }
    body
}

/// True when the failure names the output cap.
///
/// The request carried no cap, so a 400 that mentions `max_tokens` at all is
/// the endpoint asking for the field. No wording analysis: one substring check,
/// one retry. A range violation cannot be produced by a request that sent no
/// value, so there is nothing to disambiguate.
pub(crate) fn needs_max_tokens(reason: &str) -> bool {
    let m = reason.to_lowercase();
    m.contains("max_tokens") || m.contains("max_completion_tokens")
}

/// One [OI]-compatible vision call. `Err` carries the reason so the
/// caller can log it and roll to the next candidate.
pub(crate) async fn try_vision_candidate(
    client: &reqwest::Client,
    candidate: &VisionCandidate,
    question: &str,
    image_url: &str,
    session: Option<Uuid>,
) -> std::result::Result<String, String> {
    let api_key = &candidate.api_key;
    let base_url = &candidate.base_url;
    let vision_model = &candidate.model;

    let send = |max_tokens: Option<u32>| {
        let body = vision_request_body(vision_model, question, image_url, max_tokens);
        // Auth plus the gateway's client identification, from one place: an
        // opencode endpoint answers a request without `X-Opencode-Session` with
        // a flat 400, which is what made every candidate in the chain look
        // broken (#1792).
        let mut request = client.post(base_url.as_str());
        for (key, value) in vision_headers(base_url, api_key, session) {
            request = request.header(key.as_str(), value.as_str());
        }
        async move {
            let response = request
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Vision request failed: {e}"))?;

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let err_body = response.text().await.unwrap_or_default();
                return Err(format!("Vision API error {status}: {err_body}"));
            }

            let json: Value = response
                .json()
                .await
                .map_err(|e| format!("Vision response was not JSON: {e}"))?;
            let choice = json["choices"]
                .as_array()
                .and_then(|choices| choices.first());
            if choice.and_then(|c| c["finish_reason"].as_str()) == Some("length") {
                // The answer hit the budget. Worth a log line: this is the
                // exact symptom #2120 was filed for, and with no cap sent it
                // means the endpoint's own default is the ceiling.
                tracing::warn!(
                    "analyze_image: {vision_model} finished on 'length' (cap {max_tokens:?}); \
                     the answer may be truncated. Set providers.<name>.vision_max_tokens \
                     to raise it."
                );
            }
            let result_text = choice
                .and_then(|choice| choice["message"]["content"].as_str())
                .unwrap_or("")
                .to_string();
            if result_text.is_empty() {
                return Err("No text response from vision model".to_string());
            }
            Ok(result_text)
        }
    };

    match send(candidate.max_tokens).await {
        Ok(text) => Ok(text),
        Err(reason) if candidate.max_tokens.is_none() && needs_max_tokens(&reason) => {
            // This endpoint demands an explicit cap. Send one, once.
            tracing::warn!(
                "analyze_image: {vision_model} @ {base_url} rejected a request without \
                 max_tokens ({reason}); retrying once with {VISION_RETRY_MAX_TOKENS}"
            );
            send(Some(VISION_RETRY_MAX_TOKENS)).await
        }
        Err(reason) => Err(reason),
    }
}

/// Placeholder `analyze_image` registered when no vision backend is configured
/// (no provider `vision_model`, no Gemini `image.vision` key). Instead of the
/// tool simply being absent — which leaves the agent unable to explain why it
/// can't see an image — this returns a clear, actionable setup hint the agent
/// relays to the user on the TUI or a channel.
pub struct VisionSetupHintTool;

#[async_trait]
impl Tool for VisionSetupHintTool {
    fn name(&self) -> &str {
        "analyze_image"
    }

    fn description(&self) -> &str {
        "Analyze an image. NOTE: image analysis is not configured on this \
         install yet — calling this returns setup instructions to relay to the \
         user."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "image": { "type": "string", "description": "Image path or URL" }
            },
            "required": ["image"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolExecutionContext,
    ) -> super::error::Result<ToolResult> {
        Ok(ToolResult::error(VISION_SETUP_HINT.to_string()))
    }
}
