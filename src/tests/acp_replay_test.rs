//! ACP transcript-replay mapping: stored message rows → session/update
//! chunk shapes, split out of protocol.rs to keep the module under the
//! 500-line ceiling.

use crate::acp::protocol::{replay_updates, replay_usage};
use crate::db::models::Message;
use serde_json::{Value, json};

fn msg(role: &str, content: &str, thinking: Option<&str>) -> Message {
    Message {
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::new_v4(),
        role: role.to_string(),
        content: content.to_string(),
        sequence: 0,
        created_at: chrono::Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: thinking.map(str::to_string),
        duration_secs: None,
    }
}

fn kind_of(update: &Value) -> &str {
    update["sessionUpdate"].as_str().unwrap()
}

#[test]
fn replays_user_and_assistant_in_order() {
    let messages = vec![
        msg("user", "hello", None),
        msg("assistant", "hi there", None),
        msg("user", "next", None),
    ];
    let updates = replay_updates(&messages);
    let kinds: Vec<&str> = updates.iter().map(kind_of).collect();
    assert_eq!(
        kinds,
        [
            "user_message_chunk",
            "agent_message_chunk",
            "user_message_chunk"
        ]
    );
    assert_eq!(updates[1]["content"]["text"], json!("hi there"));
}

#[test]
fn replays_thinking_and_inline_reasoning_as_thought_chunks() {
    // Markers are line-start-anchored upstream (#1587): real stored content
    // newline-delimits them, so the fixture must too — inline markers parse
    // as one unclosed reasoning block, by contract.
    let messages = vec![msg(
        "assistant",
        "<!-- reasoning -->\npondering\n<!-- /reasoning -->\nvisible answer",
        Some("persisted thought"),
    )];
    let updates = replay_updates(&messages);
    let kinds: Vec<&str> = updates.iter().map(kind_of).collect();
    assert_eq!(
        kinds,
        [
            "agent_thought_chunk",
            "agent_thought_chunk",
            "agent_message_chunk"
        ]
    );
    assert_eq!(updates[0]["content"]["text"], json!("persisted thought"));
    assert_eq!(updates[1]["content"]["text"], json!("pondering"));
    assert_eq!(updates[2]["content"]["text"], json!("visible answer"));
}

#[test]
fn skips_empty_and_non_transcript_roles() {
    let messages = vec![
        msg("user", "   ", None),
        msg("system", "[SYSTEM: Compact context now.]", None),
        msg("tool", "tool output", None),
        msg(
            crate::services::message::PLAN_COMPLETED_ROLE,
            "raw-plan-json",
            None,
        ),
        msg("user", "real", None),
    ];
    let updates = replay_updates(&messages);
    assert_eq!(updates.len(), 1);
    assert_eq!(kind_of(&updates[0]), "user_message_chunk");
}

#[test]
fn replay_usage_takes_last_assistant_input_tokens() {
    let earlier = Message {
        input_tokens: Some(1_000),
        ..msg("assistant", "earlier answer", None)
    };
    let later = Message {
        input_tokens: Some(4_242),
        ..msg("assistant", "latest answer", None)
    };
    let messages = vec![
        msg("user", "hello", None),
        earlier,
        msg("user", "again", None),
        later,
        msg(
            crate::services::message::PLAN_COMPLETED_ROLE,
            "raw-plan-json",
            None,
        ),
    ];
    // The provider's own measurement from the LAST assistant row — not the
    // first, not a user row, not a sum.
    assert_eq!(replay_usage(&messages), Some(4_242));
}

#[test]
fn replay_usage_none_without_assistant_measurements() {
    // Sessions predating usage persistence, or user-only: caller falls back.
    let messages = vec![msg("user", "hello", None), msg("assistant", "answer", None)];
    assert_eq!(replay_usage(&messages), None);
    assert_eq!(replay_usage(&[]), None);
}

#[test]
fn replay_usage_null_on_latest_assistant_does_not_fall_back() {
    // The newest assistant row speaks: a NULL `input_tokens` there means
    // unknown, even when older assistant rows carried values. Pinning this
    // so "skip NULLs and use an older measurement" cannot land silently: a
    // a stale older size would overstate the context the client renders.
    let earlier = Message {
        input_tokens: Some(1_200),
        ..msg("assistant", "first answer", None)
    };
    let messages = vec![
        earlier,
        msg("user", "more", None),
        msg("assistant", "second", None),
    ];
    assert_eq!(replay_usage(&messages), None);
}
