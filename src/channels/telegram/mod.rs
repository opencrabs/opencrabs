//! Telegram Bot Integration
//!
//! Runs a Telegram bot alongside the TUI, forwarding messages from
//! allowlisted users to the AgentService and replying with responses.

pub(crate) mod agent;
pub(crate) mod commands_tg;
pub(crate) mod cowork;
pub(crate) mod delivery;
pub(crate) mod edit_retry;
pub(crate) mod ephemeral;
pub(crate) mod flow;
pub(crate) mod flow_chrome;
pub(crate) mod governor;
pub(crate) mod group_name;
pub(crate) mod guest;
pub(crate) mod handler;
pub(crate) mod inbound_media;
pub(crate) mod intermediates;
pub(crate) mod keyboards;
pub(crate) mod markdown;
pub(crate) mod media;
pub(crate) mod member_events;
pub(crate) mod menu_auto;
pub(crate) mod menu_refresh;
pub(crate) mod menu_scope;
pub(crate) mod outbound_dedup;
pub(crate) mod picker_limits;
pub(crate) mod plan_card;
pub(crate) mod progress;
pub(crate) mod rate_limit;
pub(crate) mod raw_updates;
pub(crate) mod reaction_prompt;
pub(crate) mod resume;
pub(crate) mod rich;
pub(crate) mod rich_decode;
pub(crate) mod send;
pub(crate) mod session_gate;
pub(crate) mod session_resolve;
pub(crate) mod stream_loop;
pub(crate) mod suggest_options;
pub(crate) mod telemetry;
pub(crate) mod titles;
pub(crate) mod typing;

/// Record a created or renamed forum topic in the message store so `list_topics`
/// and session isolation immediately see it.
pub(crate) async fn record_topic_created(
    pool: Option<crate::db::Pool>,
    chat_id: i64,
    thread_id: i32,
    topic_name: &str,
    is_rename: bool,
) {
    let Some(pool) = pool.or_else(|| crate::db::global_pool().cloned()) else {
        tracing::warn!("record_topic_created: no DB pool — topic not recorded");
        return;
    };
    let repo = crate::db::ChannelMessageRepository::new(pool);
    let msg_type = if is_rename {
        "topic_edited"
    } else {
        "topic_created"
    };
    let content = if is_rename {
        format!("topic renamed to \"{topic_name}\"")
    } else {
        format!("topic created \"{topic_name}\"")
    };
    let row = crate::db::models::ChannelMessage::new(
        "telegram".into(),
        chat_id.to_string(),
        None,
        "bot".into(),
        "bot".into(),
        content,
        msg_type.into(),
        Some(thread_id.to_string()),
    )
    .with_thread(Some(thread_id.to_string()), Some(topic_name.to_string()));

    if let Err(e) = repo.insert(&row).await {
        tracing::warn!("Failed to persist topic record: {e}");
    }
}

pub use agent::TelegramAgent;
pub(crate) use agent::register_bot_commands;
#[cfg(test)]
pub(crate) use agent::{sanitize_command_name, truncate_description};

pub(crate) mod state;
pub use state::*;
#[cfg(feature = "telegram-userbot")]
pub(crate) mod userbot;
