//! Thread-aware Telegram send helpers.
//!
//! Wraps teloxide's `bot.send_message` / `send_photo` / `send_chat_action`
//! constructors with an `Option<ThreadId>` parameter so forum-topic replies
//! land in the originating topic instead of the group's General chat
//! (issue #130).
//!
//! Each helper returns the underlying teloxide request type, so existing
//! chains (`.parse_mode()`, `.reply_markup()`, `.reply_to_message_id()`,
//! `.await`) continue to work unchanged. The only call-site delta is the
//! function name + an extra `thread_id` argument.
//!
//! `thread_id = None` is a no-op — the helper produces the same request
//! you'd get from `bot.send_message(chat_id, text)` directly. Safe to use
//! everywhere even in non-topic chats.

use teloxide::Bot;
use teloxide::payloads::ForwardMessageSetters;
use teloxide::payloads::SendChatActionSetters;
use teloxide::payloads::SendDocumentSetters;
use teloxide::payloads::SendLocationSetters;
use teloxide::payloads::SendMessageSetters;
use teloxide::payloads::SendPhotoSetters;
use teloxide::payloads::SendPollSetters;
use teloxide::payloads::SendVoiceSetters;
use teloxide::prelude::Requester;
use teloxide::requests::JsonRequest;
use teloxide::types::{ChatAction, ChatId, InlineKeyboardMarkup, InputFile, MessageId, ThreadId};

/// Look up the thread_id of the most recent Telegram message stored for
/// `chat_id` in `channel_messages`. Returns `None` when no row exists,
/// when the row's thread_id is `NULL` (regular non-topic chat), or when
/// the stored value can't be parsed as an `i32`. Used by proactive send
/// paths (`telegram_send` tool, startup resume in cli/ui.rs) that have
/// no incoming `Message` to read `thread_id` from.
///
/// Reads via `crate::db::global_pool()` because the proactive surfaces
/// don't carry a `Pool` through their call chain. Returns `None` if the
/// global pool hasn't been initialized yet (early startup, tests).
pub async fn latest_thread_id_for_chat(chat_id: i64) -> Option<ThreadId> {
    let pool = crate::db::global_pool()?;
    let repo = crate::db::ChannelMessageRepository::new(pool.clone());
    let chat_id_str = chat_id.to_string();
    let rows = repo
        .recent(Some("telegram"), &chat_id_str, 1, None, None)
        .await
        .ok()?;
    let row = rows.into_iter().next()?;
    let tid_str = row.thread_id?;
    // A stored "1" is the General scoping key, not a thread anyone can post
    // to, so it resolves to no thread like every other read (#1319).
    tid_str
        .parse::<i32>()
        .ok()
        .and_then(|n| super::session_resolve::delivery_thread_id(Some(n)))
}

/// The thread a push for `session_id` must be delivered to (#1200, #1319).
///
/// A session's OWN binding is authoritative and the chat-wide lookup is the
/// last resort. [`super::session_resolve::push_target`] holds the decision
/// (and the reasoning); this is the I/O around it:
///
/// 1. The in-memory binding — the same data the ingress path just wrote. The
///    connect-time re-registration of #1224 loads EVERY persisted row into
///    these maps, so after connect they mirror the durable ones.
/// 2. The durable `session_bindings` row, read only when the maps hold
///    nothing for the session: a push can arrive in the window before that
///    re-registration has run.
/// 3. [`latest_thread_id_for_chat`] — correct only for a session with no
///    binding at all. Reaching it for a BOUND session is the bug: the push
///    lands in whichever topic spoke last.
pub async fn session_push_thread(
    state: &super::TelegramState,
    session_id: uuid::Uuid,
    chat_id: i64,
) -> Option<ThreadId> {
    let in_memory = state.session_binding(session_id).await;
    let durable = if in_memory.is_none() {
        persisted_binding(session_id).await
    } else {
        None
    };
    match super::session_resolve::push_target(in_memory, durable, chat_id) {
        super::session_resolve::PushTarget::Bound(topic) => {
            super::session_resolve::delivery_thread_id(topic)
        }
        super::session_resolve::PushTarget::Unbound => latest_thread_id_for_chat(chat_id).await,
    }
}

/// A session's persisted `(chat_id, topic)` binding (#1224). `None` when no
/// pool is initialized (early startup, tests) or the session has no row.
async fn persisted_binding(session_id: uuid::Uuid) -> Option<(i64, Option<i32>)> {
    let pool = crate::db::global_pool()?;
    let repo = crate::db::SessionBindingRepository::new(pool.clone());
    let row = repo
        .by_session(&session_id.to_string())
        .await
        .ok()
        .flatten()?;
    let chat = row.chat_id.parse::<i64>().ok()?;
    Some((chat, row.thread_id))
}

/// `bot.send_message(chat_id, text)` with optional `message_thread_id`.
/// Returns the teloxide request so callers can chain `.parse_mode()`,
/// `.reply_markup()`, etc. before `.await`.
pub fn message_in_thread<C, T>(
    bot: &Bot,
    chat_id: C,
    thread_id: Option<ThreadId>,
    text: T,
) -> JsonRequest<teloxide::payloads::SendMessage>
where
    C: Into<ChatId>,
    T: Into<String>,
{
    let req = bot.send_message(chat_id.into(), text.into());
    match thread_id {
        Some(t) => req.message_thread_id(t),
        None => req,
    }
}

/// `bot.send_photo(chat_id, photo)` with optional `message_thread_id`.
pub fn photo_in_thread<C>(
    bot: &Bot,
    chat_id: C,
    thread_id: Option<ThreadId>,
    photo: InputFile,
) -> teloxide::requests::MultipartRequest<teloxide::payloads::SendPhoto>
where
    C: Into<ChatId>,
{
    let req = bot.send_photo(chat_id.into(), photo);
    match thread_id {
        Some(t) => req.message_thread_id(t),
        None => req,
    }
}

/// `bot.send_document(chat_id, document)` with optional `message_thread_id`.
/// Completes the `*_in_thread` family for #1079: documents landed in General
/// in forum groups because the tool arm built its own request.
pub fn document_in_thread<C>(
    bot: &Bot,
    chat_id: C,
    thread_id: Option<ThreadId>,
    document: InputFile,
) -> teloxide::requests::MultipartRequest<teloxide::payloads::SendDocument>
where
    C: Into<ChatId>,
{
    let req = bot.send_document(chat_id.into(), document);
    match thread_id {
        Some(t) => req.message_thread_id(t),
        None => req,
    }
}

/// `bot.send_voice(chat_id, voice)` with optional `message_thread_id` (#1683):
/// TTS voice notes landed in General in forum groups because this was the
/// last send path calling the bare `bot.send_voice`.
pub fn voice_in_thread<C>(
    bot: &Bot,
    chat_id: C,
    thread_id: Option<ThreadId>,
    voice: InputFile,
) -> teloxide::requests::MultipartRequest<teloxide::payloads::SendVoice>
where
    C: Into<ChatId>,
{
    let req = bot.send_voice(chat_id.into(), voice);
    match thread_id {
        Some(t) => req.message_thread_id(t),
        None => req,
    }
}

/// `bot.send_location(chat_id, lat, lng)` with optional `message_thread_id` (#1079).
pub fn location_in_thread<C>(
    bot: &Bot,
    chat_id: C,
    thread_id: Option<ThreadId>,
    latitude: f64,
    longitude: f64,
) -> JsonRequest<teloxide::payloads::SendLocation>
where
    C: Into<ChatId>,
{
    let req = bot.send_location(chat_id.into(), latitude, longitude);
    match thread_id {
        Some(t) => req.message_thread_id(t),
        None => req,
    }
}

/// `bot.send_poll(chat_id, question, options)` with optional `message_thread_id`
/// (#1079).
///
/// `anonymous` is explicit because the Bot API default is TRUE, and an
/// anonymous vote carries no voter in `poll_answer`: the bot sees the option
/// but not who chose it, which makes a poll unless for a group decision (#2288
/// default is non-anonymous).
pub fn poll_in_thread<C>(
    bot: &Bot,
    chat_id: C,
    thread_id: Option<ThreadId>,
    question: String,
    options: Vec<teloxide::types::InputPollOption>,
    anonymous: bool,
) -> JsonRequest<teloxide::payloads::SendPoll>
where
    C: Into<ChatId>,
{
    let req = bot
        .send_poll(chat_id.into(), question, options)
        .is_anonymous(anonymous);
    match thread_id {
        Some(t) => req.message_thread_id(t),
        None => req,
    }
}

/// `bot.forward_message(to_chat, from_chat, message_id)` with optional
/// `message_thread_id` (#1079): forwards landed in General in forum groups.
pub fn forward_in_thread<C>(
    bot: &Bot,
    to_chat_id: C,
    from_chat_id: ChatId,
    message_id: MessageId,
    thread_id: Option<ThreadId>,
) -> JsonRequest<teloxide::payloads::ForwardMessage>
where
    C: Into<ChatId>,
{
    let req = bot.forward_message(to_chat_id.into(), from_chat_id, message_id);
    match thread_id {
        Some(t) => req.message_thread_id(t),
        None => req,
    }
}

/// `bot.send_chat_action(chat_id, action)` with optional `message_thread_id`.
/// The "typing" indicator goes to the right topic instead of the General
/// chat — important for forum groups where the bot is mentioned across
/// multiple topics.
pub fn chat_action_in_thread<C>(
    bot: &Bot,
    chat_id: C,
    thread_id: Option<ThreadId>,
    action: ChatAction,
) -> JsonRequest<teloxide::payloads::SendChatAction>
where
    C: Into<ChatId>,
{
    let req = bot.send_chat_action(chat_id.into(), action);
    match thread_id {
        Some(t) => req.message_thread_id(t),
        None => req,
    }
}

/// Delete a Telegram message, tolerating failure. Cleanup paths (streaming
/// placeholder teardown, recreate swaps, aborted flows) must never break on
/// a delete Telegram rejects — but silently dropping the error (the old
/// `let _ =` culture) hides real breakage. Warn on anything except
/// "already gone", which is the outcome the caller wanted anyway.
/// Model: `fire_reaction`. (#1085 P3)
pub async fn best_effort_delete<C>(bot: &Bot, chat_id: C, msg_id: MessageId, why: &str)
where
    C: Into<ChatId>,
{
    let chat = chat_id.into();
    super::telemetry::log_request(
        "system",
        why,
        "-",
        "delete",
        "deleteMessage",
        chat.0,
        None,
        Some(i64::from(msg_id.0)),
    );
    if let Err(e) = bot.delete_message(chat, msg_id).await {
        let text = e.to_string();
        let quiet =
            text.contains("message to delete not found") || text.contains("message id is invalid");
        if !quiet {
            // Review F4: the ids are in hand — a delete warn that cannot be
            // correlated to a chat is half a forensics record.
            tracing::warn!(
                "Telegram: best-effort delete failed ({}): chat={} msg={} err={}",
                why,
                chat.0,
                msg_id.0,
                e
            );
        }
    }
}

/// Fire a chat action (typing indicator) and warn if Telegram rejects it.
/// Pure cosmetics on the wire — never breaks a turn — but the failure line
/// tells forensics the bot was mid-turn when the API hiccuped. Awaited
/// sibling of [`chat_action_in_thread`] so call sites stop discarding the
/// Result. (#1085 P3)
///
/// G1 flood governor (#1211): every typing path on the surface funnels through
/// this one function, so the per-forum token bucket + per-topic coalescing in
/// [`super::governor::admit_chat_action`] applies here and nowhere else. A
/// suppressed refresh is a pure cosmetic loss — the indicator is stateless
/// and the next tick re-fires it.
pub async fn fire_chat_action<C>(
    bot: &Bot,
    chat_id: C,
    thread_id: Option<ThreadId>,
    action: ChatAction,
    why: &str,
) where
    C: Into<ChatId>,
{
    let chat = chat_id.into();
    if !super::governor::admit_chat_action(chat, thread_id.map(|t| t.0.0)).await {
        return;
    }
    // #721 E1: emitted AFTER the admit gate — a refresh the governor dropped
    // never goes on the wire, so it is not a request and must not inflate the
    // rate this line exists to measure.
    super::telemetry::log_request(
        "turn",
        why,
        "-",
        "typing",
        "sendChatAction",
        chat.0,
        thread_id.map(|t| i64::from(t.0.0)),
        None,
    );
    if let Err(e) = chat_action_in_thread(bot, chat, thread_id, action)
        .await
        .map(|_| ())
    {
        tracing::warn!("Telegram: chat action failed ({}): {}", why, e);
    }
}

/// Fire a message without breaking the caller, with correlation telemetry
/// on both exits (#1085 review F10). Single attempt by design: these are
/// system notices (welcomes, cowork setup, agent help replies) — the #297
/// delay-never-drop contract covers command replies, not courtesy pings,
/// and a single attempt can never stall a turn. Model: `best_effort_delete`.
#[allow(clippy::too_many_arguments)]
pub async fn best_effort_note<C>(
    bot: &Bot,
    chat_id: C,
    thread_id: Option<ThreadId>,
    text: &str,
    parse_mode: Option<teloxide::types::ParseMode>,
    origin: &str,
    origin_detail: &str,
    why: &str,
) where
    C: Into<ChatId>,
{
    let chat = chat_id.into();
    // G3 send pacing (#1211): system notices ride the same per-chat pacer as
    // every other full message. DMs (positive ids) pass straight through.
    super::governor::pace_send(chat).await;
    let len = text.len();
    let hash8 = super::telemetry::content_hash8(text);
    let request = message_in_thread(bot, chat, thread_id, text);
    let request = match parse_mode {
        Some(mode) => request.parse_mode(mode),
        None => request,
    };
    match request.await {
        Ok(m) => super::telemetry::log_send_success(
            origin,
            origin_detail,
            "-",
            "note",
            why,
            chat.0,
            thread_id.map(|t| t.0.0),
            m.id.0,
            len,
            &hash8,
        ),
        Err(e) => match super::edit_retry::classify(&e) {
            super::edit_retry::EditErr::RetryAfter(wait) => {
                tracing::warn!(
                    "Telegram: best-effort note 429 (retry after {wait:?}) — deferring one retry \
                     ({origin}/{origin_detail} {why}): chat={}",
                    chat.0
                );
                // #68: the note re-fires after the server-instructed wait;
                // the caller is long gone (this fn is fire-and-forget), so
                // exhaustion just warns — same net effect as before, but the
                // retry now lands instead of dying with attempt 1.
                let bot2 = bot.clone();
                let chat2 = chat;
                let thread2 = thread_id;
                let text2 = text.to_string();
                let mode2 = parse_mode;
                let origin2 = origin.to_string();
                let detail2 = origin_detail.to_string();
                let why2 = why.to_string();
                super::edit_retry::spawn_deferred(
                    wait,
                    move || async move {
                        let request = message_in_thread(&bot2, chat2, thread2, &text2);
                        let request = match mode2 {
                            Some(mode) => request.parse_mode(mode),
                            None => request,
                        };
                        request.await.map(|_| ())
                    },
                    move || async move {
                        tracing::warn!(
                            "Telegram: best-effort note dropped after deferred retry \
                             ({origin2}/{detail2} {why2}): chat={}",
                            chat.0
                        );
                    },
                );
            }
            super::edit_retry::EditErr::Fatal(msg) => {
                tracing::warn!(
                    "Telegram: best-effort note failed ({origin}/{origin_detail} {why}): chat={} err={msg}",
                    chat.0
                );
            }
        },
    }
}

/// Result of an outbox send, pairing the delivered message IDs and chunk texts
/// with the *effective* thread ID after any stale-topic fallback (#116, #169).
/// When a dead topic (HTTP 400 `message thread not found`) causes an unthreaded
/// retry, `effective_thread_id` becomes `None` (General/DM) so callers and
/// persistence cannot accidentally re-poison the database with the evicted topic.
#[derive(Debug, Clone)]
pub(crate) struct OutboxSent {
    pub(crate) sent: Vec<(i32, String)>,
    pub(crate) effective_thread_id: Option<teloxide::types::ThreadId>,
}

impl OutboxSent {
    /// Persist delivered outbox messages for reply recovery using the effective
    /// thread ID resolved during transmission (#169).
    pub(crate) async fn record_outgoing(&self, pool: Option<crate::db::Pool>, chat_id: i64) {
        record_outgoing(pool, chat_id, self.effective_thread_id, &self.sent).await;
    }
}

/// One send ladder for every proactive Telegram writer (#1085 P1b R2).
///
/// Owns the wire path end to end: rich-gate (whole message, never chunked —
/// a split table breaks) → `markdown_to_telegram_html` → 4096 chunks →
/// [`send_html_or_plain`] (which retries 429s per #297 and falls back to
/// plain text when Telegram rejects the markup). Callers keep their
/// delivery *decisions*; this function owns retry, thread routing,
/// fallback and telemetry so they stop being per-writer choices. This is
/// the deliberate Q4 behavior change from the #1085 grill: cron and the
/// telegram_send tool previously had NO plain-text fallback and would 400
/// on markup Telegram rejects — now they inherit it.
///
/// `origin`/`origin_detail` feed the correlation telemetry (cron → job
/// name, tool → arm name). Returns [`OutboxSent`] containing message ID
/// pairs and the effective thread ID for reply-recovery persistence.
/// Errors describe the failing attempt and name any chunks already delivered.
pub(crate) async fn send_markdown_outbox(
    bot: &Bot,
    chat_id: ChatId,
    mut thread_id: Option<ThreadId>,
    markdown: &str,
    origin: &str,
    origin_detail: &str,
    reply_to: Option<i32>,
) -> std::result::Result<OutboxSent, String> {
    // 1. Native rich, as a whole message. `post_rich` owns the telemetry
    // line for this send (with origin + detail threaded through), so the
    // outbox does not double-log the rich success (review F3/F8).
    if super::rich::should_send_native_rich(markdown) {
        match super::rich::send_rich_with_mermaid_target_id(
            bot.api_url().as_str(),
            bot.token(),
            chat_id.0,
            thread_id,
            reply_to,
            markdown,
            origin,
            origin_detail,
        )
        .await
        {
            Ok(id) => {
                return Ok(OutboxSent {
                    sent: vec![(id, markdown.to_string())],
                    effective_thread_id: thread_id,
                });
            }
            Err(e) => {
                // Stale-topic auto-route (#116): a remembered topic that was
                // deleted on Telegram's side makes EVERY thread-carrying send
                // fail with 400 `message thread not found` — rich AND (before
                // this fix) the plain fallback below, which re-used the same
                // poisoned thread. Evict the dead address chat-scoped and
                // retry this send ONCE unthreaded (General/DM = absence of a
                // thread, #1319). Any other rich failure falls through to the
                // HTML ladder with the thread intact.
                if e.to_string().contains("message thread not found") && thread_id.is_some() {
                    if let Some(tid) = thread_id {
                        let evicted = evict_dead_topic(chat_id.0, tid.0.0).await;
                        tracing::warn!(
                            "{origin}/{origin_detail}: remembered topic {} is gone \
                             (message thread not found) — evicted {evicted} rows, retrying unthreaded",
                            tid.0.0
                        );
                    }
                    thread_id = None;
                    match super::rich::send_rich_with_mermaid_target_id(
                        bot.api_url().as_str(),
                        bot.token(),
                        chat_id.0,
                        None,
                        reply_to,
                        markdown,
                        origin,
                        origin_detail,
                    )
                    .await
                    {
                        Ok(id) => {
                            return Ok(OutboxSent {
                                sent: vec![(id, markdown.to_string())],
                                effective_thread_id: None,
                            });
                        }
                        Err(e2) => {
                            tracing::warn!(
                                "{origin}/{origin_detail}: native rich send failed after \
                                 stale-topic fallback ({e2}) — falling back to HTML"
                            );
                        }
                    }
                } else {
                    tracing::warn!(
                        "{origin}/{origin_detail}: native rich send failed ({e}) — falling back to HTML"
                    );
                }
            }
        }
    }

    // 2. Universal HTML ladder, chunked to Telegram's limit. When the
    // stale-topic eviction above fired, `thread_id` is now None — the
    // ladder (and its plain-text fallback, the #116 poisoning leg) is
    // re-addressed to General/DM instead of the dead topic.
    let thread = thread_id.map(|t| t.0.0);
    let html = super::handler::markdown_to_telegram_html(markdown);
    let chunks = super::handler::split_message(&html, 4096);
    let total = chunks.len();
    let mut sent: Vec<(i32, String)> = Vec::new();
    for (i, chunk) in chunks.into_iter().enumerate() {
        match super::intermediates::send_html_or_plain(
            bot, chat_id, thread_id, chunk, origin, reply_to,
        )
        .await
        {
            Ok(mid) => {
                super::telemetry::log_send_success(
                    origin,
                    origin_detail,
                    "-",
                    "outbox",
                    "html_chunk",
                    chat_id.0,
                    thread,
                    mid.0,
                    chunk.len(),
                    &super::telemetry::content_hash8(chunk),
                );
                sent.push((mid.0, chunk.to_string()));
            }
            Err(e) => {
                // Plain-path leg of the #116 poisoning chain: a message that
                // is NOT rich-shaped never entered the rich arm, so its first
                // sight of the dead topic is here — the HTML ladder 400s and
                // (pre-fix) the plain fallback re-used the same thread and
                // 400'd too. Same medicine: evict chat-scoped, retry the
                // chunk once unthreaded. Any other error is returned as
                // before.
                let es = e.to_string();
                if es.contains("message thread not found") && thread_id.is_some() {
                    if let Some(tid) = thread_id {
                        let evicted = evict_dead_topic(chat_id.0, tid.0.0).await;
                        tracing::warn!(
                            "{origin}/{origin_detail}: HTML ladder hit dead topic {} \
                             — evicted {evicted} rows, retrying chunk unthreaded",
                            tid.0.0
                        );
                    }
                    thread_id = None;
                    match super::intermediates::send_html_or_plain(
                        bot, chat_id, None, chunk, origin, reply_to,
                    )
                    .await
                    {
                        Ok(mid) => {
                            super::telemetry::log_send_success(
                                origin,
                                origin_detail,
                                "-",
                                "outbox",
                                "html_chunk_unthreaded",
                                chat_id.0,
                                None,
                                mid.0,
                                chunk.len(),
                                &super::telemetry::content_hash8(chunk),
                            );
                            sent.push((mid.0, chunk.to_string()));
                            continue;
                        }
                        Err(e2) => {
                            let partial = if sent.is_empty() {
                                String::new()
                            } else {
                                format!(" ({} of {total} chunks already delivered)", sent.len())
                            };
                            return Err(format!(
                                "{origin}/{origin_detail} chunk {}/{total} failed after \
                                 stale-topic fallback{partial}: {e2}",
                                i + 1
                            ));
                        }
                    }
                }
                let partial = if sent.is_empty() {
                    String::new()
                } else {
                    format!(" ({} of {total} chunks already delivered)", sent.len())
                };
                return Err(format!(
                    "{origin}/{origin_detail} chunk {}/{total} failed{partial}: {e}",
                    i + 1
                ));
            }
        }
    }
    Ok(OutboxSent {
        sent,
        effective_thread_id: thread_id,
    })
}

/// Evict a dead forum-topic address chat-scoped (#116): clear the
/// `thread_id` on `channel_messages` rows of THIS chat that carry it, so
/// `latest_thread_id_for_chat` never serves the deleted topic again. The
/// in-memory session-topic map self-heals on the chat's next inbound
/// `is_topic_message` (re-registered on every event), so only the stored
/// rows need clearing here.
pub(crate) async fn evict_dead_topic(chat_id: i64, thread_id: i32) -> u64 {
    let Some(pool) = crate::db::global_pool().cloned() else {
        return 0;
    };
    let repo = crate::db::ChannelMessageRepository::new(pool);
    match repo
        .clear_thread_for_chat("telegram", &chat_id.to_string(), &thread_id.to_string())
        .await
    {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                "stale-topic eviction for chat {chat_id} thread {thread_id} failed: {e}"
            );
            0
        }
    }
}

/// Persist delivered outbox messages for reply recovery (#234, #1085 P1b
/// R2). One implementation of what cron's `deliver_telegram` and the tool's
/// `persist_outgoing` previously built separately as byte-identical rows.
/// `thread_id` is stamped when known (cron rows previously lost it).
pub(crate) async fn record_outgoing(
    pool: Option<crate::db::Pool>,
    chat_id: i64,
    thread_id: Option<ThreadId>,
    sent: &[(i32, String)],
) {
    if sent.is_empty() {
        return;
    }
    let Some(pool) = pool.or_else(|| crate::db::global_pool().cloned()) else {
        tracing::warn!("telegram outbox: no DB pool — outgoing messages not persisted");
        return;
    };
    let repo = crate::db::ChannelMessageRepository::new(pool);
    let chat_id_str = chat_id.to_string();
    let thread = thread_id.map(|t| t.0.0.to_string());
    for (mid, content) in sent {
        if content.trim().is_empty() {
            continue;
        }
        let cm = crate::db::models::ChannelMessage::new(
            "telegram".to_string(),
            chat_id_str.clone(),
            None,
            "bot:opencrabs".to_string(),
            "OpenCrabs".to_string(),
            content.clone(),
            "text".to_string(),
            Some(mid.to_string()),
        )
        .with_thread(thread.clone(), None);
        if let Err(e) = repo.insert(&cm).await {
            tracing::warn!(
                "telegram outbox: failed to persist message {mid} for reply-recovery: {e}"
            );
        }
    }
}

/// Raw Bot API `sendMessage` with an inline keyboard — the #118 fix.
///
/// The teloxide request chain (`message_in_thread(...).parse_mode(..)
/// .reply_markup(..)`) on this build silently drops both setters: stored
/// probes carry no entities and no `reply_markup` while the arm logs ok.
/// This helper posts the exact same payload as raw JSON to the Bot API —
/// the same wire the rich plane (`rich::api::post_rich`) and the ephemeral
/// sends (`ephemeral::post`) already use in production, where keyboards
/// store correctly. Returns the sent message on Telegram's `ok:true`.
///
/// 429s wait out `retry_after` (capped) and retry once, mirroring
/// `ephemeral::post`'s ladder; other failures return the API error text.
pub(crate) async fn send_buttons_raw(
    token: &str,
    chat_id: i64,
    thread_id: Option<ThreadId>,
    html: &str,
    keyboard: &InlineKeyboardMarkup,
) -> Result<serde_json::Value, String> {
    let mut payload = serde_json::json!({
        "chat_id": chat_id,
        "text": html,
        "parse_mode": "HTML",
        "reply_markup": keyboard,
    });
    if let Some(t) = thread_id {
        payload["message_thread_id"] = serde_json::json!(t.0.0);
    }
    // #118 wire evidence: log the EXACT payload leaving the process — body bytes
    // (len+hash8) and the serialized keyboard row count. This is the logging gap
    // that cost a full morning: text telemetry alone cannot distinguish a dropped
    // keyboard from a malformed one.
    let kb_rows = keyboard.inline_keyboard.len();
    let wire_body = serde_json::to_string(&payload).unwrap_or_default();
    tracing::info!(
        "send_buttons wire: body_len={} body_hash8={} kb_rows={} kb_len={} chat={} thread={:?}",
        wire_body.len(),
        crate::channels::telegram::telemetry::content_hash8(&wire_body),
        kb_rows,
        serde_json::to_string(&keyboard)
            .map(|s| s.len())
            .unwrap_or(0),
        chat_id,
        thread_id.map(|t| t.0.0),
    );
    if kb_rows == 0 {
        return Err(
            "no buttons parsed from 'buttons' input — refusing to send a keyboard-less message"
                .to_string(),
        );
    }
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let client = reqwest::Client::new();
    let mut attempt = 0u32;
    loop {
        let resp = client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("transport: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if status.as_u16() == 429 {
            if attempt >= 1 {
                return Err("rate-limited after retry".to_string());
            }
            attempt += 1;
            let wait = parsed
                .get("parameters")
                .and_then(|p| p.get("retry_after"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(5)
                .min(15);
            let wait = std::time::Duration::from_secs(wait);
            // This path sleeps on its own rather than going through `wait_out`,
            // so without this the 429 stays private to one send — both to the
            // process and to this chat's own buckets (#635).
            super::governor::note_429_pause(chat_id, wait);
            tokio::time::sleep(wait).await;
            continue;
        }
        if status.is_success()
            && parsed.get("ok").and_then(serde_json::Value::as_bool) == Some(true)
        {
            return Ok(parsed
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null));
        }
        let desc = parsed
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("({status}): {desc}"));
    }
}
