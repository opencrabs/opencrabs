//! Core types for LLM provider abstraction
//!
//! Defines common types used across all LLM providers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Role of a message in the conversation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// User message
    User,
    /// Assistant message
    Assistant,
    /// System message (not all providers support this)
    System,
}

/// A message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Role of the message sender
    pub role: Role,
    /// Content blocks of the message
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// Create a new user message with text content
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// Create a new assistant message with text content
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// Create a new system message with text content
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

/// Content block in a message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content
    Text { text: String },
    /// Image content (base64 or URL)
    Image { source: ImageSource },
    /// Tool use request from assistant
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Tool result from user
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    /// Thinking/reasoning content (extended thinking)
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

/// Image source for image content blocks
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Base64-encoded image
    Base64 { media_type: String, data: String },
    /// Image URL
    Url { url: String },
}

/// LLM request parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMRequest {
    /// Model to use
    pub model: String,
    /// Conversation messages
    pub messages: Vec<Message>,
    /// System brain content (if supported). The STABLE, cacheable prefix —
    /// providers stamp cache_control on it, so it stays byte-stable across
    /// sessions. Volatile per-session lines live in `system_suffix` (#658).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Uncached tail: the per-session Runtime Info block (model / provider /
    /// working dir / date) split out of the brain so it can't poison the cached
    /// prefix. Cache-capable providers place it after the cache breakpoint;
    /// others concatenate it via [`LLMRequest::system_full`] (#658).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_suffix: Option<String>,
    /// Available tools
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// Temperature (0.0-1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum tokens to generate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Whether to stream the response
    #[serde(skip)]
    pub stream: bool,
    /// Additional metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
    /// Working directory for proxy-aware providers (not serialized to API)
    #[serde(skip)]
    pub working_directory: Option<String>,
    /// Session ID — used by CLI providers to isolate sessions via --session-id
    #[serde(skip)]
    pub session_id: Option<Uuid>,
}

impl LLMRequest {
    /// Create a new LLM request
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            system: None,
            system_suffix: None,
            tools: None,
            temperature: None,
            max_tokens: None,
            stream: false,
            metadata: None,
            working_directory: None,
            session_id: None,
        }
    }

    /// The full system prompt: stable brain plus the uncached runtime suffix.
    /// Providers that don't do prefix caching use this so the model still sees
    /// the runtime info; it is byte-identical to the pre-split single-string
    /// system prompt (#658).
    pub fn system_full(&self) -> Option<String> {
        match (&self.system, &self.system_suffix) {
            (Some(s), Some(suffix)) if !suffix.trim().is_empty() => {
                Some(format!("{s}\n\n{suffix}"))
            }
            (Some(s), _) => Some(s.clone()),
            (None, Some(suffix)) if !suffix.trim().is_empty() => Some(suffix.clone()),
            (None, _) => None,
        }
    }

    /// Set system brain content as a SINGLE string.
    ///
    /// The #658 cache split (pulling the Runtime Info block into `system_suffix`
    /// so the stable prefix caches) is DISABLED: it made providers send a 2-part
    /// (array) system message, and OpenAI-compatible endpoints — qwen Model
    /// Studio, modelscope, zhipu, and the other fallbacks — mishandle an
    /// array-shaped system prompt and stop emitting tool calls, so the agent
    /// narrated endlessly and never acted (#693). Sending the whole system as one
    /// string restores the request shape the model reliably called tools with.
    /// The prompt-cache hit is sacrificed for a working agent. `system_suffix`
    /// stays `None` so every provider takes its single-string path.
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self.system_suffix = None;
        self
    }

    /// Set tools
    pub fn with_tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Set temperature
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Set max tokens
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Enable streaming
    pub fn with_streaming(mut self) -> Self {
        self.stream = true;
        self
    }
}

/// Tool definition for LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: String,
    /// Input schema (JSON Schema)
    pub input_schema: serde_json::Value,
}

/// LLM response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMResponse {
    /// Response ID
    pub id: String,
    /// Model used
    pub model: String,
    /// Content blocks
    pub content: Vec<ContentBlock>,
    /// Stop reason
    pub stop_reason: Option<StopReason>,
    /// Token usage
    pub usage: TokenUsage,
    /// Sum of active-streaming windows for this response, in seconds.
    /// Excludes idle gaps >1s between content deltas (which would be
    /// tool execution, approval waits, or between-message-block
    /// round-trips). Used as the denominator for an accurate
    /// tokens-per-second display so the channel footer reports the
    /// model's actual sustained generation rate, not output-tokens
    /// divided by full turn wall-clock (which silently halves the rate
    /// on every tool-heavy turn). Only the streaming path populates
    /// this; non-streaming `parse_response` sites leave it `None`.
    #[serde(default)]
    pub streaming_active_secs: Option<f64>,
    /// True when the provider detected tool-call JSON dumped as plain text
    /// and could not recover it into structured ToolUse blocks (rescue
    /// failed). Raw call-shaped JSON is stripped from `content` at the
    /// parse boundary; the tool loop uses this flag for one corrective
    /// retry, then fails clean instead of delivering the residue.
    /// (fork #66, ex-upstream opencrabs/opencrabs#1260)
    #[serde(default)]
    pub tool_text_leak: bool,
}

/// Reason why the model stopped generating
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Natural end of response
    EndTurn,
    /// Hit max tokens
    MaxTokens,
    /// Stop sequence encountered
    StopSequence,
    /// Tool use requested
    ToolUse,
}

/// Token usage information
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input tokens (non-cached)
    pub input_tokens: u32,
    /// Output tokens
    pub output_tokens: u32,
    /// Reasoning tokens billed inside `output_tokens` (provider
    /// `usage.reasoning_tokens`). Zero for providers that don't split
    /// reasoning out. Lets the tool loop reconcile billed output against
    /// visible text — `output - reasoning ≈ visible tokens` — instead of
    /// letting thinking-heavy turns fake a usage gap (#36).
    #[serde(default)]
    pub reasoning_tokens: u32,
    /// Cache creation input tokens (Anthropic-specific)
    #[serde(default)]
    pub cache_creation_tokens: u32,
    /// Cache read input tokens (Anthropic-specific)
    #[serde(default)]
    pub cache_read_tokens: u32,
    /// True when the provider's response carried no cache-read field at all, so
    /// the zero in `cache_read_tokens` means "not reported", not "none" (#2139).
    /// Stored inverted so the derived default (false) keeps every provider that
    /// does not set it recording its value.
    #[serde(default)]
    pub cache_read_unreported: bool,
    /// Billing: cumulative cache creation across all CLI tool rounds.
    /// For non-CLI providers or single-round calls, this stays 0 and
    /// cache_creation_tokens is used for both context and billing.
    #[serde(default)]
    pub billing_cache_creation: u32,
    /// Billing: cumulative cache read across all CLI tool rounds.
    #[serde(default)]
    pub billing_cache_read: u32,
    /// Provider-reported cost for this call, in USD — e.g. OpenRouter's
    /// `usage.cost`, LiteLLM-style gateways. Where a provider tells us what
    /// it charged, that is the invoice; the local pricing table is only a
    /// guess about it, and a wrong guess is the drift reported in #1707.
    /// Accounting prefers this over the table computation when present.
    /// `None` = provider doesn't report dollars. `Some(0.0)` = the provider
    /// genuinely charged nothing (free model) — not a missing field.
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// Pick the cost a turn writes to the ledger. `reported_sum` is the total of
/// provider-reported dollars across the counted iterations (None when no
/// iteration reported); `missing_cost_iters` counts iterations that reported
/// no cost — even one makes the partial sum untrustworthy and the table wins.
/// `table_cost` is the local pricing-table computation (#1707).
pub fn authoritative_cost(
    reported_sum: Option<f64>,
    missing_cost_iters: u32,
    table_cost: f64,
) -> f64 {
    match (reported_sum, missing_cost_iters) {
        (Some(reported), 0) => reported,
        _ => table_cost,
    }
}

impl TokenUsage {
    /// Non-cached tokens only — for context window tracking.
    pub fn total(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }

    /// All tokens including cache — for billing/usage display.
    /// Uses cumulative billing fields when available (CLI multi-round),
    /// falls back to per-call cache fields for single-round providers.
    pub fn billable_input(&self) -> u32 {
        let cc = if self.billing_cache_creation > 0 {
            self.billing_cache_creation
        } else {
            self.cache_creation_tokens
        };
        let cr = if self.billing_cache_read > 0 {
            self.billing_cache_read
        } else {
            self.cache_read_tokens
        };
        self.input_tokens + cc + cr
    }

    /// Context window tokens — per-call cache values representing
    /// the actual prompt size sent to the model on the last API call.
    pub fn context_input(&self) -> u32 {
        self.input_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }

    /// Total billable tokens (input + cache + output).
    pub fn billable_total(&self) -> u32 {
        self.billable_input() + self.output_tokens
    }
}

/// Streaming event from LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Stream started
    MessageStart { message: StreamMessage },
    /// Content block started
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    /// Content block delta (incremental update)
    ContentBlockDelta { index: usize, delta: ContentDelta },
    /// Content block stopped
    ContentBlockStop { index: usize },
    /// Message completed
    MessageDelta {
        delta: MessageDelta,
        usage: TokenUsage,
    },
    /// Stream finished
    MessageStop,
    /// Ping event (keep-alive)
    Ping,
    /// Error event
    Error { error: String },
    /// Claude CLI background-task lifecycle event, surfaced from the CLI's
    /// system channel (#1776). Informational: consumers may ignore it like
    /// [`StreamEvent::Ping`].
    BackgroundTask {
        /// `task_started` | `task_notification` | `background_tasks_changed`
        subtype: String,
        task_id: Option<String>,
        status: Option<String>,
        description: Option<String>,
        output_file: Option<String>,
    },
}

/// Partial message information at stream start
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMessage {
    pub id: String,
    pub model: String,
    pub role: Role,
    pub usage: TokenUsage,
}

/// Content delta for streaming updates
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentDelta {
    /// Text delta
    TextDelta { text: String },
    /// Tool input delta (JSON)
    InputJsonDelta { partial_json: String },
    /// Reasoning/thinking content delta (display-only, not part of response text)
    ReasoningDelta { text: String },
    /// Anthropic native thinking delta (extended thinking — same as reasoning but uses `thinking` field)
    ThinkingDelta { thinking: String },
}

/// Message delta for final updates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDelta {
    pub stop_reason: Option<StopReason>,
    pub stop_sequence: Option<String>,
}
