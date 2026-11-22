//! Ephemeral group replies: Bot API 10.2 (`receiver_user_id`), 2026-07-14.
//!
//! Telegram can deliver a group message to a single member: nobody else in
//! the chat ever sees it. Every OpenCrabs slash command is owner-gated, so
//! today the whole group watches replies addressed to one person. Scoping
//! those to the invoker keeps the group context and drops the noise (#756).
//!
//! teloxide 0.17 / teloxide-core 0.13 has no binding for the parameter, so
//! this calls `sendMessage` directly over HTTP, mirroring [`super::rich::api`].
//! Whether `sendRichMessage` also accepts the parameter is settled at runtime
//! rather than assumed: see [`try_send_rich`]. Scoped replies fall back to
//! HTML only once the server has actually refused the rich variant.
//!
//! Nothing here returns an error. A send that does not land reports how much
//! was delivered and the caller finishes the job on its normal public path,
//! which is exactly the pre-10.2 behaviour. That covers both a Bot API server
//! older than 10.2 and a future rename of the parameter.

use std::sync::atomic::{AtomicU8, Ordering};
use teloxide::Bot;
use teloxide::types::{ChatId, ThreadId};

/// The user an ephemeral reply should be scoped to, or `None` when the reply
/// must stay an ordinary message.
///
/// A DM is already private and has no other members to hide the reply from,
/// so `receiver_user_id` buys nothing there, and asking for it would trade a
/// working plain send for an untested one.
pub(crate) fn receiver_for(is_dm: bool, user_id: i64) -> Option<i64> {
    if is_dm { None } else { Some(user_id) }
}

/// Build the `sendMessage` body for an ephemeral reply. Split out from the
/// transport so the request shape is unit-testable without a live bot.
///
/// `parse_html` mirrors the caller's existing choice: command output that
/// went through `command_md_to_html` needs `parse_mode`, bare status acks
/// must not have it or their `<`/`&` would be read as markup.
pub(crate) fn build_body(
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    text: &str,
    parse_html: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "receiver_user_id": receiver_user_id,
    });
    if parse_html {
        body["parse_mode"] = serde_json::json!("HTML");
    }
    if let Some(t) = thread_id {
        // ThreadId wraps a MessageId(i32).
        body["message_thread_id"] = serde_json::json!(t.0.0);
    }
    body
}

/// Deliver one ephemeral reply. `true` when it landed; `false` means the
/// caller must send `text` publicly as it did before 10.2.
pub(crate) async fn send_one(
    token: &str,
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    text: &str,
    parse_html: bool,
) -> bool {
    let body = build_body(chat_id, thread_id, receiver_user_id, text, parse_html);
    post(token, "sendMessage", &body).await == Outcome::Sent
}

/// Build the scoped `sendRichMessage` body: exactly what the public rich path
/// sends, plus the scoping field. Split out so the shape stays testable and so
/// the two paths cannot drift.
pub(crate) fn build_rich_body(
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    markdown: &str,
) -> serde_json::Value {
    let mut body = super::rich::api::build_body(chat_id, thread_id, markdown, None);
    body["receiver_user_id"] = serde_json::json!(receiver_user_id);
    body
}

/// Whether `sendRichMessage` accepts `receiver_user_id`, as answered by the
/// server rather than assumed here. See [`try_send_rich`].
static RICH_SCOPING: AtomicU8 = AtomicU8::new(RICH_UNKNOWN);
const RICH_UNKNOWN: u8 = 0;
const RICH_SUPPORTED: u8 = 1;
const RICH_UNSUPPORTED: u8 = 2;

/// Try to deliver `markdown` as a *native rich* message scoped to one user,
/// so a table or heading keeps its real Telegram rendering while staying
/// private. `true` when it landed.
///
/// The 10.2 changelog enumerates the methods that gained `receiver_user_id`
/// (`sendMessage` and the media senders) and `sendRichMessage` is not among
/// them, but that method's own parameter table was never available to confirm
/// the omission. So this asks the server instead of hard-coding the guess: the
/// first group reply attempts it, and the answer is remembered for the life of
/// the process. If the API does support it, every later reply gets native rich
/// blocks *and* privacy; if not, exactly one call is wasted before the HTML
/// path takes over for good.
///
/// A transport error leaves the answer `UNKNOWN` on purpose: a dropped
/// connection is not the server declining the parameter, and caching it as one
/// would forfeit native rich for the rest of the process over a network blip.
pub(crate) async fn try_send_rich(
    token: &str,
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    markdown: &str,
) -> bool {
    if RICH_SCOPING.load(Ordering::Relaxed) == RICH_UNSUPPORTED {
        return false;
    }
    let body = build_rich_body(chat_id, thread_id, receiver_user_id, markdown);
    match post(token, "sendRichMessage", &body).await {
        Outcome::Sent => {
            if RICH_SCOPING.swap(RICH_SUPPORTED, Ordering::Relaxed) == RICH_UNKNOWN {
                tracing::info!(
                    "Telegram: sendRichMessage accepts receiver_user_id, \
                     scoped command replies keep native rich rendering"
                );
            }
            true
        }
        Outcome::Rejected => {
            RICH_SCOPING.store(RICH_UNSUPPORTED, Ordering::Relaxed);
            tracing::info!(
                "Telegram: sendRichMessage does not accept receiver_user_id, \
                 scoped command replies will render as HTML from here on"
            );
            false
        }
        Outcome::Transport => false,
    }
}

/// Deliver `chunks` in order, scoped to `receiver_user_id`.
///
/// Returns how many *leading* chunks landed, so the caller can finish the
/// message publicly without duplicating what already arrived: `0` means fall
/// back wholesale, a short count means public-send `chunks[n..]`. Stops at the
/// first failure rather than interleaving delivered and dropped chunks, which
/// would reorder a split reply.
pub(crate) async fn send_html_chunks(
    token: &str,
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    chunks: &[&str],
) -> usize {
    let mut delivered = 0usize;
    for chunk in chunks {
        if !send_one(token, chat_id, thread_id, receiver_user_id, chunk, true).await {
            break;
        }
        delivered += 1;
    }
    delivered
}

/// Send a one-off command ack: scoped to `receiver` when there is one, and
/// on the ordinary rate-limit-retrying public path otherwise.
///
/// `receiver` comes from [`receiver_for`], so `None` (a DM) skips straight to
/// the public send. Acks are plain text: they carry no markup and must not be
/// parsed as HTML.
pub(crate) async fn send_ack(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    receiver: Option<i64>,
    text: &str,
) -> std::result::Result<(), teloxide::RequestError> {
    if let Some(rx) = receiver
        && send_one(bot.token(), chat_id.0, thread_id, rx, text, false).await
    {
        return Ok(());
    }
    super::intermediates::send_retrying_rate_limit("command reply", || {
        super::send::message_in_thread(bot, chat_id, thread_id, text)
    })
    .await
    .map(|_| ())
}

/// What the server did with an ephemeral send.
///
/// `Rejected` and `Transport` both leave the caller sending publicly, but only
/// `Rejected` is the server stating an opinion. A dropped connection says
/// nothing about what the API supports, so the two must not be conflated when
/// caching a capability.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Sent,
    Rejected,
    Transport,
}

/// POST an ephemeral send to `method`, logging why it did not land.
///
/// A 400 here is the expected shape of "this server predates 10.2" or "the
/// bot may not scope messages in this chat", so it is a warning and the
/// caller recovers, but it is never swallowed, because a silent failure
/// looks identical to the feature working while every reply stays public.
///
/// A 429 is neither an opinion nor a plain transport failure: it is throttling.
/// Waiting out the server's `retry_after` (capped) and retrying once usually
/// lands the send; if it does not, the outcome is `Transport`, never
/// `Rejected` — caching a throttle as a capability verdict would forfeit the
/// feature for the life of the process over a busy minute (the exact mistake
/// `Outcome`'s doc warns about).
async fn post(token: &str, method: &str, body: &serde_json::Value) -> Outcome {
    const RETRY_AFTER_CAP: std::time::Duration = std::time::Duration::from_secs(15);
    let url = format!("https://api.telegram.org/bot{token}/{method}");
    let mut resp = match crate::utils::http::shared()
        .post(&url)
        .json(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Telegram: ephemeral {method} transport error: {e}, sending publicly");
            return Outcome::Transport;
        }
    };
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let text = resp.text().await.unwrap_or_default();
        let retry_after = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("parameters")
                    .and_then(|p| p.get("retry_after"))
                    .and_then(serde_json::Value::as_u64)
            })
            .map(std::time::Duration::from_secs)
            .unwrap_or(RETRY_AFTER_CAP)
            .min(RETRY_AFTER_CAP);
        tracing::warn!(
            "Telegram: ephemeral {method} throttled, waiting {retry_after:?} and retrying once"
        );
        tokio::time::sleep(retry_after).await;
        resp = match crate::utils::http::shared()
            .post(&url)
            .json(body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "Telegram: ephemeral {method} transport error on retry: {e}, sending publicly"
                );
                return Outcome::Transport;
            }
        };
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!(
                "Telegram: ephemeral {method} still throttled after retry, sending publicly"
            );
            return Outcome::Transport;
        }
    }
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();

    if status.is_success() && parsed.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Outcome::Sent;
    }

    let desc = parsed
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&text);
    tracing::warn!("Telegram: ephemeral {method} rejected ({status}): {desc}, sending publicly");
    Outcome::Rejected
}
