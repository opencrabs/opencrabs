//! Pure Telegram session title + label-drift helpers (testable without teloxide).
//!
//! Issue #121: naive full-title comparison reverted auto-titled DM sessions back
//! to the default `Telegram: DM …` template on every subsequent message.

/// Build the canonical session title for a Telegram chat.
///
/// `topic_id` scopes the title to a forum topic (#215): `Some` appends
/// `:topic:<id>` to the suffix so each topic in a supergroup resolves to its
/// own session row instead of all topics collapsing onto the bare
/// `[chat:<id>]` session. `None` (DMs, non-forum groups, the General topic)
/// produces the exact same title as before topics existed.
pub fn build_session_title(
    is_dm: bool,
    user_name: &str,
    user_id: i64,
    chat_title: &str,
    chat_id: i64,
    topic_id: Option<i32>,
    topic_name: Option<&str>,
) -> String {
    let suffix = chat_id_suffix(chat_id, topic_id);
    if is_dm {
        format!("Telegram: DM {user_name} ({user_id}) {suffix}")
    } else {
        // A forum topic gets its NAME in the display ("… / Devops"), so the
        // session reads as the topic instead of the numeric thread id. The
        // suffix stays the numeric `:topic:<id>` form for stable resolution.
        match topic_name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) if topic_id.is_some() => {
                format!("Telegram: {chat_title} / {name} {suffix}")
            }
            _ => format!("Telegram: {chat_title} {suffix}"),
        }
    }
}

/// Legacy title format (pre suffix) for migration lookups.
pub fn build_legacy_session_title(
    is_dm: bool,
    user_name: &str,
    user_id: i64,
    chat_title: &str,
) -> String {
    if is_dm {
        format!("Telegram: DM {user_name} ({user_id})")
    } else {
        format!("Telegram: {chat_title}")
    }
}

/// Title/lookup suffix for a chat, optionally scoped to a forum topic (#215).
///
/// Base chats stay `[chat:<id>]`; a real forum topic becomes
/// `[chat:<id>:topic:<tid>]`. `find_session_by_title_suffix` matches with
/// `LIKE '%suffix'`, and a base title never ends with `:topic:<n>]` while a
/// topic title never ends with the bare `[chat:<id>]`, so the two never
/// cross-resolve.
pub fn chat_id_suffix(chat_id: i64, topic_id: Option<i32>) -> String {
    match topic_id {
        Some(tid) => format!("[chat:{chat_id}:topic:{tid}]"),
        None => format!("[chat:{chat_id}]"),
    }
}

/// Resolve the forum-topic id used to key a session (#215). Returns the thread
/// id ONLY for a genuine forum-topic message. DMs, non-forum groups, the
/// General topic, and plain reply-threads report `is_topic_message == false`
/// (or no thread id) and resolve to `None`, so they keep sharing the base
/// `[chat:<id>]` session exactly as before. Gating on `is_topic_message` is
/// deliberate: a bare `thread_id` can appear on ordinary reply-threads, which
/// must NOT spawn isolated sessions.
pub fn topic_session_id(is_topic_message: bool, thread_id: Option<i32>) -> Option<i32> {
    if is_topic_message { thread_id } else { None }
}

/// Canonical Telegram thread id of a forum group's built-in General topic.
/// General messages carry NO explicit `message_thread_id` and are not flagged
/// `is_topic_message`, which historically made them indistinguishable from
/// DMs / non-forum groups (#1220).
pub use crate::channels::GENERAL_TOPIC_ID;

/// The `message_thread_id` to put on the wire for a session scoped to `topic`.
///
/// [`GENERAL_TOPIC_ID`] is a SESSION-SCOPING KEY, never an address. #1220
/// invented it so a forum's General topic would not collide with a DM on the
/// same chat, and that is right — but General messages carry no
/// `message_thread_id` at all, so sending `1` asks Telegram for a thread that
/// does not exist and every message-CREATING call is refused with
/// `Bad Request: message thread not found` (#1319).
///
/// Edits hid this for a long time: they address a `message_id` and never
/// consult a thread, so a General-bound session kept updating its status
/// cards in place and only failed the moment it had something new to say.
///
/// Every sender goes through here rather than converting the stored topic
/// itself. Six call sites were each doing their own `ThreadId(MessageId(t))`,
/// and patching six is an invitation for a seventh to reintroduce it.
pub fn delivery_thread_id(topic: Option<i32>) -> Option<teloxide::types::ThreadId> {
    match topic {
        // General: the absence of a thread IS the address.
        Some(GENERAL_TOPIC_ID) | None => None,
        Some(real) => Some(teloxide::types::ThreadId(teloxide::types::MessageId(real))),
    }
}

/// Where a push that BELONGS to a session must be delivered (pure, #1200).
///
/// The distinction the delivery paths kept losing is THREE-state, and it has
/// to survive the trip from the binding to the wire:
///
/// * `Bound(Some(topic))` — the session lives in a real forum topic.
/// * `Bound(None)` — the session lives in the General topic of a forum, or in
///   a DM / non-forum group. Both are addressed by the ABSENCE of a thread
///   (#1319): General's `Some(1)` is a scoping key, never an address.
/// * `Unbound` — no binding exists for this session anywhere, and only then
///   is the chat's most recent topic the best answer available.
///
/// `TelegramState::session_topic` flattens the first two arms into one `None`
/// (its `.flatten()` collapses `Some(None)` and a missing entry), so a caller
/// that resolves through it and then falls back to the chat-wide lookup sends
/// a General-bound session's message into whichever topic spoke last. That is
/// the defect this function exists to make unrepeatable: on 2026-09-21 the
/// hourly digest of the "Общее / Неразобранное" topic (session bound to
/// General) was delivered into the project topic that happened to be the most
/// recent speaker.
///
/// `in_memory` is the live binding (`TelegramState::session_binding`), which
/// the connect-time re-registration of #1224 fills from the durable rows;
/// `durable` is the `session_bindings` row itself, read only when the maps
/// hold nothing for the session (a push can arrive before that re-registration
/// has run). A binding for a DIFFERENT chat is not this chat's business, so
/// both are matched against `chat_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushTarget {
    /// The session's own binding names the address — `Some(topic)` for a real
    /// topic, `None` for General / a DM / a non-forum group.
    Bound(Option<i32>),
    /// No binding for this session: fall back to the chat-wide lookup.
    Unbound,
}

/// Decide [`PushTarget`] from the two stores, pure (#1200).
pub fn push_target(
    in_memory: Option<(i64, Option<i32>)>,
    durable: Option<(i64, Option<i32>)>,
    chat_id: i64,
) -> PushTarget {
    if let Some((_, topic)) = in_memory.filter(|(chat, _)| *chat == chat_id) {
        return PushTarget::Bound(topic);
    }
    if let Some((_, topic)) = durable.filter(|(chat, _)| *chat == chat_id) {
        return PushTarget::Bound(topic);
    }
    PushTarget::Unbound
}

/// #1220: normalize a raw topic resolution for KNOWN forum chats.
///
/// `raw` is the output of [`topic_session_id`]; `known_forum` is the
/// evidence-based verdict that this chat has hosted at least one
/// thread-scoped message (see `TelegramState::note_thread_evidence`).
///
/// In a known forum, a message with no explicit topic IS the General topic,
/// not "no topic" — so it gets its own session bucket keyed on
/// `Some(GENERAL_TOPIC_ID)` instead of collapsing into the base
/// `[chat:<id>]` session. This makes bg-task / subagent pushes (#1200)
/// route back to General instead of falling to the chat-wide-latest
/// lookup and landing in whatever topic spoke last.
///
/// Cold start: until the chat's first thread-scoped message is observed
/// (in-memory cache), `known_forum` is false and behaviour is unchanged.
pub fn normalize_topic(raw: Option<i32>, known_forum: bool) -> Option<i32> {
    raw.or(if known_forum {
        Some(GENERAL_TOPIC_ID)
    } else {
        None
    })
}

/// Session-scoping topic id for ANY Telegram event: an incoming message or an
/// inline-button callback. Single entry point so the two paths cannot drift.
///
/// They did drift (#1248): ingress composed
/// `normalize_topic(topic_session_id(..), known_forum)` while the callback
/// resolver called `topic_session_id` alone. In a known forum the General
/// topic then resolved `None` for button presses and `Some(GENERAL_TOPIC_ID)`
/// for messages, so `/models` taps looked up `(chat, None)` — the pre-#1220
/// base session — and wrote the provider/model pick into a row that serves no
/// messages, while the chat kept answering on its real `(chat, Some(1))`
/// session. The switch appeared to be silently ignored.
pub fn session_topic_for_event(
    is_topic_message: bool,
    thread_id: Option<i32>,
    known_forum: bool,
) -> Option<i32> {
    normalize_topic(topic_session_id(is_topic_message, thread_id), known_forum)
}

/// True when a session exceeded the configured idle window (same rule as handler suffix path).
pub fn session_idle_expired(
    updated_at: chrono::DateTime<chrono::Utc>,
    idle_hours: Option<f64>,
) -> bool {
    idle_hours.is_some_and(|h| {
        let elapsed = (chrono::Utc::now() - updated_at).num_seconds();
        elapsed > (h * 3600.0) as i64
    })
}

/// Handler resolve policy: explicit chat binding wins over suffix `updated_at` winner.
///
/// Cold-start behaviour: the `chat_sessions` map that feeds `chat_bound`
/// is in-memory and process-scoped — every opencrabs restart starts the
/// map empty. The first message in any chat after a restart therefore
/// returns `ResolveSource::Suffix` (no binding yet) and falls through to
/// `find_session_by_title_suffix`. That's correct behaviour, just worth
/// being aware of when reading the policy in isolation: an empty map
/// doesn't mean "create a new session", it means "look up the existing
/// row by suffix".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveSource {
    ChatBound,
    Suffix,
    Create,
}

pub fn choose_resolve_source(
    chat_bound: Option<uuid::Uuid>,
    bound_archived: bool,
    suffix_match: Option<uuid::Uuid>,
) -> ResolveSource {
    if chat_bound.is_some() && !bound_archived {
        ResolveSource::ChatBound
    } else if suffix_match.is_some() {
        ResolveSource::Suffix
    } else {
        ResolveSource::Create
    }
}

/// Whether to overwrite a stored session title with the freshly built template.
///
/// - Default DM titles: refresh when the template default changed (display name).
/// - Auto-titled / custom DM titles: never clobber (issue #121).
/// - Telegram groups: refresh when the visible group label changed (suffix stable).
pub fn should_refresh_label(stored: &str, template: &str) -> bool {
    if stored == template {
        return false;
    }

    if crate::brain::agent::service::AgentService::is_default_channel_title(stored) {
        return crate::brain::agent::service::AgentService::is_default_channel_title(template)
            && stored != template;
    }

    if is_telegram_group_session_title(stored) && is_telegram_group_session_title(template) {
        return telegram_middle_label(stored) != telegram_middle_label(template);
    }

    false
}

fn is_telegram_group_session_title(title: &str) -> bool {
    let Some(rest) = title.strip_prefix("Telegram: ") else {
        return false;
    };
    !rest.starts_with("DM ") && title.contains("[chat:")
}

fn telegram_middle_label(title: &str) -> String {
    let body = title.strip_prefix("Telegram: ").unwrap_or(title).trim();
    let suffix = crate::brain::agent::service::AgentService::extract_chat_id_suffix(title);
    if suffix.is_empty() {
        return body.to_string();
    }
    body.strip_suffix(suffix).unwrap_or(body).trim().to_string()
}
