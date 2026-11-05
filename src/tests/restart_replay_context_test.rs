//! A replayed turn sees the follow-up the user queued before the restart
//! (#1401).
//!
//! The interrupted Slack turn had a mid-turn user message injected seconds
//! before the restart, persisted as a user row and followed by the empty
//! assistant placeholder the killed turn left behind. Replay rebuilds the
//! context from the database, so the follow-up must survive that rebuild
//! and the empty placeholder must not reach the provider.

use uuid::Uuid;

use crate::brain::agent::context::AgentContext;
use crate::brain::provider::{ContentBlock, Role};
use crate::db::models::Message as DbMessage;

fn row(session: Uuid, role: &str, content: &str, seq: i32) -> DbMessage {
    DbMessage::new(session, role.to_string(), content.to_string(), seq)
}

#[test]
fn queued_follow_up_survives_the_rebuild_and_the_empty_placeholder_does_not() {
    let session = Uuid::new_v4();
    let rows = vec![
        row(session, "user", "card 394 is still not fixed", 1),
        row(session, "assistant", "Checking the v2 docs now.", 2),
        row(
            session,
            "user",
            "dev server has the latest code by the way",
            3,
        ),
        // The killed turn's assistant row: created at turn start, never filled.
        row(session, "assistant", "", 4),
    ];
    let context = AgentContext::from_db_messages(session, rows, 100_000);
    let messages = &context.messages;
    assert_eq!(messages.len(), 3, "the empty placeholder must be dropped");
    let last = messages.last().expect("three messages");
    assert_eq!(last.role, Role::User);
    assert!(
        matches!(&last.content[..], [ContentBlock::Text { text }] if text == "dev server has the latest code by the way"),
        "the queued follow-up must be the last thing the replayed model reads: {:?}",
        last.content
    );
    assert!(
        messages.iter().all(|m| m
            .content
            .iter()
            .all(|b| !matches!(b, ContentBlock::Text { text } if text.is_empty()))),
        "no empty text block may reach the provider"
    );
}

#[test]
fn plan_completed_excluded_from_provider_context() {
    let session = Uuid::new_v4();
    let rows = vec![
        row(session, "user", "keep this question", 1),
        row(
            session,
            crate::services::message::PLAN_COMPLETED_ROLE,
            r#"{"version":1,"plan":{"title":"raw-plan-json","tasks":[]}} <<IMG:/tmp/private-plan.png>>"#,
            2,
        ),
        row(session, "assistant", "keep this answer", 3),
    ];
    let context = AgentContext::from_db_messages(session, rows, 100_000);
    assert_eq!(context.messages.len(), 2);
    assert_eq!(context.messages[0].role, Role::User);
    assert_eq!(context.messages[1].role, Role::Assistant);
    assert!(context.messages.iter().all(|message| {
        message.content.len() == 1 && matches!(&message.content[0], ContentBlock::Text { .. })
    }));
    assert!(context.messages.iter().all(|message| {
        message.content.iter().all(|block| {
            matches!(block, ContentBlock::Text { text }
                if !text.contains("raw-plan-json") && !text.contains("<<IMG:"))
        })
    }));
}
