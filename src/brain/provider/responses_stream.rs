//! Responses API SSE adapter: one `(event, data)` pair -> [`StreamEvent`]s.
//!
//! The Responses API is a typed event stream, not chat's anonymous
//! `choices[].delta` chunks. Each SSE frame carries an event name plus a JSON
//! body, and the caller feeds those here. This module is pure: it holds no
//! state and does no I/O, so the framing (line splitting, `data:` prefix,
//! `[DONE]`) stays in the dispatch layer and the mapping is unit-testable.
//!
//! Wire mapping:
//! - `response.created` -> [`StreamEvent::MessageStart`] (id/model from the
//!   response object, role assistant, zero usage).
//! - `response.output_item.added` -> [`StreamEvent::ContentBlockStart`] at
//!   `output_index`: `message` opens a text block, `function_call` opens a
//!   tool block (id from `call_id`), `reasoning` opens a thinking block.
//!   Unknown item types are ignored.
//! - `response.output_item.done` -> [`StreamEvent::ContentBlockStop`] so a
//!   block's accumulated JSON is finalized exactly once.
//! - `response.output_text.delta` -> [`ContentDelta::TextDelta`].
//! - `response.function_call_arguments.delta` -> [`ContentDelta::InputJsonDelta`].
//! - `response.reasoning_summary_text.delta` / `response.reasoning_text.delta`
//!   -> [`ContentDelta::ReasoningDelta`] (display only).
//! - `response.completed` -> [`StreamEvent::MessageDelta`] (stop reason from
//!   status/output, usage mapped through the shared helper) then
//!   [`StreamEvent::MessageStop`].
//! - `response.incomplete` -> same, with max-tokens stop when the reason says
//!   `max_output_tokens` or the body carries no explicit reason.
//! - `response.failed` / `error` -> [`StreamEvent::Error`] then
//!   [`StreamEvent::MessageStop`], never a silent hang.

use serde_json::Value;

use super::responses_response::{ResponsesUsage, usage_to_token_usage};
use super::types::{
    ContentBlock, ContentDelta, MessageDelta, Role, StopReason, StreamEvent, StreamMessage,
    TokenUsage,
};

/// Translate one SSE frame into the stream events it represents.
///
/// An empty return means "nothing to surface", which is the correct handling
/// for keep-alives, bookkeeping events (`response.in_progress`) and any event
/// name this adapter does not know yet.
pub(crate) fn effects_for_event(event: &str, data: &Value) -> Vec<StreamEvent> {
    match event {
        "response.created" => vec![StreamEvent::MessageStart {
            message: StreamMessage {
                id: nested_str(data, &["response", "id"]),
                model: nested_str(data, &["response", "model"]),
                role: Role::Assistant,
                usage: TokenUsage::default(),
            },
        }],
        "response.output_item.added" => block_start(data),
        "response.output_item.done" => match nested_index(data, &["output_index"]) {
            Some(index) => vec![StreamEvent::ContentBlockStop { index }],
            None => Vec::new(),
        },
        "response.output_text.delta" => delta_event(data, |text| ContentDelta::TextDelta { text }),
        "response.function_call_arguments.delta" => delta_event(data, |partial_json| {
            ContentDelta::InputJsonDelta { partial_json }
        }),
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            delta_event(data, |text| ContentDelta::ReasoningDelta { text })
        }
        "response.completed" | "response.incomplete" => finish(data),
        "response.failed" | "error" => error_event(data),
        _ => Vec::new(),
    }
}

/// Open a content block for an `output_item.added` frame.
fn block_start(data: &Value) -> Vec<StreamEvent> {
    let Some(index) = nested_index(data, &["output_index"]) else {
        return Vec::new();
    };
    let item = data.get("item").cloned().unwrap_or(Value::Null);
    let content_block = match item.get("type").and_then(|t| t.as_str()) {
        Some("message") => ContentBlock::Text {
            text: String::new(),
        },
        Some("function_call") => ContentBlock::ToolUse {
            id: nested_str(&item, &["call_id"]),
            name: nested_str(&item, &["name"]),
            input: Value::Object(serde_json::Map::new()),
        },
        Some("reasoning") => ContentBlock::Thinking {
            thinking: String::new(),
            signature: None,
        },
        // A future item type degrades to "ignored" instead of failing the
        // whole stream.
        _ => return Vec::new(),
    };
    vec![StreamEvent::ContentBlockStart {
        index,
        content_block,
    }]
}

/// Emit a delta at `output_index` by running `build` on the `delta` string.
fn delta_event(data: &Value, build: impl FnOnce(String) -> ContentDelta) -> Vec<StreamEvent> {
    let Some(index) = nested_index(data, &["output_index"]) else {
        return Vec::new();
    };
    vec![StreamEvent::ContentBlockDelta {
        index,
        delta: build(nested_str(data, &["delta"])),
    }]
}

/// Terminal frames carry the whole response object: resolve the stop reason
/// the same way the non-streaming adapter does and report final usage.
fn finish(data: &Value) -> Vec<StreamEvent> {
    let response = data.get("response").unwrap_or(data);
    let usage = response
        .get("usage")
        .cloned()
        .and_then(|u| serde_json::from_value::<ResponsesUsage>(u).ok())
        .unwrap_or_default();
    vec![
        StreamEvent::MessageDelta {
            delta: MessageDelta {
                stop_reason: Some(stop_reason(response)),
                stop_sequence: None,
            },
            usage: usage_to_token_usage(usage),
        },
        StreamEvent::MessageStop,
    ]
}

/// `incomplete` + `max_output_tokens` wins; otherwise a function_call in the
/// final output means the tool loop continues; otherwise the turn ended.
fn stop_reason(response: &Value) -> StopReason {
    let status = response.get("status").and_then(|s| s.as_str());
    let detail = nested_str(response, &["incomplete_details", "reason"]);
    if status == Some("incomplete") && (detail == "max_output_tokens" || detail.is_empty()) {
        return StopReason::MaxTokens;
    }
    let has_call = response
        .get("output")
        .and_then(|o| o.as_array())
        .map(|items| {
            items
                .iter()
                .any(|i| i.get("type").and_then(|t| t.as_str()) == Some("function_call"))
        })
        .unwrap_or(false);
    if has_call {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    }
}

/// A failure frame always yields an error and then a stop, so the tool loop
/// sees a terminated stream rather than waiting on a body that never comes.
fn error_event(data: &Value) -> Vec<StreamEvent> {
    let candidates = [
        nested_str(data, &["response", "error", "message"]),
        nested_str(data, &["error", "message"]),
        nested_str(data, &["message"]),
    ];
    let message = candidates
        .into_iter()
        .find(|m| !m.is_empty())
        .unwrap_or_else(|| "responses stream failed".to_string());
    vec![
        StreamEvent::Error { error: message },
        StreamEvent::MessageStop,
    ]
}

/// Walk a JSON path, returning `None` at the first missing key.
fn nested<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

/// String at a JSON path, empty when absent or not a string.
fn nested_str(value: &Value, path: &[&str]) -> String {
    nested(value, path)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Non-negative integer at a JSON path, as a block index.
fn nested_index(value: &Value, path: &[&str]) -> Option<usize> {
    nested(value, path)
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn effects(event: &str, data: Value) -> Vec<StreamEvent> {
        effects_for_event(event, &data)
    }

    #[test]
    fn created_emits_message_start_with_id_and_model() {
        let events = effects(
            "response.created",
            json!({ "response": { "id": "resp_1", "model": "gpt-6" } }),
        );
        match &events[0] {
            StreamEvent::MessageStart { message } => {
                assert_eq!(message.id, "resp_1");
                assert_eq!(message.model, "gpt-6");
                assert_eq!(message.role, Role::Assistant);
            }
            other => panic!("expected MessageStart, got {other:?}"),
        }
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn text_delta_maps_to_text_delta_at_output_index() {
        let events = effects(
            "response.output_text.delta",
            json!({ "output_index": 0, "delta": "hello" }),
        );
        match &events[0] {
            StreamEvent::ContentBlockDelta { index, delta } => {
                assert_eq!(*index, 0);
                match delta {
                    ContentDelta::TextDelta { text } => assert_eq!(text, "hello"),
                    other => panic!("expected TextDelta, got {other:?}"),
                }
            }
            other => panic!("expected ContentBlockDelta, got {other:?}"),
        }
    }

    #[test]
    fn function_call_added_starts_tool_block_then_arguments_accumulate() {
        let opened = effects(
            "response.output_item.added",
            json!({
                "output_index": 1,
                "item": { "type": "function_call", "call_id": "call_a", "name": "get_weather" }
            }),
        );
        match &opened[0] {
            StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                assert_eq!(*index, 1);
                match content_block {
                    ContentBlock::ToolUse { id, name, input } => {
                        assert_eq!(id, "call_a");
                        assert_eq!(name, "get_weather");
                        assert!(input.is_object(), "tool input starts as an object");
                    }
                    other => panic!("expected ToolUse, got {other:?}"),
                }
            }
            other => panic!("expected ContentBlockStart, got {other:?}"),
        }

        let frag = effects(
            "response.function_call_arguments.delta",
            json!({ "output_index": 1, "delta": "{\"city\":" }),
        );
        match &frag[0] {
            StreamEvent::ContentBlockDelta { index, delta } => {
                assert_eq!(*index, 1);
                match delta {
                    ContentDelta::InputJsonDelta { partial_json } => {
                        assert_eq!(partial_json, "{\"city\":");
                    }
                    other => panic!("expected InputJsonDelta, got {other:?}"),
                }
            }
            other => panic!("expected ContentBlockDelta, got {other:?}"),
        }
    }

    #[test]
    fn output_item_done_closes_the_block_at_its_index() {
        let events = effects("response.output_item.done", json!({ "output_index": 2 }));
        match &events[0] {
            StreamEvent::ContentBlockStop { index } => assert_eq!(*index, 2),
            other => panic!("expected ContentBlockStop, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_summary_delta_maps_to_reasoning_delta() {
        let events = effects(
            "response.reasoning_summary_text.delta",
            json!({ "output_index": 0, "delta": "thinking..." }),
        );
        match &events[0] {
            StreamEvent::ContentBlockDelta { delta, .. } => match delta {
                ContentDelta::ReasoningDelta { text } => assert_eq!(text, "thinking..."),
                other => panic!("expected ReasoningDelta, got {other:?}"),
            },
            other => panic!("expected ContentBlockDelta, got {other:?}"),
        }
    }

    #[test]
    fn completed_emits_message_delta_with_usage_and_message_stop() {
        let events = effects(
            "response.completed",
            json!({
                "response": {
                    "status": "completed",
                    "output": [{ "type": "message" }],
                    "usage": {
                        "input_tokens": 100,
                        "output_tokens": 40,
                        "input_tokens_details": { "cached_tokens": 25 },
                        "output_tokens_details": { "reasoning_tokens": 12 }
                    }
                }
            }),
        );
        match &events[0] {
            StreamEvent::MessageDelta { delta, usage } => {
                assert_eq!(delta.stop_reason, Some(StopReason::EndTurn));
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 40);
                assert_eq!(usage.reasoning_tokens, 12);
                assert_eq!(usage.cache_read_tokens, 25);
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
        assert!(matches!(events[1], StreamEvent::MessageStop));
    }

    #[test]
    fn completed_with_function_call_output_stops_for_tool_use() {
        let events = effects(
            "response.completed",
            json!({
                "response": {
                    "status": "completed",
                    "output": [
                        { "type": "reasoning" },
                        { "type": "function_call", "call_id": "c" }
                    ]
                }
            }),
        );
        match &events[0] {
            StreamEvent::MessageDelta { delta, .. } => {
                assert_eq!(delta.stop_reason, Some(StopReason::ToolUse));
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    #[test]
    fn incomplete_stops_for_max_tokens() {
        let events = effects(
            "response.incomplete",
            json!({
                "response": {
                    "status": "incomplete",
                    "incomplete_details": { "reason": "max_output_tokens" },
                    "output": [{ "type": "function_call", "call_id": "c" }]
                }
            }),
        );
        match &events[0] {
            StreamEvent::MessageDelta { delta, .. } => {
                assert_eq!(delta.stop_reason, Some(StopReason::MaxTokens));
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    #[test]
    fn failed_and_error_frames_surface_an_error_then_stop() {
        let failed = effects(
            "response.failed",
            json!({ "response": { "error": { "message": "server exploded" } } }),
        );
        match &failed[0] {
            StreamEvent::Error { error } => assert_eq!(error, "server exploded"),
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(matches!(failed[1], StreamEvent::MessageStop));

        let bare = effects("error", json!({ "message": "bad request" }));
        match &bare[0] {
            StreamEvent::Error { error } => assert_eq!(error, "bad request"),
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(matches!(bare[1], StreamEvent::MessageStop));
    }

    #[test]
    fn unknown_and_bookkeeping_events_are_ignored() {
        assert!(effects("response.in_progress", json!({})).is_empty());
        assert!(effects("response.content_part.added", json!({ "output_index": 0 })).is_empty());
        assert!(effects("some.future.event", json!({ "anything": 1 })).is_empty());
    }

    #[test]
    fn unknown_output_item_type_opens_no_block() {
        let events = effects(
            "response.output_item.added",
            json!({ "output_index": 3, "item": { "type": "web_search_call" } }),
        );
        assert!(events.is_empty());
    }
}
