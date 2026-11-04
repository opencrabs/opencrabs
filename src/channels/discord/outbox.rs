//! Outbound transcript recording for Discord (#2196).
//!
//! `channel_messages` used to be written from exactly two places, both inside
//! `handler.rs`: the inbound typed message and the bot's reply on that same
//! branch. Resumed turns, background turns, suggestion taps, select menus,
//! modals, reactions and every `discord_send` tool call wrote nothing, so
//! `channel_search` and the guild `recent()` context saw questions without
//! answers. Four Discord chats in this instance ended with a user message in
//! the table, which reads as "unanswered" when the truth is "unrecorded".
//!
//! Recording now happens where a message actually leaves the bot. The
//! governor is that chokepoint, but the plain [`super::governor::say`] is also
//! how the tool-group bubble and the flow ticker repaint themselves, and
//! rendered tool-group markdown has no business in the transcript. So the
//! governor grows explicit recorded variants ([`super::governor::say_recorded`],
//! [`super::governor::send_content_recorded`]) and only the answer-delivery
//! sites and the `discord_send` tool use them. One helper, one row shape, no
//! per-handler branches to forget.
//!
//! Mirrors `crate::channels::telegram::send::record_outgoing`: same skip on
//! empty content, same `global_pool` fallback, same "log and continue" on a
//! write failure so a transcript problem can never fail a send that already
//! reached Discord.

use serenity::model::channel::Message;

use crate::db::{ChannelMessageRepository, Pool, models::ChannelMessage};

/// Record one outbound Discord message that already reached the wire.
///
/// `pool` is explicit for callers that have one (tests, tool context) and
/// `None` for the governor, which resolves the process pool. A missing pool
/// is a warning, never an error: the message is already delivered.
pub(crate) async fn record_sent(pool: Option<Pool>, msg: &Message, content: &str) {
    let guild = msg.guild_id.map(|g| g.get().to_string());
    let bot_id = msg.author.id.get();
    record_row(
        pool,
        msg.channel_id.get(),
        guild,
        msg.id.get(),
        Some(bot_id),
        content,
    )
    .await;
}

/// The row builder every recorded write goes through.
///
/// Split from [`record_sent`] so the shape is testable without a live
/// gateway: `Message` cannot be constructed in a unit test, the row can.
pub(crate) async fn record_row(
    pool: Option<Pool>,
    channel_id: u64,
    guild_name: Option<String>,
    message_id: u64,
    sender_id: Option<u64>,
    content: &str,
) {
    if content.trim().is_empty() {
        return;
    }
    let Some(pool) = pool.or_else(|| crate::db::global_pool().cloned()) else {
        tracing::warn!("discord outbox: no DB pool, outgoing message not recorded");
        return;
    };
    let sender_id = sender_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "bot:opencrabs".to_string());
    let cm = ChannelMessage::new(
        "discord".to_string(),
        channel_id.to_string(),
        guild_name,
        sender_id,
        "OpenCrabs".to_string(),
        content.to_string(),
        "text".to_string(),
        Some(message_id.to_string()),
    );
    let repo = ChannelMessageRepository::new(pool);
    if let Err(e) = repo.insert(&cm).await {
        tracing::warn!("discord outbox: failed to record message {message_id}: {e}");
    }
}
