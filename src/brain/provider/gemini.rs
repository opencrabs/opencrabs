//! Google Gemini Provider Implementation
//!
//! Implements the Provider trait for Google's Gemini models.
//! Uses the Gemini REST API (different format from OpenAI-compatible APIs).
//!
//! ## API Format
//! - Base URL: `https://generativelanguage.googleapis.com/v1beta`
//! - Auth: `x-goog-api-key` request header
//! - Chat: `POST /models/{model}:generateContent`
//! - Stream: `POST /models/{model}:streamGenerateContent?alt=sse`
//!
//! ## Role Mapping
//! Gemini uses `"user"` and `"model"` (not `"assistant"`)
//!
//! ## Supported Models
//! - gemini-2.0-flash
//! - gemini-3.1-flash-image-preview
//! - gemini-1.5-pro
//! - gemini-1.5-flash

use super::error::{ProviderError, Result};
use super::r#trait::{Provider, ProviderStream};
use super::types::*;
use async_trait::async_trait;
use futures::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use std::sync::Arc;

const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Google Gemini provider
#[derive(Clone)]
pub struct GeminiProvider {
    api_key: String,
    client: Client,
    /// Client for `stream()`: no total wall-clock ceiling, so a healthy long
    /// stream is never cut (#1687).
    stream_client: Client,
    /// The resolved `[providers.gemini] timeout_secs` / `[agent] timeout_secs`
    /// (#1688), mirrored from the client `with_timeout` rebuilt. `None` means
    /// the compiled `DEFAULT_TIMEOUT` is what the request client carries.
    request_timeout: Option<Duration>,
    /// The resolved `stream_idle_timeout_secs` at either tier. Inter-chunk
    /// silence is enforced in `brain/agent/service/helpers.rs`, which asks the
    /// provider for this; `None` defers to that file's runtime table.
    stream_idle_timeout: Option<Duration>,
    /// The resolved thinking-loop guard ceiling in seconds (#1690), from
    /// `[providers.gemini]` then `[agent]`. `Some(0)` disables the guard for
    /// this provider, which is why this is seconds rather than a `Duration`.
    thinking_loop_timeout: Option<u64>,
    model: String,
    cached_content_name: Arc<std::sync::Mutex<Option<String>>>,
    /// User override from `providers.gemini.context_window` in config.toml.
    /// When set, becomes the compaction budget (overrides agent.context_limit).
    configured_context_window: Option<u32>,
}

/// Strip JSON-Schema keys that Gemini's `function_declarations.parameters`
/// validator rejects (issue #99). Gemini accepts a narrow subset of
/// OpenAPI 3 — built-in tools like `http`, `tool_manage`, etc. declare
/// `additionalProperties` on dynamic dict fields (headers, query
/// params) which is valid JSON-Schema and accepted by every other
/// provider, but Gemini returns 400 INVALID_ARGUMENT
/// `Unknown name "additionalProperties"`.
///
/// Beyond `additionalProperties`, Gemini's schema validator also rejects
/// `default` and `example` values inside property definitions (e.g. in
/// `grep.rs` where fields have `"default": false`). These are valid JSON
/// Schema keywords but not supported in Gemini function declarations.
///
/// Walks the schema recursively and removes any of the following keys
/// from every object node: `additionalProperties`, `default`, `example`.
/// Other Gemini-incompatible keys (`$ref`, `oneOf`, `nullable`, etc.)
/// are left alone for now; expand here if real-world usage surfaces them.
pub(crate) fn sanitize_schema_for_gemini(value: Value) -> Value {
    match value {
        Value::Object(mut map) => {
            map.remove("additionalProperties");
            map.remove("default");
            map.remove("example");
            for v in map.values_mut() {
                let owned = std::mem::replace(v, Value::Null);
                *v = sanitize_schema_for_gemini(owned);
            }
            Value::Object(map)
        }
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(sanitize_schema_for_gemini).collect())
        }
        other => other,
    }
}

impl GeminiProvider {
    /// Create a new Gemini provider
    pub fn new(api_key: String) -> Self {
        let client = crate::utils::http::buffered_transport(DEFAULT_TIMEOUT);
        let stream_client = crate::utils::http::streaming_transport();

        Self {
            api_key,
            client,
            stream_client,
            request_timeout: None,
            stream_idle_timeout: None,
            thinking_loop_timeout: None,
            model: "gemini-2.0-flash".to_string(),
            cached_content_name: Arc::new(std::sync::Mutex::new(None)),
            configured_context_window: None,
        }
    }

    /// Set the NON-STREAMING request ceiling and rebuild that client only.
    ///
    /// `stream_client` is never touched, so a user raising or lowering
    /// `timeout_secs` cannot put a wall clock back on an SSE body (#1687).
    /// Pairs with the `request_timeout()` trait accessor.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = crate::utils::http::buffered_transport(timeout);
        self.request_timeout = Some(timeout);
        self
    }

    /// Set the inter-chunk streaming inactivity timeout (#1688/#1689).
    ///
    /// Stored rather than baked into a client: the stream loop in
    /// `helpers.rs` owns the clock and asks the provider for this value,
    /// falling back to its runtime table when it is `None`.
    pub fn with_stream_idle_timeout(mut self, timeout: Duration) -> Self {
        self.stream_idle_timeout = Some(timeout);
        self
    }

    /// Set the resolved thinking-loop guard ceiling in seconds (#1690).
    ///
    /// `0` is a value here, not an absent one: it disables the guard for this
    /// provider. The factory resolves `[providers.gemini]` → `[agent]` and
    /// always calls this, so a provider built from config never falls back to
    /// the `[agent]` read in `brain/agent/service/helpers.rs`.
    pub fn with_thinking_loop_timeout(mut self, secs: u64) -> Self {
        self.thinking_loop_timeout = Some(secs);
        self
    }

    /// Set the default model
    pub fn with_model(mut self, model: String) -> Self {
        self.model = model;
        self
    }

    /// Override the context-window budget from `providers.gemini.context_window`.
    pub fn with_context_window(mut self, context_window: u32) -> Self {
        self.configured_context_window = Some(context_window);
        self
    }

    /// Build the generate content URL for a given model
    fn generate_url(&self, model: &str, stream: bool) -> String {
        if stream {
            format!(
                "{}/models/{}:streamGenerateContent?alt=sse",
                GEMINI_BASE_URL, model
            )
        } else {
            format!("{}/models/{}:generateContent", GEMINI_BASE_URL, model)
        }
    }

    /// Convert our LLMRequest to the Gemini request format
    fn build_gemini_request(&self, request: &LLMRequest) -> Value {
        let mut contents: Vec<Value> = Vec::new();
        let mut pending_tool_results: Vec<Value> = Vec::new();

        for msg in &request.messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "model",
                Role::System => continue, // System messages go into systemInstruction
            };

            // Collect parts for this message
            let mut text_parts: Vec<Value> = Vec::new();
            let mut tool_use_parts: Vec<Value> = Vec::new();
            let mut tool_result_parts: Vec<Value> = Vec::new();

            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        text_parts.push(serde_json::json!({"text": text}));
                    }
                    ContentBlock::Thinking { .. } => {
                        // Gemini doesn't use Anthropic-style thinking blocks; skip.
                    }
                    ContentBlock::Image { source } => {
                        let inline_data = match source {
                            ImageSource::Base64 { media_type, data } => {
                                serde_json::json!({
                                    "inlineData": {
                                        "mimeType": media_type,
                                        "data": data
                                    }
                                })
                            }
                            ImageSource::Url { url } => {
                                serde_json::json!({
                                    "fileData": {
                                        "fileUri": url
                                    }
                                })
                            }
                        };
                        text_parts.push(inline_data);
                    }
                    ContentBlock::ToolUse { id: _, name, input } => {
                        tool_use_parts.push(serde_json::json!({
                            "functionCall": {
                                "name": name,
                                "args": input
                            }
                        }));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id: _,
                        content,
                        is_error: _,
                    } => {
                        tool_result_parts.push(serde_json::json!({
                            "functionResponse": {
                                "name": "tool_result",
                                "response": {"output": content}
                            }
                        }));
                    }
                }
            }

            // Tool results must be bundled into a "user" message with functionResponse parts
            if !tool_result_parts.is_empty() {
                pending_tool_results.extend(tool_result_parts);
                continue;
            }

            // Flush any pending tool results before this message
            if !pending_tool_results.is_empty() {
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": pending_tool_results.clone()
                }));
                pending_tool_results.clear();
            }

            let mut all_parts = text_parts;
            all_parts.extend(tool_use_parts);

            if all_parts.is_empty() {
                all_parts.push(serde_json::json!({"text": ""}));
            }

            contents.push(serde_json::json!({
                "role": role,
                "parts": all_parts
            }));
        }

        // Flush any remaining tool results
        if !pending_tool_results.is_empty() {
            contents.push(serde_json::json!({
                "role": "user",
                "parts": pending_tool_results
            }));
        }

        let mut body = serde_json::json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": request.max_tokens.unwrap_or(65536)
            }
        });

        // Reuse a previously created cachedContents resource if one is set.
        if let Ok(guard) = self.cached_content_name.lock()
            && let Some(name) = guard.as_ref()
        {
            body["cachedContent"] = serde_json::Value::String(name.clone());
        }

        // System instruction (full: stable brain + runtime suffix)
        if let Some(system) = request.system_full() {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": system}]
            });
        }

        // Tools
        if let Some(ref tools) = request.tools
            && !tools.is_empty()
        {
            let function_declarations: Vec<Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": sanitize_schema_for_gemini(t.input_schema.clone()),
                    })
                })
                .collect();
            body["tools"] = serde_json::json!([{
                "functionDeclarations": function_declarations
            }]);
            body["toolConfig"] = serde_json::json!({
                "functionCallingConfig": {"mode": "AUTO"}
            });
        }

        body
    }

    /// Parse a Gemini response JSON into an LLMResponse
    fn parse_response(&self, model: &str, json: Value) -> LLMResponse {
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut stop_reason = Some(StopReason::EndTurn);

        let empty_vec = vec![];
        let candidates = json["candidates"].as_array().unwrap_or(&empty_vec);

        if let Some(candidate) = candidates.first() {
            let finish_reason = candidate["finishReason"].as_str().unwrap_or("");
            stop_reason = match finish_reason {
                "STOP" => Some(StopReason::EndTurn),
                "MAX_TOKENS" => Some(StopReason::MaxTokens),
                "TOOL_CODE" | "TOOL_CALLS" => Some(StopReason::ToolUse),
                _ => Some(StopReason::EndTurn),
            };

            let empty_parts = vec![];
            let parts = candidate["content"]["parts"]
                .as_array()
                .unwrap_or(&empty_parts);
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    if !text.is_empty() {
                        content_blocks.push(ContentBlock::Text {
                            text: text.to_string(),
                        });
                    }
                } else if part["functionCall"].is_object() {
                    let fc = &part["functionCall"];
                    let name = fc["name"].as_str().unwrap_or("unknown").to_string();
                    let args = fc["args"].clone();
                    let id = format!("gemini-tc-{}", uuid::Uuid::new_v4().simple());
                    content_blocks.push(ContentBlock::ToolUse {
                        id,
                        name,
                        input: args,
                    });
                    stop_reason = Some(StopReason::ToolUse);
                }
            }
        }

        let usage_meta = &json["usageMetadata"];
        let input_tokens = usage_meta["promptTokenCount"].as_u64().unwrap_or(0) as u32;
        let output_tokens = usage_meta["candidatesTokenCount"].as_u64().unwrap_or(0) as u32;
        let cache_read_tokens = usage_meta["cachedContentTokenCount"].as_u64().unwrap_or(0) as u32;
        let cache_creation_tokens = usage_meta["cacheTokenCount"].as_u64().unwrap_or(0) as u32;

        LLMResponse {
            id: format!("gemini-{}", uuid::Uuid::new_v4().simple()),
            model: model.to_string(),
            content: content_blocks,
            stop_reason,
            usage: TokenUsage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                ..Default::default()
            },
            // Non-streaming path — active streaming time only meaningful for stream_complete.
            streaming_active_secs: None,
            tool_text_leak: false,
        }
    }

    /// Handle API error response
    async fn handle_error(&self, response: reqwest::Response) -> ProviderError {
        let status = response.status().as_u16();
        if let Ok(body) = response.json::<Value>().await {
            let message = body["error"]["message"]
                .as_str()
                .unwrap_or("Unknown error")
                .to_string();
            if status == 429 {
                return ProviderError::RateLimitExceeded(message);
            }
            return ProviderError::ApiError {
                status,
                message,
                error_type: body["error"]["status"].as_str().map(|s| s.to_string()),
            };
        }
        ProviderError::ApiError {
            status,
            message: "Unknown error".to_string(),
            error_type: None,
        }
    }

    /// Create or reuse a cachedContents resource for prompt caching.
    /// Gemini prompt caching works by creating a named cachedContents resource
    /// via a separate API call, then referencing it in subsequent generateContent requests.
    async fn ensure_cached_content(&self, system: &str, tools: &[Tool]) -> Result<()> {
        // Check if we already have a cached content name
        {
            let guard = self.cached_content_name.lock().unwrap();
            if guard.is_some() {
                return Ok(());
            }
        }

        let cached_content_req = CachedContentRequest {
            model: format!("models/{}", self.model),
            system_instruction: CachedContentSystemInstruction {
                parts: vec![GeminiPart {
                    text: system.to_string(),
                }],
            },
            tools: if tools.is_empty() {
                None
            } else {
                Some(
                    tools
                        .iter()
                        .map(|t| CachedContentTool {
                            function_declarations: vec![CachedContentFunctionDecl {
                                name: t.name.clone(),
                                description: t.description.clone(),
                                parameters: sanitize_schema_for_gemini(t.input_schema.clone()),
                            }],
                        })
                        .collect(),
                )
            },
            ttl: Some("3600s".to_string()), // 1 hour TTL
        };

        let url = format!("{}/cachedContents", GEMINI_BASE_URL);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", &self.api_key)
            .json(&cached_content_req)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            // Non-fatal: log the error and return without caching
            let error_text = response.text().await.unwrap_or_default();
            tracing::warn!(
                "Failed to create cachedContent for Gemini (status={}): {}",
                status,
                error_text
            );
            return Ok(());
        }

        let cached_response: CachedContentResponse = response.json().await?;

        // Store the cached content name for future requests
        if let Ok(mut guard) = self.cached_content_name.lock() {
            // The name comes back as "cachedContents/xxxxxx" — use it as-is
            tracing::info!(
                "Gemini cachedContent created: name={}, tokens={:?}",
                cached_response.name,
                cached_response
                    .usage_metadata
                    .and_then(|u| u.total_token_count)
            );
            *guard = Some(cached_response.name);
        }

        Ok(())
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    async fn complete(&self, request: LLMRequest) -> Result<LLMResponse> {
        use crate::utils::retry::{RetryConfig, retry};

        let model = request.model.clone();
        let message_count = request.messages.len();
        tracing::info!(
            "Gemini API request: model={}, messages={}",
            model,
            message_count
        );

        // Create cachedContent on first request if system prompt exists
        if let (Some(system), Some(tools)) = (request.system_full(), request.tools.as_ref())
            && !tools.is_empty()
            && let Err(e) = self.ensure_cached_content(&system, tools).await
        {
            tracing::warn!(error = %e, "failed to ensure cached content for Gemini");
        }

        let body = self.build_gemini_request(&request);
        let url = self.generate_url(&model, false);
        let retry_config = RetryConfig::default();

        let result = retry(
            || async {
                let response = self
                    .client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("x-goog-api-key", &self.api_key)
                    .json(&body)
                    .send()
                    .await?;

                let status = response.status();
                if !status.is_success() {
                    return Err(self.handle_error(response).await);
                }

                let json: Value = response.json().await?;
                let llm_response = self.parse_response(&model, json);

                tracing::info!(
                    "Gemini API response: input_tokens={}, output_tokens={}, stop_reason={:?}",
                    llm_response.usage.input_tokens,
                    llm_response.usage.output_tokens,
                    llm_response.stop_reason
                );

                Ok(llm_response)
            },
            &retry_config,
        )
        .await;

        if let Err(ref e) = result {
            tracing::error!("Gemini API request failed: {}", e);
        }

        result
    }

    async fn stream(&self, request: LLMRequest) -> Result<ProviderStream> {
        use crate::utils::retry::{RetryConfig, retry};

        let model = request.model.clone();
        let message_count = request.messages.len();
        tracing::info!(
            "Gemini streaming request: model={}, messages={}",
            model,
            message_count
        );

        // Create cachedContent on first request if system prompt exists
        if let (Some(system), Some(tools)) = (request.system_full(), request.tools.as_ref())
            && !tools.is_empty()
            && let Err(e) = self.ensure_cached_content(&system, tools).await
        {
            tracing::warn!(error = %e, "failed to ensure cached content for Gemini");
        }

        let body = self.build_gemini_request(&request);
        let url = self.generate_url(&model, true);
        let retry_config = RetryConfig::default();
        // Bound ONE send's connect + TLS + response headers (#680/#682). The
        // budget is per attempt, so a retry's backoff sleep is charged to the
        // retry policy and never to this clock.
        let send_budget = crate::brain::agent::service::helpers::send_handshake_timeout_for(
            false,
            self.base_url(),
        );

        let response = retry(
            || async {
                let send = self
                    .stream_client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("x-goog-api-key", &self.api_key)
                    .json(&body)
                    .send();
                let response = match send_budget {
                    Some(budget) => match tokio::time::timeout(budget, send).await {
                        Ok(res) => res,
                        Err(_) => return Err(ProviderError::Timeout(budget.as_secs())),
                    },
                    None => send.await,
                }?;

                if !response.status().is_success() {
                    return Err(self.handle_error(response).await);
                }

                Ok(response)
            },
            &retry_config,
        )
        .await?;

        let model_clone = model.clone();
        let byte_stream = response.bytes_stream();
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        // Track accumulated state for tool calls across chunks
        let state = std::sync::Arc::new(std::sync::Mutex::new(GeminiStreamState {
            emitted_message_start: false,
            accumulated_text: String::new(),
            tool_call_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }));

        let event_stream = byte_stream
            .map(
                move |chunk_result| -> Vec<std::result::Result<StreamEvent, ProviderError>> {
                    match chunk_result {
                        Err(e) => vec![Err(ProviderError::StreamError(e.to_string()))],
                        Ok(chunk) => {
                            let text = String::from_utf8_lossy(&chunk);
                            let mut buf = buffer.lock().expect("SSE buffer lock");
                            buf.push_str(&text);

                            let mut events = Vec::new();
                            let mut st = state.lock().expect("SSE state lock");

                            while let Some(newline_pos) = buf.find('\n') {
                                let line = buf[..newline_pos].trim().to_string();
                                buf.drain(..=newline_pos);

                                let json_str = if let Some(s) = line.strip_prefix("data: ") {
                                    s
                                } else {
                                    continue;
                                };

                                if json_str == "[DONE]" {
                                    continue;
                                }

                                let json: Value = match serde_json::from_str(json_str) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        tracing::warn!(
                                            "Gemini: failed to parse SSE JSON: {} | data: {}",
                                            e,
                                            &json_str[..json_str.floor_char_boundary(200)]
                                        );
                                        continue;
                                    }
                                };

                                // Emit MessageStart once
                                if !st.emitted_message_start {
                                    st.emitted_message_start = true;
                                    events.push(Ok(StreamEvent::MessageStart {
                                        message: StreamMessage {
                                            id: format!("gemini-{}", uuid::Uuid::new_v4().simple()),
                                            model: model_clone.clone(),
                                            role: Role::Assistant,
                                            usage: TokenUsage {
                                                input_tokens: 0,
                                                output_tokens: 0,
                                                ..Default::default()
                                            },
                                        },
                                    }));
                                    events.push(Ok(StreamEvent::ContentBlockStart {
                                        index: 0,
                                        content_block: ContentBlock::Text {
                                            text: String::new(),
                                        },
                                    }));
                                }

                                let empty_candidates = vec![];
                                let candidates =
                                    json["candidates"].as_array().unwrap_or(&empty_candidates);

                                for candidate in candidates {
                                    let empty_parts = vec![];
                                    let parts = candidate["content"]["parts"]
                                        .as_array()
                                        .unwrap_or(&empty_parts);

                                    for part in parts {
                                        if let Some(text) = part["text"].as_str() {
                                            if !text.is_empty() {
                                                st.accumulated_text.push_str(text);
                                                events.push(Ok(StreamEvent::ContentBlockDelta {
                                                    index: 0,
                                                    delta: ContentDelta::TextDelta {
                                                        text: text.to_string(),
                                                    },
                                                }));
                                            }
                                        } else if part["functionCall"].is_object() {
                                            let fc = &part["functionCall"];
                                            let name =
                                                fc["name"].as_str().unwrap_or("").to_string();
                                            let args = fc["args"].clone();
                                            let id = format!(
                                                "gemini-tc-{}",
                                                uuid::Uuid::new_v4().simple()
                                            );
                                            let tool_idx = st.tool_call_count;
                                            st.tool_call_count += 1;

                                            // Emit tool use as a new content block
                                            events.push(Ok(StreamEvent::ContentBlockStop {
                                                index: 0,
                                            }));
                                            events.push(Ok(StreamEvent::ContentBlockStart {
                                                index: tool_idx + 1,
                                                content_block: ContentBlock::ToolUse {
                                                    id,
                                                    name,
                                                    input: args,
                                                },
                                            }));
                                            events.push(Ok(StreamEvent::ContentBlockStop {
                                                index: tool_idx + 1,
                                            }));
                                        }
                                    }
                                }

                                // Capture token usage from usageMetadata
                                if let Some(usage) = json["usageMetadata"].as_object() {
                                    if let Some(v) = usage.get("promptTokenCount") {
                                        st.input_tokens = v.as_u64().unwrap_or(0) as u32;
                                    }
                                    if let Some(v) = usage.get("candidatesTokenCount") {
                                        st.output_tokens = v.as_u64().unwrap_or(0) as u32;
                                    }
                                    if let Some(v) = usage.get("cachedContentTokenCount") {
                                        st.cache_read_tokens = v.as_u64().unwrap_or(0) as u32;
                                    }
                                    if let Some(v) = usage.get("cacheTokenCount") {
                                        st.cache_creation_tokens = v.as_u64().unwrap_or(0) as u32;
                                    }
                                }
                            }

                            if events.is_empty() {
                                vec![Ok(StreamEvent::Ping)]
                            } else {
                                events
                            }
                        }
                    }
                },
            )
            .flat_map(futures::stream::iter);

        Ok(Box::pin(event_stream))
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_vision(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "gemini"
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn supported_models(&self) -> Vec<String> {
        vec![
            "gemini-2.0-flash".to_string(),
            "gemini-3.1-flash-image-preview".to_string(),
            "gemini-1.5-pro".to_string(),
            "gemini-1.5-flash".to_string(),
        ]
    }

    async fn fetch_models(&self) -> Vec<String> {
        // Fetch live model list from Gemini models API
        let url = format!("{}/models?pageSize=100", GEMINI_BASE_URL);

        #[derive(serde::Deserialize)]
        struct ModelEntry {
            name: String,
        }
        #[derive(serde::Deserialize)]
        struct ModelsResponse {
            models: Option<Vec<ModelEntry>>,
        }

        match self
            .client
            .get(&url)
            .header("x-goog-api-key", &self.api_key)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<ModelsResponse>().await {
                    Ok(body) => {
                        let models: Vec<String> = body
                            .models
                            .unwrap_or_default()
                            .into_iter()
                            .map(|m| {
                                // Strip "models/" prefix from name like "models/gemini-2.0-flash"
                                m.name
                                    .strip_prefix("models/")
                                    .unwrap_or(&m.name)
                                    .to_string()
                            })
                            .filter(|m| {
                                // Only surfacing generative text/multimodal models
                                m.contains("gemini") || m.contains("gemma")
                            })
                            .collect();
                        if models.is_empty() {
                            self.supported_models()
                        } else {
                            models
                        }
                    }
                    Err(_) => self.supported_models(),
                }
            }
            _ => self.supported_models(),
        }
    }

    fn configured_context_window(&self) -> Option<u32> {
        self.configured_context_window
    }

    fn request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }

    fn stream_idle_timeout(&self) -> Option<Duration> {
        self.stream_idle_timeout
    }

    fn thinking_loop_timeout(&self) -> Option<u64> {
        self.thinking_loop_timeout
    }

    fn context_window(&self, model: &str) -> Option<u32> {
        match model {
            "gemini-2.0-flash" => Some(1_000_000),
            "gemini-3.1-flash-image-preview" => Some(1_000_000),
            "gemini-1.5-pro" => Some(2_000_000),
            "gemini-1.5-flash" => Some(1_000_000),
            _ => Some(1_000_000),
        }
    }

    fn calculate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        // Gemini 2.0 Flash pricing: $0.075/M input, $0.30/M output (≤128k)
        match model {
            m if m.contains("flash") => {
                let input_cost = (input_tokens as f64 / 1_000_000.0) * 0.075;
                let output_cost = (output_tokens as f64 / 1_000_000.0) * 0.30;
                input_cost + output_cost
            }
            m if m.contains("pro") => {
                let input_cost = (input_tokens as f64 / 1_000_000.0) * 1.25;
                let output_cost = (output_tokens as f64 / 1_000_000.0) * 5.0;
                input_cost + output_cost
            }
            _ => {
                let input_cost = (input_tokens as f64 / 1_000_000.0) * 0.075;
                let output_cost = (output_tokens as f64 / 1_000_000.0) * 0.30;
                input_cost + output_cost
            }
        }
    }
}

/// Streaming state persisted across SSE chunks
struct GeminiStreamState {
    emitted_message_start: bool,
    accumulated_text: String,
    /// Count of tool calls emitted so far this stream, used only to index
    /// successive ContentBlockStart events; the calls themselves are emitted
    /// inline, never re-read, so we track the count rather than the payloads.
    tool_call_count: usize,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

// ── Prompt caching structs ──────────────────────────────────────────────

/// Request body for POST /v1beta/cachedContents
#[derive(Serialize)]
struct CachedContentRequest {
    model: String,
    system_instruction: CachedContentSystemInstruction,
    tools: Option<Vec<CachedContentTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<String>,
}

#[derive(Serialize)]
struct CachedContentSystemInstruction {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct CachedContentTool {
    function_declarations: Vec<CachedContentFunctionDecl>,
}

#[derive(Serialize)]
struct CachedContentFunctionDecl {
    name: String,
    description: String,
    parameters: Value,
}

/// Response from POST /v1beta/cachedContents
#[derive(Deserialize)]
struct CachedContentResponse {
    name: String,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<CachedContentUsageMetadata>,
}

#[derive(Deserialize)]
struct CachedContentUsageMetadata {
    #[serde(rename = "totalTokenCount")]
    total_token_count: Option<u32>,
}
