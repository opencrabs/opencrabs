//! Guest mode (#2087): answer `@botname` tags in chats the bot is not in.
//!
//! Bot API 10.0 ("Guest Mode", May 8 2026) added `Update.guest_message`, the
//! fields `Message.guest_query_id`, `Message.guest_bot_caller_user` and
//! `Message.guest_bot_caller_chat`, and the method `answerGuestQuery`
//! (verified against core.telegram.org/bots/api on 2026-10-18). The pinned
//! teloxide-core 0.13.0 knows none of it: `grep -rni guest` over its source
//! returns nothing, so a guest update parses into `UpdateKind::Error` and no
//! dispatch branch matches it. The whole path therefore lives on the raw HTTP
//! layer that already polls `getUpdates` (`raw_updates.rs`), exactly like
//! `message_reaction` did before teloxide grew a typed variant for it.
//!
//! Default deny, in two independent places. A guest reply answers a stranger
//! in a chat we were never invited to, so it needs `guest_queries = true` in
//! config AND the caller to be the owner, an admin, or listed for the caller's
//! own chat. A group's `open = true` grants nothing here: it records that the
//! owner opened THAT chat for the bot, not that the bot answers anywhere that
//! group's members happen to tag it.
//!
//! Tools that need approval are denied for a guest turn. The approval surface
//! is buttons in a chat, and a guest turn has no chat to put them in; denying
//! is the only safe default when the caller does not own the machine.
//!
//! # The reply carries the progress (#2109)
//!
//! A guest summon gets one reply, and the bot is not in the chat, so there is
//! no typing indicator and no second bubble to post. The reply itself is the
//! only surface, which is exactly the surface Telegram designed: its Guest
//! Bots page lists "AI Assistants ... reporting its progress back to the chat
//! or group" as the first use case.
//!
//! So the reply is posted the moment the turn starts (a plain
//! [`GUEST_PLACEHOLDER`] line, since the initial `answerGuestQuery` body
//! carries no parse mode) and then edited in place: `answerGuestQuery` returns
//! `SentGuestMessage.inline_message_id`, and `editMessageText` accepts
//! `inline_message_id` (required when `chat_id`/`message_id` are absent) and
//! returns `True`. Edits ride the same 1.5s cadence the regular chat's edit
//! loop uses, and the state being rendered is the regular
//! [`StreamingState`] fed by the regular
//! [`apply_progress_event`](super::progress::apply_progress_event) mapping, so
//! the block, the tool lines, the counters and the settled footer are the same
//! code as a normal session rather than a guest-shaped copy.
//!
//! # Rich when the user enabled it (#2127 follow-up)
//!
//! `channels.telegram.rich_messages` is one setting, so it governs guest
//! replies too. With it on, the reply goes out as `InputRichMessageContent`
//! (Bot API lists it among the `InputMessageContent` types an inline or guest
//! result may carry) and is edited with `rich_message`, which raises the
//! ceiling from 4096 characters to 32768 and renders the markdown instead of
//! showing its asterisks. With it off, the reply is the same plain
//! `message_text` payload it always was. A refused rich edit falls back to the
//! HTML text edit rather than stranding the placeholder.
//!
//! Both planes carry MARKDOWN on a rich reply, never the Telegram-HTML the
//! classic path builds. The markdown plane is the one whose server-side render
//! keeps headings, lists and pipe tables native (#679: the html plane flattens
//! tables), and our HTML conversion leans on `\n` for line breaks, which a
//! markdown parse collapses into a single unbroken paragraph. That collapse is
//! exactly what a guest reply looked like when the rich path shipped HTML.
//!
//! Two things this deliberately does NOT copy from the regular path: the
//! typing indicator (the bot is not a member, so there is nothing to type in)
//! and the per-tool messages (one reply is all the API gives).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::flow::{
    FlowEntry, FlowOutcome, StreamingState, drain_display_queue, folded_duplicates_final,
    humanize_duration, last_folded_text, pop_trailing_folded_texts, render_flow,
    render_flow_rich_state,
};
use super::handler::{build_midturn_queued_message, thinking_status_excerpt};
use super::markdown::markdown_to_telegram_html;
use super::progress::apply_progress_event;
use crate::brain::agent::AgentService;
use crate::channels::telegram::TelegramState;
use crate::config::Config;
use crate::services::SessionService;

/// Telegram's ceiling for `InputTextMessageContent.message_text`.
const GUEST_TEXT_LIMIT: usize = 4096;
/// Telegram's ceiling for a rich message: "Up to 32768 UTF-8 characters in the
/// rich message text" (Bot API, Rich Message Limits). Eight times the plain
/// text ceiling, which is why the rich path never needs a truncation marker.
const GUEST_RICH_TEXT_LIMIT: usize = 32768;
/// Inline result id, kept well under Telegram's 64-byte cap.
const GUEST_RESULT_ID: &str = "opencrabs-guest";
/// Appended when a reply has to be cut to fit the message ceiling.
const TRUNCATION_MARKER: &str = "\n\n[truncated]";
/// What the reply says between being posted and the first real edit. One line
/// of plain text, so it reads the same whether the reply is opened as
/// `message_text` or as a rich message.
const GUEST_PLACEHOLDER: &str = "⏳ Working on it…";
/// How often the open reply is edited while the turn runs. Same cadence as
/// the regular chat's edit loop.
const GUEST_EDIT_INTERVAL: Duration = Duration::from_millis(1500);
/// What a guest follow-up gets back when the turn it belongs to is still
/// running. One line, because the running turn owns the reply that carries
/// the work: this only exists so the query id is answered before it expires.
const GUEST_FOLDED_NOTE: &str = "⏳ Added to the turn already running.";
/// How many leftover follow-ups one guest turn answers before it stops.
/// Each round is a full model call, so the cap keeps a chatty caller from
/// turning one summon into an unbounded loop.
const GUEST_FOLD_MAX_ROUNDS: usize = 3;
/// Below this much room left for the answer, the block is not worth showing:
/// a footer with three words under it helps nobody, so the answer ships alone.
const MIN_ANSWER_ROOM: usize = 200;

/// One parsed guest query: who summoned the bot, in which chat, and what they
/// said. Text only: a guest turn never sees the surrounding chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuestQuery {
    /// `Message.guest_query_id`, the value `answerGuestQuery` needs.
    pub query_id: String,
    /// `Message.guest_bot_caller_user.id`: who tagged the bot.
    pub user_id: i64,
    pub username: Option<String>,
    /// `Message.guest_bot_caller_chat.id`: the chat the tag came from. This id
    /// may collide with a chat the bot is already in, which is why guest
    /// sessions never bind to it in the shared chat map.
    pub chat_id: Option<i64>,
    pub chat_title: Option<String>,
    pub text: String,
}

/// Parse `Update.guest_message` out of a raw update payload.
///
/// `None` means "not a guest query we can answer": not a guest update at all,
/// no `guest_query_id` to answer into, or no identifiable caller (in which
/// case the gate has nothing to check and the query is dropped rather than
/// answered on faith).
pub(crate) fn parse_guest_query(update: &Value) -> Option<GuestQuery> {
    let msg = update.get("guest_message")?;
    let query_id = msg
        .get("guest_query_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if query_id.is_empty() {
        return None;
    }
    let caller_user = msg.get("guest_bot_caller_user").or_else(|| msg.get("from"));
    let user_id = caller_user
        .and_then(|u| u.get("id"))
        .and_then(Value::as_i64)?;
    let username = caller_user
        .and_then(|u| u.get("username"))
        .and_then(Value::as_str)
        .map(str::to_string);
    // The API says the tagged message belongs to the caller's chat; fall back
    // to the message's own `chat` when `guest_bot_caller_chat` is absent, so a
    // listed user still passes the per-chat check.
    let caller_chat = msg.get("guest_bot_caller_chat").or_else(|| msg.get("chat"));
    let chat_id = caller_chat
        .and_then(|c| c.get("id"))
        .and_then(Value::as_i64);
    let chat_title = caller_chat
        .and_then(|c| c.get("title"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let text = msg
        .get("text")
        .or_else(|| msg.get("caption"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(GuestQuery {
        query_id: query_id.to_string(),
        user_id,
        username,
        chat_id,
        chat_title,
        text,
    })
}

/// Drop `<<...>>` directives (reactions, image markers) from a guest reply.
///
/// Those directives are channel instructions, and this is the one surface
/// where the raw text leaves the process into a stranger's chat: an
/// un-processed `<<IMG:/Users/...>>` would post a local file path there.
pub(crate) fn strip_directives(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<<") {
        let after = &rest[start + 2..];
        match after.find(">>") {
            Some(end) if !after[..end].contains('\n') => {
                out.push_str(&rest[..start]);
                rest = &after[end + 2..];
            }
            _ => {
                out.push_str(&rest[..start + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Length in the units Telegram counts: UTF-16 code units after entities are
/// parsed, so an emoji counts 2. `chars().count()` under-counts astral text
/// and lets an over-long message through to a "message is too long" error.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Characters Telegram will actually show, i.e. everything outside `<...>`
/// tags. Conservative where it is approximate: `&amp;` counts 5, not 1, so the
/// budget it produces is always small enough to fit.
pub(crate) fn visible_len(html: &str) -> usize {
    let mut count = 0usize;
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => count += ch.len_utf16(),
            _ => {}
        }
    }
    count
}

/// Trim text to `room` UTF-16 code units, marker included.
fn fit_text(text: &str, room: usize) -> String {
    if utf16_len(text) <= room {
        return text.to_string();
    }
    let marker = utf16_len(TRUNCATION_MARKER);
    let keep = room.saturating_sub(marker);
    let mut used = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let width = ch.len_utf16();
        if used + width > keep {
            break;
        }
        out.push(ch);
        used += width;
    }
    if room >= marker {
        out.push_str(TRUNCATION_MARKER);
    }
    out
}

/// Trim a reply down to what a guest message may carry.
fn clamp_guest_text(text: &str) -> String {
    fit_text(text, GUEST_TEXT_LIMIT)
}

/// Fit `text` into `room` UTF-8 characters without a truncation marker.
///
/// The rich path measures characters, not UTF-16 units: the Bot API states its
/// ceiling as "32768 UTF-8 characters". This ceiling is a safety valve so an
/// oversized payload is never sent, not a limit a real answer meets.
fn fit_rich_text(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    text.chars().take(room).collect()
}

/// Trim a reply to the rich ceiling. No marker: the whole point of the rich
/// path is that a long answer arrives whole, so stamping `[truncated]` here is
/// the bug this exists to avoid.
fn clamp_guest_rich_text(text: &str) -> String {
    fit_rich_text(text, GUEST_RICH_TEXT_LIMIT)
}

/// The one message a guest reply can be: the turn's block (tool lines plus the
/// settled footer) with the answer under it.
///
/// Always inside the ceiling that applies: [`GUEST_TEXT_LIMIT`] for a plain
/// text reply, [`GUEST_RICH_TEXT_LIMIT`] when the reply goes out rich. When the
/// block leaves too little room for a real answer the answer ships alone,
/// because the block is chrome and the answer is the point.
///
/// The two paths take different markup, and that is the whole point. A plain
/// reply is Telegram HTML, because `parse_mode` on the classic content is the
/// only thing that can render it. A rich reply is MARKDOWN on both halves: the
/// rich API parses the source itself, so headings, lists and pipe tables come
/// out native. Handing it HTML instead lands on the html plane, which flattens
/// tables (#679), and the HTML we build relies on `\n` for line breaks, so a
/// markdown parse collapses the whole answer into one unbroken wall.
pub(crate) fn compose_guest_message(block: &str, answer_markdown: &str, rich: bool) -> String {
    if rich {
        let answer = clamp_guest_rich_text(answer_markdown.trim());
        let room = GUEST_RICH_TEXT_LIMIT.saturating_sub(block.chars().count() + 2);
        if block.is_empty() || room < MIN_ANSWER_ROOM {
            return answer;
        }
        let fitted = fit_rich_text(answer_markdown.trim(), room);
        let composed = if fitted.is_empty() {
            block.to_string()
        } else {
            format!("{block}\n\n{fitted}")
        };
        if composed.chars().count() > GUEST_RICH_TEXT_LIMIT {
            return answer;
        }
        return composed;
    }
    let answer = clamp_guest_text(answer_markdown.trim());
    let room = GUEST_TEXT_LIMIT.saturating_sub(visible_len(block) + 2);
    if block.is_empty() || room < MIN_ANSWER_ROOM {
        return answer;
    }
    let fitted = fit_text(answer_markdown.trim(), room);
    let composed = if fitted.is_empty() {
        block.to_string()
    } else {
        format!("{block}\n\n{}", markdown_to_telegram_html(&fitted))
    };
    if visible_len(&composed) > GUEST_TEXT_LIMIT {
        return answer;
    }
    composed
}

/// Take the answer out of the flow log so the settled reply carries it once.
///
/// The streamed answer also reaches the flow log as intermediate text, so a
/// settled block renders it and the reply renders it again: the caller sees
/// the same paragraph twice in one message (#2113). The regular delivery path
/// reclaims that copy before it sends the answer (#300); a guest reply is a
/// single message, so the same reclaim has to happen here.
///
/// - No content from the turn: the folded run IS the answer and comes back as
///   the reply. With `options_pending` the answer sits before the trailing
///   tool entry, so the popper lifts that entry aside and restores it.
/// - Content that duplicates the folded run: the folded copy leaves the block.
/// - A folded run that is neither stays where it is, because a narration line
///   that merely ended the turn is not the answer.
pub(crate) fn settle_guest_answer(
    entries: &mut Vec<FlowEntry>,
    content: &str,
    options_pending: bool,
) -> String {
    if content.trim().is_empty() {
        let (host, trailer) = pop_trailing_folded_texts(entries, options_pending);
        return match (host, trailer) {
            (Some(host), Some(trailer)) => format!("{host}\n\n{trailer}"),
            (Some(host), None) => host,
            (None, _) => String::new(),
        };
    }
    // The reply carries the answer, so any copy of it left in the block would
    // be read twice. Drop only the runs that duplicate it: a distinct narration
    // line that merely ended the turn is not the answer and stays where it is.
    while last_folded_text(entries).is_some_and(|folded| folded_duplicates_final(folded, content)) {
        let Some(pos) = entries
            .iter()
            .rposition(|e| matches!(e, FlowEntry::Text(_)))
        else {
            break;
        };
        entries.remove(pos);
    }
    // A turn that halted on the options surface keeps its answer BEFORE the
    // trailing tool entry, where `last_folded_text` refuses to look. The shared
    // popper is the only thing that knows how to lift that entry aside and put
    // it back, so it runs on a copy and is kept only when the run it reclaimed
    // really is the answer.
    if options_pending {
        let mut trial = entries.clone();
        let (host, trailer) = pop_trailing_folded_texts(&mut trial, true);
        let reclaimed = [host, trailer]
            .into_iter()
            .flatten()
            .any(|run| folded_duplicates_final(&run, content));
        if reclaimed {
            *entries = trial;
        }
    }
    content.to_string()
}

/// Short single-line title for the inline article. Telegram shows it in the
/// client while the message is being sent, so it must never be the full reply.
fn guest_title(text: &str) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .unwrap_or("Reply");
    let mut title: String = first.chars().take(64).collect();
    if title.is_empty() {
        title.push_str("Reply");
    }
    title
}

/// The `answerGuestQuery` request body: the query id plus one
/// `InlineQueryResultArticle` carrying the reply as its message content.
///
/// Two shapes, chosen by `channels.telegram.rich_messages`:
///
/// - rich: `InputRichMessageContent` with `rich_message.markdown`, carrying the
///   MARKDOWN source (never our Telegram-HTML conversion). The Bot API lists
///   `InputRichMessageContent` among the `InputMessageContent` types a result
///   may carry, its markdown plane is the one that renders headings, lists and
///   native tables, and a rich message takes up to 32768 characters, so a long
///   answer arrives whole instead of being cut at 4096 with `[truncated]` on
///   the end.
/// - plain: `message_text`, unchanged. Telegram caps that at 4096 characters
///   and has no other field to put the overflow in, so the classic path keeps
///   the marker.
pub(crate) fn answer_guest_query_body(query_id: &str, text: &str, rich: bool) -> Value {
    let content = if rich {
        json!({ "rich_message": { "markdown": clamp_guest_rich_text(text) } })
    } else {
        json!({ "message_text": clamp_guest_text(text) })
    };
    json!({
        "guest_query_id": query_id,
        "result": {
            "type": "article",
            "id": GUEST_RESULT_ID,
            "title": guest_title(text),
            "input_message_content": content,
        }
    })
}

/// A reply as it goes on the wire: directives stripped, whitespace trimmed,
/// secrets scrubbed.
///
/// The scrub is not optional here. `redact_secrets_scoped(text, true)` would
/// let credentials through, which is right for the owner's own DM and wrong
/// for a stranger's group, so a guest reply always takes the scrubbing branch.
pub(crate) fn prepare_guest_text(reply: &str) -> String {
    let text = crate::utils::sanitize::strip_llm_artifacts(reply);
    let stripped = strip_directives(&text).trim().to_string();
    crate::utils::redact_secrets_scoped(&stripped, false)
}

/// Approval callback for a guest turn.
///
/// A guest surface has no chat to put an approval button in and the caller is
/// not the machine's owner, so anything that still needs a human is denied.
/// What it must NOT do is deny a call the machine's policy already approved:
/// passing a deny-all callback here also flipped the tool loop's
/// `has_override_approval` flag, which forces `needs_approval` on even under
/// `auto-always`, so every gated tool was denied on every guest turn no matter
/// how the owner had configured the box.
pub(crate) fn guest_approval_callback() -> crate::brain::agent::ApprovalCallback {
    Arc::new(|_info| {
        Box::pin(async {
            match crate::utils::check_approval_policy() {
                Some(verdict) => Ok(verdict),
                None => Ok((false, false)),
            }
        })
    })
}

/// Fold progress events into the turn's state.
///
/// The same mapping a regular Telegram turn uses; only the surface that
/// renders the state differs (a chat message here, an edited guest reply
/// there).
fn guest_progress_cb(state: &Arc<Mutex<StreamingState>>) -> crate::brain::agent::ProgressCallback {
    let st = Arc::clone(state);
    Arc::new(move |_session_id, event| apply_progress_event(&st, event))
}

/// Runs guest turns: gates them, drives one agent turn, answers the query.
pub(crate) struct GuestRunner {
    token: Arc<String>,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    config_rx: tokio::sync::watch::Receiver<Config>,
    /// Shared with the regular chat path. A guest turn claims its session here
    /// before it starts, so a follow-up folds into the running turn instead of
    /// spawning a second one, and it registers its cancel token here so
    /// `/stop` can reach it.
    state: Arc<TelegramState>,
}

/// Drops a guest turn's ownership record when the turn ends, whatever path it
/// ends on (#2143).
///
/// The failed-turn path returns early, so cleanup written at the bottom of
/// `answer` would never run for exactly the turns a caller most wants stopped:
/// the session would stay in `guest_owners` and a later `/stop` would report a
/// cancellation that cancelled nothing.
struct GuestOwnerGuard {
    state: Arc<TelegramState>,
    session_id: Uuid,
}

impl Drop for GuestOwnerGuard {
    fn drop(&mut self) {
        self.state.forget_guest_owner(self.session_id);
    }
}

impl GuestRunner {
    pub(crate) fn new(
        token: Arc<String>,
        agent: Arc<AgentService>,
        session_svc: SessionService,
        config_rx: tokio::sync::watch::Receiver<Config>,
        state: Arc<TelegramState>,
    ) -> Arc<Self> {
        Arc::new(Self {
            token,
            agent,
            session_svc,
            config_rx,
            state,
        })
    }

    /// Hand a guest query to a background task. The poll loop must never wait
    /// on a guest turn: it is a full agent turn and can run for minutes.
    pub(crate) fn spawn(self: &Arc<Self>, query: GuestQuery) {
        let me = Arc::clone(self);
        tokio::spawn(async move { me.answer(query).await });
    }

    /// Session for this guest chat, resolved from the database the way a
    /// regular Telegram chat is (#2159): look up the row by its stable suffix,
    /// create it only when nothing matches. One session per group, shared by
    /// its guests. The suffix is `[guest:<chat>]`, never the owner's
    /// `[chat:<chat>]`, so a guest can never land in the owner's session.
    async fn session_for(&self, query: &GuestQuery) -> Uuid {
        let chat = query.chat_id.unwrap_or(query.user_id);
        let suffix = format!("[guest:{chat}]");
        let base = match (&query.chat_title, query.chat_id) {
            (Some(t), Some(c)) => format!("Guest: {t} ({c})"),
            (Some(t), None) => format!("Guest: {t}"),
            (None, Some(c)) => format!("Guest chat {c}"),
            (None, None) => "Guest chat".to_string(),
        };
        let current_title = format!("{base} {suffix}");
        let session_id = match crate::channels::session_resolve::resolve_or_create_channel_session(
            &self.session_svc,
            &suffix,
            &base,
            &current_title,
            None,
            "Telegram guest",
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                // Not bound to a fresh id silently: the query is dropped with a
                // log line, so the next query retries the lookup.
                tracing::warn!("Telegram guest: session resolve failed: {e:#}");
                return Uuid::new_v4();
            }
        };
        // Guest scope is re-applied on every resolve, so a restart cannot drop it.
        self.agent.mark_guest_session(session_id);
        crate::memory::mark_session_shared(session_id);
        session_id
    }

    async fn answer(self: &Arc<Self>, query: GuestQuery) {
        let cfg = self.config_rx.borrow().clone();
        // One decision for the whole turn: the reply is opened, edited and
        // settled in the same mode, so a rich reply is never half plain.
        let rich = cfg.channels.telegram.rich_messages;
        let chat_key = query.chat_id.map(|c| c.to_string());
        if !cfg
            .channels
            .telegram
            .guest_allowed(&query.user_id.to_string(), chat_key.as_deref())
        {
            tracing::info!(
                "Telegram guest: refusing query {} from user {} ({}) in chat {:?} \
                 (guest_queries={}); owner, admin, or a listing for that chat is required",
                query.query_id,
                query.user_id,
                query.username.as_deref().unwrap_or("unknown"),
                query.chat_id,
                cfg.channels.telegram.guest_queries,
            );
            return;
        }
        if query.text.trim().is_empty() {
            tracing::info!(
                "Telegram guest: query {} from user {} carries no text, nothing to answer \
                 (guest turns are text only)",
                query.query_id,
                query.user_id
            );
            return;
        }
        tracing::info!(
            "Telegram guest: answering query {} from user {} ({}) in chat {:?} (\"{}\")",
            query.query_id,
            query.user_id,
            query.username.as_deref().unwrap_or("unknown"),
            query.chat_id,
            query.chat_title.as_deref().unwrap_or("unknown"),
        );
        let session_id = self.session_for(&query).await;
        // One turn per guest session, through the same gate a regular chat
        // uses (#501). Without it every summon spawned its own task, so a
        // follow-up 27 seconds later started a SECOND agent turn over the same
        // history: two concurrent loops interleaving tool calls, and one reply
        // budget spent twice.
        let _turn_guard = match self.state.try_begin_turn(session_id) {
            Some(guard) => guard,
            None => {
                tracing::info!(
                    "Telegram guest: query {} arrived mid-turn on session {session_id} — folded \
                     into the running turn instead of starting a second one",
                    query.query_id
                );
                let queued = build_midturn_queued_message(None, &query.text, &query.text);
                self.state.enqueue_reaction(session_id, queued);
                // The query id can only be answered once and it expires, so a
                // folded query still gets an answer of its own. One line: the
                // running turn owns the reply that carries the work.
                if let Err(e) = self
                    .post_answer(&answer_guest_query_body(
                        &query.query_id,
                        GUEST_FOLDED_NOTE,
                        rich,
                    ))
                    .await
                {
                    tracing::warn!(
                        "Telegram guest: could not answer folded query {}: {e}",
                        query.query_id
                    );
                }
                return;
            }
        };
        // Never a DM: a guest reply lands in a chat we do not control, so the
        // state is built in the scrubbing scope and both surfaces (the block
        // and the answer) are redacted before they reach the wire.
        let state = Arc::new(Mutex::new(StreamingState::new_turn(
            false,
            self.agent
                .provider_for_session(session_id)
                .cli_handles_tools(),
        )));
        let cancel = CancellationToken::new();
        // Register the token where `/stop` looks. It used to be a local nobody
        // could reach, so a runaway guest turn had no kill switch: `/stop
        // <prefix>` resolves its target from the state's live turns, and this
        // is what puts the guest session in that list.
        self.state
            .store_cancel_token(session_id, cancel.clone())
            .await;
        // And record who started it, so plain `/stop` typed by the same caller
        // in a chat where the bot IS a member reaches this turn. The session
        // itself lives in a chat the bot is not in, so `/stop` there could
        // never resolve to it (#2143).
        self.state.register_guest_owner(session_id, query.user_id);
        let _owner = GuestOwnerGuard {
            state: Arc::clone(&self.state),
            session_id,
        };

        // Open the reply before the turn runs. Guest mode gives the bot one
        // reply per summon and nothing else, so the message has to exist
        // first: an edit can only change a message that is already there, and
        // posting it up front is what turns that single reply into a live
        // progress surface (#2109). A failure here is not fatal: the turn
        // still runs and the answer is posted in one piece at the end.
        let opened = match self
            .post_answer(&answer_guest_query_body(
                &query.query_id,
                GUEST_PLACEHOLDER,
                rich,
            ))
            .await
        {
            Ok(inline_message_id) => Some(inline_message_id),
            Err(e) => {
                tracing::warn!(
                    "Telegram guest: could not open the reply for query {} (the answer will be \
                     posted in one piece at the end): {e}",
                    query.query_id
                );
                None
            }
        };
        let edit_target = opened.as_ref().filter(|id| !id.is_empty()).cloned();

        let edit_handle = edit_target.as_ref().map(|inline_message_id| {
            tokio::spawn(edit_tick_loop(
                Arc::clone(self),
                Arc::clone(&state),
                inline_message_id.clone(),
                cancel.clone(),
                rich,
            ))
        });

        let input = format!(
            "[Channel: Telegram guest query: you were tagged in \"{}\" (chat {}), a chat you \
             are NOT a member of. Your text response is posted there, and while the turn runs \
             the caller sees a status block listing the tools you use; that block is edited in \
             place, so do not narrate your own progress, just do the work. The caller sees no \
             tool output and has no access to that chat's history, so keep the answer short and \
             self-contained. This turn serves someone who is not the owner, so your tools are \
             scoped to {} and a file read stays inside the working directory; do not reach for \
             anything else, it will be refused. Do NOT call telegram_send; this conversation has \
             no chat id.]\n\n{}",
            query.chat_title.as_deref().unwrap_or("a chat"),
            chat_key.as_deref().unwrap_or("unknown"),
            crate::brain::tools::scope::GUEST_ALLOWED_TOOLS.join(", "),
            query.text,
        );
        let mut response = self
            .agent
            .send_message_with_tools_and_display(
                session_id,
                input,
                Some(query.text.clone()),
                None,
                Some(cancel.clone()),
                Some(guest_approval_callback()),
                Some(guest_progress_cb(&state)),
                "telegram",
                None,
                None,
            )
            .await;

        // A follow-up that landed after the loop's last between-round drain has
        // no consumer left, and the regular path flushes it in
        // `flush_queued_after_turn`. The guest equivalent is to answer it in
        // the SAME reply: leaving it queued means the caller's follow-up goes
        // unanswered until they summon the bot again, which is the second
        // request they never asked for.
        for round in 1..=GUEST_FOLD_MAX_ROUNDS {
            if response.is_err() {
                break;
            }
            let Some(extra) = self.state.drain_reaction(session_id) else {
                break;
            };
            tracing::info!(
                "Telegram guest: a follow-up was still queued when session {session_id} finished \
                 its round — continuing the same turn (round {round})"
            );
            response = self
                .agent
                .send_message_with_tools_and_display(
                    session_id,
                    extra.context_text,
                    Some(extra.display_text),
                    None,
                    Some(cancel.clone()),
                    Some(guest_approval_callback()),
                    Some(guest_progress_cb(&state)),
                    "telegram",
                    None,
                    None,
                )
                .await;
        }

        // Read before the tick loop is stopped below: from here on the token is
        // cancelled on every path, so asking later cannot tell a `/stop` from a
        // turn that simply ended.
        let was_cancelled = cancel.is_cancelled();
        // The turn is over: stop ticking before the terminal render, so a live
        // edit can never land after the settled one.
        cancel.cancel();
        if let Some(handle) = edit_handle {
            let _ = handle.await;
        }

        let response = match response {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(
                    "Telegram guest: turn {} for query {} (session {session_id}): {e}",
                    if was_cancelled { "cancelled" } else { "failed" },
                    query.query_id
                );
                // The reply is already in the chat: settle it rather than
                // leaving a stranger watching a live footer, and say which of
                // the two happened instead of reporting a `/stop` as a crash.
                if let Some(inline_message_id) = edit_target.as_deref() {
                    let outcome = if was_cancelled {
                        FlowOutcome::Cancelled
                    } else {
                        FlowOutcome::Failed
                    };
                    let block = settle_block(&state, outcome, None, rich);
                    if let Err(e) = self.edit_answer(inline_message_id, &block, rich).await {
                        tracing::debug!(
                            "Telegram guest: could not settle the failed reply for query {}: {e}",
                            query.query_id
                        );
                    }
                }
                return;
            }
        };

        // The answer also streamed into the flow log as intermediate text, so
        // rendering the settled block as-is would carry it twice: once inside
        // the block and once under it (#2113). Reclaim it the way the regular
        // delivery path does (#300) before the block is rendered.
        //
        // The queue has to be drained FIRST: the answer is still sitting in
        // `display_queue` at this point, so settling `flow_entries` on its own
        // sees an empty list and leaves the copy to be drained into the block
        // by `settle_block` afterwards. That is what put a `<blockquote
        // expandable>` holding the answer above a footer on a turn that ran
        // zero tools, and the answer again under it.
        let content = prepare_guest_text(&response.content);
        let ctx_max = self.agent.context_limit_for_session(session_id);
        let ctx = crate::utils::format_ctx_footer(
            response.context_tokens,
            ctx_max,
            response.tokens_per_second,
        );
        // Quiet ❕ while the context-pressure hint is active, same as the
        // regular delivery path: the footer mirrors the nudge that is in the
        // prompt, so a guest sees the same signal an owner does.
        let ctx = if !ctx.is_empty() && self.agent.pressure_warning_active(session_id) {
            format!("{ctx} ❕")
        } else {
            ctx
        };
        let (message, reply) = settled_guest_message(&state, &content, ctx, rich);
        if reply.is_empty() {
            tracing::warn!(
                "Telegram guest: query {} produced an empty reply, sending the block alone",
                query.query_id
            );
        }
        self.deliver(
            &query.query_id,
            opened.as_deref(),
            edit_target.as_deref(),
            &message,
            &reply,
            rich,
        )
        .await;
    }

    /// Put the finished message in the chat.
    ///
    /// The normal path edits the reply that was opened before the turn. The
    /// fallback posts it in one piece, which is what happens when opening
    /// failed earlier (so the turn still answers) or when the final edit was
    /// refused. When the reply is open but the API never gave an id for it,
    /// there is nothing to edit and nothing to post, so the turn ends there
    /// rather than answering twice.
    async fn deliver(
        &self,
        query_id: &str,
        opened: Option<&str>,
        edit_target: Option<&str>,
        message: &str,
        plain: &str,
        rich: bool,
    ) {
        match edit_target {
            Some(inline_message_id) => {
                // The placeholder already consumed the query id, so
                // `answerGuestQuery` cannot answer it a second time: the answer
                // has to land through an edit. A rate limit therefore waits out
                // the window Telegram named and retries the SAME reply, instead
                // of falling through to a post that is guaranteed to be refused
                // as "query is too old".
                let mut last: Option<String> = None;
                for _ in 0..2 {
                    match self.edit_answer(inline_message_id, message, rich).await {
                        Ok(()) => {
                            tracing::info!(
                                "Telegram guest: answered query {query_id} by editing the open reply"
                            );
                            return;
                        }
                        Err(e @ EditError::RateLimited(_)) => {
                            let waited = super::rate_limit::wait_global_cooldown().await;
                            tracing::warn!(
                                "Telegram guest: final edit for query {query_id} rate-limited \
                                 ({e}); waited {}s and retrying the same reply",
                                waited.as_secs()
                            );
                            last = Some(e.to_string());
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Telegram guest: final edit for query {query_id} failed ({e}); \
                                 retrying once"
                            );
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            last = Some(e.to_string());
                        }
                    }
                }
                tracing::error!(
                    "Telegram guest: could not deliver the answer for query {query_id} into the \
                     open reply ({}); the reply keeps its last live state",
                    last.unwrap_or_default()
                );
            }
            None if opened.is_none() => self.post_once(query_id, plain, rich).await,
            None => tracing::warn!(
                "Telegram guest: query {query_id} has an open reply but no inline_message_id \
                 to edit it by; leaving the reply as posted"
            ),
        }
    }

    /// Post the answer as its own message. Used when the reply could not be
    /// opened, and after a refused final edit.
    async fn post_once(&self, query_id: &str, plain: &str, rich: bool) {
        if plain.is_empty() {
            tracing::warn!("Telegram guest: query {query_id} has no text to post");
            return;
        }
        match self
            .post_answer(&answer_guest_query_body(query_id, plain, rich))
            .await
        {
            Ok(_) => tracing::info!("Telegram guest: answered query {query_id} in one piece"),
            Err(e) => {
                tracing::warn!("Telegram guest: answerGuestQuery failed for query {query_id}: {e}")
            }
        }
    }

    /// POST `answerGuestQuery` through the raw HTTP layer and return the
    /// `inline_message_id` of the message it placed. That id is the handle
    /// every later edit needs, and it is the reason the reply is posted before
    /// the turn instead of after it.
    ///
    /// Neither the method nor `SentGuestMessage` has a typed binding in
    /// teloxide-core 0.13.0, same as the rest of the guest path.
    async fn post_answer(&self, body: &Value) -> Result<String, String> {
        let url = format!(
            "https://api.telegram.org/bot{}/answerGuestQuery",
            self.token
        );
        let resp = reqwest::Client::new()
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if status.is_success() && ok {
            // Absent id is not an error: the reply is in the chat, it just
            // cannot be edited. An empty id says exactly that to the caller.
            return Ok(value
                .get("result")
                .and_then(|r| r.get("inline_message_id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string());
        }
        if status.as_u16() == 429 {
            // Same rule as the edit path: the guest call is raw HTTP, so the
            // cooldown has to be armed here or nothing else in the bot sees it.
            let window =
                super::rate_limit::parse_retry_after(&text).unwrap_or(Duration::from_secs(5));
            super::rate_limit::record_global_429(window);
        }
        Err(format!(
            "status {status}: {}",
            crate::utils::truncate_str(&text, 300)
        ))
    }

    /// Edit the open reply in place. The twin of the regular path's
    /// `edit_rich_markdown` / HTML edit, one API method over.
    ///
    /// Rich first when the reply went out rich: `editMessageText` takes
    /// `rich_message` alongside `inline_message_id`, so the edit carries the
    /// same markdown the reply was opened with. A refusal falls back to the
    /// HTML text edit rather than leaving a stranger staring at the placeholder,
    /// which is the one place the markdown still has to be converted.
    async fn edit_answer(
        &self,
        inline_message_id: &str,
        text: &str,
        rich: bool,
    ) -> Result<(), EditError> {
        if rich {
            match self
                .post_edit(&edit_rich_body(inline_message_id, text))
                .await
            {
                Ok(()) => return Ok(()),
                // A transport failure says nothing about the body: Telegram
                // never judged it. Falling back here moves a healthy rich reply
                // onto the HTML plane, which flattens tables (#679), and a
                // reply already on screen flips to a wall of text because of a
                // dropped socket. So the SAME body goes out once more, and if
                // that also fails the caller decides, never the plane.
                Err(EditError::Transport(e)) => {
                    tracing::warn!(
                        "Telegram guest: rich edit hit a transport error ({e}); retrying the same body"
                    );
                    match self
                        .post_edit(&edit_rich_body(inline_message_id, text))
                        .await
                    {
                        Ok(()) => return Ok(()),
                        Err(EditError::Transport(e2)) => return Err(EditError::Transport(e2)),
                        // A rate limit is not a verdict on the body either, and
                        // the classifier already armed the cooldown: retrying on
                        // the HTML plane now would only spend another call from
                        // the same exhausted budget.
                        Err(e2 @ EditError::RateLimited(_)) => return Err(e2),
                        Err(EditError::Refused(e2)) => tracing::warn!(
                            "Telegram guest: rich edit refused on retry ({e2}); falling back to the HTML edit"
                        ),
                    }
                }
                Err(e @ EditError::RateLimited(_)) => return Err(e),
                Err(EditError::Refused(e)) => tracing::warn!(
                    "Telegram guest: rich edit refused ({e}); falling back to the HTML edit"
                ),
            }
            let html = markdown_to_telegram_html(text);
            return self
                .post_edit(&edit_html_body(inline_message_id, &html))
                .await;
        }
        self.post_edit(&edit_html_body(inline_message_id, text))
            .await
    }

    /// POST one `editMessageText` body and read the response the same way for
    /// both shapes.
    async fn post_edit(&self, body: &Value) -> Result<(), EditError> {
        let url = format!("https://api.telegram.org/bot{}/editMessageText", self.token);
        let resp = reqwest::Client::new()
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| EditError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let outcome = classify_edit_reply(status, &text);
        if matches!(outcome, Err(EditError::RateLimited(_))) {
            // Arm the process-wide cooldown from the window Telegram names, the
            // same deadline every other send path waits out (#814). This path
            // goes over raw HTTP, so without this its 429s were invisible to
            // the rest of the bot and each 1.5s tick kept the penalty alive.
            let window =
                super::rate_limit::parse_retry_after(&text).unwrap_or(Duration::from_secs(5));
            super::rate_limit::record_global_429(window);
        }
        outcome
    }
}

/// Read one `editMessageText` reply and say whether the edit landed.
///
/// Pure so the two failure classes can be told apart in a test: a transport
/// error never reaches here (there is no reply to read), while an answer with
/// a non-2xx status and a rich-shape rejection is a refusal, which is the only
/// case that may move the reply to the HTML plane.
pub(crate) fn classify_edit_reply(status: u16, body: &str) -> Result<(), EditError> {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if edit_response_ok(&value) {
        return Ok(());
    }
    let message = format!("status {status}: {}", crate::utils::truncate_str(body, 300));
    // 429 is a pause, not a verdict on the body: it gets its own class so the
    // rich path never answers it by dropping to the HTML plane, which is what
    // flattened rich guest replies into a wall of text during a rate-limit
    // storm. Arming the cooldown is `post_edit`'s job, so this stays pure.
    if status == 429 {
        return Err(EditError::RateLimited(message));
    }
    Err(EditError::Refused(message))
}

/// Why an `editMessageText` call did not land.
///
/// The two cases are not interchangeable, and the rich path turns on the
/// difference: a refusal means Telegram parsed the body and rejected it, so
/// retrying the same body is pointless and the HTML plane is the way to keep
/// the reply readable. A transport error means the request never got an answer,
/// so the body was never judged and the reply must keep the plane it was
/// opened on.
#[derive(Debug)]
pub(crate) enum EditError {
    /// The request produced no HTTP answer (connect, timeout, TLS, reset).
    Transport(String),
    /// Telegram answered, and the answer was not a success.
    Refused(String),
    /// Telegram answered 429: it asked for a pause, it did not judge the body.
    /// This must never move the reply to the HTML plane: doing that is what
    /// turned rich guest replies into a flat wall of text during a rate-limit
    /// storm, and the window it names is already recorded in the process-wide
    /// cooldown by the classifier.
    RateLimited(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::Transport(e) => write!(f, "transport: {e}"),
            EditError::Refused(e) => write!(f, "refused: {e}"),
            EditError::RateLimited(e) => write!(f, "rate limited: {e}"),
        }
    }
}

/// The `editMessageText` body for a rich guest edit.
///
/// `markdown`, not `html`: the guest reply is composed as markdown on both
/// halves (see [`compose_guest_message`]), and the markdown plane is the one
/// whose server-side render keeps pipe tables native (#679). `InputRichMessage`
/// accepts exactly one of `html`, `markdown` or `blocks`.
pub(crate) fn edit_rich_body(inline_message_id: &str, markdown: &str) -> Value {
    json!({
        "inline_message_id": inline_message_id,
        "rich_message": { "markdown": markdown },
    })
}

/// The `editMessageText` body for a plain guest edit: unchanged, and still the
/// fallback when a rich edit is refused.
pub(crate) fn edit_html_body(inline_message_id: &str, html: &str) -> Value {
    json!({
        "inline_message_id": inline_message_id,
        "text": html,
        "parse_mode": "HTML",
    })
}

/// Whether an `editMessageText` response means the edit landed.
///
/// Writing identical text is not a failure: nothing moved, which is what the
/// caller wanted. Telegram still reports it as an error, so it is read here
/// and treated as success. A false positive needs a response that is not 200
/// and not that exact error string, which the API does not send.
pub(crate) fn edit_response_ok(body: &Value) -> bool {
    if body.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return true;
    }
    body.get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("message is not modified")
}

/// The stripping the regular edit loop runs before a line reaches the log:
/// artifacts off first, then secrets scrubbed, then the channel markers out.
/// The split matters: chrome is our own text, so it only needs the scrub,
/// while model narration also carries artifacts and markers.
///
/// A guest reply lands in a chat we do not control, so every pass runs in the
/// scrubbing scope and an `<<IMG:/Users/...>>` can never show up there as a
/// local path.
pub(crate) fn guest_sanitize(text: &str, system: bool) -> String {
    if system {
        return crate::utils::redact_secrets_scoped(text, false);
    }
    let text = crate::utils::sanitize::strip_llm_artifacts(text);
    let text = crate::utils::redact_secrets_scoped(&text, false);
    let (text, _images) = crate::utils::extract_img_markers(&text);
    let (text, _reaction) = crate::utils::extract_react_marker(&text);
    text
}

/// Render the live reply: drain the queue, advance the clock and the status
/// line, then render exactly what a regular chat renders.
///
/// `rich` picks the plane, same as [`settle_block`]: a live edit on a rich
/// reply carries markdown, so the streamed block renders as the real thing
/// instead of flashing raw tags on the way to the final answer.
fn pump(state: &Arc<Mutex<StreamingState>>, rich: bool) -> String {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    drain_display_queue(&mut s, guest_sanitize);
    let elapsed = s.turn_started_at.elapsed().as_secs();
    // Mirrors `tick_flow_header`: no duration at 0s, then the humanized one.
    s.flow_status = (elapsed > 0).then(|| humanize_duration(elapsed));
    s.header_preview = thinking_status_excerpt(&s.thinking);
    if rich {
        render_flow_rich_state(&s)
    } else {
        render_flow(&s)
    }
}

/// Fold the finished turn's queued display items into the log, then take the
/// answer back out of it, returning the text the reply carries under the block.
///
/// The order is the whole point (#2113). The answer reaches the log as a queued
/// `DisplayItem::Intermediate`; settling `flow_entries` before that queue is
/// drained sees an empty log, does nothing, and leaves the copy to be drained
/// into the block afterwards by [`settle_block`]. A turn that ran no tools then
/// renders the answer as the block's log body: a `<blockquote expandable>` that
/// exists only because the answer is inside it, with the answer repeated under
/// the footer. Draining first puts the copy where the reclaim can find it, and a
/// zero-tool turn renders exactly what a regular turn renders, a footer and the
/// answer and no expandable at all.
pub(crate) fn settle_reply(state: &Arc<Mutex<StreamingState>>, content: &str) -> String {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    drain_display_queue(&mut s, guest_sanitize);
    let options_pending = s.pending_suggestions.is_some();
    settle_guest_answer(&mut s.flow_entries, content, options_pending)
}

/// Settle the turn's block and render it. `ctx` is display-only chrome set at
/// settle time, matching the regular delivery path.
///
/// `rich` picks the plane the block is rendered on, because the guest reply
/// carries the block and the answer in one message: markdown for a rich reply
/// (the plane that renders headings and native tables), Telegram HTML for the
/// classic one.
fn settle_block(
    state: &Arc<Mutex<StreamingState>>,
    outcome: FlowOutcome,
    ctx: Option<String>,
    rich: bool,
) -> String {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    drain_display_queue(&mut s, guest_sanitize);
    s.flow_outcome = Some(outcome);
    s.tools_started_at = None;
    if ctx.is_some() {
        s.sections.ctx = ctx;
    }
    if rich {
        render_flow_rich_state(&s)
    } else {
        render_flow(&s)
    }
}

/// The finished guest reply: the turn's block, when it has one, with the answer
/// under it, plus the text the reply is meant to carry on its own.
///
/// This is the whole settle step in one place, because the order inside it is
/// the bug it fixes (#2113) and a caller that repeats the steps can repeat them
/// wrong. Two rules, both about landing on what a regular chat shows:
///
/// - The answer leaves the log before the block renders. It reached the log as
///   a queued intermediate, so the queue is drained first; settling the entries
///   before that sees nothing to reclaim and ships the answer twice.
/// - A turn with no log gets no block. A regular turn opens no block when no
///   tool ran and nothing was narrated, so the answer is the whole message; a
///   guest reply cannot skip opening one (that is the only handle the Bot API
///   gives it), so the block is dropped here instead of shipping a footer over
///   nothing.
pub(crate) fn settled_guest_message(
    state: &Arc<Mutex<StreamingState>>,
    content: &str,
    ctx: String,
    rich: bool,
) -> (String, String) {
    let reply = settle_reply(state, content);
    let has_log = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        !s.flow_entries.is_empty()
    };
    let block = if has_log {
        settle_block(state, FlowOutcome::Finished, Some(ctx), rich)
    } else {
        String::new()
    };
    (compose_guest_message(&block, &reply, rich), reply)
}

/// Edit the open reply on the regular cadence until the turn ends.
async fn edit_tick_loop(
    me: Arc<GuestRunner>,
    state: Arc<Mutex<StreamingState>>,
    inline_message_id: String,
    cancel: CancellationToken,
    rich: bool,
) {
    let mut last = String::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(GUEST_EDIT_INTERVAL) => {}
        }
        // A live edit is cosmetic. During a 429 cooldown it is dropped rather
        // than attempted: every rejected call risks extending the window
        // (#814), and the settled render at the end still carries the answer.
        if super::rate_limit::is_global_cooldown_active() {
            continue;
        }
        let body = pump(&state, rich);
        if body.is_empty() || body == last {
            continue;
        }
        match me.edit_answer(&inline_message_id, &body, rich).await {
            Ok(()) => last = body,
            // A live edit is cosmetic: a rate limit or a dropped socket must
            // not end the turn, so it is logged and the next tick renders the
            // newer state anyway.
            Err(e) => tracing::debug!("Telegram guest: live edit skipped: {e}"),
        }
    }
}
