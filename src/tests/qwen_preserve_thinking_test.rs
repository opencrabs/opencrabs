//! Qwen `preserve_thinking` round-trip (#654).
//!
//! qwen3.8-max-preview (and the Qwen family generally) always thinks and
//! returns `reasoning_content` as a separate field; its `preserve_thinking`
//! contract requires that reasoning passed back as `reasoning_content`, never
//! concatenated into `content`. We persist reasoning as `<!-- reasoning -->`
//! content markers, and on reload `strip_llm_artifacts` removed only the marker
//! tags — leaving the reasoning text in `content`, which made the model spill
//! fresh chain-of-thought into its answer (leaked to the TUI and to Telegram).
//!
//! These lock in:
//! - `preserves_thinking` covers the Qwen family and Moonshot kimi, not plain
//!   OpenAI/Anthropic endpoints.
//! - `hoist_reasoning_blocks` moves reasoning out of `content`.
//! - the two composed so `from_db_messages` rehydrates the reasoning as a
//!   leading `ContentBlock::Thinking` with a marker-free `content`.

use crate::brain::agent::context::AgentContext;
use crate::brain::provider::custom_openai_compatible::preserves_thinking;
use crate::brain::provider::{ContentBlock, Role};
use crate::db::models::Message as DbMessage;
use crate::utils::sanitize::hoist_reasoning_blocks;
use chrono::Utc;
use uuid::Uuid;

// ─── preserves_thinking predicate ───────────────────────────────────

#[test]
fn preserves_thinking_matches_qwen_family() {
    // Custom Model Studio / DashScope endpoint.
    assert!(preserves_thinking(
        Some("https://ws-abc.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"),
        "qwen3.8-max-preview"
    ));
    // Built-in Qwen provider: model name alone is enough, any base_url.
    assert!(preserves_thinking(
        Some("https://example.test/v1"),
        "qwen3.8-max-preview"
    ));
    assert!(preserves_thinking(None, "qwen3.7-plus"));
    // DashScope base_url even with a non-qwen-prefixed alias.
    assert!(preserves_thinking(
        Some("https://dashscope.aliyuncs.com/v1"),
        "max"
    ));
}

#[test]
fn preserves_thinking_matches_moonshot_kimi() {
    assert!(preserves_thinking(
        Some("https://api.moonshot.ai/v1"),
        "kimi-k2.6"
    ));
}

#[test]
fn preserves_thinking_ignores_plain_endpoints() {
    assert!(!preserves_thinking(
        Some("https://api.openai.com/v1"),
        "gpt-4o"
    ));
    assert!(!preserves_thinking(
        Some("https://api.anthropic.com"),
        "claude-opus-4-8"
    ));
    assert!(!preserves_thinking(
        Some("https://openrouter.ai/api/v1"),
        "deepseek/deepseek-r1"
    ));
}

// ─── hoist_reasoning_blocks ─────────────────────────────────────────

#[test]
fn hoist_extracts_single_block_and_leaves_answer() {
    let content = "<!-- reasoning -->\nThe user asked X, so Y then Z.\n<!-- /reasoning -->\n\nHere is the answer.";
    let (cleaned, reasoning) = hoist_reasoning_blocks(content);
    assert_eq!(cleaned, "Here is the answer.");
    assert_eq!(reasoning.as_deref(), Some("The user asked X, so Y then Z."));
}

#[test]
fn hoist_concatenates_multiple_blocks() {
    let content = "<!-- reasoning -->\nfirst thought\n<!-- /reasoning -->\n\nStep one.\n\n\
                   <!-- reasoning -->\nsecond thought\n<!-- /reasoning -->\n\nStep two.";
    let (cleaned, reasoning) = hoist_reasoning_blocks(content);
    assert!(!cleaned.contains("<!-- reasoning -->"));
    assert!(!cleaned.contains("first thought"));
    assert!(!cleaned.contains("second thought"));
    assert!(cleaned.contains("Step one."));
    assert!(cleaned.contains("Step two."));
    assert_eq!(
        reasoning.as_deref(),
        Some("first thought\n\nsecond thought")
    );
}

#[test]
fn hoist_is_a_noop_without_markers() {
    let (cleaned, reasoning) = hoist_reasoning_blocks("Just a plain answer, no markers.");
    assert_eq!(cleaned, "Just a plain answer, no markers.");
    assert!(reasoning.is_none());
}

// ─── composition: reload rehydrates reasoning_content, content is clean ──

fn assistant_row(content: &str) -> DbMessage {
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
        thinking: None,
        duration_secs: None,
    }
}

#[test]
fn hoisted_reasoning_rehydrates_as_thinking_block_with_clean_content() {
    // Mirror the tool_loop load-site transform for a preserve_thinking model.
    let mut row = assistant_row(
        "<!-- reasoning -->\nweighing the MCP annotation options\n<!-- /reasoning -->\n\n\
         Here is the final answer.",
    );

    let (cleaned, reasoning) = hoist_reasoning_blocks(&row.content);
    if let Some(reasoning) = reasoning {
        row.content = cleaned;
        row.thinking = Some(reasoning);
    }

    // Reasoning must be out of content entirely — no marker, no reasoning text.
    assert_eq!(row.content, "Here is the final answer.");
    assert!(!row.content.contains("weighing the MCP"));

    let ctx = AgentContext::from_db_messages(Uuid::new_v4(), vec![row], 200_000);
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
            ContentBlock::Thinking { thinking, .. } if thinking.contains("weighing the MCP")
        ),
        "first block must be the rehydrated Thinking: got {:?}",
        msg.content[0]
    );
    assert!(
        matches!(&msg.content[1], ContentBlock::Text { text } if text == "Here is the final answer."),
        "second block must be the marker-free answer: got {:?}",
        msg.content[1]
    );
}
