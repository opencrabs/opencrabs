//! Raw-aware Telegram update polling (#354).
//!
//! teloxide parses each update into typed structs and silently DISCARDS any
//! content field it does not know. A forward of a rich-formatted message
//! (new Bot API content types) therefore reaches the process and dies at the
//! parse boundary: the handler sees a message with no text, no media, and —
//! depending on how the unknown content deserializes — not even the forward
//! metadata, while the user watches the content sit fully rendered in their
//! chat.
//!
//! This listener fetches updates as RAW JSON first, stashes every message's
//! raw payload keyed by (chat_id, message_id), and only then converts to the
//! typed `Update` for the normal dispatcher. When the typed accessors come
//! up empty, the handler pulls the raw payload from the stash and hands the
//! actual content to the agent instead of dropping the message.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::Value;
use teloxide::stop::{StopFlag, StopToken, mk_stop_token};
use teloxide::types::Update;
use teloxide::update_listeners::StatefulListener;

/// Bounded stash of raw message payloads keyed by (chat_id, message_id).
/// 128 entries is minutes of traffic — the handler consumes an entry within
/// milliseconds of the dispatcher receiving the typed update.
static RAW_STASH: Mutex<VecDeque<((i64, i32), Value)>> = Mutex::new(VecDeque::new());
const STASH_CAP: usize = 128;

pub(crate) fn stash_raw_message(chat_id: i64, message_id: i32, raw: Value) {
    let mut q = RAW_STASH.lock().unwrap_or_else(|e| e.into_inner());
    q.retain(|((c, m), _)| !(*c == chat_id && *m == message_id));
    q.push_back(((chat_id, message_id), raw));
    while q.len() > STASH_CAP {
        q.pop_front();
    }
}

pub(crate) fn take_raw_message(chat_id: i64, message_id: i32) -> Option<Value> {
    let mut q = RAW_STASH.lock().unwrap_or_else(|e| e.into_inner());
    let idx = q
        .iter()
        .position(|((c, m), _)| *c == chat_id && *m == message_id)?;
    q.remove(idx).map(|(_, v)| v)
}

/// Forward origin from a RAW message payload — works even when teloxide's
/// typed parse dropped it along with the unknown content type.
pub(crate) fn raw_forward_origin(raw: &Value) -> Option<String> {
    let origin = raw.get("forward_origin")?;
    if let Some(u) = origin.get("sender_user") {
        let mut label = u
            .get("first_name")
            .and_then(|v| v.as_str())
            .unwrap_or("someone")
            .to_string();
        if let Some(last) = u.get("last_name").and_then(|v| v.as_str()) {
            label.push(' ');
            label.push_str(last);
        }
        if u.get("is_bot").and_then(|v| v.as_bool()).unwrap_or(false) {
            label.push_str(" (bot)");
        }
        return Some(label);
    }
    if let Some(name) = origin.get("sender_user_name").and_then(|v| v.as_str()) {
        return Some(name.to_string());
    }
    for key in ["sender_chat", "chat"] {
        if let Some(title) = origin
            .get(key)
            .and_then(|c| c.get("title"))
            .and_then(|v| v.as_str())
        {
            return Some(title.to_string());
        }
    }
    Some("an unknown origin".to_string())
}

/// Render a raw message payload for the agent: drop the noisy envelope keys
/// the agent never needs (sender/chat objects, ids, dates) and pretty-print
/// the rest — that is where the content of an unknown type lives. Truncated
/// so a huge payload cannot blow up the turn.
pub(crate) fn raw_content_for_agent(raw: &Value) -> String {
    const DROP_KEYS: &[&str] = &[
        "from",
        "chat",
        "date",
        "message_id",
        "edit_date",
        "forward_origin",
        "has_protected_content",
        "sender_chat",
        "via_bot",
        "message_thread_id",
    ];
    let mut content = raw.clone();
    if let Some(obj) = content.as_object_mut() {
        for k in DROP_KEYS {
            obj.remove(*k);
        }
    }
    let pretty = serde_json::to_string_pretty(&content).unwrap_or_else(|_| content.to_string());
    crate::utils::truncate_str(&pretty, 4096).to_string()
}

struct RawPollState {
    http: reqwest::Client,
    token: String,
    offset: i64,
    pending: VecDeque<RawQueued>,
    flag: StopFlag,
    stop_token: StopToken,
    /// Guest-mode turn runner (#2087). `None` means guest updates are logged
    /// and dropped, which is also what an older build does.
    guest: Option<std::sync::Arc<super::guest::GuestRunner>>,
}

/// An update parked between reception and dispatcher handoff, carrying the
/// metadata the #1778 receipts need: without the stash instant and raw id,
/// a stalled drain is indistinguishable from a stalled poll.
struct RawQueued {
    update: Update,
    update_id: i64,
    queued_at: std::time::Instant,
}

/// What a failed getUpdates response actually means. Telegram answers 409
/// "Conflict: terminated by other getUpdates request" when ANOTHER instance
/// is long-polling the same bot token; treating it like any transient error
/// makes two opencrabs instances fight forever in near-silence, each
/// receiving a random subset of updates and both replying (#1721).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollFailure {
    /// Another poller owns this bot token right now.
    Conflict,
    /// Anything else: transient network/API trouble, retry shortly.
    Other,
}

/// Classify a non-ok getUpdates response body.
pub(crate) fn classify_poll_failure(value: &Value) -> PollFailure {
    let code = value.get("error_code").and_then(|v| v.as_i64());
    let description = value
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if code == Some(409) && description.contains("terminated by other getUpdates request") {
        PollFailure::Conflict
    } else {
        PollFailure::Other
    }
}

/// How a getUpdates HTTP-200 body classifies for the poll-cycle receipt
/// (#1778). Every branch maps to a distinct log line, so a stall becomes
/// attributable from logs alone: no receipt lines = the dispatcher stopped
/// pulling; `Empty` receipts = poller/server healthy and idle;
/// `MalformedResult`/`NotOk` = the API answered but poisoned the body.
pub(crate) enum PollOutcome {
    /// ok=true, result is a non-empty array (count carried separately).
    Updates,
    /// ok=true, result is an empty array: healthy long-poll timeout.
    Empty,
    /// ok=true but result missing or not an array: malformed 200.
    MalformedResult,
    /// ok=false: failure details come from `classify_poll_failure`.
    NotOk,
}

/// Pure classification of a getUpdates response body. Testable seam for the
/// receipt log's branch structure.
pub(crate) fn classify_poll_outcome(value: &Value) -> PollOutcome {
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return PollOutcome::NotOk;
    }
    match value.get("result").and_then(|v| v.as_array()) {
        Some(a) if a.is_empty() => PollOutcome::Empty,
        Some(_) => PollOutcome::Updates,
        None => PollOutcome::MalformedResult,
    }
}

/// One poll cycle with receipt logging (#1778): the call line, the inner
/// long-poll, and the return line with the received count and elapsed time.
/// A stalled inbound stream becomes attributable from logs alone — no
/// receipt lines means the dispatcher stopped pulling the stream, since the
/// unfold loop only polls when the dispatcher asks for the next update.
async fn poll_once(st: &mut RawPollState) {
    tracing::debug!(
        "Telegram getUpdates call: offset={}, pending_queue={}",
        st.offset,
        st.pending.len()
    );
    let started = std::time::Instant::now();
    let received = poll_once_inner(st).await;
    tracing::debug!(
        "Telegram getUpdates return: received={received}, pending_queue={}, offset={}, \
         elapsed_ms={}",
        st.pending.len(),
        st.offset,
        started.elapsed().as_millis()
    );
}

/// One getUpdates long-poll: stash raw message payloads, queue the typed
/// updates. Errors are logged and absorbed with a short backoff — the outer
/// dispatcher retry loop still guards against total failure. Returns the
/// number of updates the batch carried (0 on every failure shape; each
/// failure shape logs its own distinct line).
async fn poll_once_inner(st: &mut RawPollState) -> usize {
    let url = format!("https://api.telegram.org/bot{}/getUpdates", st.token);
    let body = serde_json::json!({
        "timeout": 30,
        "offset": st.offset,
        "allowed_updates": [
            "message",
            "edited_message",
            "callback_query",
            "message_reaction",
            "my_chat_member",
            // Guest mode (#2087). Bot API 10.0 added the update type; the
            // pinned teloxide-core does not know it, so this list is the only
            // place it can be asked for. `allowed_updates` is a FILTER: an
            // update type missing from it is never delivered at all.
            "guest_message",
        ],
    });
    let resp = st
        .http
        .post(&url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(40))
        .send()
        .await;
    let value: Value = match resp {
        Ok(r) => match r.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Telegram raw poll: body read failed: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                return 0;
            }
        },
        Err(e) => {
            tracing::warn!("Telegram raw poll: request failed: {e}");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            return 0;
        }
    };
    let outcome = classify_poll_outcome(&value);
    if matches!(outcome, PollOutcome::NotOk) {
        match classify_poll_failure(&value) {
            PollFailure::Conflict => {
                // #1721: escalate loudly and back off hard. The fight itself
                // is self-inflicted: another opencrabs (or any bot client)
                // is long-polling this exact token.
                tracing::error!(
                    "Telegram poll CONFLICT (409): another instance is polling this bot token. \
                     Two pollers on one token each receive a random subset of updates and both \
                     reply, producing duplicate sessions and answers. Stop the other instance, \
                     or give this one its own bot token and profile. Backing off 30s."
                );
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
            PollFailure::Other => {
                tracing::warn!(
                    "Telegram raw poll: getUpdates not ok: {}",
                    crate::utils::truncate_str(&value.to_string(), 200)
                );
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
        return 0;
    }
    if matches!(outcome, PollOutcome::MalformedResult) {
        // #1778: this 200-shape was previously silent — during a stall it
        // was indistinguishable from a healthy idle poll. Warn with the body.
        tracing::warn!(
            "Telegram raw poll: ok=true but result missing/not an array: {}",
            crate::utils::truncate_str(&value.to_string(), 200)
        );
        return 0;
    }
    let updates = value
        .get("result")
        .and_then(|v| v.as_array())
        .expect("classified Updates above");
    for u in updates {
        let update_id = u.get("update_id").and_then(|v| v.as_i64()).unwrap_or(-1);
        st.offset = st.offset.max(update_id + 1);
        // Guest mode (#2087): handled off the RAW payload and skipped before
        // the typed parse, which has no `guest_message` kind and would log a
        // parse error for a perfectly good update.
        if let Some(query) = super::guest::parse_guest_query(u) {
            tracing::info!(
                "Telegram raw poll: guest query {} from user {} in chat {:?}",
                query.query_id,
                query.user_id,
                query.chat_id
            );
            match &st.guest {
                Some(runner) => runner.spawn(query),
                None => tracing::warn!(
                    "Telegram raw poll: guest query {} arrived with no guest handler wired, \
                     dropping it",
                    query.query_id
                ),
            }
            continue;
        }
        let mut u = u.clone();
        // Stash the raw message BEFORE the typed parse can lose content, and
        // SYNTHESIZE readable text into messages whose content type this Bot
        // API client does not know: without a known content field the update
        // either parses into an empty message or into an Error kind no
        // dispatch branch matches — both ended as silence (#354). Rewriting
        // the unknown content as a plain text message makes it flow through
        // the ENTIRE normal pipeline (handler, agent, context, display).
        for key in ["message", "edited_message"] {
            if let Some(m) = u.get_mut(key) {
                let chat_id = m
                    .get("chat")
                    .and_then(|c| c.get("id"))
                    .and_then(|v| v.as_i64());
                let msg_id = m.get("message_id").and_then(|v| v.as_i64());
                if let (Some(c), Some(mid)) = (chat_id, msg_id) {
                    stash_raw_message(c, mid as i32, m.clone());
                }
                synthesize_unknown_content(m);
            }
        }
        // MUST be from_str: teloxide's Update deserializer only works from
        // string input — from_value yields UpdateKind::Error for EVERYTHING
        // (proven in telegram_raw_update_parse_test; it took the intake down).
        match serde_json::from_str::<Update>(&u.to_string()) {
            Ok(update) => {
                tracing::debug!(
                    "Telegram raw poll: update {update_id} parsed, kind={}",
                    update_kind_name(&update)
                );
                st.pending.push_back(RawQueued {
                    update,
                    update_id,
                    queued_at: std::time::Instant::now(),
                });
            }
            Err(e) => {
                // The raw payload is stashed; only the typed dispatch is
                // impossible. Loudly logged — never silently eaten.
                tracing::error!(
                    "Telegram raw poll: update {update_id} failed typed parse ({e}) — raw \
                     stashed, typed dispatch skipped: {}",
                    crate::utils::truncate_str(&u.to_string(), 300)
                );
            }
        }
    }
    if !updates.is_empty() {
        tracing::info!(
            "Telegram raw poll: batch of {} update(s), offset now {}",
            updates.len(),
            st.offset
        );
    }
    updates.len()
}

/// Message JSON keys that count as KNOWN content — if any is present the
/// typed parse can represent the message and nothing needs synthesizing.
/// Service-event keys are included so pins/joins/topic events keep their
/// normal (ignored) handling instead of being rewritten into text.
const KNOWN_CONTENT_KEYS: &[&str] = &[
    "text",
    "caption",
    "photo",
    "voice",
    "video",
    "document",
    "animation",
    "video_note",
    "sticker",
    "audio",
    "poll",
    "location",
    "contact",
    "venue",
    "dice",
    "game",
    "story",
    "invoice",
    "successful_payment",
    "new_chat_members",
    "left_chat_member",
    "pinned_message",
    "new_chat_title",
    "new_chat_photo",
    "delete_chat_photo",
    "group_chat_created",
    "supergroup_chat_created",
    "channel_chat_created",
    "message_auto_delete_timer_changed",
    "migrate_to_chat_id",
    "migrate_from_chat_id",
    "forum_topic_created",
    "forum_topic_closed",
    "forum_topic_reopened",
    "forum_topic_edited",
    "video_chat_started",
    "video_chat_ended",
    "video_chat_scheduled",
    "video_chat_participants_invited",
    "web_app_data",
    "proximity_alert_triggered",
    // Known to teloxide-core 0.13 (verified against its serde definitions):
    // these parse into typed kinds, so they must NOT take the synthesize
    // detour; content-carrying ones are rendered readably by `rich_decode`
    // in the handler's no-content branch (#359).
    "checklist",
    "checklist_tasks_done",
    "checklist_tasks_added",
    "paid_media",
    "giveaway",
    "giveaway_created",
    "giveaway_winners",
    "giveaway_completed",
    "gift",
    "unique_gift",
    "users_shared",
    "chat_shared",
    "refunded_payment",
    "boost_added",
    "chat_background_set",
    "connected_website",
    "write_access_allowed",
    "passport_data",
    "direct_message_price_changed",
    "paid_message_price_changed",
    "general_forum_topic_hidden",
    "general_forum_topic_unhidden",
];

/// If the raw message carries NONE of the known content keys, rewrite it in
/// place into a plain text message whose text is the raw content payload
/// (plus forward provenance), and drop the unknown keys so the typed parse
/// lands on a normal text message.
fn synthesize_unknown_content(m: &mut Value) {
    let Some(obj) = m.as_object_mut() else { return };
    if KNOWN_CONTENT_KEYS.iter().any(|k| obj.contains_key(*k)) {
        return;
    }
    // Envelope-only keys: what remains beyond these IS the unknown content.
    const ENVELOPE: &[&str] = &[
        "message_id",
        "from",
        "chat",
        "date",
        "edit_date",
        "forward_origin",
        "forward_from",
        "forward_date",
        "reply_to_message",
        "via_bot",
        "sender_chat",
        "message_thread_id",
        "is_topic_message",
        "has_protected_content",
        "author_signature",
        "entities",
        "link_preview_options",
        "quote",
        "external_reply",
        "effect_id",
        "is_from_offline",
        "business_connection_id",
        "sender_boost_count",
        "sender_business_bot",
        "is_automatic_forward",
        "paid_star_count",
    ];
    let unknown: serde_json::Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| !ENVELOPE.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if unknown.is_empty() {
        // Nothing beyond the envelope at all — a service shape we don't
        // recognize; leave it for the normal (ignore) path.
        return;
    }
    let origin_note = raw_forward_origin(m)
        .map(|o| format!(" forwarded from \"{o}\""))
        .unwrap_or_default();
    // A future unknown type may still carry a payload a decoder recognizes;
    // try readable decoding first, fall back to the raw-JSON dump (#359).
    let text = match super::rich_decode::decode_rich_content(m) {
        Some(decoded) => format!("[A rich message{origin_note}]:\n{decoded}"),
        None => {
            let pretty = serde_json::to_string_pretty(&Value::Object(unknown.clone()))
                .unwrap_or_else(|_| "<unrenderable>".to_string());
            format!(
                "[A message{origin_note} arrived in a format the Bot API client cannot \
                 decode natively. Its raw content payload follows — read the content \
                 directly from it:]\n```json\n{}\n```",
                crate::utils::truncate_str(&pretty, 4096)
            )
        }
    };
    tracing::warn!(
        "Telegram raw poll: synthesizing text for unknown content keys {:?} (chat={:?}, msg={:?})",
        unknown.keys().collect::<Vec<_>>(),
        m.get("chat").and_then(|c| c.get("id")),
        m.get("message_id"),
    );
    let obj = m.as_object_mut().expect("checked above");
    for k in unknown.keys() {
        obj.remove(k);
    }
    obj.remove("entities");
    obj.insert("text".to_string(), Value::String(text));
}

/// Short name of an update's kind for the poll debug log.
fn update_kind_name(u: &Update) -> &'static str {
    use teloxide::types::UpdateKind;
    match &u.kind {
        UpdateKind::Message(_) => "message",
        UpdateKind::EditedMessage(_) => "edited_message",
        UpdateKind::CallbackQuery(_) => "callback_query",
        UpdateKind::MessageReaction(_) => "message_reaction",
        UpdateKind::Error(_) => "ERROR(unparsed)",
        _ => "other",
    }
}

/// Stream of typed updates driven by the raw poll loop. A named fn (rather
/// than a closure) so the higher-ranked lifetime `StatefulListener` needs is
/// inferred correctly.
fn raw_update_stream(
    st: &mut RawPollState,
) -> impl futures::Stream<Item = Result<Update, std::convert::Infallible>> + Send + '_ {
    futures::stream::unfold(st, |st| async move {
        loop {
            if st.flag.is_stopped() {
                return None;
            }
            if let Some(q) = st.pending.pop_front() {
                // #1778 handoff receipt: per-update queue wait makes a
                // stalled drain visible — a flushed backlog shows as a burst
                // of handoff lines with growing queued_wait_ms.
                tracing::debug!(
                    "Telegram update handoff to dispatcher: update_id={}, kind={}, \
                     queued_wait_ms={}",
                    q.update_id,
                    update_kind_name(&q.update),
                    q.queued_at.elapsed().as_millis()
                );
                return Some((Ok(q.update), st));
            }
            poll_once(st).await;
        }
    })
}

fn raw_stop_token(st: &mut RawPollState) -> StopToken {
    st.stop_token.clone()
}

/// Build the raw-aware update listener for the dispatcher.
///
/// `guest` is the guest-mode runner (#2087): a guest update carries a kind the
/// typed dispatcher cannot represent, so the listener hands it straight to the
/// runner and skips the typed parse.
pub(crate) fn raw_polling_listener(
    token: String,
    guest: Option<std::sync::Arc<super::guest::GuestRunner>>,
) -> impl teloxide::update_listeners::UpdateListener<Err = std::convert::Infallible> {
    let (stop_token, flag) = mk_stop_token();
    let state = RawPollState {
        http: reqwest::Client::new(),
        token,
        offset: 0,
        pending: VecDeque::new(),
        flag,
        stop_token,
        guest,
    };
    StatefulListener::new(state, raw_update_stream, raw_stop_token)
}
