//! Best-effort Bot API title lookups for human-readable sender labels on
//! session_notify echo bubbles (#1225). Raw HTTP, same wire style as
//! `rich/api.rs`, kept on one tiny code path.
//!
//! Only `getChat` lives here: it is a real Bot API method and covers groups
//! and channels. Forum-TOPIC names do NOT belong here — the Bot API has no
//! method to query a topic's title (bots can only learn names passively;
//! telegram-bot-api issues #634/#356), so `resume.rs` resolves those from
//! the local `channel_messages.topic_name` store instead. Private chats
//! (DM sessions) never reach here either: the reader IS the chat's human
//! side, so their own name back as the sender is useless — `sender_label`
//! labels the BOT itself from the startup `get_me` cache (owner ruling
//! 2026-08-28).
//!
//! Every lookup is best-effort — `None` on any failure feeds the short-id
//! fallback in `resume.rs`, never an error bubble.

use std::time::Duration;
use teloxide::types::ChatId;

fn api_base(api_url: &str) -> &str {
    api_url.trim_end_matches('/')
}

/// One-shot POST to a Bot API method; returns `result` on `ok: true`.
async fn post_api(
    api_url: &str,
    token: &str,
    method: &str,
    body: serde_json::Value,
) -> Option<serde_json::Value> {
    let url = format!("{}/bot{token}/{method}", api_base(api_url));
    let client = crate::utils::http::shared();
    let resp = client
        .post(url)
        .timeout(Duration::from_secs(5))
        .json(&body)
        .send()
        .await
        .ok()?;
    let parsed: serde_json::Value = resp.json().await.ok()?;
    if parsed.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        parsed.get("result").cloned()
    } else {
        None
    }
}

/// Chat display name: `title` for groups/channels, else the account's
/// `username`. Private chats never reach this function — DM sessions are
/// labelled with the bot's own username in `resume.rs::sender_label`.
pub(crate) async fn chat_title(api_url: &str, token: &str, chat_id: ChatId) -> Option<String> {
    let result = post_api(
        api_url,
        token,
        "getChat",
        serde_json::json!({ "chat_id": chat_id.0 }),
    )
    .await?;
    result
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            result
                .get("username")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
}
