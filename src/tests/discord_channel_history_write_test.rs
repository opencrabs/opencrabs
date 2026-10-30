//! #2196: outbound Discord messages reach `channel_messages` no matter which
//! surface produced them.
//!
//! The table used to be written from exactly two places in `handler.rs`: the
//! inbound typed message and the reply on that same branch. Resumed turns,
//! background turns, taps, select menus, modals, reactions and every
//! `discord_send` tool call wrote nothing, so `channel_search` found questions
//! and not answers. The live probe in the issue (a message sent through the
//! tool) left zero rows.
//!
//! The row shape is tested against a real in-memory DB; the wiring is pinned
//! against the source, the house pattern for paths that need a live gateway.

use crate::channels::discord::outbox;
use crate::db::{ChannelMessageRepository, Database};

const CHANNEL: u64 = 1555478644816875633;

#[tokio::test]
async fn discord_send_records_channel_message() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    // The exact shape the `discord_send` probe in #2196 sent.
    outbox::record_row(
        Some(db.pool().clone()),
        CHANNEL,
        Some("guild".to_string()),
        4242,
        None,
        "Audit probe D11",
    )
    .await;

    let rows = repo
        .recent(Some("discord"), &CHANNEL.to_string(), 10, None, None)
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a message that reached Discord must leave exactly one row"
    );
    assert_eq!(rows[0].content, "Audit probe D11");
    assert_eq!(rows[0].channel, "discord");
    assert_eq!(rows[0].sender_name, "OpenCrabs");
    assert_eq!(
        rows[0].platform_message_id.as_deref(),
        Some("4242"),
        "the platform id is what a later reply lookup keys on"
    );
    assert_eq!(
        rows[0].channel_chat_name.as_deref(),
        Some("guild"),
        "the guild name travels with the row"
    );
}

#[tokio::test]
async fn an_empty_body_writes_nothing() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    outbox::record_row(Some(db.pool().clone()), CHANNEL, None, 1, None, "   \n ").await;

    let rows = repo
        .recent(Some("discord"), &CHANNEL.to_string(), 10, None, None)
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "a whitespace-only send must not create a transcript row"
    );
}

#[tokio::test]
async fn a_bot_row_names_the_bot_as_sender() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    // The governor has no `Message` author in hand on the recorded path, so a
    // missing sender must fall back to the bot identity rather than an empty
    // string the transcript cannot attribute.
    outbox::record_row(Some(db.pool().clone()), CHANNEL, None, 7, None, "hi").await;

    let rows = repo
        .recent(Some("discord"), &CHANNEL.to_string(), 10, None, None)
        .await
        .unwrap();
    assert_eq!(rows[0].sender_id, "bot:opencrabs");
}

/// The tool is the path the issue's probe used, and it used to bypass the
/// governor entirely with a bare `channel.send_message`.
#[test]
fn the_discord_send_tool_routes_through_the_recorded_governor() {
    let tool = include_str!("../brain/tools/discord_send.rs");
    assert!(
        tool.contains("governor::say_recorded("),
        "the tool's plain-text sends must go through the recorded path"
    );
    assert!(
        tool.contains("governor::send_content_recorded("),
        "the tool's builder sends must go through the recorded path"
    );
    assert!(
        !tool.contains(".send_message(&http") && !tool.contains(".say(&http"),
        "a raw serenity send in the tool bypasses both the budget and the transcript"
    );
}

/// Answer delivery on every turn surface uses the recorded variant; the
/// tool-group bubble and the ticker keep the plain one, because rendered group
/// markdown has no business in the transcript the next turn reads back.
#[test]
fn every_answer_surface_records_and_chrome_does_not() {
    for (name, source) in [
        ("handler", include_str!("../channels/discord/handler.rs")),
        (
            "tracked_turn",
            include_str!("../channels/discord/tracked_turn.rs"),
        ),
        (
            "interactions",
            include_str!("../channels/discord/interactions.rs"),
        ),
        (
            "reactions",
            include_str!("../channels/discord/reactions.rs"),
        ),
    ] {
        assert!(
            source.contains("governor::say_recorded("),
            "{name} delivers answers and must record them"
        );
    }

    let handler = include_str!("../channels/discord/handler.rs");
    assert!(
        handler.contains("render_content(&turn_shell)"),
        "the turn shell is still rendered on the plain path"
    );
    let shell_at = handler.find("render_content(&turn_shell)").unwrap();
    let preceding = &handler[shell_at.saturating_sub(400)..shell_at];
    assert!(
        !preceding.contains("say_recorded"),
        "the turn shell is chrome: recording it would put group markdown in the transcript"
    );
}
