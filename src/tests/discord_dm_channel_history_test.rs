//! #2025: a Discord DM turn reaches `channel_messages` on both sides, so
//! `channel_search` can find DM conversations as it can for Telegram, Slack
//! and WhatsApp. The message path needs a live gateway, so the guards are
//! pinned against the source, the house pattern for these handlers.
//!
//! #2196 replaced the handler-local reply write with the governor's recorded
//! send. The DM half of #2025 survives and is stronger: a DM reply is recorded
//! whether or not it carries a platform id, because the gate that skipped it
//! is gone.

use crate::channels::discord::outbox;
use crate::db::{ChannelMessageRepository, Database};

const HANDLER: &str = include_str!("../channels/discord/handler.rs");

#[test]
fn inbound_dm_messages_are_stored_too() {
    assert!(
        !HANDLER.contains("if !is_dm {\n        store_channel_msg(msg.content.clone()).await;"),
        "the inbound store must not be guarded by !is_dm"
    );
    assert!(
        HANDLER.contains("store_channel_msg(msg.content.clone()).await;\n"),
        "the directed message is stored unconditionally"
    );
}

#[test]
fn a_dm_reply_is_stored_without_a_reply_id_gate() {
    assert!(
        !HANDLER.contains("if (!is_dm || reply_msg_id.is_some())"),
        "the reply write no longer runs through the DM gate; the governor records it"
    );
    assert!(
        !HANDLER.contains("let mut reply_msg_id"),
        "the reply id tracker existed only for the removed handler-local write"
    );
    assert!(
        HANDLER.contains("governor::say_recorded("),
        "the typed path delivers its answer through the recorded send"
    );
}

#[tokio::test]
async fn a_dm_row_carries_no_guild_name() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    // A DM has no guild, and the row must still land: this is the exact case
    // the old gate dropped when the reply carried no message id.
    outbox::record_row(Some(db.pool().clone()), 424242, None, 99, None, "dm answer").await;

    let rows = repo
        .recent(Some("discord"), "424242", 10, None, None)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "a DM answer must reach the transcript");
    assert_eq!(rows[0].channel_chat_name, None);
}
