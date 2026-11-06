//! Regression tests for the Moonshot kimi `reasoning_content` plumbing.
//!
//! Locks in the end-to-end fix for:
//!   API error (400) [invalid_request_error]:
//!   [Moonshot AI] thinking is enabled but reasoning_content is missing in
//!   assistant tool call message at index N
//!
//! Coverage:
//! - from_db_messages rehydrates `thinking` column → leading
//!   `ContentBlock::Thinking` on assistant messages
//! - to_openai_request emits `reasoning_content` as the real captured
//!   thinking when a Thinking block is present on an assistant tool_call
//! - the safety fallback (non-empty placeholder) only fires when we truly
//!   have no reasoning AND the base_url/model pair requires the field
//! - Non-Moonshot routes never serialize the field

use crate::brain::agent::context::AgentContext;
use crate::brain::provider::custom_openai_compatible::OpenAIProvider;
use crate::brain::provider::{ContentBlock, LLMRequest, Message, Role};
use crate::db::models::Message as DbMessage;
use chrono::Utc;
use uuid::Uuid;

// ─── from_db_messages rehydration ───────────────────────────────────

fn assistant_db_row(content: &str, thinking: Option<&str>) -> DbMessage {
    DbMessage {
        forward: None,
        id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        role: "assistant".to_string(),
        content: content.to_string(),
        sequence: 0,
        created_at: Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: thinking.map(String::from),
        duration_secs: None,
    }
}

#[test]
fn from_db_messages_rehydrates_thinking_for_assistant_rows() {
    let session_id = Uuid::new_v4();
    let rows = vec![assistant_db_row(
        "Sure, here is the answer.",
        Some("The user asked for X, so I reasoned through Y then Z."),
    )];
    let ctx = AgentContext::from_db_messages(session_id, rows, 200_000);
    assert_eq!(ctx.messages.len(), 1);
    let msg = &ctx.messages[0];
    assert_eq!(msg.role, Role::Assistant);
    assert_eq!(
        msg.content.len(),
        2,
        "expected [Thinking, Text]: got {:?}",
        msg.content
    );
    assert!(
        matches!(
            &msg.content[0],
            ContentBlock::Thinking { thinking, .. } if thinking.contains("reasoned through")
        ),
        "first block must be the rehydrated Thinking: got {:?}",
        msg.content[0]
    );
    assert!(
        matches!(&msg.content[1], ContentBlock::Text { text } if text == "Sure, here is the answer."),
    );
}

#[test]
fn from_db_messages_does_not_duplicate_thinking_when_absent() {
    let session_id = Uuid::new_v4();
    let rows = vec![assistant_db_row("plain answer, no reasoning saved", None)];
    let ctx = AgentContext::from_db_messages(session_id, rows, 200_000);
    assert_eq!(ctx.messages.len(), 1);
    assert_eq!(ctx.messages[0].content.len(), 1);
    assert!(matches!(
        &ctx.messages[0].content[0],
        ContentBlock::Text { .. }
    ));
}

#[test]
fn from_db_messages_skips_empty_content_with_no_thinking() {
    let session_id = Uuid::new_v4();
    let rows = vec![assistant_db_row("", None)];
    let ctx = AgentContext::from_db_messages(session_id, rows, 200_000);
    assert!(
        ctx.messages.is_empty(),
        "empty row with no thinking should be dropped"
    );
}

#[test]
fn from_db_messages_keeps_row_when_only_thinking_present() {
    let session_id = Uuid::new_v4();
    // Edge: a turn where only reasoning was persisted (no final text yet).
    let rows = vec![assistant_db_row("", Some("midway thinking"))];
    let ctx = AgentContext::from_db_messages(session_id, rows, 200_000);
    assert_eq!(ctx.messages.len(), 1);
    assert!(matches!(
        &ctx.messages[0].content[0],
        ContentBlock::Thinking { thinking, .. } if thinking == "midway thinking"
    ));
}

#[test]
fn from_db_messages_skips_thinking_on_user_rows() {
    let session_id = Uuid::new_v4();
    // User rows should never get a Thinking block even if the column has a value —
    // only the assistant has reasoning.
    let row = DbMessage {
        forward: None,
        id: Uuid::new_v4(),
        session_id,
        role: "user".to_string(),
        content: "hello".to_string(),
        sequence: 0,
        created_at: Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: Some("leaked reasoning".to_string()),
        duration_secs: None,
    };
    let ctx = AgentContext::from_db_messages(session_id, vec![row], 200_000);
    assert_eq!(ctx.messages.len(), 1);
    assert_eq!(ctx.messages[0].content.len(), 1);
    assert!(matches!(
        &ctx.messages[0].content[0],
        ContentBlock::Text { .. }
    ));
}

// ─── to_openai_request encoding ─────────────────────────────────────

fn opencode_kimi_provider() -> OpenAIProvider {
    OpenAIProvider::with_base_url(
        "test-key".to_string(),
        "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
    )
    .with_name("opencode-kimi")
}

fn non_kimi_provider() -> OpenAIProvider {
    OpenAIProvider::with_base_url(
        "test-key".to_string(),
        "https://api.z.ai/api/coding/paas/v4/chat/completions".to_string(),
    )
    .with_name("zhipu")
}

fn assistant_with_tool_call_and_thinking(thinking: Option<&str>) -> Message {
    let mut content = Vec::new();
    if let Some(t) = thinking {
        content.push(ContentBlock::Thinking {
            thinking: t.to_string(),
            signature: None,
        });
    }
    content.push(ContentBlock::ToolUse {
        id: "call_abc123".to_string(),
        name: "bash".to_string(),
        input: serde_json::json!({"command": "echo hi"}),
    });
    Message {
        role: Role::Assistant,
        content,
    }
}

#[test]
fn encoder_emits_reasoning_content_from_thinking_block_for_kimi() {
    let provider = opencode_kimi_provider();
    let req = LLMRequest::new(
        "kimi-k2.6".to_string(),
        vec![
            Message::user("run bash".to_string()),
            assistant_with_tool_call_and_thinking(Some(
                "User wants a shell command. I'll run echo.",
            )),
        ],
    );
    let encoded = provider.to_openai_request(req);
    let body = serde_json::to_value(&encoded).expect("serialize");

    let msgs = body["messages"].as_array().expect("messages array");
    // index 0 = user, index 1 = assistant tool_call
    let asst = &msgs[1];
    assert_eq!(asst["role"], "assistant");
    assert!(asst["tool_calls"].is_array());
    assert_eq!(
        asst["reasoning_content"].as_str(),
        Some("User wants a shell command. I'll run echo."),
        "reasoning_content must equal the Thinking block text verbatim; got {}",
        asst
    );
}

#[test]
fn encoder_uses_safety_placeholder_for_kimi_when_no_thinking_block() {
    let provider = opencode_kimi_provider();
    let req = LLMRequest::new(
        "kimi-k2.6".to_string(),
        vec![
            Message::user("run bash".to_string()),
            assistant_with_tool_call_and_thinking(None),
        ],
    );
    let encoded = provider.to_openai_request(req);
    let body = serde_json::to_value(&encoded).expect("serialize");
    let asst = &body["messages"].as_array().unwrap()[1];
    // Must be present AND non-empty — Moonshot treats "" as missing.
    let rc = asst["reasoning_content"]
        .as_str()
        .expect("reasoning_content must be serialized for kimi");
    assert!(
        !rc.is_empty(),
        "reasoning_content must be non-empty for kimi fallback path; got {:?}",
        rc
    );
}

#[test]
fn encoder_omits_reasoning_content_for_non_kimi_without_thinking() {
    let provider = non_kimi_provider();
    let req = LLMRequest::new(
        "glm-5.1".to_string(),
        vec![
            Message::user("hello".to_string()),
            assistant_with_tool_call_and_thinking(None),
        ],
    );
    let encoded = provider.to_openai_request(req);
    let body = serde_json::to_value(&encoded).expect("serialize");
    let asst = &body["messages"].as_array().unwrap()[1];
    assert!(
        asst.get("reasoning_content").is_none_or(|v| v.is_null()),
        "non-kimi providers must not receive reasoning_content when we have no thinking: got {}",
        asst
    );
}

#[test]
fn encoder_passes_thinking_through_for_non_kimi_providers_too() {
    // Cross-provider safety: when we DO have Thinking (Anthropic kept it,
    // qwen streamed it, etc.) we still echo it under reasoning_content so
    // it's preserved across OpenAI-compatible hops. Unknown-field providers
    // ignore it; none of the known set rejects it.
    let provider = non_kimi_provider();
    let req = LLMRequest::new(
        "glm-5.1".to_string(),
        vec![
            Message::user("hi".to_string()),
            assistant_with_tool_call_and_thinking(Some("actual reasoning")),
        ],
    );
    let encoded = provider.to_openai_request(req);
    let body = serde_json::to_value(&encoded).expect("serialize");
    let asst = &body["messages"].as_array().unwrap()[1];
    assert_eq!(asst["reasoning_content"].as_str(), Some("actual reasoning"));
}

// ─── reasoning_effort pass-through for non-Kimi custom providers (#691) ──────

fn modelstudio_provider() -> OpenAIProvider {
    OpenAIProvider::with_base_url(
        "test-key".to_string(),
        "https://ws-x.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1".to_string(),
    )
    .with_name("modelstudio")
    .with_reasoning("xhigh".to_string())
}

#[test]
fn qwen_passes_configured_reasoning_effort_through() {
    // #691: a non-Kimi custom-provider model must SEND the configured
    // reasoning_effort verbatim, not silently drop it (the qwen/modelstudio bug).
    let provider = modelstudio_provider();
    let req = LLMRequest::new(
        "qwen3.8-max-preview".to_string(),
        vec![Message::user("hi".to_string())],
    );
    let body = serde_json::to_value(provider.to_openai_request(req)).unwrap();
    assert_eq!(body["reasoning_effort"].as_str(), Some("xhigh"));
}

#[test]
fn provider_without_reasoning_setting_sends_none() {
    // A model with no family-specific handling: nothing configured means
    // nothing on the wire. Deliberately not a qwen id, since the tiered qwen
    // family now carries its own default (see the test below).
    let provider = OpenAIProvider::with_base_url("k".into(), "https://x/v1".into()).with_name("x");
    let req = LLMRequest::new("gpt-5".to_string(), vec![Message::user("hi".to_string())]);
    let body = serde_json::to_value(provider.to_openai_request(req)).unwrap();
    assert!(body.get("reasoning_effort").is_none_or(|v| v.is_null()));
}

#[test]
fn an_unconfigured_tiered_qwen_still_gets_its_default_tier() {
    // #1034: the tiered family defaults to the recommended tier rather than
    // leaving the endpoint to pick. This holds on any remote host, not just
    // Alibaba's (#1040).
    let provider = OpenAIProvider::with_base_url("k".into(), "https://x/v1".into()).with_name("x");
    let req = LLMRequest::new(
        "qwen3.8-max-preview".to_string(),
        vec![Message::user("hi".to_string())],
    );
    let body = serde_json::to_value(provider.to_openai_request(req)).unwrap();
    assert_eq!(body["reasoning_effort"].as_str(), Some("xhigh"));
    assert!(
        body.get("enable_thinking").is_none(),
        "the tier ships alone; the switch is inert on this family"
    );
    assert_eq!(
        body["preserve_thinking"].as_bool(),
        Some(true),
        "a remote qwen target carries preserve_thinking"
    );
}

#[test]
fn kimi_k3_still_routes_through_family_resolver_as_effort() {
    let provider = opencode_kimi_provider().with_reasoning("max".to_string());
    let req = LLMRequest::new("kimi-k3".to_string(), vec![Message::user("hi".to_string())]);
    let body = serde_json::to_value(provider.to_openai_request(req)).unwrap();
    assert_eq!(body["reasoning_effort"].as_str(), Some("max"));
}

#[test]
fn kimi_on_maps_to_thinking_not_effort_passthrough() {
    // The generic pass-through must NOT capture Kimi's on/off — K2.6 "on" is a
    // thinking toggle, so reasoning_effort must stay absent (never "on").
    let provider = opencode_kimi_provider().with_reasoning("on".to_string());
    let req = LLMRequest::new(
        "kimi-k2.6".to_string(),
        vec![Message::user("hi".to_string())],
    );
    let body = serde_json::to_value(provider.to_openai_request(req)).unwrap();
    assert!(
        body.get("reasoning_effort").is_none_or(|v| v.is_null()),
        "kimi on/off must not leak into reasoning_effort: {body}"
    );
}
