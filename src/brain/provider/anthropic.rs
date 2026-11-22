//! Anthropic (Claude) Provider Implementation
//!
//! Implements the Provider trait for Anthropic's Claude models.
//! Uses standard API key auth via `x-api-key` header.
//!
//! ## Supported Models
//! - claude-opus-4-6
//! - claude-sonnet-4-5-20250929
//! - claude-haiku-4-5-20251001
//! - claude-3-5-sonnet-20241022
//! - claude-3-5-haiku-20241022
//! - claude-3-opus-20240229 (legacy)
//! - claude-3-sonnet-20240229 (legacy)
//! - claude-3-haiku-20240307 (legacy)

use super::error::{ProviderError, Result};
use super::r#trait::{Provider, ProviderStream};
use super::types::*;
use async_trait::async_trait;
use futures::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_MODELS_URL: &str = "https://api.anthropic.com/v1/models";
const ANTHROPIC_VERSION: &str = "2023-06-01";
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300); // Total request timeout

/// Anthropic provider for Claude models
#[derive(Clone)]
pub struct AnthropicProvider {
    api_key: String,
    client: Client,
    /// Client for `stream()`: no total wall-clock ceiling, so a healthy long
    /// stream is never cut (#1687).
    stream_client: Client,
    /// The resolved `[providers.anthropic] timeout_secs` / `[agent] timeout_secs`
    /// (#1688), mirrored from the client `with_timeout` rebuilt. `None` means
    /// the compiled `DEFAULT_TIMEOUT` is what the request client carries.
    request_timeout: Option<Duration>,
    /// The resolved `stream_idle_timeout_secs` at either tier. Inter-chunk
    /// silence is enforced in `brain/agent/service/helpers.rs`, which asks the
    /// provider for this; `None` defers to that file's runtime table.
    stream_idle_timeout: Option<Duration>,
    /// The resolved thinking-loop guard ceiling in seconds (#1690), from
    /// `[providers.anthropic]` then `[agent]`. `Some(0)` disables the guard for
    /// this provider, which is why this is seconds rather than a `Duration`.
    thinking_loop_timeout: Option<u64>,
    custom_default_model: Option<String>,
    /// User override from `providers.anthropic.context_window` in config.toml.
    /// When set, becomes the compaction budget (overrides agent.context_limit).
    configured_context_window: Option<u32>,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider
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
            custom_default_model: None,
            configured_context_window: None,
        }
    }

    /// Create with a custom HTTP client for NON-STREAMING calls.
    ///
    /// The stream client is always the total-timeout-free one from
    /// `utils::http::streaming_transport`, so an injected client can never
    /// reintroduce the #1687 ceiling on an SSE body.
    pub fn with_client(api_key: String, client: Client) -> Self {
        Self {
            api_key,
            client,
            stream_client: crate::utils::http::streaming_transport(),
            // An injected client's ceiling is opaque (reqwest does not read it
            // back), so the accessor reports "no user override" rather than
            // guessing a number.
            request_timeout: None,
            stream_idle_timeout: None,
            thinking_loop_timeout: None,
            custom_default_model: None,
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
    /// provider. The factory resolves `[providers.anthropic]` → `[agent]` and
    /// always calls this, so a provider built from config never falls back to
    /// the `[agent]` read in `brain/agent/service/helpers.rs`.
    pub fn with_thinking_loop_timeout(mut self, secs: u64) -> Self {
        self.thinking_loop_timeout = Some(secs);
        self
    }

    /// Set custom default model
    pub fn with_default_model(mut self, model: String) -> Self {
        self.custom_default_model = Some(model);
        self
    }

    /// Override the context-window budget from `providers.anthropic.context_window`.
    pub fn with_context_window(mut self, context_window: u32) -> Self {
        self.configured_context_window = Some(context_window);
        self
    }

    /// Build request headers with prompt-caching beta header.
    pub(crate) fn headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();

        headers.insert(
            "x-api-key",
            self.api_key.parse().expect("Invalid API key format"),
        );
        headers.insert(
            "anthropic-beta",
            "prompt-caching-2024-07-31"
                .parse()
                .expect("Invalid beta header"),
        );
        headers.insert(
            "anthropic-version",
            ANTHROPIC_VERSION.parse().expect("Invalid version"),
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().expect("valid content-type"),
        );
        headers
    }

    /// Build request headers.
    fn request_headers(&self, _request: &LLMRequest) -> reqwest::header::HeaderMap {
        self.headers()
    }

    /// Convert our generic request to Anthropic format with prompt caching.
    /// The system prompt and tools are marked with cache_control so the API
    /// caches them across turns, cutting input token costs by 30-50%.
    fn to_anthropic_request(&self, request: LLMRequest) -> AnthropicRequest {
        let cache = AnthropicCacheControl {
            cache_type: "ephemeral".to_string(),
        };

        // Convert system to cacheable blocks: the stable brain carries
        // cache_control (cached prefix); the volatile runtime suffix rides as a
        // second block AFTER it with no cache_control, so per-session runtime
        // info can't invalidate the cached brain across sessions (#658).
        let suffix = request.system_suffix.filter(|x| !x.trim().is_empty());
        let system = request.system.map(|s| {
            let mut blocks = vec![AnthropicSystemBlock {
                block_type: "text".to_string(),
                text: s,
                cache_control: Some(cache.clone()),
            }];
            if let Some(suffix) = suffix {
                blocks.push(AnthropicSystemBlock {
                    block_type: "text".to_string(),
                    text: suffix,
                    cache_control: None,
                });
            }
            AnthropicSystem::Blocks(blocks)
        });

        // Convert tools with cache_control on the last tool
        let tools = request.tools.map(|tools| {
            let len = tools.len();
            tools
                .into_iter()
                .enumerate()
                .map(|(i, t)| AnthropicTool {
                    name: t.name,
                    description: t.description,
                    input_schema: t.input_schema,
                    cache_control: if i == len - 1 {
                        Some(cache.clone())
                    } else {
                        None
                    },
                })
                .collect()
        });

        AnthropicRequest {
            model: request.model,
            messages: request.messages,
            system,
            max_tokens: request.max_tokens.unwrap_or(16384),
            temperature: request.temperature,
            tools,
            stream: Some(request.stream),
            metadata: request.metadata,
        }
    }

    /// Convert Anthropic response to our generic format
    #[allow(clippy::wrong_self_convention)]
    fn from_anthropic_response(&self, response: AnthropicResponse) -> LLMResponse {
        LLMResponse {
            id: response.id,
            model: response.model,
            content: response.content,
            stop_reason: response.stop_reason,
            usage: response.usage,
            // Non-streaming path — active streaming time only meaningful for stream_complete.
            streaming_active_secs: None,
            tool_text_leak: false,
        }
    }

    /// Handle API error response
    async fn handle_error(&self, response: reqwest::Response) -> ProviderError {
        let status = response.status().as_u16();

        // Extract Retry-After header for rate limits
        let retry_after = response.headers().get("retry-after").and_then(|v| {
            v.to_str().ok().and_then(|s| {
                // Retry-After can be either seconds or HTTP date
                // Try parsing as seconds first
                s.parse::<u64>().ok()
            })
        });

        // Try to parse error body
        let body_bytes = response.bytes().await.unwrap_or_default();
        tracing::debug!(
            "Anthropic error response ({}): {}",
            status,
            String::from_utf8_lossy(&body_bytes)
                .chars()
                .take(500)
                .collect::<String>()
        );
        if let Ok(error_body) = serde_json::from_slice::<AnthropicError>(&body_bytes) {
            let message = if status == 429 {
                super::error::rate_limit_message(&error_body.error.message, retry_after)
            } else {
                error_body.error.message
            };

            return if status == 429 {
                ProviderError::RateLimitExceeded(message)
            } else {
                ProviderError::ApiError {
                    status,
                    message,
                    error_type: Some(error_body.error.error_type),
                }
            };
        }

        // Fallback error
        if status == 429 {
            let message = if let Some(secs) = retry_after {
                format!("Rate limit exceeded (retry after {} seconds)", secs)
            } else {
                "Rate limit exceeded, please retry later".to_string()
            };
            ProviderError::RateLimitExceeded(message)
        } else {
            ProviderError::ApiError {
                status,
                message: "Unknown error".to_string(),
                error_type: None,
            }
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn complete(&self, request: LLMRequest) -> Result<LLMResponse> {
        use crate::utils::retry::{RetryConfig, retry};

        let model = request.model.clone();
        let message_count = request.messages.len();
        tracing::info!(
            "Anthropic API request: model={}, messages={}, max_tokens={}",
            model,
            message_count,
            request.max_tokens.unwrap_or(16384)
        );

        let req_headers = self.request_headers(&request);
        let anthropic_request = self.to_anthropic_request(request);
        let retry_config = RetryConfig::default();

        // Retry the entire API call with exponential backoff
        let result = retry(
            || async {
                tracing::debug!("Sending request to Anthropic API");
                let response = self
                    .client
                    .post(ANTHROPIC_API_URL)
                    .headers(req_headers.clone())
                    .json(&anthropic_request)
                    .send()
                    .await?;

                let status = response.status();
                tracing::debug!("Anthropic API response status: {}", status);

                if !status.is_success() {
                    return Err(self.handle_error(response).await);
                }

                let anthropic_response: AnthropicResponse = response.json().await?;
                let llm_response = self.from_anthropic_response(anthropic_response);

                tracing::info!(
                    "Anthropic API response: input_tokens={}, output_tokens={}, stop_reason={:?}",
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
            tracing::error!("Anthropic API request failed: {}", e);
        }

        result
    }

    async fn stream(&self, request: LLMRequest) -> Result<ProviderStream> {
        use crate::utils::retry::{RetryConfig, retry};

        let model = request.model.clone();
        let message_count = request.messages.len();
        tracing::info!(
            "Anthropic streaming request: model={}, messages={}",
            model,
            message_count
        );

        let req_headers = self.request_headers(&request);
        let mut anthropic_request = self.to_anthropic_request(request);
        anthropic_request.stream = Some(true);
        let retry_config = RetryConfig::default();
        // Bound ONE send's connect + TLS + response headers (#680/#682). The
        // budget is per attempt, so a retry's backoff sleep is charged to the
        // retry policy and never to this clock.
        let send_budget = crate::brain::agent::service::helpers::send_handshake_timeout_for(
            false,
            self.base_url(),
        );

        // Retry the stream connection establishment
        let response = retry(
            || async {
                let send = self
                    .stream_client
                    .post(ANTHROPIC_API_URL)
                    .headers(req_headers.clone())
                    .json(&anthropic_request)
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

        // Parse Server-Sent Events stream with cross-chunk buffering.
        // TCP chunks can split SSE events, so we buffer partial lines.
        let byte_stream = response.bytes_stream();
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        let event_stream = byte_stream
            .map(
                move |chunk_result| -> Vec<std::result::Result<StreamEvent, ProviderError>> {
                    match chunk_result {
                        Err(e) => vec![Err(ProviderError::StreamError(e.to_string()))],
                        Ok(chunk) => {
                            let text = String::from_utf8_lossy(&chunk);
                            let mut buf = buffer.lock().expect("SSE buffer lock poisoned");
                            buf.push_str(&text);

                            let mut events = Vec::new();

                            // Process complete lines (terminated by \n)
                            while let Some(newline_pos) = buf.find('\n') {
                                let line = buf[..newline_pos].trim().to_string();
                                buf.drain(..=newline_pos);

                                if let Some(json_str) = line.strip_prefix("data: ") {
                                    if json_str == "[DONE]" {
                                        continue;
                                    }
                                    match serde_json::from_str::<StreamEvent>(json_str) {
                                        Ok(event) => events.push(Ok(event)),
                                        Err(e) => {
                                            tracing::warn!(
                                                "Failed to parse SSE event JSON: {}. Data: {}",
                                                e,
                                                json_str.chars().take(200).collect::<String>()
                                            );
                                            // Don't propagate parse errors for individual events
                                        }
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
        "anthropic"
    }

    fn default_model(&self) -> &str {
        self.custom_default_model
            .as_deref()
            .unwrap_or("claude-opus-4-8")
    }

    fn supported_models(&self) -> Vec<String> {
        // Fallback list, used only when the live /v1/models fetch fails. Keep the
        // current models here so a fetch failure doesn't hide Fable / Opus 4.8.
        vec![
            // Claude 4.x models
            "claude-fable-5".to_string(),
            "claude-opus-4-8".to_string(),
            "claude-opus-4-7".to_string(),
            "claude-opus-4-6".to_string(),
            "claude-sonnet-4-6".to_string(),
            "claude-sonnet-4-5-20250929".to_string(),
            "claude-haiku-4-5-20251001".to_string(),
            // Claude 3.5 models
            "claude-3-5-sonnet-20241022".to_string(),
            "claude-3-5-haiku-20241022".to_string(),
            // Claude 3 models (legacy)
            "claude-3-opus-20240229".to_string(),
            "claude-3-sonnet-20240229".to_string(),
            "claude-3-5-sonnet-20240620".to_string(),
            "claude-3-haiku-20240307".to_string(),
        ]
    }

    async fn fetch_models(&self) -> Vec<String> {
        #[derive(Deserialize)]
        struct ModelEntry {
            id: String,
        }
        #[derive(Deserialize)]
        struct ModelsResponse {
            data: Vec<ModelEntry>,
        }

        let req = self
            .client
            .get(ANTHROPIC_MODELS_URL)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("x-api-key", &self.api_key)
            .header("anthropic-beta", "prompt-caching-2024-07-31");

        match req.send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<ModelsResponse>().await {
                Ok(body) => {
                    let mut models: Vec<String> = body.data.into_iter().map(|m| m.id).collect();
                    models.sort();
                    if models.is_empty() {
                        return self.supported_models();
                    }
                    models
                }
                Err(_) => self.supported_models(),
            },
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
            // 1M-context models (Fable 5, Opus 4.6+, Sonnet 4.6).
            "claude-fable-5" => Some(1_000_000),
            "claude-opus-4-8" => Some(1_000_000),
            "claude-opus-4-7" => Some(1_000_000),
            "claude-opus-4-6" => Some(1_000_000),
            "claude-sonnet-4-6" => Some(1_000_000),
            "claude-sonnet-4-5-20250929" => Some(200_000),
            "claude-haiku-4-5-20251001" => Some(200_000),
            "claude-3-5-sonnet-20241022" => Some(200_000),
            "claude-3-5-haiku-20241022" => Some(200_000),
            "claude-3-opus-20240229" => Some(200_000),
            "claude-3-sonnet-20240229" => Some(200_000),
            "claude-3-5-sonnet-20240620" => Some(200_000),
            "claude-3-haiku-20240307" => Some(200_000),
            _ => None,
        }
    }

    fn calculate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        crate::usage::pricing::PricingConfig::load()
            .map(|cfg| cfg.calculate_cost(model, input_tokens, output_tokens))
            .unwrap_or(0.0)
    }
}

// Anthropic-specific request format with prompt caching support
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<Message>,
    /// System can be a string or an array of content blocks (for caching).
    /// We use a custom serializer to decide based on whether caching is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<AnthropicSystem>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<std::collections::HashMap<String, String>>,
}

/// System prompt — either a plain string or a list of content blocks for caching.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AnthropicSystem {
    Blocks(Vec<AnthropicSystemBlock>),
}

/// A system content block — text with optional cache control.
#[derive(Debug, Serialize)]
struct AnthropicSystemBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<AnthropicCacheControl>,
}

/// Tool definition with optional cache control on the last tool.
#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<AnthropicCacheControl>,
}

/// Ephemeral cache control marker used by Anthropic prompt caching.
#[derive(Debug, Serialize, Clone)]
struct AnthropicCacheControl {
    #[serde(rename = "type")]
    cache_type: String,
}

// Anthropic-specific response format
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    id: String,
    model: String,
    content: Vec<ContentBlock>,
    stop_reason: Option<StopReason>,
    usage: TokenUsage,
}

// Anthropic error format
#[derive(Debug, Deserialize)]
struct AnthropicError {
    error: AnthropicErrorDetail,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetail {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}
