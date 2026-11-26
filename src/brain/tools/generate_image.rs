//! Generate Image Tool
//!
//! Generates images from text prompts. Two wire backends:
//!
//! * **Gemini** — historical default. Calls
//!   `POST https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent`
//!   with `x-goog-api-key` and `responseModalities: ["TEXT", "IMAGE"]`.
//!   Optionally accepts an input image (local path or HTTPS URL) for
//!   img2img editing — the input image is prepended as an `inlineData`
//!   part so Gemini can modify, restyle, or composite onto it.
//! * **OpenAI-compatible** — `POST {base_url}/images/generations` with
//!   `Authorization: Bearer {key}` and `response_format: "b64_json"`.
//!   Lets users point `generate_image` at OpenRouter, OpenAI, Together,
//!   DashScope/Qwen-Image, or any custom provider that exposes the
//!   OpenAI images endpoint by setting `[providers.<name>]
//!   generation_model = "..."` in `config.toml` (and api_key + base_url
//!   already there for chat). img2img is NOT supported on this backend.
//!
//! Candidate selection mirrors the vision chain (#1672, parity with
//! #1318): the session's current provider first, then the ordered
//! `[providers.fallback] generation` chain, then the global Gemini
//! `[image.generation]` section strictly last — and that last leg alone
//! is gated by `image.generation.enabled`. Resolution happens PER REQUEST
//! from `Config::current()`, so editing the chain needs no restart.
//!
//! Both backends save the result as a PNG file in
//! `~/.opencrabs/images/` and return the path.

use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
/// Substring match on a candidate's `base_url` is enough to decide which
/// wire protocol to use — Google's images endpoint lives under this host,
/// everyone else's `/v1/images/generations` follows the OpenAI shape.
pub const GEMINI_HOST_MARKER: &str = "generativelanguage.googleapis.com";

/// Which HTTP shape to use for the actual call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Backend {
    Gemini { api_key: String },
    OpenAi { api_key: String, base_url: String },
}

/// One resolved generation route: a wire backend plus the model to call
/// on it. The tool holds an ordered list and rolls through at request time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationCandidate {
    backend: Backend,
    model: String,
}

impl GenerationCandidate {
    /// `"gemini"` or `"openai"` — the wire protocol, for assertions and
    /// logs without exposing keys.
    pub fn backend_kind(&self) -> &'static str {
        match &self.backend {
            Backend::Gemini { .. } => "gemini",
            Backend::OpenAi { .. } => "openai",
        }
    }

    /// The model this candidate would call — read-only accessor for tests
    /// and wiring; the model is picked at resolution time, never mutated.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Every backend takes input images (#2045): Gemini as `inlineData`
    /// parts, the OpenAI-compatible wire as `/images/edits` with `image[]`.
    fn supports_image_input(&self) -> bool {
        true
    }
}

/// Image generation tool — walks an ordered candidate list, each on the
/// Gemini or OpenAI-compatible wire, picked per candidate by URL shape.
pub struct GenerateImageTool {
    /// Pinned candidates for construction-time callers (tests, explicit
    /// single-backend wiring). Empty = resolve from `Config::current()`
    /// per request — the ordinary case, same lifetime contract as
    /// `ProviderVisionTool` (#1318 → #1672).
    pinned: Vec<GenerationCandidate>,
}

/// Setup hint emitted at request time when resolution genuinely finds
/// nothing (mirrors `VISION_SETUP_HINT`: never a permanently-registered
/// hint over a config that has since gained a route).
pub const GENERATION_SETUP_HINT: &str = "Image generation is not configured. Options: \
    `/image gemini <GOOGLE_AI_KEY>` for Gemini, or set `generation_model = \"<model>\"` \
    on a provider section (`[providers.<name>]` / `[providers.custom.<name>]` with base_url) \
    to route through any OpenAI-compatible `/images/generations` endpoint. Pin a non-active \
    provider with `[providers.fallback] generation = [\"<name>\", ...]`.";

impl GenerateImageTool {
    /// Historical constructor — pinned single Gemini candidate, model
    /// defaults to whatever `cli/ui.rs` resolved from
    /// `effective_generation_model(config)`.
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            pinned: vec![GenerationCandidate {
                backend: Backend::Gemini { api_key },
                model,
            }],
        }
    }

    /// OpenAI-compatible backend — `base_url` should be the API root
    /// without a trailing slash or `/chat/completions` suffix
    /// (`active_provider_generation` already normalises that).
    pub fn with_openai_backend(api_key: String, base_url: String, model: String) -> Self {
        Self {
            pinned: vec![GenerationCandidate {
                backend: Backend::OpenAi { api_key, base_url },
                model,
            }],
        }
    }

    /// Per-request resolution mode: no pinned candidates, the chain is
    /// rebuilt from live config on every call (#1672).
    pub fn dynamic() -> Self {
        Self { pinned: Vec::new() }
    }

    /// Map a factory candidate tuple `(api_key, base_url, model)` to a
    /// backend, choosing the wire by URL shape so a Gemini-provider
    /// override (e.g. `imagen-4.0-generate-001`) still routes through the
    /// Gemini API rather than misfiring at an OpenAI endpoint that doesn't
    /// exist there.
    fn candidate_from_tuple(
        (api_key, base_url, model): (String, String, String),
    ) -> GenerationCandidate {
        GenerationCandidate {
            backend: if base_url.contains(GEMINI_HOST_MARKER) {
                Backend::Gemini { api_key }
            } else {
                Backend::OpenAi { api_key, base_url }
            },
            model,
        }
    }

    /// Global Gemini fallback leg — ONLY `[image.generation]` consults
    /// `enabled`; provider routes are never vetoed by it (#1672, gap 2).
    fn global_gemini_candidate(config: &crate::config::Config) -> Option<GenerationCandidate> {
        if !config.image.generation.enabled {
            return None;
        }
        let api_key = config.image.generation.api_key.as_ref()?.clone();
        Some(GenerationCandidate {
            backend: Backend::Gemini { api_key },
            model: config.image.generation.model.clone(),
        })
    }

    /// The ordered candidate list THIS config would roll: provider chain
    /// (session provider → `[providers.fallback] generation`) then the
    /// global Gemini fallback, deduped. Exposed for tests and wiring;
    /// `execute` rebuilds it per request from `Config::current()`.
    pub fn plan_candidates(
        config: &crate::config::Config,
        session_provider: Option<&str>,
    ) -> Vec<GenerationCandidate> {
        let mut out: Vec<GenerationCandidate> =
            crate::brain::provider::factory::generation_candidates_for(config, session_provider)
                .into_iter()
                .map(Self::candidate_from_tuple)
                .collect();
        if let Some(global) = Self::global_gemini_candidate(config)
            && !out.contains(&global)
        {
            out.push(global);
        }
        out
    }

    /// Resolve provider config → concrete tool. Returns `None` only when
    /// NO generation route exists at all: a provider candidate (active or
    /// chain, Gemini flag irrelevant) or the enabled global Gemini leg.
    /// The returned tool re-resolves per request, so registration
    /// existence is a coarse gate and the fine answer happens at call
    /// time (#1672, mirrors the unconditional `analyze_image` registration
    /// from #1318).
    pub fn from_config(config: &crate::config::Config) -> Option<Self> {
        if !Self::plan_candidates(config, None).is_empty() {
            Some(Self::dynamic())
        } else {
            None
        }
    }
}

#[async_trait]
impl Tool for GenerateImageTool {
    fn name(&self) -> &str {
        "generate_image"
    }

    fn description(&self) -> &str {
        "Generate an image from a text prompt. Returns the file path to the saved PNG. \
         Use <<IMG:path>> syntax in your reply to send the image through a channel. \
         Optionally accepts an input image (local path or HTTPS URL) for img2img editing \
         on the Gemini backend — useful for replacing elements, restyling, compositing \
         logos, or modifying user-uploaded images. The OpenAI-compatible backend does \
         not support input images."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text description of the image to generate, or editing instruction when an input image is provided"
                },
                "image": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}, "maxItems": MAX_INPUT_IMAGES}
                    ],
                    "description": "Optional input image, or a list of up to 8 (each a local file path or HTTPS URL), for img2img editing. Several images let the model combine them into one result, for example a brand asset placed into a photo. The model modifies, restyles or composites onto them based on the prompt."
                },
                "filename": {
                    "type": "string",
                    "description": "Optional filename (without path). Defaults to a UUID-based name."
                }
            },
            "required": ["prompt"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network, ToolCapability::WriteFiles]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        input: Value,
        context: &ToolExecutionContext,
    ) -> super::error::Result<ToolResult> {
        let prompt = match input["prompt"].as_str() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                return Ok(ToolResult::error(
                    "Missing required parameter: prompt".to_string(),
                ));
            }
        };

        let images = match parse_image_inputs(&input["image"]) {
            Ok(v) => v,
            Err(msg) => return Ok(ToolResult::error(msg)),
        };

        // A caller-supplied filename is used verbatim, so one without an
        // extension produced an extensionless file. Downstream consumers
        // identify images by extension: generate_document later failed with
        // "not a decodable PNG/JPEG ... The image format could not be
        // determined" on exactly such a path, and that counted against
        // generate_document rather than against whatever named the file (#889).
        let filename = input["filename"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                let name = s.trim();
                if std::path::Path::new(name).extension().is_some() {
                    name.to_string()
                } else {
                    format!("{name}.png")
                }
            })
            .unwrap_or_else(|| format!("{}.png", uuid::Uuid::new_v4().simple()));

        // Ensure images directory exists
        let images_dir = crate::config::opencrabs_home().join("images");
        if let Err(e) = tokio::fs::create_dir_all(&images_dir).await {
            return Ok(ToolResult::error(format!(
                "Failed to create images directory: {}",
                e
            )));
        }
        let save_path = images_dir.join(&filename);

        // Resolve the roll list: pinned at construction wins; otherwise
        // rebuild from live config per request so a `[providers.fallback]
        // generation` edit takes effect without a restart (#1672, same
        // lifetime contract `ProviderVisionTool` established in #1318).
        let mut candidates = if self.pinned.is_empty() {
            let config = crate::config::Config::current();
            Self::plan_candidates(&config, context.session_provider.as_deref())
        } else {
            self.pinned.clone()
        };

        if candidates.is_empty() {
            return Ok(ToolResult::error(GENERATION_SETUP_HINT.to_string()));
        }

        if !images.is_empty() {
            candidates.retain(GenerationCandidate::supports_image_input);
        }

        // Roll through candidates in order: a failure logs and tries the
        // next; the aggregate error is returned only when all fail.
        let mut failures: Vec<String> = Vec::new();
        for candidate in &candidates {
            let attempt = match &candidate.backend {
                Backend::Gemini { api_key } => {
                    Self::execute_gemini(&candidate.model, &prompt, &images, api_key, &save_path)
                        .await
                }
                Backend::OpenAi { api_key, base_url } => {
                    Self::execute_openai(
                        &candidate.model,
                        &prompt,
                        &images,
                        api_key,
                        base_url,
                        &save_path,
                    )
                    .await
                }
            };
            match attempt {
                Ok(result) if result.success => return Ok(result),
                Ok(result) => {
                    let msg = result.error.unwrap_or_else(|| result.output.clone());
                    tracing::warn!(
                        "generate_image: {} candidate '{}' failed: {msg}",
                        candidate.backend_kind(),
                        candidate.model
                    );
                    failures.push(format!(
                        "{}({}): {msg}",
                        candidate.backend_kind(),
                        candidate.model
                    ));
                }
                Err(e) => {
                    tracing::warn!(
                        "generate_image: {} candidate '{}' errored: {e}",
                        candidate.backend_kind(),
                        candidate.model
                    );
                    failures.push(format!(
                        "{}({}): {e}",
                        candidate.backend_kind(),
                        candidate.model
                    ));
                }
            }
        }
        Ok(ToolResult::error(format!(
            "All {} image generation candidates failed — {}",
            candidates.len(),
            failures.join(" | ")
        )))
    }
}

impl GenerateImageTool {
    async fn execute_gemini(
        model: &str,
        prompt: &str,
        images: &[String],
        api_key: &str,
        save_path: &std::path::Path,
    ) -> super::error::Result<ToolResult> {
        let url = format!("{}/models/{}:generateContent", GEMINI_BASE_URL, model);

        // Input images first, one inlineData part each, then the text prompt (#2045).
        let mut image_parts: Vec<Value> = Vec::with_capacity(images.len());
        for src in images {
            image_parts.push(build_image_part(src).await?);
        }
        let parts = gemini_request_parts(image_parts, prompt);

        let body = serde_json::json!({
            "contents": [{"parts": parts}],
            "generationConfig": {
                "responseModalities": ["TEXT", "IMAGE"]
            }
        });

        let client = crate::utils::http::shared();

        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", api_key)
            .json(&body)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let err_body = response.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "Gemini API error {}: {}",
                status, err_body
            )));
        }

        let json: Value = response
            .json()
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        let empty_vec = vec![];
        let candidates = json["candidates"].as_array().unwrap_or(&empty_vec);
        let mut image_data: Option<String> = None;
        let mut text_response = String::new();

        'outer: for candidate in candidates {
            let empty_parts = vec![];
            let parts = candidate["content"]["parts"]
                .as_array()
                .unwrap_or(&empty_parts);
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    text_response.push_str(text);
                }
                if let Some(data) = part["inlineData"]["data"].as_str() {
                    image_data = Some(data.to_string());
                    break 'outer;
                }
            }
        }

        match image_data {
            Some(b64) => save_decoded_image(&b64, save_path, &text_response).await,
            None => {
                if !text_response.is_empty() {
                    Ok(ToolResult::success(format!(
                        "No image generated. Gemini response: {}",
                        text_response
                    )))
                } else {
                    Ok(ToolResult::error(
                        "No image data found in Gemini response".to_string(),
                    ))
                }
            }
        }
    }

    async fn execute_openai(
        model: &str,
        prompt: &str,
        images: &[String],
        api_key: &str,
        base_url: &str,
        save_path: &std::path::Path,
    ) -> super::error::Result<ToolResult> {
        // `/images/generations` for text-only, `/images/edits` when inputs are
        // given (#2045). Both return the same `data[]` shape.
        let url = format!(
            "{}{}",
            base_url.trim_end_matches('/'),
            openai_images_path(!images.is_empty())
        );
        let body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "n": 1,
            "response_format": "b64_json",
        });

        let client = crate::utils::http::shared();

        let request = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            // Same identity the chat path sends (#3138): image generation is
            // another route into the built-in endpoint, and it builds its own
            // headers, so it never inherited the chat client's.
            .header(
                reqwest::header::USER_AGENT,
                crate::brain::provider::identity::user_agent(),
            )
            .timeout(std::time::Duration::from_secs(120));
        let request = if images.is_empty() {
            request
                .header("Content-Type", "application/json")
                .json(&body)
        } else {
            request.multipart(openai_edit_form(model, prompt, images).await?)
        };
        let response = request
            .send()
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let err_body = response.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "OpenAI images API error {}: {}",
                status, err_body
            )));
        }

        let json: Value = response
            .json()
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        let first = json["data"]
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(Value::Null);

        if let Some(b64) = first["b64_json"].as_str() {
            return save_decoded_image(b64, save_path, "").await;
        }

        if let Some(url) = first["url"].as_str() {
            let bytes = client
                .get(url)
                .send()
                .await
                .map_err(|e| super::error::ToolError::Execution(e.to_string()))?
                .bytes()
                .await
                .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;
            tokio::fs::write(save_path, &bytes)
                .await
                .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;
            let path_str = save_path.to_string_lossy().to_string();
            return Ok(ToolResult::success(format!(
                "Generated image saved to: {}\nUse <<IMG:{}>> to reference it.",
                path_str, path_str
            )));
        }

        Ok(ToolResult::error(format!(
            "No image data found in OpenAI-images response: {}",
            json
        )))
    }
}

/// Build a Gemini-compatible `inlineData` part from a local file path
/// or HTTPS URL. Reuses `base64_encode` and `detect_mime_type` from
/// `analyze_image` to stay consistent with the vision tool.
/// Most input images one call takes (#2045).
const MAX_INPUT_IMAGES: usize = 8;

/// The `image` argument as a list: a single string (the original form) or an
/// array of strings. Empty entries are ignored, as an empty string always was.
pub(crate) fn parse_image_inputs(value: &Value) -> Result<Vec<String>, String> {
    let raw: Vec<&str> = match value {
        Value::Null => return Ok(Vec::new()),
        Value::String(s) => vec![s.as_str()],
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s),
                    None => {
                        return Err(
                            "Each `image` entry must be a string (a file path or URL).".to_string()
                        );
                    }
                }
            }
            out
        }
        _ => return Err("`image` must be a string or a list of strings.".to_string()),
    };
    let images: Vec<String> = raw
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if images.len() > MAX_INPUT_IMAGES {
        return Err(format!(
            "At most {MAX_INPUT_IMAGES} input images per call, got {}.",
            images.len()
        ));
    }
    Ok(images)
}

/// Gemini request parts: every input image in order, then the text (#2045).
pub(crate) fn gemini_request_parts(image_parts: Vec<Value>, prompt: &str) -> Vec<Value> {
    let mut parts = image_parts;
    parts.push(serde_json::json!({"text": prompt}));
    parts
}

/// The OpenAI-compatible route: generations for text, edits when inputs are given.
pub(crate) fn openai_images_path(has_inputs: bool) -> &'static str {
    if has_inputs {
        "/images/edits"
    } else {
        "/images/generations"
    }
}

/// Bytes, MIME type and a file name for one input image, from a local path or
/// an HTTPS URL.
async fn fetch_image_bytes(src: &str) -> super::error::Result<(Vec<u8>, String, String)> {
    use super::analyze_image::detect_mime_type;

    if src.starts_with("http://") || src.starts_with("https://") {
        let client = crate::utils::http::shared();

        let resp = client
            .get(src)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| {
                super::error::ToolError::Execution(format!("Failed to fetch image URL: {}", e))
            })?;

        if !resp.status().is_success() {
            return Err(super::error::ToolError::Execution(format!(
                "Failed to fetch image URL: HTTP {}",
                resp.status()
            )));
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_string();
        let mime_type = content_type
            .split(';')
            .next()
            .unwrap_or("image/jpeg")
            .to_string();

        let bytes = resp.bytes().await.map_err(|e| {
            super::error::ToolError::Execution(format!("Failed to read image bytes: {}", e))
        })?;
        let name = src
            .split(['?', '#'])
            .next()
            .unwrap_or(src)
            .rsplit('/')
            .next()
            .filter(|n| !n.is_empty())
            .unwrap_or("image.png")
            .to_string();
        Ok((bytes.to_vec(), mime_type, name))
    } else {
        let bytes = tokio::fs::read(src).await.map_err(|e| {
            super::error::ToolError::Execution(format!(
                "Failed to read image file '{}': {}",
                src, e
            ))
        })?;
        let name = std::path::Path::new(src)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.png")
            .to_string();
        Ok((bytes, detect_mime_type(src).to_string(), name))
    }
}

/// One Gemini `inlineData` part for an input image.
async fn build_image_part(src: &str) -> super::error::Result<Value> {
    use super::analyze_image::base64_encode;

    let (bytes, mime_type, _) = fetch_image_bytes(src).await?;
    Ok(serde_json::json!({
        "inlineData": { "mimeType": mime_type, "data": base64_encode(&bytes) }
    }))
}

/// The multipart body for `/images/edits`: the fields, then one `image[]` per input.
async fn openai_edit_form(
    model: &str,
    prompt: &str,
    images: &[String],
) -> super::error::Result<reqwest::multipart::Form> {
    let mut form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .text("prompt", prompt.to_string())
        .text("n", "1")
        .text("response_format", "b64_json");
    for src in images {
        let (bytes, mime, name) = fetch_image_bytes(src).await?;
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(name)
            .mime_str(&mime)
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;
        form = form.part("image[]", part);
    }
    Ok(form)
}

async fn save_decoded_image(
    b64: &str,
    save_path: &std::path::Path,
    leading_text: &str,
) -> super::error::Result<ToolResult> {
    let bytes = match base64_decode(b64) {
        Ok(b) => b,
        Err(e) => {
            return Ok(ToolResult::error(format!(
                "Failed to decode image data: {}",
                e
            )));
        }
    };
    tokio::fs::write(save_path, &bytes)
        .await
        .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;
    let path_str = save_path.to_string_lossy().to_string();
    let mut output = format!(
        "Generated image saved to: {}\nUse <<IMG:{}>> to reference it.",
        path_str, path_str
    );
    if !leading_text.trim().is_empty() {
        output = format!("{}\n\n{}", leading_text.trim(), output);
    }
    Ok(ToolResult::success(output))
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    // Use base64 via the standard approach — decode without padding issues
    let clean: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
        .collect();
    base64_decode_inner(&clean)
}

fn base64_decode_inner(input: &str) -> Result<Vec<u8>, String> {
    // Simple base64 decode without external crate (reqwest already depends on base64 indirectly)
    // Use the engine from the existing base64 crate that reqwest pulls in
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| e.to_string())
}
