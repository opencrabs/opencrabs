//! Responses API response adapter: non-streaming `POST /v1/responses` body
//! -> [`LLMResponse`].
//!
//! Wire mapping:
//! - `output[]` items of `type: "message"` flatten their `content` parts into
//!   [`ContentBlock::Text`] per `output_text` part.
//! - `type: "function_call"` items become [`ContentBlock::ToolUse`] keyed by
//!   `call_id` (the Responses replacement for chat's `tool_calls[].id`),
//!   with `arguments` parsed through the same partial-repair fallback as the
//!   chat path so a truncated call surfaces partial intent instead of `{}`.
//! - `type: "reasoning"` items are dropped: the chat path drops
//!   `reasoning_content` on responses too, and nothing downstream consumes a
//!   Thinking block on the response side today.
//! - Stop reason: `status: "incomplete"` with
//!   `incomplete_details.reason == "max_output_tokens"` wins (the Responses
//!   equivalent of chat `finish_reason: "length"`); otherwise any
//!   `function_call` item means [`StopReason::ToolUse`] (chat's
//!   `finish_reason: "tool_calls"`); otherwise [`StopReason::EndTurn`].
//! - Usage: `input_tokens` / `output_tokens` straight across,
//!   `output_tokens_details.reasoning_tokens` into the reasoning split (#36),
//!   `input_tokens_details.cached_tokens` into cache-read.
//!
//! `status: "failed"` bodies carry a top-level `error` object; that is the
//! dispatch layer's job to raise (the adapter stays a pure conversion).

use super::custom_openai_compatible::stable_model_id;
use super::types::{ContentBlock, LLMResponse, StopReason, TokenUsage};

/// The Responses API non-streaming response body.
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
pub(crate) struct ResponsesResponseBody {
    pub(crate) id: String,
    pub(crate) model: String,
    pub(crate) status: Option<String>,
    pub(crate) incomplete_details: Option<IncompleteDetails>,
    pub(crate) output: Vec<ResponsesOutputItem>,
    pub(crate) usage: ResponsesUsage,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub(crate) struct IncompleteDetails {
    pub(crate) reason: Option<String>,
}

/// One typed entry of the `output` array. Unknown types are preserved as
/// [`ResponsesOutputItem::Unknown`] so a new item type degrades to "ignored"
/// instead of failing the whole body.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponsesOutputItem {
    Message {
        #[serde(default)]
        content: Vec<ResponsesContentPart>,
    },
    FunctionCall {
        #[serde(default)]
        call_id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        arguments: String,
    },
    /// Deliberately dropped (see module docs).
    Reasoning,
    #[serde(other)]
    Unknown,
}

/// A content part inside a `message` output item.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponsesContentPart {
    OutputText {
        #[serde(default)]
        text: String,
    },
    /// Refusals and any future part type degrade to "ignored" here; a refusal
    /// turn surfaces as empty content rather than an error.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
pub(crate) struct ResponsesUsage {
    pub(crate) input_tokens: u32,
    pub(crate) output_tokens: u32,
    pub(crate) input_tokens_details: Option<InputTokensDetails>,
    pub(crate) output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
pub(crate) struct InputTokensDetails {
    pub(crate) cached_tokens: u32,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
pub(crate) struct OutputTokensDetails {
    pub(crate) reasoning_tokens: u32,
}

/// Map a Responses `usage` object onto the shared [`TokenUsage`].
///
/// Shared with the streaming adapter so both dialects report the same
/// splits (`reasoning_tokens`, `cached_tokens`) from the same wire shape.
pub(crate) fn usage_to_token_usage(usage: ResponsesUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_tokens: usage
            .output_tokens_details
            .map(|d| d.reasoning_tokens)
            .unwrap_or(0),
        cache_read_tokens: usage
            .input_tokens_details
            .map(|d| d.cached_tokens)
            .unwrap_or(0),
        // No provider-reported cost field on this dialect.
        cost_usd: None,
        ..Default::default()
    }
}

/// Parse a raw JSON body into an [`LLMResponse`].
pub(crate) fn parse_responses_json(
    raw: serde_json::Value,
    requested_model: &str,
) -> Result<LLMResponse, serde_json::Error> {
    let body: ResponsesResponseBody = serde_json::from_value(raw)?;
    Ok(convert(body, requested_model))
}

fn convert(body: ResponsesResponseBody, requested_model: &str) -> LLMResponse {
    let mut content: Vec<ContentBlock> = Vec::new();
    let mut has_function_call = false;

    for item in body.output {
        match item {
            ResponsesOutputItem::Message { content: parts } => {
                for part in parts {
                    if let ResponsesContentPart::OutputText { text } = part {
                        content.push(ContentBlock::Text { text });
                    }
                }
            }
            ResponsesOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                has_function_call = true;
                let input = super::json_repair::parse_or_repair(&arguments);
                content.push(ContentBlock::ToolUse {
                    id: call_id,
                    name,
                    input,
                });
            }
            // Dropped: no consumer for response-side reasoning today.
            ResponsesOutputItem::Reasoning | ResponsesOutputItem::Unknown => {}
        }
    }

    let stop_reason = if body.status.as_deref() == Some("incomplete")
        && body
            .incomplete_details
            .as_ref()
            .and_then(|d| d.reason.as_deref())
            == Some("max_output_tokens")
    {
        Some(StopReason::MaxTokens)
    } else if has_function_call {
        Some(StopReason::ToolUse)
    } else {
        Some(StopReason::EndTurn)
    };

    let usage = usage_to_token_usage(body.usage);
    LLMResponse {
        id: body.id,
        model: stable_model_id(body.model, requested_model),
        content,
        stop_reason,
        usage,
        // Non-streaming parse path.
        streaming_active_secs: None,
        tool_text_leak: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: serde_json::Value) -> LLMResponse {
        parse_responses_json(v, "gpt-6").expect("parse")
    }

    #[test]
    fn message_output_text_flattens_into_text_blocks() {
        let r = parse(serde_json::json!({
            "id": "resp_1",
            "model": "gpt-6-2026-01",
            "status": "completed",
            "output": [
                { "type": "reasoning", "summary": [] },
                { "type": "message", "role": "assistant", "content": [
                    { "type": "output_text", "text": "hello ", "annotations": [] },
                    { "type": "output_text", "text": "world", "annotations": [] }
                ]}
            ],
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }));
        assert_eq!(r.id, "resp_1");
        // The requested id wins: a gateway's backend-local model name must
        // not replace the identifier the next turn keys on.
        assert_eq!(r.model, "gpt-6");
        let texts: Vec<&str> = r
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["hello ", "world"], "reasoning item dropped");
        assert_eq!(r.stop_reason, Some(StopReason::EndTurn));
    }

    #[test]
    fn function_call_becomes_tool_use_with_parsed_arguments() {
        let r = parse(serde_json::json!({
            "id": "resp_2",
            "model": "gpt-6",
            "status": "completed",
            "output": [
                { "type": "function_call", "call_id": "call_ab", "name": "get_weather",
                  "arguments": "{\"city\":\"Coimbra\"}" }
            ],
            "usage": { "input_tokens": 8, "output_tokens": 12 }
        }));
        assert_eq!(r.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(r.content.len(), 1);
        match &r.content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_ab", "call_id is the block id");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "Coimbra");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn truncated_arguments_repair_to_partial_intent() {
        let r = parse(serde_json::json!({
            "id": "resp_3",
            "model": "gpt-6",
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [
                { "type": "function_call", "call_id": "call_x", "name": "f",
                  "arguments": "{\"a\": 1, \"b\": \"trun" }
            ],
            "usage": { "input_tokens": 8, "output_tokens": 16 }
        }));
        // max_output_tokens wins over the tool-call presence.
        assert_eq!(r.stop_reason, Some(StopReason::MaxTokens));
        match &r.content[0] {
            ContentBlock::ToolUse { input, .. } => {
                assert_eq!(input["a"], 1, "partial JSON repaired, not dropped to {{}}");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn usage_maps_reasoning_and_cached_tokens() {
        let r = parse(serde_json::json!({
            "id": "resp_4",
            "model": "gpt-6",
            "status": "completed",
            "output": [],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "input_tokens_details": { "cached_tokens": 40 },
                "output_tokens_details": { "reasoning_tokens": 30 }
            }
        }));
        assert_eq!(r.usage.input_tokens, 100);
        assert_eq!(r.usage.output_tokens, 50);
        assert_eq!(r.usage.reasoning_tokens, 30);
        assert_eq!(r.usage.cache_read_tokens, 40);
        assert_eq!(r.usage.cost_usd, None);
    }

    #[test]
    fn unknown_output_items_and_parts_are_ignored_not_fatal() {
        let r = parse(serde_json::json!({
            "id": "resp_5",
            "model": "gpt-6",
            "status": "completed",
            "output": [
                { "type": "web_search_call", "id": "ws_1" },
                { "type": "message", "role": "assistant", "content": [
                    { "type": "refusal", "refusal": "no" },
                    { "type": "output_text", "text": "fine" }
                ]}
            ],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }));
        let texts: Vec<&str> = r
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["fine"]);
        assert_eq!(r.stop_reason, Some(StopReason::EndTurn));
    }

    #[test]
    fn empty_output_yields_empty_content_and_end_turn() {
        let r = parse(serde_json::json!({
            "id": "resp_6",
            "model": "gpt-6",
            "status": "completed",
            "output": [],
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        }));
        assert!(r.content.is_empty());
        assert_eq!(r.stop_reason, Some(StopReason::EndTurn));
    }

    #[test]
    fn a_gateway_backend_model_id_never_replaces_the_requested_one() {
        let r = parse(serde_json::json!({
            "id": "resp_7",
            "model": "ft:gpt-6:backend-local-abc",
            "status": "completed",
            "output": [],
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        }));
        assert_eq!(r.model, "gpt-6");
    }
}
