//! Telegram Send Tool
//!
//! Agent-callable tool for full Telegram control: send, reply, edit, delete,
//! pin/unpin, forward, media, polls, inline buttons, chat info, moderation,
//! and reactions. Always prefer this tool over http_request — credentials
//! are handled securely.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolHints, ToolResult};
use crate::channels::telegram::TelegramState;
use crate::channels::telegram::intermediates::send_retrying_rate_limit;
use crate::channels::telegram::telemetry::{
    content_hash8, log_request, log_send_failure, log_send_success,
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use teloxide::payloads::SendDocumentSetters;
use teloxide::payloads::SendPhotoSetters;
use teloxide::prelude::*;
use teloxide::types::{
    ChatId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, MessageId, ReactionType,
    ReplyParameters, ThreadId, UserId,
};
use uuid::Uuid;

/// Tool for comprehensive Telegram bot control (19 actions).
pub struct TelegramSendTool {
    telegram_state: Arc<TelegramState>,
}

impl TelegramSendTool {
    pub fn new(telegram_state: Arc<TelegramState>) -> Self {
        Self { telegram_state }
    }
}

/// Extract a required non-empty string param, returning ToolResult::error on failure.
#[allow(clippy::result_large_err)]
fn get_str<'a>(input: &'a Value, key: &str) -> std::result::Result<&'a str, ToolResult> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(ToolResult::error(format!(
            "Missing required parameter '{key}'."
        ))),
    }
}

/// Extract an i64 from a JSON value, coercing numeric strings (#646).
/// Schemas declare "integer" but models often quote the value
/// (`"chat_id": "123456"`), and the old `as_i64()` silently dropped it —
/// `get_id` errored on required params and `chat_or_err` misrouted to the
/// session fallback.
pub(crate) fn value_as_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
}

/// Same string coercion for f64 params — latitude/longitude (#646).
pub(crate) fn value_as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
}

/// Parse a required integer param as i64.
#[allow(clippy::result_large_err)]
fn get_id(input: &Value, key: &str) -> std::result::Result<i64, ToolResult> {
    match input.get(key).and_then(value_as_i64) {
        Some(id) => Ok(id),
        None => Err(ToolResult::error(format!(
            "Missing required parameter '{key}' (must be an integer)."
        ))),
    }
}

/// Resolve a media reference (`photo_url` / `document_url`) into a Telegram
/// `InputFile`. An HTTP(S) URL is handed to Telegram as-is (it fetches the
/// remote file). Anything else is treated as a local path: the file is read
/// into memory and uploaded directly, the same way the channel handler sends
/// generated images and voice notes. Without this, local paths were passed to
/// `InputFile::url()` and rejected as invalid URLs (#181).
#[allow(clippy::result_large_err)]
pub(crate) async fn resolve_input_file(
    reference: &str,
    label: &str,
) -> std::result::Result<InputFile, ToolResult> {
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return reference
            .parse()
            .map(InputFile::url)
            .map_err(|e| ToolResult::error(format!("Invalid {label}: {e}")));
    }

    // Local file: read bytes and upload from memory.
    let path = crate::brain::tools::error::expand_tilde(reference);
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            Ok(InputFile::memory(bytes).file_name(name))
        }
        Err(e) => Err(ToolResult::error(format!(
            "Failed to read local {label} '{}': {e}",
            path.display()
        ))),
    }
}

/// Resolve chat_id: explicit param or owner fallback.
#[allow(clippy::result_large_err)]
/// Resolve a forum-topic `thread_id` for a proactive Telegram send.
///
/// Precedence:
///   0. Explicit `thread_id: null` — "General / no thread". Distinct from
///      the field being absent, which means "wherever this session lives".
///   1. Explicit `thread_id` field in the tool input — the agent
///      asked for a specific topic, honour it (thread 1 means General, so it
///      resolves to no thread). Lets cron jobs / the
///      agent route messages to a topic OTHER than the most recent
///      one (e.g. "post the release notes in #announcements even
///      though the last message came from #dev").
///   2. Session origin binding via `session_push_thread` — ONLY when the
///      resolved target chat IS the session-origin chat (#127). The forum
///      topic this interaction started in; makes replies land back in the
///      originating topic with no explicit routing, without leaking the
///      topic into other chats (the #116 poisoning chain). A session bound
///      to General resolves to no thread and STOPS here (#478) — it must not
///      reach step 3, which would post into whichever topic spoke last.
///   3. Auto-lookup via `latest_thread_id_for_chat(chat_id)` — the
///      fallback that closed #130, picking up the most recently
///      stored topic so non-forum chats and routine replies still
///      land in the right place without the agent having to know. Reached
///      only for a session with no binding at all.
///
/// Returns `None` when no path produces a value (non-forum chat, no
/// session origin, empty channel history, explicit value outside i32 range).
pub(crate) async fn resolve_thread_id(
    input: &Value,
    chat_id: i64,
    session_id: Uuid,
    state: &TelegramState,
) -> Option<teloxide::types::ThreadId> {
    // Explicit null is "post to General / no thread", and is distinct from
    // the field being ABSENT, which means "wherever this session lives"
    // (#1319). Without the distinction there was no way to express General at
    // all: omitting the field fell through to the session topic below, which
    // put the synthetic 1 back on the wire, so naming the topic and omitting
    // it failed identically.
    if matches!(input.get("thread_id"), Some(serde_json::Value::Null)) {
        return None;
    }
    if let Some(tid) = input.get("thread_id").and_then(value_as_i64)
        && let Ok(tid_i32) = i32::try_from(tid)
    {
        // Through the boundary: an agent naming thread 1 means General, and
        // General is addressed by the ABSENCE of a thread.
        return crate::channels::telegram::session_resolve::delivery_thread_id(Some(tid_i32));
    }
    // Session origin topic — the forum topic this interaction started in.
    // SAME-ORIGIN ONLY (#127): the origin topic is applied ONLY when the
    // resolved target chat IS the session-origin chat. Inheriting a topic
    // across chats is what poisoned cross-chat sends (#116): a session
    // living in group topic 7198, sending to the owner DM with thread_id
    // omitted, had topic 7198 — which does not exist in the DM — put on the
    // wire.
    //
    // The question here is whether the session has a binding AT ALL, not
    // whether its topic is non-None. `session_topic` returns `None` for BOTH
    // "bound to General / a DM" and "not bound", and the connect-time route
    // restore (#1224) writes a General binding as the durable `NULL` — so
    // asking it directly fell through to the chat-wide lookup for a session
    // whose address was known, posting into whichever topic spoke last
    // (#1319, #478). `session_push_thread` asks the binding instead, and
    // keeps the same-origin rule: it only honours a binding whose chat is
    // this one.
    //
    // A remembered topic can outlive its existence on Telegram's side
    // (deleted while we were away). Its first hard evidence is the send
    // itself failing with `message thread not found` (#116) — handled at
    // the send seams (rich + HTML ladder), which evict chat-scoped and
    // retry unthreaded. Here we just resolve the address; the map
    // re-registers on the chat's next inbound topic message.
    crate::channels::telegram::send::session_push_thread(state, session_id, chat_id).await
}

/// A resolved destination for a message-creating Telegram action (#1080).
///
/// Constructed only by `resolve_new_target`, which folds the chat fallback
/// (explicit `chat_id` > session origin > owner) and the thread precedence
/// (see `resolve_thread_id`) into one call. An action that creates a message
/// holds one of these instead of assembling `chat_id` / `thread_id` itself —
/// that arm-local assembly is exactly what let six arms skip forum-topic
/// routing in #1079.
#[derive(Debug)]
pub(crate) struct NewTarget {
    pub(crate) chat_id: i64,
    pub(crate) thread_id: Option<teloxide::types::ThreadId>,
}

/// A resolved destination for a message-addressing action (edit, delete,
/// pin, react). The message id already pins the forum topic, so no thread
/// lookup exists on this path — provably, not by convention.
#[derive(Debug)]
pub(crate) struct ExistingTarget {
    pub(crate) chat_id: i64,
    pub(crate) message_id: i64,
}

/// A resolved chat-scoped destination (unpin, info, moderation). No message,
/// no topic.
#[derive(Debug)]
pub(crate) struct ChatTarget {
    pub(crate) chat_id: i64,
}

/// Resolve a message-creating target: chat fallback first, then thread
/// precedence, both in one place (#1080). A caller cannot take the chat and
/// skip the topic decision — the decision is already made by the time the
/// `NewTarget` is in hand.
#[allow(clippy::result_large_err)]
pub(crate) async fn resolve_new_target(
    input: &Value,
    session_id: Uuid,
    state: &TelegramState,
) -> std::result::Result<NewTarget, ToolResult> {
    let chat_id = chat_or_err(input, state, session_id).await?;
    let thread_id = resolve_thread_id(input, chat_id, session_id, state).await;
    Ok(NewTarget { chat_id, thread_id })
}

/// Landing echo for success output (#127): names the RESOLVED destination —
/// chat always, topic when one is on the route (name when the DB can supply
/// it, numeric id otherwise). Every message-creating action appends this to
/// its success text so the calling model sees where the message actually
/// landed, regardless of how resolution happened (explicit ids, session
/// origin, chat-wide lookup) — a stale-topic route or session-origin
/// fallback becomes visible without a human complaint.
pub(crate) async fn landing_echo(
    chat_id: i64,
    thread_id: Option<teloxide::types::ThreadId>,
) -> String {
    let topic = match thread_id {
        None => "no topic (General/DM)".to_string(),
        Some(tid) => {
            let numeric = tid.0.0;
            match thread_name(chat_id, numeric).await {
                Some(name) => format!("topic {numeric} ({name})"),
                None => format!("topic {numeric}"),
            }
        }
    };
    format!(" Landed: chat {chat_id}, {topic}.")
}

/// Best-effort topic name lookup for the landing echo (#127): the most
/// recent non-null `topic_name` persisted for this chat+thread. Any failure
/// (no pool, DB error, unnamed topic) degrades to the numeric id — the echo
/// must never fail a successful send.
async fn thread_name(chat_id: i64, thread_id: i32) -> Option<String> {
    let pool = crate::db::global_pool()?;
    let repo = crate::db::ChannelMessageRepository::new(pool.clone());
    repo.latest_topic_name("telegram", &chat_id.to_string(), &thread_id.to_string())
        .await
        .ok()
        .flatten()
        .filter(|n| !n.trim().is_empty())
}

/// Resolve a message-addressing target. Chat fallback, then the required
/// `message_id` — same error precedence the edit/delete/pin arms had before
/// extraction.
#[allow(clippy::result_large_err)]
pub(crate) async fn resolve_existing_target(
    input: &Value,
    session_id: Uuid,
    state: &TelegramState,
) -> std::result::Result<ExistingTarget, ToolResult> {
    let chat_id = chat_or_err(input, state, session_id).await?;
    let message_id = get_id(input, "message_id")?;
    Ok(ExistingTarget {
        chat_id,
        message_id,
    })
}

/// Resolve a chat-scoped target: chat fallback only.
#[allow(clippy::result_large_err)]
pub(crate) async fn resolve_chat_target(
    input: &Value,
    session_id: Uuid,
    state: &TelegramState,
) -> std::result::Result<ChatTarget, ToolResult> {
    let chat_id = chat_or_err(input, state, session_id).await?;
    Ok(ChatTarget { chat_id })
}

/// Persist outgoing bot messages to `channel_messages` keyed by their Telegram
/// `message_id`, so a later reply can recover their text by id — exactly like
/// the normal reply path does. Without this, a user replying to a message the
/// bot posted proactively (a report, a cron post, any `telegram_send`) hits an
/// empty `channel_messages` lookup, and because rich/cron messages arrive with
/// no readable text in the reply, the agent can only honestly say it cannot see
/// it. `sent` is `(message_id, content)` pairs (one per chunk for plain sends).
/// Refuse a destination a scheduled job was never given.
///
/// A cron turn has no channel origin, so the proactive send path takes its
/// destination from the tool input — and a job's turn can pick that input up
/// from anywhere it reads, including a recalled memory. On 2026-08-21 a memory
/// note from two weeks earlier carried a chat id and thread id under the
/// heading "CONTINUE THIS TASK", and a job posted its report into that group:
/// one it was never configured for, whose members had asked for nothing.
///
/// A chat id found in memory or in earlier context is not permission to post
/// there. Outside a cron turn this is transparent.
#[allow(clippy::result_large_err)]
fn guard_cron_target(chat_id: i64) -> std::result::Result<i64, ToolResult> {
    if crate::cron::send_scope::may_send_to(chat_id) {
        return Ok(chat_id);
    }
    let reason = crate::cron::send_scope::refusal(chat_id);
    tracing::warn!("telegram_send: {reason}");
    Err(ToolResult::error(reason))
}

#[allow(clippy::result_large_err)]
async fn chat_or_err(
    input: &Value,
    state: &TelegramState,
    session_id: Uuid,
) -> std::result::Result<i64, ToolResult> {
    if let Some(id) = input.get("chat_id").and_then(value_as_i64) {
        return guard_cron_target(id);
    }
    // Session origin chat — where this interaction started, same map
    // the interactive-question tool used (#450).
    if let Some(id) = state.session_chat(session_id).await {
        return guard_cron_target(id);
    }
    // The owner's chat, for a session that never bound one. A cron job is
    // exactly such a session, so this used to hand every targetless job a
    // destination it was never given — the owner's DM. Guarded like the rest:
    // with no deliver_to the job sends nowhere and reports in its own session.
    //
    // #1889: a session anchored on Discord/Slack/WhatsApp also arrives here
    // with no Telegram session_chat, and the owner fallback used to jump
    // platforms silently: the artifact landed in the owner's Telegram chat
    // while the conversation stayed on Discord. Refuse before the wire call
    // when the session's binding names another platform; targetless cron
    // keeps the guarded owner path.
    let owner = state.owner_chat_id().await;
    if let Some(reason) = cross_platform_refusal(
        state.session_origin_channel(session_id).await.as_deref(),
        owner,
    ) {
        tracing::warn!("telegram_send: {reason}");
        return Err(ToolResult::error(reason));
    }
    match owner {
        Some(id) => guard_cron_target(id),
        None => Err(ToolResult::error(
            "No owner chat ID known yet and no 'chat_id' parameter provided. \
             The owner needs to send at least one message to the bot first, \
             or specify a chat_id."
                .to_string(),
        )),
    }
}

// Macro to early-return Ok(err_result) when a param helper returns Err.
macro_rules! pget {
    ($expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => return Ok(e),
        }
    };
}

#[async_trait]
impl Tool for TelegramSendTool {
    fn name(&self) -> &str {
        "telegram_send"
    }

    fn description(&self) -> &str {
        "Full Telegram control: send messages, reply, edit, delete, pin/unpin, forward, \
         send photos/documents/locations/polls, inline buttons, get chat info, list admins, \
         check member count/status, ban/unban users, and set emoji reactions. \
         Always use telegram_send instead of http_request — credentials handled securely. \
         Requires Telegram to be connected first."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "send", "reply", "edit", "delete", "pin", "unpin",
                        "forward", "send_photo", "send_document", "send_location",
                        "send_poll", "send_buttons", "get_chat",
                        "get_chat_administrators", "get_chat_member_count", "get_chat_member",
                        "ban_user", "unban_user", "set_reaction", "list_topics",
                        "create_topic", "rename_topic", "bind_topic"
                    ],
                    "description": "The Telegram action to perform. \
                        `list_topics` returns ONLY the bot-observed (thread_id, topic_name) pairs \
                        recorded in local DB for a forum-enabled supergroup — it does NOT enumerate \
                        the full forum surface (Telegram Bot API has no getForumTopics endpoint). \
                        Use this to translate an already observed topic name like \"#announcements\" to \
                        the numeric thread_id passed to `send` / `reply` via `thread_id`. For exhaustive \
                        MTProto forum enumeration, use MTProto client tools (e.g. fast-mcp-telegram tg_get_chat_info). \
                        `create_topic` creates a new forum topic (requires `name`, 1-128 chars). Optionally accepts `bind: true` to bind the calling session immediately. \
                        `rename_topic` renames an existing forum topic (requires `thread_id` and `name`, 1-128 chars). \
                        `bind_topic` binds the calling session to a forum topic (requires `thread_id`, optional `chat_id`)."
                },
                "name": {
                    "type": "string",
                    "description": "Topic name (1–128 characters) for create_topic and rename_topic"
                },
                "bind": {
                    "type": "boolean",
                    "description": "Optional flag for create_topic: if true, immediately binds the calling session to the newly created topic."
                },
                "message": {
                    "type": "string",
                    "description": "Message text (send, reply, edit, send_buttons)"
                },
                "chat_id": {
                    "type": "integer",
                    "description": "Telegram chat ID. Omit to use owner's chat."
                },
                "thread_id": {
                    "type": "integer",
                    "description": "Optional forum-topic ID for groups with topics enabled. Omit to auto-route to the most recent topic seen in the chat (the usual case for replies to ongoing conversations). Pass an explicit value to route to a DIFFERENT topic — e.g. post a release announcement in #announcements when the latest message came from #dev. Ignored for non-forum chats."
                },
                "caption": {
                    "type": "string",
                    "description": "Caption for send_photo / send_document (0-1024 chars). Attaches text context to the media."
                },
                "message_id": {
                    "type": "integer",
                    "description": "Target message ID for reply/edit/delete/pin/unpin/forward/set_reaction, or the message to reply to when used with send_photo/send_document"
                },
                "from_chat_id": {
                    "type": "integer",
                    "description": "Source chat ID for forward action"
                },
                "photo_url": {
                    "type": "string",
                    "description": "Photo for send_photo: an HTTPS URL or a local file path (e.g. /tmp/chart.png or ~/.opencrabs/out.png)"
                },
                "document_url": {
                    "type": "string",
                    "description": "Document for send_document: an HTTPS URL or a local file path (e.g. /tmp/report.pdf or ~/.opencrabs/data.csv)"
                },
                "latitude": {
                    "type": "number",
                    "description": "Latitude for send_location"
                },
                "longitude": {
                    "type": "number",
                    "description": "Longitude for send_location"
                },
                "poll_question": {
                    "type": "string",
                    "description": "Poll question text for send_poll"
                },
                "poll_options": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Array of poll option strings (2–10) for send_poll"
                },
                "poll_anonymous": {
                    "type": "boolean",
                    "description": "send_poll only. Defaults to false so votes are attributed. An anonymous poll returns no voter, so the bot sees the option picked but not who picked it."
                },
                "buttons": {
                    "type": "array",
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": {"type": "string"},
                                "callback_data": {"type": "string"}
                            }
                        }
                    },
                    "description": "2D array of button rows for send_buttons. Each button has 'text' and 'callback_data'."
                },
                "user_id": {
                    "type": "integer",
                    "description": "Telegram user ID for ban_user/unban_user"
                },
                "emoji": {
                    "type": "string",
                    "description": "Emoji for set_reaction (e.g. \"👍\")"
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    fn hints(&self) -> ToolHints {
        ToolHints {
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        }
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let action = match input.get("action").and_then(|v| v.as_str()) {
            Some(a) if !a.is_empty() => a.to_string(),
            _ => {
                return Ok(ToolResult::error(
                    "Missing required 'action' parameter.".to_string(),
                ));
            }
        };

        let bot = match self.telegram_state.bot().await {
            Some(b) => b,
            None => {
                return Ok(ToolResult::error(
                    "Telegram is not connected. Ask the user to connect Telegram first \
                     (use the telegram_connect tool)."
                        .to_string(),
                ));
            }
        };

        // Thin dispatch (#1080): every action body lives in an `action_*`
        // method below, and every method obtains its destination through one
        // of the typed resolvers (`resolve_new_target` /
        // `resolve_existing_target` / `resolve_chat_target`). There is no
        // arm-local path from raw input to a teloxide request, so a future
        // arm cannot forget forum-topic routing the way six arms did (#1079).
        let input = &input;
        match action.as_str() {
            "send" => self.action_send(&bot, input, context).await,
            "reply" => self.action_reply(&bot, input, context).await,
            "edit" => self.action_edit(&bot, input, context).await,
            "delete" => self.action_delete(&bot, input, context).await,
            "pin" => self.action_pin(&bot, input, context).await,
            "unpin" => self.action_unpin(&bot, input, context).await,
            "forward" => self.action_forward(&bot, input, context).await,
            "send_photo" => self.action_send_photo(&bot, input, context).await,
            "send_document" => self.action_send_document(&bot, input, context).await,
            "send_location" => self.action_send_location(&bot, input, context).await,
            "send_poll" => self.action_send_poll(&bot, input, context).await,
            "send_buttons" => self.action_send_buttons(&bot, input, context).await,
            "get_chat" => self.action_get_chat(&bot, input, context).await,
            "get_chat_administrators" => {
                self.action_get_chat_administrators(&bot, input, context)
                    .await
            }
            "get_chat_member_count" => {
                self.action_get_chat_member_count(&bot, input, context)
                    .await
            }
            "get_chat_member" => self.action_get_chat_member(&bot, input, context).await,
            "ban_user" => self.action_ban_user(&bot, input, context).await,
            "unban_user" => self.action_unban_user(&bot, input, context).await,
            "set_reaction" => self.action_set_reaction(&bot, input, context).await,
            "list_topics" => self.action_list_topics(&bot, input, context).await,
            "create_topic" => self.action_create_topic(&bot, input, context).await,
            "rename_topic" => self.action_rename_topic(&bot, input, context).await,
            "bind_topic" => self.action_bind_topic(input, context).await,
            unknown => Ok(ToolResult::error(format!(
                "Unknown action '{unknown}'. Valid actions: send, reply, edit, delete, pin, \
                 unpin, forward, send_photo, send_document, send_location, send_poll, \
                 send_buttons, get_chat, get_chat_administrators, get_chat_member_count, \
                 get_chat_member, ban_user, unban_user, set_reaction, list_topics, \
                 create_topic, rename_topic, bind_topic"
            ))),
        }
    }
}

impl TelegramSendTool {
    /// `send` — text message into a (possibly forum) chat.
    async fn action_send(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let text = pget!(get_str(input, "message")).to_string();
        let NewTarget { chat_id, thread_id } =
            pget!(resolve_new_target(input, context.session_id, &self.telegram_state).await);
        // Structured messages (tables, headings, lists, math) go through
        // the native rich path as a whole — never chunked, since a split
        // table would break. Plain prose, and any rich failure, fall back
        // to the chunked plain-text send so a message is never dropped
        // and Telegram's parser never reinterprets incidental characters.
        // Track sent (message_id, content) so the message is persisted
        // for reply-recovery below.
        // One send ladder for the wire path (#1085 P1b R2): rich-gate →
        // HTML → 4096 chunks → plain-text fallback, all inside
        // send_markdown_outbox. The old inline ladder claimed a
        // plain-text fallback it never implemented (comment at :504 vs
        // HTML-only chunks) — now the claim is true and telemetry carries
        // origin=tool on every landing.
        let outbox = match crate::channels::telegram::send::send_markdown_outbox(
            bot,
            ChatId(chat_id),
            thread_id,
            &text,
            "tool",
            "send",
            None,
        )
        .await
        {
            Ok(outbox) => outbox,
            Err(e) => return Ok(ToolResult::error(format!("Failed to send: {e}"))),
        };
        // Persist so a later reply to this message can be read back by id
        // (a report/cron post replied-to would otherwise be unrecoverable).
        // Uses outbox.effective_thread_id so stale-topic evictions are never
        // re-poisoned into the database (#169).
        outbox.record_outgoing(None, chat_id).await;
        Ok(ToolResult::success(format!(
            "Message sent to chat {chat_id}.{}",
            landing_echo(chat_id, outbox.effective_thread_id).await
        )))
    }

    /// `reply` — text message replying to an existing message.
    async fn action_reply(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let text = pget!(get_str(input, "message")).to_string();
        let NewTarget { chat_id, thread_id } =
            pget!(resolve_new_target(input, context.session_id, &self.telegram_state).await);
        let message_id = pget!(get_id(input, "message_id"));
        // Reply rich-first through the shared outbox ladder (#1230): a table
        // in a reply renders as a native rich message (real grid), exactly
        // like the `send` action — instead of the old direct
        // markdown_to_telegram_html+ParseMode::Html path which degraded
        // tables to a monospace `<pre>` grid. The outbox owns the reply
        // target via `reply_to`, retry, chunking and plain-text fallback.
        let outbox = match crate::channels::telegram::send::send_markdown_outbox(
            bot,
            ChatId(chat_id),
            thread_id,
            &text,
            "tool",
            "reply",
            Some(message_id as i32),
        )
        .await
        {
            Ok(outbox) => outbox,
            Err(e) => return Ok(ToolResult::error(format!("Failed to reply: {e}"))),
        };
        // Persist for reply-recovery (a user can reply to this bot reply).
        // Uses outbox.effective_thread_id so stale-topic evictions are never
        // re-poisoned into the database (#169).
        outbox.record_outgoing(None, chat_id).await;
        Ok(ToolResult::success(format!(
            "Reply sent to message {message_id}.{}",
            landing_echo(chat_id, outbox.effective_thread_id).await
        )))
    }

    /// `edit` — rewrite the text of an existing message.
    async fn action_edit(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let text = pget!(get_str(input, "message")).to_string();
        let ExistingTarget {
            chat_id,
            message_id,
        } = pget!(resolve_existing_target(input, context.session_id, &self.telegram_state).await);
        // Rich-first edit (#1230): a structured edit (table / heading /
        // list) rewrites the message as a native rich message, so a table
        // stays a real grid rather than degrading to a monospace `<pre>`
        // block under the old HTML edit. Plain prose skips rich so Telegram
        // never reinterprets incidental characters; on any rich failure we
        // fall through to the classic HTML edit below so the edit always
        // lands exactly like the `send` outbox fallback design.
        if crate::channels::telegram::rich::should_send_native_rich(&text) {
            match crate::channels::telegram::rich::api::edit_rich_markdown(
                bot.api_url().as_str(),
                bot.token(),
                chat_id,
                message_id as i32,
                &text,
                None,
                "tool",
                "edit",
            )
            .await
            {
                Ok(()) => {
                    log_send_success(
                        "tool",
                        "edit",
                        "edit",
                        &context.session_id.to_string(),
                        "rich",
                        chat_id,
                        None,
                        message_id as i32,
                        text.len(),
                        &content_hash8(&text),
                    );
                    return Ok(ToolResult::success(format!("Message {message_id} edited.")));
                }
                Err(e) => {
                    // Fall through to the HTML edit on any rich failure so
                    // the edit is never dropped.
                    tracing::warn!(
                        "telegram_send edit: native rich edit failed ({e}) — falling back to HTML"
                    );
                }
            }
        }
        // Convert markdown to Telegram HTML, same as the "send"
        // action, so formatting renders correctly (#834).
        let html = crate::channels::telegram::handler::markdown_to_telegram_html(&text);
        match send_retrying_rate_limit("telegram_send edit", || {
            bot.edit_message_text(ChatId(chat_id), MessageId(message_id as i32), html.clone())
                .parse_mode(teloxide::types::ParseMode::Html)
        })
        .await
        {
            Ok(_) => {
                log_send_success(
                    "tool",
                    "edit",
                    "edit",
                    &context.session_id.to_string(),
                    "html",
                    chat_id,
                    None,
                    message_id as i32,
                    html.len(),
                    &content_hash8(&html),
                );
                Ok(ToolResult::success(format!("Message {message_id} edited.")))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "edit",
                    "edit",
                    &context.session_id.to_string(),
                    "html",
                    chat_id,
                    None,
                    html.len(),
                    &content_hash8(&html),
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to edit: {e}")))
            }
        }
    }

    /// `delete` — remove a message.
    async fn action_delete(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ExistingTarget {
            chat_id,
            message_id,
        } = pget!(resolve_existing_target(input, context.session_id, &self.telegram_state).await);
        // #721 E1: the tool path calls `delete_message` DIRECTLY — it does not
        // go through `best_effort_delete` — so it needs its own request line.
        log_request(
            "tool",
            "delete",
            &context.session_id.to_string(),
            "delete",
            "deleteMessage",
            chat_id,
            None,
            Some(message_id),
        );
        match send_retrying_rate_limit("telegram_send delete", || {
            bot.delete_message(ChatId(chat_id), MessageId(message_id as i32))
        })
        .await
        {
            Ok(_) => {
                log_send_success(
                    "tool",
                    "delete",
                    "delete",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    None,
                    message_id as i32,
                    0,
                    "-",
                );
                Ok(ToolResult::success(format!(
                    "Message {message_id} deleted."
                )))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "delete",
                    "delete",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    None,
                    0,
                    "-",
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to delete: {e}")))
            }
        }
    }

    /// `pin` — pin a message in its chat.
    async fn action_pin(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ExistingTarget {
            chat_id,
            message_id,
        } = pget!(resolve_existing_target(input, context.session_id, &self.telegram_state).await);
        match send_retrying_rate_limit("telegram_send pin", || {
            bot.pin_chat_message(ChatId(chat_id), MessageId(message_id as i32))
        })
        .await
        {
            Ok(_) => {
                log_send_success(
                    "tool",
                    "pin",
                    "pin",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    None,
                    message_id as i32,
                    0,
                    "-",
                );
                Ok(ToolResult::success(format!("Message {message_id} pinned.")))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "pin",
                    "pin",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    None,
                    0,
                    "-",
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to pin: {e}")))
            }
        }
    }

    /// `unpin` — unpin the most recent pinned message of a chat.
    async fn action_unpin(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        match send_retrying_rate_limit("telegram_send unpin", || {
            bot.unpin_chat_message(ChatId(chat_id))
        })
        .await
        {
            Ok(_) => {
                log_send_success(
                    "tool",
                    "unpin",
                    "unpin",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    None,
                    0,
                    0,
                    "-",
                );
                Ok(ToolResult::success(
                    "Latest pinned message unpinned.".to_string(),
                ))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "unpin",
                    "unpin",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    None,
                    0,
                    "-",
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to unpin: {e}")))
            }
        }
    }

    /// `forward` — copy a message from one chat into a (possibly forum) chat.
    async fn action_forward(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let NewTarget {
            chat_id: to_chat,
            thread_id,
        } = pget!(resolve_new_target(input, context.session_id, &self.telegram_state).await);
        let from_chat = pget!(get_id(input, "from_chat_id"));
        let message_id = pget!(get_id(input, "message_id"));
        match send_retrying_rate_limit("telegram_send forward", || {
            crate::channels::telegram::send::forward_in_thread(
                bot,
                ChatId(to_chat),
                ChatId(from_chat),
                MessageId(message_id as i32),
                thread_id,
            )
        })
        .await
        {
            Ok(m) => {
                log_send_success(
                    "tool",
                    "forward",
                    "forward",
                    &context.session_id.to_string(),
                    "action",
                    to_chat,
                    thread_id.map(|t| t.0.0),
                    m.id.0,
                    0,
                    "-",
                );
                Ok(ToolResult::success(format!(
                    "Message {message_id} forwarded from chat {from_chat} to {to_chat}.{}",
                    landing_echo(to_chat, thread_id).await
                )))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "forward",
                    "forward",
                    &context.session_id.to_string(),
                    "action",
                    to_chat,
                    thread_id.map(|t| t.0.0),
                    0,
                    "-",
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to forward: {e}")))
            }
        }
    }

    /// `send_photo` — photo by URL or local path, with optional caption.
    async fn action_send_photo(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let NewTarget { chat_id, thread_id } =
            pget!(resolve_new_target(input, context.session_id, &self.telegram_state).await);
        let reference = pget!(get_str(input, "photo_url")).to_string();
        let caption = input
            .get("caption")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // Collapse an identical photo+caption re-sent to the same chat
        // within the dedup window (#721) — model repeats or post-timeout
        // retries otherwise land the same media twice back-to-back.
        if !self.telegram_state.claim_media_send(
            "send_photo",
            chat_id,
            &reference,
            caption.as_deref(),
        ) {
            tracing::info!(
                "telegram_send: suppressed duplicate send_photo to chat {chat_id} ({reference})"
            );
            return Ok(ToolResult::success(format!(
                "Photo already sent to chat {chat_id} moments ago — skipped the duplicate."
            )));
        }
        let file = pget!(resolve_input_file(&reference, "photo_url").await);
        let reply_to = input.get("message_id").and_then(value_as_i64);
        match send_retrying_rate_limit("telegram_send send_photo", || {
            let mut req = crate::channels::telegram::send::photo_in_thread(
                bot,
                ChatId(chat_id),
                thread_id,
                file.clone(),
            );
            if let Some(ref c) = caption {
                req = req.caption(c.clone());
            }
            if let Some(mid) = reply_to {
                req = req.reply_parameters(ReplyParameters::new(MessageId(mid as i32)));
            }
            req
        })
        .await
        {
            Ok(m) => {
                log_send_success(
                    "tool",
                    "send_photo",
                    "send_photo",
                    &context.session_id.to_string(),
                    "media",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    m.id.0,
                    reference.len(),
                    &content_hash8(&reference),
                );
                Ok(ToolResult::success(format!(
                    "Photo sent to chat {chat_id}.{}",
                    landing_echo(chat_id, thread_id).await
                )))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "send_photo",
                    "send_photo",
                    &context.session_id.to_string(),
                    "media",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    reference.len(),
                    &content_hash8(&reference),
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to send photo: {e}")))
            }
        }
    }

    /// `send_document` — file by URL or local path, with optional caption.
    async fn action_send_document(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let NewTarget { chat_id, thread_id } =
            pget!(resolve_new_target(input, context.session_id, &self.telegram_state).await);
        let reference = pget!(get_str(input, "document_url")).to_string();
        let caption = input
            .get("caption")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // Collapse an identical document+caption re-sent to the same
        // chat within the dedup window (#721) — a large upload that
        // times out client-side after Telegram already delivered it,
        // or a model repeat, otherwise lands the same file twice.
        if !self.telegram_state.claim_media_send(
            "send_document",
            chat_id,
            &reference,
            caption.as_deref(),
        ) {
            tracing::info!(
                "telegram_send: suppressed duplicate send_document to chat {chat_id} ({reference})"
            );
            return Ok(ToolResult::success(format!(
                "Document already sent to chat {chat_id} moments ago — skipped the duplicate."
            )));
        }
        let file = pget!(resolve_input_file(&reference, "document_url").await);
        let reply_to = input.get("message_id").and_then(value_as_i64);
        match send_retrying_rate_limit("telegram_send send_document", || {
            let mut req = crate::channels::telegram::send::document_in_thread(
                bot,
                ChatId(chat_id),
                thread_id,
                file.clone(),
            );
            if let Some(ref c) = caption {
                req = req.caption(c.clone());
            }
            if let Some(mid) = reply_to {
                req = req.reply_parameters(ReplyParameters::new(MessageId(mid as i32)));
            }
            req
        })
        .await
        {
            Ok(m) => {
                log_send_success(
                    "tool",
                    "send_document",
                    "send_document",
                    &context.session_id.to_string(),
                    "media",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    m.id.0,
                    reference.len(),
                    &content_hash8(&reference),
                );
                Ok(ToolResult::success(format!(
                    "Document sent to chat {chat_id}.{}",
                    landing_echo(chat_id, thread_id).await
                )))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "send_document",
                    "send_document",
                    &context.session_id.to_string(),
                    "media",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    reference.len(),
                    &content_hash8(&reference),
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to send document: {e}")))
            }
        }
    }

    /// `send_location` — geographic point.
    async fn action_send_location(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let NewTarget { chat_id, thread_id } =
            pget!(resolve_new_target(input, context.session_id, &self.telegram_state).await);
        let lat = match input.get("latitude").and_then(value_as_f64) {
            Some(v) => v,
            None => {
                return Ok(ToolResult::error(
                    "Missing required 'latitude' parameter.".to_string(),
                ));
            }
        };
        let lng = match input.get("longitude").and_then(value_as_f64) {
            Some(v) => v,
            None => {
                return Ok(ToolResult::error(
                    "Missing required 'longitude' parameter.".to_string(),
                ));
            }
        };
        let coords = format!("{lat},{lng}");
        match send_retrying_rate_limit("telegram_send send_location", || {
            crate::channels::telegram::send::location_in_thread(
                bot,
                ChatId(chat_id),
                thread_id,
                lat,
                lng,
            )
        })
        .await
        {
            Ok(m) => {
                log_send_success(
                    "tool",
                    "send_location",
                    "send_location",
                    &context.session_id.to_string(),
                    "media",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    m.id.0,
                    coords.len(),
                    &content_hash8(&coords),
                );
                Ok(ToolResult::success(format!(
                    "Location ({lat}, {lng}) sent to chat {chat_id}.{}",
                    landing_echo(chat_id, thread_id).await
                )))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "send_location",
                    "send_location",
                    &context.session_id.to_string(),
                    "media",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    coords.len(),
                    &content_hash8(&coords),
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to send location: {e}")))
            }
        }
    }

    /// `send_poll` — question with 2+ options.
    async fn action_send_poll(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let NewTarget { chat_id, thread_id } =
            pget!(resolve_new_target(input, context.session_id, &self.telegram_state).await);
        let question = pget!(get_str(input, "poll_question")).to_string();
        let opts: Vec<String> = match input.get("poll_options").and_then(|v| v.as_array()) {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            None => {
                return Ok(ToolResult::error(
                    "Missing required 'poll_options' parameter.".to_string(),
                ));
            }
        };
        if opts.len() < 2 {
            return Ok(ToolResult::error(
                "'poll_options' must have at least 2 options.".to_string(),
            ));
        }
        // Anonymous by default in the Bot API, and an anonymous vote carries
        // no voter, so the agent could never tell who answered (#2288). Callers
        // have to ask for it explicitly.
        let anonymous = input
            .get("poll_anonymous")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let poll_labels = opts.clone();
        let poll_opts: Vec<teloxide::types::InputPollOption> =
            opts.into_iter().map(|s| s.into()).collect();
        match send_retrying_rate_limit("telegram_send send_poll", || {
            crate::channels::telegram::send::poll_in_thread(
                bot,
                ChatId(chat_id),
                thread_id,
                question.clone(),
                poll_opts.clone(),
                anonymous,
            )
        })
        .await
        {
            Ok(m) => {
                // The vote arrives later as its own update carrying only the
                // poll id, so remember the send now or the vote is unroutable.
                match poll_id_of(&m) {
                    Some(poll_id) => self.telegram_state.register_poll(
                        &poll_id,
                        crate::channels::telegram::state::PollOrigin::new(
                            context.session_id,
                            chat_id,
                            thread_id.map(|t| t.0.0),
                            question.clone(),
                            poll_labels,
                        ),
                    ),
                    None => tracing::warn!(
                        "Telegram: send_poll returned a message without a poll payload; \
                         votes for this poll cannot be routed (#2288)"
                    ),
                }
                log_send_success(
                    "tool",
                    "send_poll",
                    "send_poll",
                    &context.session_id.to_string(),
                    "media",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    m.id.0,
                    question.len(),
                    &content_hash8(&question),
                );
                Ok(ToolResult::success(format!(
                    "Poll sent to chat {chat_id}.{}",
                    landing_echo(chat_id, thread_id).await
                )))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "send_poll",
                    "send_poll",
                    &context.session_id.to_string(),
                    "media",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    question.len(),
                    &content_hash8(&question),
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to send poll: {e}")))
            }
        }
    }

    /// `send_buttons` — text message with an inline keyboard.
    async fn action_send_buttons(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let text = pget!(get_str(input, "message")).to_string();
        let NewTarget { chat_id, thread_id } =
            pget!(resolve_new_target(input, context.session_id, &self.telegram_state).await);
        // Collect callback_data strings for origin tracking (#878)
        let mut origin_keys: Vec<String> = Vec::new();
        let rows: Vec<Vec<InlineKeyboardButton>> =
            match input.get("buttons").and_then(|v| v.as_array()) {
                Some(outer) => outer
                    .iter()
                    .filter_map(|row| row.as_array())
                    .map(|row| {
                        row.iter()
                            .filter_map(|btn| {
                                let text = btn.get("text").and_then(|v| v.as_str())?.to_string();
                                let data = btn
                                    .get("callback_data")
                                    .and_then(|v| v.as_str())?
                                    .to_string();
                                origin_keys.push(data.clone());
                                Some(InlineKeyboardButton::callback(text, data))
                            })
                            .collect()
                    })
                    .collect(),
                None => {
                    return Ok(ToolResult::error(
                        "Missing required 'buttons' parameter.".to_string(),
                    ));
                }
            };
        // Register callback_data → session_id so the callback
        // dispatcher routes taps to THIS session (#878).
        self.telegram_state
            .register_callback_origins(context.session_id, origin_keys);
        let keyboard = InlineKeyboardMarkup::new(rows);
        let html = crate::channels::telegram::handler::markdown_to_telegram_html(&text);
        // Raw Bot API JSON path (#118): the teloxide request chain on this
        // build silently drops BOTH `.parse_mode(Html)` and
        // `.reply_markup(keyboard)` (stored probes carry no entities and no
        // reply_markup, while the request arm logs ok). The raw-JSON path is
        // the proven wire in this codebase — the rich plane, plan cards and
        // ephemeral sends all ride it and keyboards store correctly — so the
        // buttons arm rides it too.
        let token = bot.token();
        match crate::channels::telegram::send::send_buttons_raw(
            token, chat_id, thread_id, &html, &keyboard,
        )
        .await
        {
            Ok(m) => {
                let mid = m
                    .get("message_id")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0) as i32;
                log_send_success(
                    "tool",
                    "send_buttons",
                    &context.session_id.to_string(),
                    "send_buttons",
                    "html",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    mid,
                    html.len(),
                    &content_hash8(&html),
                );
                Ok(ToolResult::success(format!(
                    "Message with buttons sent to chat {chat_id}.{}",
                    landing_echo(chat_id, thread_id).await
                )))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "send_buttons",
                    &context.session_id.to_string(),
                    "send_buttons",
                    "html",
                    chat_id,
                    thread_id.map(|t| t.0.0),
                    html.len(),
                    &content_hash8(&html),
                    &e,
                );
                Ok(ToolResult::error(format!(
                    "Failed to send message with buttons: {e}"
                )))
            }
        }
    }

    /// `get_chat` — type/title metadata for a chat.
    async fn action_get_chat(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        match bot.get_chat(ChatId(chat_id)).await {
            Ok(chat) => {
                let info = format!(
                    "Chat {}: type={:?}, title={:?}",
                    chat.id,
                    chat.kind,
                    chat.title()
                );
                Ok(ToolResult::success(info))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to get chat: {e}"))),
        }
    }

    /// `get_chat_administrators` — list admins with roles.
    async fn action_get_chat_administrators(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        match bot.get_chat_administrators(ChatId(chat_id)).await {
            Ok(admins) => {
                let lines: Vec<String> = admins
                    .iter()
                    .map(|m| {
                        let u = &m.user;
                        let role = match m.kind {
                            teloxide::types::ChatMemberKind::Owner { .. } => "owner",
                            teloxide::types::ChatMemberKind::Administrator { .. } => "admin",
                            _ => "member",
                        };
                        let handle = u
                            .username
                            .as_ref()
                            .map(|h| format!(" @{h}"))
                            .unwrap_or_default();
                        format!("- {} (id={}){} [{}]", u.first_name, u.id, handle, role)
                    })
                    .collect();
                Ok(ToolResult::success(format!(
                    "Chat {} administrators ({}):\n{}",
                    chat_id,
                    admins.len(),
                    lines.join("\n")
                )))
            }
            Err(e) => Ok(ToolResult::error(format!(
                "Failed to get administrators: {e}"
            ))),
        }
    }

    /// `get_chat_member_count` — member count for a chat.
    async fn action_get_chat_member_count(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        match bot.get_chat_member_count(ChatId(chat_id)).await {
            Ok(count) => Ok(ToolResult::success(format!(
                "Chat {chat_id} has {count} members."
            ))),
            Err(e) => Ok(ToolResult::error(format!(
                "Failed to get member count: {e}"
            ))),
        }
    }

    /// `get_chat_member` — one member's status.
    async fn action_get_chat_member(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        let uid = pget!(get_id(input, "user_id"));
        match bot
            .get_chat_member(ChatId(chat_id), UserId(uid as u64))
            .await
        {
            Ok(member) => {
                let u = &member.user;
                let status = match member.kind {
                    teloxide::types::ChatMemberKind::Owner { .. } => "owner",
                    teloxide::types::ChatMemberKind::Administrator { .. } => "administrator",
                    teloxide::types::ChatMemberKind::Member(_) => "member",
                    teloxide::types::ChatMemberKind::Restricted { .. } => "restricted",
                    teloxide::types::ChatMemberKind::Left => "left",
                    teloxide::types::ChatMemberKind::Banned { .. } => "banned",
                };
                let handle = u
                    .username
                    .as_ref()
                    .map(|h| format!(" @{h}"))
                    .unwrap_or_default();
                Ok(ToolResult::success(format!(
                    "User {} (id={}){}: status={}",
                    u.first_name, u.id, handle, status
                )))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to get chat member: {e}"))),
        }
    }

    /// `ban_user` — remove a user from a chat.
    async fn action_ban_user(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        let user_id = pget!(get_id(input, "user_id"));
        match send_retrying_rate_limit("telegram_send ban_user", || {
            bot.ban_chat_member(ChatId(chat_id), UserId(user_id as u64))
        })
        .await
        {
            Ok(_) => Ok(ToolResult::success(format!(
                "User {user_id} banned from chat {chat_id}."
            ))),
            Err(e) => Ok(ToolResult::error(format!("Failed to ban user: {e}"))),
        }
    }

    /// `unban_user` — re-admit a user to a chat.
    async fn action_unban_user(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        let user_id = pget!(get_id(input, "user_id"));
        match send_retrying_rate_limit("telegram_send unban_user", || {
            bot.unban_chat_member(ChatId(chat_id), UserId(user_id as u64))
        })
        .await
        {
            Ok(_) => Ok(ToolResult::success(format!(
                "User {user_id} unbanned from chat {chat_id}."
            ))),
            Err(e) => Ok(ToolResult::error(format!("Failed to unban user: {e}"))),
        }
    }

    /// `set_reaction` — emoji reaction on a message.
    async fn action_set_reaction(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ExistingTarget {
            chat_id,
            message_id,
        } = pget!(resolve_existing_target(input, context.session_id, &self.telegram_state).await);
        let emoji = pget!(get_str(input, "emoji")).to_string();
        let reactions = vec![ReactionType::Emoji {
            emoji: emoji.clone(),
        }];
        // #721 E1: the tool path calls `set_message_reaction` DIRECTLY —
        // outside `fire_reaction` — so it needs its own request line.
        log_request(
            "tool",
            "set_reaction",
            &context.session_id.to_string(),
            "reaction",
            "setMessageReaction",
            chat_id,
            None,
            Some(message_id),
        );
        match send_retrying_rate_limit("telegram_send set_reaction", || {
            bot.set_message_reaction(ChatId(chat_id), MessageId(message_id as i32))
                .reaction(reactions.clone())
        })
        .await
        {
            Ok(_) => {
                log_send_success(
                    "tool",
                    "set_reaction",
                    "set_reaction",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    None,
                    message_id as i32,
                    emoji.len(),
                    &content_hash8(&emoji),
                );
                Ok(ToolResult::success(format!(
                    "Reaction {emoji} set on message {message_id}."
                )))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "set_reaction",
                    "set_reaction",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    None,
                    emoji.len(),
                    &content_hash8(&emoji),
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to set reaction: {e}")))
            }
        }
    }

    /// `list_topics` — forum topics the bot has observed for a chat.
    async fn action_list_topics(
        &self,
        _bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        let Some(pool) = crate::db::global_pool() else {
            return Ok(ToolResult::error(
                "Channel message store unavailable (DB not initialised).".to_string(),
            ));
        };
        let repo = crate::db::ChannelMessageRepository::new(pool.clone());
        let chat_id_str = chat_id.to_string();
        let topics = match repo.topics_for_chat("telegram", &chat_id_str).await {
            Ok(t) => t,
            Err(e) => {
                return Ok(ToolResult::error(format!("Failed to list topics: {e}")));
            }
        };
        if topics.is_empty() {
            return Ok(ToolResult::success(format!(
                "No forum topics observed yet for chat {chat_id}. \
                 Telegram's Bot API has no listForumTopics endpoint — the bot only \
                 learns topic names from messages it sees. Ask a user to post once in \
                 each topic so the bot can capture their names, then retry."
            )));
        }
        // Render a compact human/agent-readable table.
        let mut out = format!(
            "Topics in chat {chat_id} (bot-observed topics from local DB only — does not enumerate full forum surface):\n"
        );
        out.push_str("  thread_id | topic_name              | messages | last_seen\n");
        for t in &topics {
            let name = t.topic_name.as_deref().unwrap_or("(unknown)");
            // Convert epoch seconds (the schema's storage
            // format for created_at) to a human-readable
            // UTC timestamp so the agent and any user
            // reading the output don't have to decode.
            let last_seen = chrono::DateTime::from_timestamp(t.last_message_at, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| t.last_message_at.to_string());
            out.push_str(&format!(
                "  {:<9} | {:<23} | {:>8} | {}\n",
                t.thread_id,
                name.chars().take(23).collect::<String>(),
                t.message_count,
                last_seen,
            ));
        }
        out.push_str(
            "\nPass the thread_id back into `send` / `reply` / `send_photo` etc. \
             via the optional `thread_id` field to route a message into a specific topic.",
        );
        Ok(ToolResult::success(out))
    }

    /// `create_topic` — create a new forum topic in a supergroup.
    async fn action_create_topic(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        let name = pget!(get_str(input, "name")).trim();
        if name.is_empty() || name.chars().count() > 128 {
            return Ok(ToolResult::error(
                "Parameter 'name' must be between 1 and 128 characters.".to_string(),
            ));
        }

        match send_retrying_rate_limit("telegram_send create_topic", || {
            bot.create_forum_topic(ChatId(chat_id), name.to_string())
        })
        .await
        {
            Ok(topic) => {
                let thread_id = topic.thread_id.0.0;
                // create_forum_topic succeeded on the API — this chat IS a
                // forum; the evidence is API-proven, not inferred.
                self.telegram_state
                    .note_thread_evidence(chat_id, true, Some(thread_id))
                    .await;
                crate::channels::telegram::record_topic_created(
                    None,
                    chat_id,
                    thread_id,
                    &topic.name,
                    false,
                )
                .await;
                log_send_success(
                    "tool",
                    "create_topic",
                    "create_topic",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    Some(thread_id),
                    0,
                    topic.name.len(),
                    "-",
                );
                let bind = input.get("bind").and_then(|v| v.as_bool()).unwrap_or(false);
                if bind {
                    let bind_res = self
                        .telegram_state
                        .bind_session_topic(context.session_id, chat_id, Some(thread_id))
                        .await;
                    if let Err(e) = bind_res {
                        tracing::warn!("create_topic: failed to bind session to new topic: {e}");
                    }
                }
                let res = serde_json::json!({
                    "status": "success",
                    "chat_id": chat_id,
                    "thread_id": thread_id,
                    "name": topic.name,
                    "bound": bind
                });
                Ok(ToolResult::success(res.to_string()))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "create_topic",
                    "create_topic",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    None,
                    name.len(),
                    "-",
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to create topic: {e}")))
            }
        }
    }

    /// `rename_topic` — rename an existing forum topic in a supergroup.
    async fn action_rename_topic(
        &self,
        bot: &teloxide::Bot,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        let thread_id_raw = pget!(get_id(input, "thread_id"));
        let name = pget!(get_str(input, "name")).trim();
        if name.is_empty() || name.chars().count() > 128 {
            return Ok(ToolResult::error(
                "Parameter 'name' must be between 1 and 128 characters.".to_string(),
            ));
        }

        let thread_id = ThreadId(MessageId(thread_id_raw as i32));
        use teloxide::payloads::EditForumTopicSetters;
        match send_retrying_rate_limit("telegram_send rename_topic", || {
            bot.edit_forum_topic(ChatId(chat_id), thread_id)
                .name(name.to_string())
        })
        .await
        {
            Ok(_) => {
                // edit_forum_topic succeeded on the API — this chat IS a
                // forum; the evidence is API-proven, not inferred.
                self.telegram_state
                    .note_thread_evidence(chat_id, true, Some(thread_id_raw as i32))
                    .await;
                crate::channels::telegram::record_topic_created(
                    None,
                    chat_id,
                    thread_id_raw as i32,
                    name,
                    true,
                )
                .await;
                log_send_success(
                    "tool",
                    "rename_topic",
                    "rename_topic",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    Some(thread_id_raw as i32),
                    0,
                    name.len(),
                    "-",
                );
                let res = serde_json::json!({
                    "status": "success",
                    "chat_id": chat_id,
                    "thread_id": thread_id_raw,
                    "name": name
                });
                Ok(ToolResult::success(res.to_string()))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "rename_topic",
                    "rename_topic",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    Some(thread_id_raw as i32),
                    name.len(),
                    "-",
                    &e.to_string(),
                );
                Ok(ToolResult::error(format!("Failed to rename topic: {e}")))
            }
        }
    }

    /// `bind_topic` — bind the calling session to a forum topic.
    async fn action_bind_topic(
        &self,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let ChatTarget { chat_id } =
            pget!(resolve_chat_target(input, context.session_id, &self.telegram_state).await);
        let thread_id_raw = pget!(get_id(input, "thread_id"));
        let topic_id = Some(thread_id_raw as i32);

        match self
            .telegram_state
            .bind_session_topic(context.session_id, chat_id, topic_id)
            .await
        {
            Ok(_) => {
                // bind_topic is an explicit forum operation by name: the
                // caller asserted a topic id, so record topic-typed evidence
                // (#1708: the message path no longer leaks stray reply-chain
                // ids into this arm via the header).
                self.telegram_state
                    .note_thread_evidence(chat_id, true, topic_id)
                    .await;
                log_send_success(
                    "tool",
                    "bind_topic",
                    "bind_topic",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    topic_id,
                    0,
                    0,
                    "-",
                );
                let res = serde_json::json!({
                    "status": "success",
                    "session_id": context.session_id.to_string(),
                    "chat_id": chat_id,
                    "thread_id": thread_id_raw
                });
                Ok(ToolResult::success(res.to_string()))
            }
            Err(e) => {
                log_send_failure(
                    "tool",
                    "bind_topic",
                    "bind_topic",
                    &context.session_id.to_string(),
                    "action",
                    chat_id,
                    topic_id,
                    0,
                    "-",
                    &e,
                );
                Ok(ToolResult::error(format!("Failed to bind topic: {e}")))
            }
        }
    }
}

/// #1889: may the owner fallback carry a telegram_send on this session?
///
/// Pure decision — origin is the session's durable binding, `None` meaning
/// unknown. Sending for a session the user is talking to ON Telegram stays
/// allowed (the fallback predates the binding lookup and cron jobs rely on
/// it); so does an unknown origin. What it kills is the silent platform jump:
/// the Discord conversation stays on Discord while the artifact lands in the
/// owner's Telegram chat, with no receipt the user asked for anywhere near
/// the message they were reading. Unknown origin is no objection; a binding
/// that names another platform is.
#[derive(Debug, PartialEq, Eq)]
enum OriginRuling {
    /// Origin is telegram, or unknown — owner fallback may carry the send.
    Allow,
    /// Origin is another surface — refuse before the wire call.
    Refuse { channel: String },
}

fn rule_on_origin(origin: Option<&str>) -> OriginRuling {
    match origin {
        None | Some("telegram") => OriginRuling::Allow,
        Some(channel) => OriginRuling::Refuse {
            channel: channel.to_string(),
        },
    }
}

/// Error text when the ruling is a refusal, `None` when allowed. An absent
/// owner id keeps its own older error further up the call chain, so this
/// returns nothing to let that path speak.
fn cross_platform_refusal(origin: Option<&str>, owner: Option<i64>) -> Option<String> {
    owner?;
    match rule_on_origin(origin) {
        OriginRuling::Allow => None,
        OriginRuling::Refuse { channel } => Some(format!(
            "telegram_send refused: this session lives on '{channel}', not Telegram, \
             and no chat_id was given. Sending here would jump platforms silently \
             (#1889). Use the {channel} tool, pass an explicit chat_id, or send from \
             a Telegram session."
        )),
    }
}

/// Pull the poll id out of a sent-message payload (#2288).
///
/// teloxide models a poll as message media, so the id sits three levels down.
/// A send_poll response always carries one, but the match is exhaustive rather
/// than unwrapped: losing the id means silently dropping every vote.
fn poll_id_of(message: &teloxide::types::Message) -> Option<String> {
    match &message.kind {
        teloxide::types::MessageKind::Common(common) => match &common.media_kind {
            teloxide::types::MediaKind::Poll(media) => Some(media.poll.id.0.clone()),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod cross_platform_tests {
    use super::*;

    #[test]
    fn discord_origin_refuses_the_owner_fallback() {
        let r = cross_platform_refusal(Some("discord"), Some(7711740248));
        let msg = r.expect("a discord session must not silently use the telegram owner chat");
        assert!(msg.contains("discord"), "{msg}");
        assert!(msg.contains("#1889"), "{msg}");
    }

    #[test]
    fn slack_and_whatsapp_refuse_too() {
        for ch in ["slack", "whatsapp"] {
            let msg = cross_platform_refusal(Some(ch), Some(1))
                .unwrap_or_else(|| panic!("{ch} origin must refuse"));
            assert!(msg.contains(ch), "{msg}");
        }
    }

    #[test]
    fn telegram_origin_is_allowed() {
        assert_eq!(cross_platform_refusal(Some("telegram"), Some(1)), None);
    }

    #[test]
    fn unknown_origin_is_no_objection() {
        // Targetless cron and A2A sessions carry no binding; the guarded
        // owner fallback is their long-standing route and must keep working.
        assert_eq!(cross_platform_refusal(None, Some(1)), None);
        assert_eq!(rule_on_origin(None), OriginRuling::Allow);
    }

    #[test]
    fn no_owner_keeps_the_older_error_path() {
        // Without an owner id, this guard has nothing to route to; the None
        // branch in chat_or_err still fires its own message.
        assert_eq!(cross_platform_refusal(Some("discord"), None), None);
    }
}
