//! Responses API request adapter (`POST /v1/responses` wire dialect).
//!
//! Converts our generic [`LLMRequest`] into the OpenAI Responses body. This is
//! a separate type from [`super::custom_openai_compatible::OpenAIRequest`] so
//! the Chat Completions path stays byte-identical while `wire_dialect` routing
//! (#2059) picks between the two.
//!
//! Wire mapping:
//! - `system` brain + `system_suffix` + any `Role::System` messages become the
//!   top-level `instructions` string (the Responses equivalent of the chat
//!   `system` role). Order: stable brain, runtime suffix, then message systems.
//! - user messages become `input` items of `type: "message"` with
//!   `input_text` / `input_image` content parts.
//! - assistant text becomes a `message` item with `output_text` parts.
//! - assistant [`ContentBlock::ToolUse`] becomes flat `function_call` items;
//!   [`ContentBlock::ToolResult`] becomes `function_call_output` items keyed
//!   by `call_id` (the Responses replacement for chat's
//!   `tool_calls[]` + `role: "tool"` pairing).
//! - [`Tool`] uses the FLAT Responses tool schema: `{type, name, description,
//!   parameters}` — no nested `function` object like Chat Completions.
//! - the token cap is always `max_output_tokens` (there is no
//!   `max_tokens`/`max_completion_tokens` split on this dialect).
//! - a configured `reasoning_effort` rides as `reasoning: { effort }`.
//! - `store` is always `false`: the client is stateless and resends the full
//!   history every turn, exactly like the chat path.
//!
//! Known follow-up, deliberately out of scope here: stateless reasoning
//! tool loops can preserve the model's chain across turns with
//! `include: ["reasoning.encrypted_content"]`. The chat path has no
//! equivalent today either, so the MVP mirrors it and this stays a follow-up.

use super::types::{ContentBlock, ImageSource, LLMRequest, Role, Tool};

/// A Responses API request body.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ResponsesRequest {
    pub(crate) model: String,
    /// Conversation as typed input items (messages, function calls, outputs).
    pub(crate) input: Vec<ResponsesInputItem>,
    /// System prompt: stable brain + runtime suffix + system-role messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tools: Option<Vec<ResponsesTool>>,
    /// "auto" whenever tools are present, mirroring the chat path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    /// The ONLY token-cap field on this dialect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<ResponsesReasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream: Option<bool>,
    /// Stateless client: never persist turns server-side.
    pub(crate) store: bool,
}

/// One typed entry of the `input` array.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponsesInputItem {
    /// A role message (user or assistant; system folds into `instructions`).
    Message {
        role: &'static str,
        content: Vec<ResponsesContentPart>,
    },
    /// An assistant tool invocation.
    FunctionCall {
        call_id: String,
        name: String,
        /// JSON-encoded arguments string, same encoding as chat `tool_calls`.
        arguments: String,
    },
    /// The tool result handed back to the model.
    FunctionCallOutput { call_id: String, output: String },
}

/// A content part inside a `message` input item.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponsesContentPart {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

/// FLAT Responses tool schema. Chat nests these under a `function` object;
/// Responses does not.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ResponsesTool {
    /// Always "function" — the flat schema keeps the type tag, it just
    /// drops chat's nested `function` object.
    pub(crate) r#type: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) parameters: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ResponsesReasoning {
    pub(crate) effort: String,
}

/// Build a Responses body from a generic request.
///
/// `reasoning_effort` is the provider's configured `reasoning_effort` setting
/// (native-OpenAI hosts take it straight through; the Kimi/qwen/deepseek/glm
/// family resolvers in the chat path are chat-dialect-only and do not apply).
pub(crate) fn build_responses_request(
    request: LLMRequest,
    reasoning_effort: Option<&str>,
) -> ResponsesRequest {
    // `instructions` = stable brain + runtime suffix + system-role messages,
    // joined with blank lines in that order.
    let mut instruction_parts: Vec<String> = Vec::new();
    if let Some(full) = request.system_full()
        && !full.trim().is_empty()
    {
        instruction_parts.push(full);
    }
    for msg in &request.messages {
        if msg.role == Role::System {
            let text = text_of(&msg.content);
            if !text.is_empty() {
                instruction_parts.push(text);
            }
        }
    }
    let instructions = if instruction_parts.is_empty() {
        None
    } else {
        Some(instruction_parts.join("\n\n"))
    };

    let mut input: Vec<ResponsesInputItem> = Vec::new();
    for msg in &request.messages {
        let role = match msg.role {
            Role::System => continue, // folded into instructions above
            Role::User => "user",
            Role::Assistant => "assistant",
        };

        let mut text_parts: Vec<String> = Vec::new();
        let mut image_parts: Vec<ResponsesContentPart> = Vec::new();
        let mut tool_uses: Vec<ResponsesInputItem> = Vec::new();
        let mut tool_outputs: Vec<ResponsesInputItem> = Vec::new();

        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => text_parts.push(text.clone()),
                ContentBlock::Image { source } => {
                    let url = match source {
                        ImageSource::Base64 { media_type, data } => {
                            format!("data:{media_type};base64,{data}")
                        }
                        ImageSource::Url { url } => url.clone(),
                    };
                    image_parts.push(ResponsesContentPart::InputImage { image_url: url });
                }
                ContentBlock::ToolUse { id, name, input } => {
                    tool_uses.push(ResponsesInputItem::FunctionCall {
                        call_id: id.clone(),
                        name: name.clone(),
                        arguments: serde_json::to_string(input).unwrap_or_default(),
                    });
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    tool_outputs.push(ResponsesInputItem::FunctionCallOutput {
                        call_id: tool_use_id.clone(),
                        output: content.clone(),
                    });
                }
                // Reasoning is server-side state on this dialect; with
                // `store: false` there is no item reference to replay it
                // through, so it is dropped (same as the non-echo chat path).
                ContentBlock::Thinking { .. } => {}
            }
        }

        if !text_parts.is_empty() || !image_parts.is_empty() {
            let content = if role == "assistant" {
                text_parts
                    .into_iter()
                    .map(|text| ResponsesContentPart::OutputText { text })
                    .chain(image_parts)
                    .collect()
            } else {
                text_parts
                    .into_iter()
                    .map(|text| ResponsesContentPart::InputText { text })
                    .chain(image_parts)
                    .collect()
            };
            input.push(ResponsesInputItem::Message { role, content });
        }
        // Assistant tool-call turn: function_call items after any text item.
        input.extend(tool_uses);
        // Tool results ride at the turn boundary as their own items.
        input.extend(tool_outputs);
    }

    let tools = request.tools.map(|tools| {
        tools
            .iter()
            .map(|tool: &Tool| ResponsesTool {
                r#type: "function".to_string(),
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            })
            .collect::<Vec<ResponsesTool>>()
    });
    let tool_choice = tools
        .as_ref()
        .filter(|t| !t.is_empty())
        .map(|_| serde_json::json!("auto"));

    ResponsesRequest {
        model: request.model,
        input,
        instructions,
        tools,
        tool_choice,
        temperature: request.temperature,
        // No model gates here: this dialect has a single token-cap field.
        max_output_tokens: request.max_tokens,
        reasoning: reasoning_effort
            .filter(|s| !s.trim().is_empty())
            .map(|effort| ResponsesReasoning {
                effort: effort.to_string(),
            }),
        stream: Some(request.stream),
        store: false,
    }
}

/// Concatenated text content of a message (images and tool blocks ignored).
fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<&str>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::super::types::Message;
    use super::*;
    use std::collections::HashMap;

    fn req(model: &str, messages: Vec<Message>) -> LLMRequest {
        LLMRequest::new(model, messages)
    }

    fn body(r: LLMRequest, effort: Option<&str>) -> serde_json::Value {
        serde_json::to_value(build_responses_request(r, effort)).expect("serialize")
    }

    #[test]
    fn system_becomes_instructions_and_message_systems_join_in_order() {
        let mut r = req(
            "gpt-6",
            vec![
                Message::user("hi"),
                Message::system("be terse"),
                Message::user("again"),
            ],
        );
        r.system = Some("BRAIN".to_string());
        r.system_suffix = Some("RUNTIME".to_string());
        let v = body(r, None);
        assert_eq!(
            v["instructions"].as_str(),
            Some("BRAIN\n\nRUNTIME\n\nbe terse")
        );
        // No system entry may leak into `input`.
        let input = v["input"].as_array().expect("input array");
        assert_eq!(input.len(), 2, "only the two user messages ride as input");
    }

    #[test]
    fn user_text_maps_to_message_item_with_input_text_part() {
        let v = body(req("gpt-6", vec![Message::user("hello")]), None);
        let item = &v["input"][0];
        assert_eq!(item["type"], "message");
        assert_eq!(item["role"], "user");
        assert_eq!(item["content"][0]["type"], "input_text");
        assert_eq!(item["content"][0]["text"], "hello");
    }

    #[test]
    fn assistant_text_uses_output_text_part_type() {
        let v = body(req("gpt-6", vec![Message::assistant("here you go")]), None);
        let item = &v["input"][0];
        assert_eq!(item["role"], "assistant");
        assert_eq!(item["content"][0]["type"], "output_text");
        assert_eq!(item["content"][0]["text"], "here you go");
    }

    #[test]
    fn images_become_input_image_parts_with_data_url() {
        let msg = Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "what is this".to_string(),
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".to_string(),
                        data: "AAAA".to_string(),
                    },
                },
            ],
        };
        let v = body(req("gpt-6", vec![msg]), None);
        let parts = v["input"][0]["content"].as_array().expect("parts");
        assert_eq!(parts[0]["type"], "input_text");
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(parts[1]["image_url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn tool_use_and_tool_result_become_call_output_pairs() {
        let assistant = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "checking".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "get_weather".to_string(),
                    input: serde_json::json!({ "city": "Coimbra" }),
                },
            ],
        };
        let result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "sunny".to_string(),
                is_error: None,
            }],
        };
        let v = body(req("gpt-6", vec![assistant, result]), None);
        let input = v["input"].as_array().expect("input array");
        assert_eq!(
            input.len(),
            3,
            "text message, function_call, function_call_output"
        );

        let call = &input[1];
        assert_eq!(call["type"], "function_call");
        assert_eq!(call["call_id"], "call_1");
        assert_eq!(call["name"], "get_weather");
        assert_eq!(
            call["arguments"].as_str(),
            Some(r#"{"city":"Coimbra"}"#),
            "arguments is the JSON-encoded string"
        );

        let out = &input[2];
        assert_eq!(out["type"], "function_call_output");
        assert_eq!(out["call_id"], "call_1");
        assert_eq!(out["output"], "sunny");
    }

    #[test]
    fn tools_use_the_flat_schema_and_tool_choice_auto() {
        let mut r = req("gpt-6", vec![Message::user("hi")]);
        r.tools = Some(vec![Tool {
            name: "get_weather".to_string(),
            description: "weather lookup".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        }]);
        let v = body(r, None);
        let tool = &v["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["name"], "get_weather");
        assert_eq!(tool["description"], "weather lookup");
        assert!(tool.get("function").is_none(), "no nested function object");
        assert_eq!(v["tool_choice"], "auto");

        let bare = body(req("gpt-6", vec![Message::user("hi")]), None);
        assert!(bare.get("tool_choice").is_none() || bare["tool_choice"].is_null());
    }

    #[test]
    fn wire_fields_max_output_tokens_reasoning_store_and_stream() {
        let mut r = req("gpt-6", vec![Message::user("hi")]);
        r.max_tokens = Some(4_096);
        r.temperature = Some(0.2);
        r.stream = true;
        let v = body(r, Some("high"));
        assert!(v.get("max_tokens").is_none(), "chat field must not appear");
        assert!(v.get("max_completion_tokens").is_none());
        assert_eq!(v["max_output_tokens"], 4_096);
        assert_eq!(v["reasoning"]["effort"], "high");
        assert_eq!(v["store"], false);
        assert_eq!(v["stream"], true);
        assert_eq!(v["model"], "gpt-6");

        // No effort configured -> no reasoning object at all.
        let plain = body(req("gpt-6", vec![Message::user("hi")]), None);
        assert!(plain.get("reasoning").is_none() || plain["reasoning"].is_null());
    }

    #[test]
    fn blank_effort_and_blank_system_do_not_produce_empty_fields() {
        let mut r = req("gpt-6", vec![Message::user("hi")]);
        r.system = Some("   ".to_string());
        let v = body(r, Some("  "));
        assert!(
            v.get("instructions").is_none() || v["instructions"].is_null(),
            "blank system must not ship as instructions"
        );
        assert!(
            v.get("reasoning").is_none() || v["reasoning"].is_null(),
            "blank effort must not ship as reasoning"
        );
    }

    #[test]
    fn metadata_and_transport_fields_never_reach_the_wire() {
        let mut r = req("gpt-6", vec![Message::user("hi")]);
        r.metadata = Some(HashMap::new());
        r.working_directory = Some("/tmp".to_string());
        r.session_id = Some(uuid::Uuid::new_v4());
        let v = body(r, None);
        let obj = v.as_object().expect("object");
        for banned in ["metadata", "working_directory", "session_id"] {
            assert!(!obj.contains_key(banned), "{banned} must not serialize");
        }
    }
}
