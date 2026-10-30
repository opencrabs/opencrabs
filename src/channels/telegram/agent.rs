//! Telegram Agent
//!
//! Agent struct and startup logic.

use super::TelegramState;
use super::handler::{handle_message, handle_reaction};
use crate::brain::agent::AgentService;
use crate::config::Config;
use crate::db::ChannelMessageRepository;
use crate::db::SessionBindingRepository;
use crate::services::{ServiceContext, SessionService};
use crate::utils::retry::{RetryConfig, retry_with_check};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::MessageId;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Telegram bot that forwards messages to the agent
pub struct TelegramAgent {
    agent_service: Arc<AgentService>,
    session_service: SessionService,
    /// Shared session ID from the TUI — owner user shares the terminal session
    shared_session_id: Arc<Mutex<Option<Uuid>>>,
    telegram_state: Arc<TelegramState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
    session_binding_repo: SessionBindingRepository,
}

impl TelegramAgent {
    pub fn new(
        agent_service: Arc<AgentService>,
        service_context: ServiceContext,
        shared_session_id: Arc<Mutex<Option<Uuid>>>,
        telegram_state: Arc<TelegramState>,
        config_rx: tokio::sync::watch::Receiver<Config>,
        channel_msg_repo: ChannelMessageRepository,
        session_binding_repo: SessionBindingRepository,
    ) -> Self {
        Self {
            agent_service,
            session_service: SessionService::new(service_context),
            shared_session_id,
            telegram_state,
            config_rx,
            channel_msg_repo,
            session_binding_repo,
        }
    }

    /// Start the bot as a background task. Returns a JoinHandle.
    pub fn start(self, token: String) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // Validate token format BEFORE creating Bot: "numbers:alphanumeric"
            // e.g., "123456789:ABCdefGHIjklMNOpqrsTUVwxyz"
            if token.is_empty() {
                tracing::debug!("Telegram bot token is empty, skipping bot start");
                return;
            }

            if !token.contains(':') {
                tracing::debug!("Telegram bot token missing ':' separator, skipping bot start");
                return;
            }

            let parts: Vec<&str> = token.splitn(2, ':').collect();
            if parts.len() != 2 {
                tracing::debug!("Telegram bot token has invalid format, skipping bot start");
                return;
            }

            // First part must be numeric (bot ID)
            if parts[0].parse::<u64>().is_err() {
                tracing::debug!("Telegram bot token has invalid bot ID, skipping bot start");
                return;
            }

            // Second part must be at least 30 chars (API key)
            if parts[1].len() < 30 {
                tracing::debug!("Telegram bot token has too short API key, skipping bot start");
                return;
            }

            // Read initial config for logging
            let cfg = self.config_rx.borrow().clone();
            tracing::info!(
                "Starting Telegram bot with {} allowed user(s), STT={}, TTS={}",
                cfg.channels.telegram.allowed_users.len(),
                cfg.voice_config().stt_enabled,
                cfg.voice_config().tts_enabled,
            );

            let bot = Bot::new(token.clone());
            // Kept for the raw-aware update listener (`token` itself is moved
            // into the shared deps as `bot_token` further down).
            let listener_token = token.clone();

            // Verify token works with Telegram API before setting up dispatcher
            // #1785: retry on transient network/timeout errors; InvalidToken is
            // permanent so we skip retrying that case.
            match retry_with_check(
                || async { bot.get_me().await },
                &RetryConfig::api_aggressive(),
                |e: &teloxide::RequestError| {
                    !matches!(
                        e,
                        teloxide::RequestError::Api(api)
                            if api == &teloxide::ApiError::InvalidToken
                    )
                },
            )
            .await
            {
                Ok(me) => {
                    if let Some(ref username) = me.username {
                        tracing::info!("Telegram: bot username is @{}", username);
                        self.telegram_state.set_bot_username(username.clone()).await;
                    }
                    // Store bot's numeric user ID for reply-to-bot detection
                    self.telegram_state
                        .set_bot_user_id(me.user.id.0 as i64)
                        .await;
                    tracing::info!("Telegram: bot user ID is {}", me.user.id.0);
                    // Store bot in state for proactive messaging only after successful auth
                    self.telegram_state.set_bot(bot.clone()).await;

                    // Register slash commands so they appear in Telegram's / menu
                    register_bot_commands(&bot).await;

                    // #1317: seed the published-skills signature so the first
                    // post-boot message does not redundantly re-publish the
                    // menus registered just above.
                    self.telegram_state
                        .set_menu_skills_sig(super::menu_refresh::skills_signature())
                        .await;

                    // One-time: organize any pre-subdir flat attachments under
                    // channel_attachments/telegram/ (#513). Idempotent.
                    super::media::migrate_flat_channel_attachments();
                    me
                }
                Err(e) => {
                    tracing::warn!("Telegram: token validation failed: {}. Bot not started.", e);
                    return;
                }
            };

            let agent = self.agent_service.clone();
            let session_svc = self.session_service.clone();
            let bot_token = Arc::new(token);
            let shared_session = self.shared_session_id.clone();
            let telegram_state = self.telegram_state.clone();
            let config_rx = self.config_rx.clone();
            let channel_msg_repo = self.channel_msg_repo.clone();
            let session_binding_repo = self.session_binding_repo.clone();

            // Shared dependencies handed to every agent dispatch (first-frame,
            // settled-after-streaming, or immediate). `bot_token` and
            // `channel_msg_repo` are only needed here, so move them in; the
            // rest are cloned because the callback handler below reuses them.
            let deps = DispatchDeps {
                agent: agent.clone(),
                session_svc: session_svc.clone(),
                bot_token,
                shared_session: shared_session.clone(),
                telegram_state: telegram_state.clone(),
                config_rx: config_rx.clone(),
                channel_msg_repo,
                session_binding_repo,
            };

            // Pending edit-stream buffer keyed by (chat, message). Peer bots in
            // a group stream their replies by editing one message progressively
            // (the same mechanism we use). Reacting to the first frame makes us
            // read a half-written message and wrongly conclude it was "cut off".
            // We hold a bot's text message until its edit stream settles, then
            // process the FINAL text. See `spawn_settle_watcher`.
            let pending_edits: PendingEdits = Arc::new(Mutex::new(HashMap::new()));

            // ── Message handler ───────────────────────────────────────────────
            let msg_handler = Update::filter_message().endpoint({
                let deps = deps.clone();
                let pending_edits = pending_edits.clone();
                move |bot: Bot, msg: Message| {
                    let deps = deps.clone();
                    let pending_edits = pending_edits.clone();
                    async move {
                        // A peer bot's first frame: defer until the stream
                        // settles instead of processing the partial.
                        if is_stream_candidate(&msg) {
                            let key = (msg.chat.id, msg.id);
                            let generation = {
                                let mut map = pending_edits.lock().await;
                                let entry = map.entry(key).or_insert((0, msg.clone()));
                                entry.0 += 1;
                                entry.1 = msg.clone();
                                entry.0
                            };
                            spawn_settle_watcher(key, generation, pending_edits, bot, deps);
                            return ResponseResult::Ok(());
                        }
                        // Everything else (humans, DMs, non-text) processes
                        // immediately, in the background so the dispatcher stays
                        // free for callback queries while the agent runs.
                        spawn_handle_message(bot, msg, deps);
                        ResponseResult::Ok(())
                    }
                }
            });

            // ── Edited-message handler ────────────────────────────────────────
            // Receives the progressive edits of a peer bot's streamed reply.
            // While the message is still pending we reset its settle timer to
            // the newest frame; once it has already been processed we only
            // reconcile stored history so context reflects the final text.
            let edited_handler = Update::filter_edited_message().endpoint({
                let deps = deps.clone();
                let pending_edits = pending_edits.clone();
                move |bot: Bot, msg: Message| {
                    let deps = deps.clone();
                    let pending_edits = pending_edits.clone();
                    async move {
                        if !is_stream_candidate(&msg) {
                            return ResponseResult::Ok(());
                        }
                        let key = (msg.chat.id, msg.id);
                        let generation = {
                            let mut map = pending_edits.lock().await;
                            map.get_mut(&key).map(|entry| {
                                entry.0 += 1;
                                entry.1 = msg.clone();
                                entry.0
                            })
                        };
                        match generation {
                            // Still streaming — push the settle deadline out.
                            Some(generation) => {
                                spawn_settle_watcher(key, generation, pending_edits, bot, deps);
                            }
                            // Edit landed after we processed the settled message
                            // (e.g. a manual late edit). Rewrite stored history;
                            // don't reprocess — that would loop on every edit.
                            None => {
                                if let Some(text) = msg.text() {
                                    let chat_id = msg.chat.id.0.to_string();
                                    let pmid = msg.id.0.to_string();
                                    if let Err(e) = deps
                                        .channel_msg_repo
                                        .update_content("telegram", &chat_id, &pmid, text)
                                        .await
                                    {
                                        tracing::warn!(
                                            "Telegram: failed to reconcile edited message \
                                             {pmid} content: {e}"
                                        );
                                    }
                                }
                            }
                        }
                        ResponseResult::Ok(())
                    }
                }
            });

            // ── Callback query handler (for Approve / Deny inline buttons) ────
            let cb_handler = Update::filter_callback_query().endpoint({
                let telegram_state = telegram_state.clone();
                let agent = agent.clone();
                let session_svc = session_svc.clone();
                let shared_session = shared_session.clone();
                let config_rx = config_rx.clone();
                move |bot: Bot, query: CallbackQuery| {
                    let state = telegram_state.clone();
                    let agent = agent.clone();
                    let session_svc = session_svc.clone();
                    let shared_session = shared_session.clone();
                    let config_rx = config_rx.clone();
                    async move {
                        if let Some(data) = query.data.as_deref() {
                            tracing::info!("Telegram callback query received: data={}", data);

                            // Stale keyboard from before the follow-up tools
                            // were merged into suggest_options (#1178 M7): the
                            // deleted question tool emitted `q:{id}:{idx}`
                            // buttons. Ignore them explicitly and loudly — a
                            // silent fall-through would route the tap into the
                            // generic unknown-callback path (echoed to the
                            // agent as user text), which reads as a bug.
                            if data.starts_with("q:") {
                                tracing::warn!("stale keyboard, ignored: {data}");
                                crate::channels::telegram::keyboards::ack_callback(
                                    &bot,
                                    &query,
                                    "This keyboard is outdated.",
                                )
                                .await;
                                return ResponseResult::Ok(());
                            }

                            // Optional follow-up suggestion tapped (#597): inject
                            // the chosen suggestion as the user's next message (a
                            // fresh turn). Non-blocking — the whole set is consumed.
                            if let Some(rest) = data.strip_prefix(
                                crate::channels::telegram::suggest_options::FOLLOWUP_PREFIX,
                            ) {
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "followup").await;
                                tracing::info!("Telegram followup tap: rest={rest}");
                                let parsed = rest.rsplit_once(':').and_then(|(tok, i)| {
                                    Some((tok.to_string(), i.parse::<usize>().ok()?))
                                });
                                let taken = match parsed {
                                    Some((cb_token, idx)) => {
                                        let t = state.take_pending_followup(&cb_token, idx).await;
                                        if t.is_none() {
                                            tracing::warn!(
                                                "Telegram followup tap: unknown/expired \
                                                 keyboard token {cb_token} idx {idx} \
                                                 (consumed or superseded — #1217 guard)"
                                            );
                                            // Stale shell (#1226): the picker behind this
                                            // keyboard was consumed or lost to a deploy
                                            // or to a #597 clear. Strip the dead keyboard
                                            // so the bubble stops silently eating taps.
                                            // #59: the strip is HOST-AWARE — the #597
                                            // clear rescues the merged-host record, so the
                                            // shape is known: rich hosts carry the buttons
                                            // INSIDE the body (a markup strip there is a
                                            // guaranteed "message is not modified" no-op —
                                            // the zombie); classic/unknown hosts carry a
                                            // reply-markup.
                                            if let Some(msg) = query
                                                .message
                                                .as_ref()
                                                .and_then(|m| m.regular_message())
                                            {
                                                let stale_host =
                                                    state.peek_stale_host(&cb_token).await;
                                                let strip = match &stale_host {
                                                    Some(h) if h.rich => {
                                                        // Rich host: rewrite the body without
                                                        // the button rows; no reply-markup
                                                        // ever existed on this bubble.
                                                        // Markdown-plane hosts (#79 piece 4)
                                                        // ride the markdown strip source so
                                                        // tables survive the strip (#679).
                                                        if let Some(md) = &h.markdown {
                                                            let body =
                                                                super::suggest_options::strip_button_rows(md);
                                                            // Fence-safe (#98): the strip redraw
                                                            // must not re-soup a delivered diagram.
                                                            super::suggest_options::edit_rich_md_fencesafe(
                                                                bot.api_url().as_str(),
                                                                bot.token(),
                                                                msg.chat.id.0,
                                                                msg.id.0,
                                                                &body,
                                                                None,
                                                                "stale-strip",
                                                                "#59 stale rich strip",
                                                            )
                                                            .await
                                                        } else {
                                                            let body = super::suggest_options::
                                                                strip_button_rows(&h.html);
                                                            super::rich::api::edit_rich_html(
                                                                bot.api_url().as_str(),
                                                                bot.token(),
                                                                msg.chat.id.0,
                                                                msg.id.0,
                                                                &body,
                                                                None,
                                                                "stale-strip",
                                                                "#59 stale rich strip",
                                                            )
                                                            .await
                                                            .map_err(|e| e.to_string())
                                                        }
                                                    }
                                                    _ => {
                                                        // Classic/unknown: markup strip.
                                                        // Unknown = pre-#59 record (or none):
                                                        // keep the #1226 blind strip as the
                                                        // last resort — it is the correct
                                                        // move whenever markup exists.
                                                        bot.edit_message_reply_markup(
                                                                msg.chat.id, msg.id)
                                                            .reply_markup(
                                                                super::suggest_options::
                                                                    empty_keyboard(),
                                                            )
                                                            .await
                                                            .map(|_| ())
                                                            .map_err(|e| e.to_string())
                                                    }
                                                };
                                                match strip {
                                                    Ok(_) => {
                                                        state.forget_stale_host(&cb_token).await;
                                                        // #1226: the strip used to ride bare
                                                        // rich_edit telemetry with nothing
                                                        // naming it — log the outcome so a
                                                        // stale-shell tap is re-derivable.
                                                        tracing::info!(
                                                            "Telegram followup tap: stripped \
                                                             dead keyboard from msg {} \
                                                             (expired token {cb_token}, \
                                                             #59 host-matched={}, #1226)",
                                                            msg.id,
                                                            stale_host.is_some()
                                                        );
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(
                                                            "Telegram followup tap: stale \
                                                             keyboard strip failed: {e}"
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        // (session, chosen text, merged host). The host is
                                        // Some when the keyboard was merged onto the answer
                                        // bubble (#tg-suggest-merge) — the pick record must
                                        // then PRESERVE that answer text instead of
                                        // replacing it.
                                        // (session, text, host): the session
                                        // comes from the stash entry, never
                                        // from client callback data (#1217)
                                        t
                                    }
                                    None => {
                                        tracing::warn!(
                                            "Telegram followup tap: unparseable callback data \
                                             '{rest}'"
                                        );
                                        None
                                    }
                                };
                                if let Some((entry, text, picked_idx)) = taken {
                                    let sid = entry.session_id;
                                    let merged_host = entry.host.clone();
                                    let (chat_id, thread_id, prompt_msg_id) = query
                                        .message
                                        .as_ref()
                                        .map(|m| {
                                            (
                                                m.chat().id,
                                                m.regular_message().and_then(|r| r.thread_id),
                                                Some(m.id()),
                                            )
                                        })
                                        .unwrap_or((teloxide::types::ChatId(0), None, None));
                                    // Who tapped, so the record names them rather
                                    // than reading as anonymous bot text in a
                                    // group (#893). The Bot API cannot post AS a
                                    // user, but the callback carries the tapper's
                                    // identity and it was simply discarded.
                                    let chooser = {
                                        let u = &query.from;
                                        let name = match &u.last_name {
                                            Some(last) => format!("{} {last}", u.first_name),
                                            None => u.first_name.clone(),
                                        };
                                        let name = name.trim().to_string();
                                        if name.is_empty() {
                                            u.username.clone()
                                        } else {
                                            Some(name)
                                        }
                                    };
                                    let agent_clone = agent.clone();
                                    let bot_clone = bot.clone();
                                    let state_clone = state.clone();
                                    tokio::spawn(async move {
                                        // Record the pick ON the suggestion block
                                        // rather than posting a new message.
                                        //
                                        // The Bot API has no send-as-user, so any
                                        // fresh message carries the bot's name,
                                        // avatar and badge: a continuation the USER
                                        // chose renders as the bot saying it. #787
                                        // tried to fix that with a `>` quote, but
                                        // quote formatting sits inside a bubble
                                        // that is still labelled as the bot, so the
                                        // attribution never changed (#844).
                                        //
                                        // Editing reads as a selected control, and
                                        // drops the keyboard at the moment it stops
                                        // working: take_pending_followup consumes
                                        // the whole set, so the other buttons are
                                        // already dead after one tap.
                                        // #96: the pick record is built per plane —
                                        // md-plane hosts append it as plain markdown,
                                        // html hosts as html (see pick_rewrite).
                                        let pick_line = crate::channels::telegram::
                                            suggest_options::picked_block(
                                            &text,
                                            chooser.as_deref(),
                                        );
                                        let picked = crate::channels::telegram::markdown::md_to_html(
                                            &pick_line,
                                        );
                                        let picked_md = pick_line;
                                        let mut echo_deferred = false;
                                        let recorded = match prompt_msg_id {
                                            Some(mid) => {
                                                // Merged keyboard (#tg-suggest-merge): the
                                                // buttons live ON the answer bubble, so a
                                                // whole-text replace would ERASE the answer.
                                                // When this bubble is the recorded host,
                                                // keep its HTML and append the pick record
                                                // instead — and strip the now-dead controls.
                                                // Rich hosts ride edit_rich_html; classic
                                                // hosts ride teloxide's edit_message_text.
                                                let host_info =
                                                    merged_host.as_ref().and_then(|h| {
                                                        (h.message_id == mid).then(|| {
                                                            (
                                                                h.html.clone(),
                                                                h.rich,
                                                                h.markdown.clone(),
                                                            )
                                                        })
                                                    });
                                                let empty_kb = teloxide::types::
                                                    InlineKeyboardMarkup::new(
                                                        Vec::<Vec<teloxide::types::InlineKeyboardButton>>::new(),
                                                    );
                                                // #39: the pick record is baked into the body
                                                // BEFORE any transport arm runs — one format
                                                // site, so no arm can drop the choice again
                                                // (the classic merged host used to edit the
                                                // answer HTML alone and lose the record).
                                                let rewrite =
                                                    super::suggest_options::pick_rewrite(
                                                        host_info.as_ref().map(|(full, rich, md)| {
                                                            (full.as_str(), *rich, md.as_deref())
                                                        }),
                                                        &picked,
                                                        &picked_md,
                                                        picked_idx,
                                                    );
                                                let outcome: Result<(), String> = match rewrite.clone() {
                                                    super::suggest_options::PickRewrite::RichMarkdownHost(
                                                        body,
                                                    ) => super::suggest_options::edit_rich_md_fencesafe(
                                                        bot_clone.api_url().as_str(),
                                                        bot_clone.token(),
                                                        chat_id.0,
                                                        mid.0,
                                                        &body,
                                                        Some(&serde_json::json!(empty_kb)),
                                                        "turn",
                                                        "-",
                                                    )
                                                    .await,
                                                    super::suggest_options::PickRewrite::RichHost(
                                                        body,
                                                    ) => super::rich::api::edit_rich_html(
                                                        bot_clone.api_url().as_str(),
                                                        bot_clone.token(),
                                                        chat_id.0,
                                                        mid.0,
                                                        &body,
                                                        Some(&serde_json::json!(empty_kb)),
                                                        "turn",
                                                        "-",
                                                    )
                                                    .await
                                                    .map(|_| ())
                                                    .map_err(|e| e.to_string()),
                                                    super::suggest_options::PickRewrite::ClassicHost(
                                                        body,
                                                    ) => bot_clone
                                                        .edit_message_text(chat_id, mid, &body)
                                                        .parse_mode(teloxide::types::ParseMode::Html)
                                                        .reply_markup(empty_kb.clone())
                                                        .await
                                                        .map(|_| ())
                                                        .map_err(|e| e.to_string()),
                                                    super::suggest_options::PickRewrite::Standalone(
                                                        body,
                                                    ) => bot_clone
                                                        .edit_message_text(chat_id, mid, &body)
                                                        .parse_mode(teloxide::types::ParseMode::Html)
                                                        .await
                                                        .map(|_| ())
                                                        .map_err(|e| e.to_string()),
                                                };
                                                if let Err(e) = outcome {
                                                    match super::edit_retry::classify_str(&e) {
                                                        super::edit_retry::EditErr::RetryAfter(wait) => {
                                                            tracing::warn!(
                                                                "Telegram followup tap: pick-record edit \
                                                                 429 (retry after {wait:?}) — deferring one \
                                                                 identical retry"
                                                            );
                                                            let bot_r = bot_clone.clone();
                                                            let bot_e = bot_clone.clone();
                                                            let retry = (
                                                                chat_id,
                                                                mid,
                                                                rewrite.clone(),
                                                                empty_kb.clone(),
                                                            );
                                                            let echo_text = text.clone();
                                                            let echo_chooser = chooser.clone();
                                                            super::edit_retry::spawn_deferred(
                                                                wait,
                                                                move || async move {
                                                                    refire_pick_edit(&bot_r, retry)
                                                                        .await
                                                                },
                                                                move || async move {
                                                                    // Contract point 2 (#76): the
                                                                    // pick-record edit failed TWICE —
                                                                    // the bubble never showed the pick
                                                                    // (#71 lesson: telemetry ≠ visual).
                                                                    // The echo below is the only record.
                                                                    tracing::warn!(
                                                                        "Telegram followup tap: pick \
                                                                         REDRAW_FAILED on msg {} in chat \
                                                                         {} — second 429; choice registered \
                                                                         via echo fallback only",
                                                                        mid, chat_id
                                                                    );
                                                                    // The legacy quoted echo — now
                                                                    // safely outside the 429 window
                                                                    // that killed attempt 1 (#68).
                                                                    let echo = crate::channels::telegram::
                                                                        markdown::md_to_html(
                                                                            &crate::channels::telegram::
                                                                                suggest_options::echo_fallback(
                                                                                    &echo_text,
                                                                                    echo_chooser.as_deref(),
                                                                                ),
                                                                        );
                                                                    if let Err(e) =
                                                                        crate::channels::telegram::send::
                                                                            message_in_thread(
                                                                                &bot_e, chat_id, thread_id,
                                                                                echo,
                                                                            )
                                                                            .parse_mode(teloxide::types::
                                                                                ParseMode::Html)
                                                                            .await
                                                                    {
                                                                        tracing::warn!(
                                                                            "Telegram followup tap: echo \
                                                                             fallback also failed: {e}"
                                                                        );
                                                                    }
                                                                },
                                                            );
                                                            echo_deferred = true;
                                                        }
                                                        super::edit_retry::EditErr::Fatal(_) => {
                                                            tracing::warn!(
                                                                "Telegram followup tap: could not edit the \
                                                                 suggestion block ({e}) — falling back to \
                                                                 a quoted echo"
                                                            );
                                                        }
                                                    }
                                                    false
                                                } else {
                                                    // #1226: the pick-record edit also strips
                                                    // the keyboard — name it so teardown is
                                                    // visible in logs, not just failure.
                                                    tracing::info!(
                                                        "Telegram followup tap: pick recorded \
                                                         on msg {mid}, keyboard stripped from markup"
                                                    );
                                                    true
                                                }
                                            }
                                            None => false,
                                        };
                                        if !recorded && !echo_deferred {
                                            // The block is gone or too old to edit.
                                            // A quoted echo is worse attribution
                                            // but better than losing the record of
                                            // what was chosen.
                                            let echo =
                                                crate::channels::telegram::markdown::md_to_html(
                                                    &crate::channels::telegram::suggest_options::
                                                        echo_fallback(&text, chooser.as_deref()),
                                                );
                                            if let Err(e) =
                                                crate::channels::telegram::send::message_in_thread(
                                                    &bot_clone, chat_id, thread_id, echo,
                                                )
                                                .parse_mode(teloxide::types::ParseMode::Html)
                                                .await
                                            {
                                                tracing::warn!(
                                                    "Telegram followup tap: echo fallback also \
                                                     failed: {e}"
                                                );
                                            }
                                        }
                                        // #869: route through the full streaming pipeline
                                        // (typing indicator, streaming edits, plan card
                                        // refresh, flow chrome, deliver_final_response)
                                        // instead of the stripped-down run_resume_turn.
                                        let _guard =
                                            match state_clone.try_begin_turn(sid) {
                                                Some(g) => g,
                                                None => {
                                                    // #136: the session is already mid-turn.
                                                    // The host bubble was redrawn above with the pick and
                                                    // buttons stripped, so enqueue the choice into the reaction queue.
                                                    // It will be drained either at the next tool round boundary
                                                    // via reaction_queue_callback or on turn end via flush_queued_after_turn.
                                                    tracing::info!(
                                                        "Telegram followup tap: session {sid} \
                                                         mid-turn — enqueuing choice for delivery (#136)"
                                                    );
                                                    let queued =
                                                        crate::channels::telegram::handler::build_midturn_queued_message(
                                                            None,
                                                            &text,
                                                            &text,
                                                        );
                                                    state_clone.enqueue_reaction(sid, queued);
                                                    return;
                                                }
                                            };
                                        if let Err(e) =
                                            crate::channels::telegram::resume::resume_session_inner(
                                                bot_clone,
                                                chat_id,
                                                thread_id,
                                                sid,
                                                text,
                                                agent_clone,
                                                state_clone,
                                                false, // user button tap, not a push wake (#12)
                                            )
                                            .await
                                        {
                                            tracing::warn!(
                                                "Telegram followup tap: resume_session \
                                                 failed for session {sid}: {e}"
                                            );
                                        }
                                    });
                                }
                                return Ok::<(), teloxide::RequestError>(());
                            }

                            // Setup callback for unconfigured providers — show the
                            // help text from `unconfigured_provider_help` instead
                            // of trying to switch (no API key would just fail).
                            // Bots cannot delete user messages in DMs, so we
                            // never prompt for the key inline (issue #126, B.1).
                            if let Some(provider_name) = data.strip_prefix("setup:") {
                                let help = crate::channels::commands::unconfigured_provider_help(provider_name);
                                if let Some(msg_ref) = query.message.as_ref() {
                                    let chat_id = msg_ref.chat().id;
                                    let thread_id = msg_ref.regular_message().and_then(|m| m.thread_id);
                                    crate::channels::telegram::send::best_effort_note(
                                        &bot,
                                        chat_id,
                                        thread_id,
                                        &crate::channels::telegram::markdown::md_to_html(&help),
                                        Some(teloxide::types::ParseMode::Html),
                                        "system",
                                        "model_switch_help",
                                        "unconfigured provider help",
                                    )
                                    .await;
                                }
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "model switch").await;
                                return Ok::<(), teloxide::RequestError>(());
                            }

                            // Provider picker callback → show models for that provider
                            // Page navigation on the model picker. Without this the
                            // arrows drew but did nothing, which is the same dead-end the
                            // picker itself was in before it paged at all.
                            if data == "noop" {
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "page indicator").await;
                                return ResponseResult::Ok(());
                            }
                            if let Some((page_num, provider_name, filter)) =
                                crate::channels::telegram::picker_limits::parse_nav_callback(data)
                            {
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "model page").await;
                                let resp = crate::channels::commands::models_for_provider(&provider_name).await;
                                if let Some(msg) = &query.message {
                                    use teloxide::types::InlineKeyboardMarkup;
                                    use crate::channels::telegram::picker_limits as picker;
                                    let page = picker::page_of(&resp.models, page_num, filter.as_deref());
                                    let keyboard = InlineKeyboardMarkup::new(picker::page_keyboard(
                                        &resp.provider_name,
                                        &resp.current_model,
                                        &page,
                                    ));
                                    let text = crate::channels::telegram::markdown::md_to_html(
                                        &picker::page_text(
                                            &resp.provider_name,
                                            &resp.current_model,
                                            resp.models.len(),
                                            &page,
                                        ),
                                    );
                                    super::edit_retry::edit_text_ui(
                                        bot.clone(),
                                        msg.chat().id,
                                        msg.id(),
                                        text,
                                        true,
                                        Some(keyboard),
                                        "model page update",
                                    );
                                }
                                return ResponseResult::Ok(());
                            }

                            if let Some(provider_name) = data.strip_prefix("provider:") {
                                let resp = crate::channels::commands::models_for_provider(provider_name).await;

                                // Agent-handled providers (OpenRouter 300+ models, custom)
                                // Switch to default if set, then let the agent follow up.
                                if resp.agent_handled {
                                    // Resolve session from the chat where the button was pressed,
                                    // not from shared_session (which is the TUI session).
                                    let session_id = resolve_callback_session(&query, &state, &shared_session).await;
                                    let display = crate::channels::commands::provider_display_name(provider_name);
                                    // Switch to this provider with its default model. Pin the
                                    // provider to THIS session so another channel/session
                                    // doesn't get yanked onto it — the model callback's
                                    // switch_model then reads from the same per-session slot.
                                    let config = crate::config::Config::current();
                                    if let Ok(new_provider) = crate::brain::provider::factory::create_provider_by_name(&config, provider_name).await
                                    {
                                        match session_id {
                                            Some(sid) => agent.swap_provider_for_session(sid, new_provider.clone(), new_provider.default_model().to_string()),
                                            None => agent.swap_provider(new_provider),
                                        }
                                    }
                                    if !resp.current_model.is_empty()
                                        && let Err(e) = crate::channels::commands::switch_model(
                                            &agent,
                                            &resp.current_model,
                                            session_id,
                                            Some(provider_name),
                                        )
                                        .await
                                    {
                                        tracing::warn!(error = %e, "failed to switch model in Telegram handler");
                                    }
                                    crate::channels::telegram::keyboards::ack_callback(&bot, &query, "model switch").await;
                                    // Send synthetic message to agent so it handles follow-up
                                    let prompt = if resp.current_model.is_empty() {
                                        format!(
                                            "[System: User selected {} provider but no default model is set. \
                                             Ask them which model they want. Use config_manager tool to read \
                                             providers section, then set the default_model. Keep current provider \
                                             until a model is chosen.]",
                                            display
                                        )
                                    } else {
                                        format!(
                                            "[System: User switched to {} provider with model {}. \
                                             Confirm the switch. Ask if they want a different model — \
                                             if so, use config_manager to update providers.{}.default_model \
                                             and confirm.]",
                                            display, resp.current_model,
                                            if provider_name == "openrouter" { "openrouter" } else { provider_name }
                                        )
                                    };
                                    if let Some(sid) = session_id {
                                        let agent_clone = agent.clone();
                                        let bot_clone = bot.clone();
                                        let (chat_id, thread_id) = query
                                            .message
                                            .as_ref()
                                            .map(|m| {
                                                (
                                                    m.chat().id,
                                                    m.regular_message().and_then(|r| r.thread_id),
                                                )
                                            })
                                            .unwrap_or((teloxide::types::ChatId(0), None));
                                        tokio::spawn(async move {
                                            match agent_clone.send_message(sid, prompt, None).await {
                                                Ok(resp) => {
                                                    let clean = crate::utils::sanitize::sanitize_outbound(&resp.content);
                                                    let html = crate::channels::telegram::markdown::md_to_html(&clean);
                                                    crate::channels::telegram::send::best_effort_note(
                                                        &bot_clone, chat_id, thread_id, &html,
                                                        Some(teloxide::types::ParseMode::Html),
                                                        "system",
                                                        "agent_followup",
                                                        "agent follow-up reply",
                                                    )
                                                    .await;
                                                }
                                                Err(e) => {
                                                    tracing::error!("Agent follow-up failed: {}", e);
                                                }
                                            }
                                        });
                                    }
                                    return ResponseResult::Ok(());
                                }

                                if resp.models.is_empty() {
                                    if let Err(e) = bot
                                        .answer_callback_query(query.id.clone())
                                        .text("No models available for this provider")
                                        .await
                                    {
                                        tracing::warn!("Telegram: callback UI update failed: {e}");
                                    }
                                    return ResponseResult::Ok(());
                                }
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "callback").await;
                                if let Some(msg) = &query.message {
                                    use teloxide::payloads::EditMessageTextSetters;
                                    use teloxide::prelude::Requester;
                                    use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
                                    // OpenRouter carries several hundred models. Rendering
                                    // one button each, with the same list enumerated in the
                                    // text above it, pushed the message past Telegram's 4096
                                    // characters: editMessageText answered MESSAGE_TOO_LONG,
                                    // the failure was a log line, and the picker did nothing
                                    // at all from the user's side. Page it instead of capping,
                                    // so the rest of the catalogue stays reachable.
                                    use crate::channels::telegram::picker_limits as picker;
                                    let page = picker::page_of(&resp.models, 0, None);
                                    let rows: Vec<Vec<InlineKeyboardButton>> =
                                        picker::page_keyboard(
                                            &resp.provider_name,
                                            &resp.current_model,
                                            &page,
                                        );
                                    let keyboard = InlineKeyboardMarkup::new(rows);
                                    let text = crate::channels::telegram::markdown::md_to_html(
                                        &picker::page_text(
                                            &resp.provider_name,
                                            &resp.current_model,
                                            resp.models.len(),
                                            &page,
                                        ),
                                    );
                                    let chat = msg.chat().id;
                                    let mid = msg.id();
                                    match bot
                                        .edit_message_text(chat, mid, &text)
                                        .parse_mode(teloxide::types::ParseMode::Html)
                                        .reply_markup(keyboard.clone())
                                        .await
                                    {
                                        Ok(_) => {}
                                        Err(e) => match super::edit_retry::classify(&e) {
                                            super::edit_retry::EditErr::RetryAfter(wait) => {
                                                // Deferred identical retry first (#68);
                                                // the legacy fallback runs only on
                                                // exhaustion — safely outside the 429
                                                // window that killed the old immediate
                                                // fallback.
                                                super::edit_retry::spawn_deferred(
                                                    wait,
                                                    {
                                                        let bot = bot.clone();
                                                        let text = text.clone();
                                                        let keyboard = keyboard.clone();
                                                        move || async move {
                                                            bot.edit_message_text(chat, mid, &text)
                                                                .parse_mode(
                                                                    teloxide::types::ParseMode::Html,
                                                                )
                                                                .reply_markup(keyboard)
                                                                .await
                                                                .map(|_| ())
                                                        }
                                                    },
                                                    {
                                                        let bot = bot.clone();
                                                        let provider = resp.provider_name.clone();
                                                        move || async move {
                                                            // Say something. This failing to a
                                                            // log line is why the picker looked
                                                            // like it was still loading forever.
                                                            let fallback = format!(
                                                                "Could not show the model list for {provider}: still rate-limited after a deferred retry\n\nSet one directly with /models <name>."
                                                            );
                                                            if let Err(e2) = bot
                                                                .edit_message_text(chat, mid, fallback)
                                                                .await
                                                            {
                                                                tracing::warn!(
                                                                    "Telegram: could not report the picker failure either: {e2}"
                                                                );
                                                            }
                                                        }
                                                    },
                                                );
                                            }
                                            super::edit_retry::EditErr::Fatal(e) => {
                                                tracing::warn!(
                                                    "Telegram: callback UI update failed: {e}"
                                                );
                                                // Say something. This failing to a log line is why the
                                                // picker looked like it was still loading forever.
                                                let fallback = format!(
                                                    "Could not show the model list for {}: {e}\n\nSet one                                              directly with /models <name>.",
                                                    resp.provider_name
                                                );
                                                if let Err(e2) = bot
                                                    .edit_message_text(chat, mid, fallback)
                                                    .await
                                                {
                                                    tracing::warn!(
                                                        "Telegram: could not report the picker failure either: {e2}"
                                                    );
                                                }
                                            }
                                        },
                                    }
                                }
                                return ResponseResult::Ok(());
                            }

                            // Model switch callback (format: model:<provider>|<model>).
                            // Pipe — not colon — between provider and model so a
                            // custom-provider name (`custom:dialagram`) and an
                            // OpenRouter-style model suffix (`:free`, `:thinking`)
                            // don't fold into each other on parse.
                            // Apply-to-all-sessions (#468): bulk-write the pair
                            // the user just switched to. The current session is
                            // already switched; this covers the rest, which pick
                            // it up on their next message.
                            // Apply-to-all, step one (#2042): ask first. Shows how many
                            // sessions would change, with Confirm and Cancel. Nothing is
                            // written by this tap.
                            if let Some(rest) = data.strip_prefix("allq:") {
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "apply-all").await;
                                if let (Some((prov, model)), Some(msg)) = (rest.split_once('|'), &query.message) {
                                    let session_svc = crate::services::SessionService::new(
                                        agent.context().clone(),
                                    );
                                    let (text, markup) = match session_svc
                                        .count_provider_model_all_sessions(prov, model)
                                        .await
                                    {
                                        Ok(n) => {
                                            let text = crate::channels::commands::apply_all_prompt_text(prov, model, n);
                                            let mut rows = Vec::new();
                                            if n > 0
                                                && let Some(confirm) = crate::channels::commands::apply_all_confirm_data(prov, model)
                                            {
                                                rows.push(vec![teloxide::types::InlineKeyboardButton::callback("Confirm", confirm)]);
                                            }
                                            rows.push(vec![teloxide::types::InlineKeyboardButton::callback("Cancel", "allx")]);
                                            (text, teloxide::types::InlineKeyboardMarkup::new(rows))
                                        }
                                        Err(e) => {
                                            tracing::warn!("apply-all count failed: {e}");
                                            (format!("⚠️ Could not count sessions: {e}"), teloxide::types::InlineKeyboardMarkup::default())
                                        }
                                    };
                                    super::edit_retry::edit_text_ui(
                                        bot.clone(),
                                        msg.chat().id,
                                        msg.id(),
                                        text,
                                        false,
                                        Some(markup),
                                        "apply-all prompt",
                                    );
                                }
                                return ResponseResult::Ok(());
                            }
                            // Cancel on the apply-all prompt: nothing was written (#2042).
                            if data == "allx" {
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "apply-all").await;
                                if let Some(msg) = &query.message {
                                    super::edit_retry::edit_text_ui(
                                        bot.clone(),
                                        msg.chat().id,
                                        msg.id(),
                                        "Not applied. Sessions keep their models.".to_string(),
                                        false,
                                        Some(teloxide::types::InlineKeyboardMarkup::default()),
                                        "apply-all cancel",
                                    );
                                }
                                return ResponseResult::Ok(());
                            }
                            // Apply-to-all, step two: Confirm. The only tap that writes.
                            if let Some(rest) = data.strip_prefix("allm:") {
                                let reply = if let Some((prov, model)) = rest.split_once('|') {
                                    let session_svc = crate::services::SessionService::new(
                                        agent.context().clone(),
                                    );
                                    match session_svc
                                        .set_provider_model_all_sessions(prov, model)
                                        .await
                                    {
                                        Ok(n) => format!(
                                            "✅ {prov}/{model} applied to {n} other session(s); \
                                             each picks it up on its next message."
                                        ),
                                        Err(e) => format!("⚠️ Scope-all write failed: {e}"),
                                    }
                                } else {
                                    "⚠️ Malformed apply-all payload.".to_string()
                                };
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "apply-all").await;
                                if let Some(msg) = &query.message {
                                    use teloxide::prelude::Requester;
                                    if let Err(e) = bot.send_message(msg.chat().id, reply).await {
                                        tracing::warn!(
                                            target: "telegram::send",
                                            chat_id = msg.chat().id.0,
                                            error = %e,
                                            "apply-all reply send failed"
                                        );
                                    }
                                }
                                return ResponseResult::Ok(());
                            }
                            if let Some(rest) = data.strip_prefix("model:") {
                                let (provider_name, model_token) = if let Some((p, m)) = rest.split_once('|') {
                                    (Some(p), m)
                                } else {
                                    (None, rest)
                                };
                                // An index-encoded button (model:<provider>|#<i>) — the
                                // generator falls back to this when the literal model name
                                // would overflow Telegram's 64-byte callback_data. Resolve
                                // it back to the name via the same config list. Plain names
                                // (short models, other callers) pass through unchanged.
                                let resolved_owned: Option<String> =
                                    match (provider_name, model_token.strip_prefix('#')) {
                                        (Some(pname), Some(idx)) => idx
                                            .parse::<usize>()
                                            .ok()
                                            .and_then(|i| {
                                                crate::channels::commands::model_at_index(pname, i)
                                            }),
                                        _ => None,
                                    };
                                let model_name: &str =
                                    resolved_owned.as_deref().unwrap_or(model_token);
                                // Resolve session BEFORE the provider swap so the swap
                                // lands on the right per-session slot. Leaving the
                                // resolve below the swap (as it was) made the provider
                                // change visible to other sessions via the global slot
                                // until the per-session pin from switch_model landed.
                                let session_id = resolve_callback_session(&query, &state, &shared_session).await;
                                // Switch provider if specified and different
                                let mut provider_err: Option<String> = None;
                                if let Some(pname) = provider_name {
                                    match crate::config::Config::load() {
                                        Ok(config) => match crate::brain::provider::factory::create_provider_by_name(&config, pname).await {
                                            Ok(new_provider) => match session_id {
                                                Some(sid) => agent.swap_provider_for_session(sid, new_provider.clone(), new_provider.default_model().to_string()),
                                                None => agent.swap_provider(new_provider),
                                            },
                                            Err(e) => provider_err = Some(format!("Failed to create provider '{}': {}", pname, e)),
                                        },
                                        Err(e) => provider_err = Some(format!("Failed to load config: {}", e)),
                                    }
                                }
                                let (switch_ok, display_text) = if let Some(err) = provider_err {
                                    (false, format!("⚠️ {}", err))
                                } else {
                                    match crate::channels::commands::switch_model(&agent, model_name, session_id, provider_name).await {
                                        Ok(_) => (true, format!("✅ Model switched to <code>{}</code>", model_name)),
                                        Err(e) => (false, format!("⚠️ {}", e)),
                                    }
                                };
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "approval").await;
                                // #1149: offer Apply-to-all-sessions right where the user
                                // just switched via buttons — the textual /models <p m>
                                // path already does this (#468). Same central 64-byte guard;
                                // a picker tap can never come from an `all`-scoped command,
                                // so no extra scoping check applies here.
                                let mut switch_markup = teloxide::types::InlineKeyboardMarkup::default();
                                if switch_ok
                                    && let Some(pname) = provider_name
                                    && let Some(data) = crate::channels::commands::apply_all_callback_data(pname, model_name)
                                {
                                    switch_markup = teloxide::types::InlineKeyboardMarkup::new(vec![vec![
                                        teloxide::types::InlineKeyboardButton::callback(
                                            "Apply to all sessions",
                                            data,
                                        ),
                                    ]]);
                                }
                                if let Some(msg) = &query.message {
                                    super::edit_retry::edit_text_ui(
                                        bot.clone(),
                                        msg.chat().id,
                                        msg.id(),
                                        display_text.clone(),
                                        true,
                                        Some(switch_markup),
                                        "callback UI update",
                                    );
                                }
                                if !switch_ok {
                                    tracing::warn!("Telegram model switch failed: {}", display_text);
                                }
                                return ResponseResult::Ok(());
                            }

                            // Session switch callback
                            if let Some(session_id_str) = data.strip_prefix("session:") {
                                if let Ok(new_id) = session_id_str.parse::<Uuid>() {
                                    // Determine if caller is owner
                                    let cfg = config_rx.borrow().clone();
                                    let caller_id_raw = query.from.id.0;
                                    let is_owner = cfg.channels.telegram.is_owner(&caller_id_raw.to_string());

                                    if is_owner {
                                        *shared_session.lock().await = Some(new_id);
                                    }
                                    // Owner and guest: bind chat_id → session_id for
                                    // handle_message (issue #121). Guest extra_sessions
                                    // map was removed — it was never read on ingest.
                                    let switch_chat_id = query
                                        .message
                                        .as_ref()
                                        .map(|m| m.chat().id.0)
                                        .unwrap_or(caller_id_raw as i64);
                                    // Scope the binding to the forum topic the switch
                                    // happened in, so the next message in that topic
                                    // resolves to the switched session (#215).
                                    let switch_topic_id =
                                        query.message.as_ref().and_then(|m| m.regular_message()).and_then(|m| {
                                            super::session_resolve::topic_session_id(
                                                m.is_topic_message,
                                                m.thread_id.map(|t| t.0.0),
                                            )
                                        });
                                    state
                                        .register_session_chat(new_id, switch_chat_id, switch_topic_id)
                                        .await;

                                    // Touch updated_at so find_session_by_title_suffix returns this session on next message
                                    if let Ok(Some(s)) = session_svc.get_session(new_id).await
                                        && let Err(e) = session_svc.update_session(&s).await
                                    {
                                        tracing::warn!(error = %e, "failed to update Telegram session");
                                    }

                                    if let Err(e) = bot
                                        .answer_callback_query(query.id.clone())
                                        .text("Session switched")
                                        .await
                                    {
                                        tracing::warn!("Telegram: callback UI update failed: {e}");
                                    }
                                    if let Some(msg) = &query.message {
                                        let display = match session_svc.get_session(new_id).await {
                                            Ok(Some(s)) => s.title.unwrap_or_else(|| session_id_str[..8.min(session_id_str.len())].to_string()),
                                            _ => session_id_str[..8.min(session_id_str.len())].to_string(),
                                        };
                                        let switched =
                                            format!("✅ Switched to session <code>{}</code>", display);
                                        super::edit_retry::edit_text_ui(
                                            bot.clone(),
                                            msg.chat().id,
                                            msg.id(),
                                            switched,
                                            true,
                                            Some(teloxide::types::InlineKeyboardMarkup::default()),
                                            "callback UI update",
                                        );
                                    }
                                } else {
                                    if let Err(e) = bot
                                        .answer_callback_query(query.id.clone())
                                        .text("Invalid session ID")
                                        .await
                                    {
                                        tracing::warn!("Telegram: callback UI update failed: {e}");
                                    }
                                }
                                return ResponseResult::Ok(());
                            }

// Directory browser callbacks: cd:sel:{idx}, cd:up, cd:pg:{n}, cd:here, cd:noop
                            if data.starts_with("cd:") {
                                // Owner-only: the browser exposes the host
                                // filesystem. Even though /cd is owner-gated, the
                                // inline keyboard sits in the chat where any
                                // allowlisted user could tap it, so re-check the
                                // tapper here.
                                let caller_is_owner = config_rx
                                    .borrow()
                                    .channels
                                    .telegram
                                    .is_owner(&query.from.id.0.to_string());
                                if !caller_is_owner {
                                    if let Err(e) = bot
                                        .answer_callback_query(query.id.clone())
                                        .text("🔒 Owner only")
                                        .show_alert(true)
                                        .await
                                    {
                                        tracing::warn!("Telegram: callback UI update failed: {e}");
                                    }
                                    return ResponseResult::Ok(());
                                }
                                let chat_id = query.message.as_ref().map(|m| m.chat().id.0).unwrap_or(0);
                                // Answer callback query — deferred to each branch
                                // to allow custom text popups (cd:sel on files).
                                let topic_id = query.message.as_ref()
                                    .and_then(|m| m.regular_message())
                                    .and_then(|r| r.thread_id)
                                    .map(|t| t.0.0);

                                // Handle cd:noop (page indicator, no action)
                                if data == "cd:noop" {
                                    crate::channels::telegram::keyboards::ack_callback(&bot, &query, "cd noop").await;
                                    return ResponseResult::Ok(());
                                }

                                // Get current browser state
                                let browser_state = state.get_dir_browser(chat_id, topic_id).await;
                                let (current_path, current_filter) = browser_state.unwrap_or_else(|| {
                                    let home = dirs::home_dir()
                                        .map(|p| p.to_string_lossy().to_string())
                                        .unwrap_or_else(|| "/".to_string());
                                    (home, None)
                                });

                                let new_state: Option<crate::channels::commands::DirBrowserResponse> = if data == "cd:up" {
                                    crate::channels::telegram::keyboards::ack_callback(&bot, &query, "cd up").await;
                                    // Navigate to parent
                                    let parent = std::path::PathBuf::from(&current_path)
                                        .parent()
                                        .map(|p| p.to_string_lossy().to_string())
                                        .unwrap_or_else(|| "/".to_string());
                                    Some(crate::channels::commands::rebuild_cd_browser(&parent, 0, current_filter.as_deref()))
                                } else if data == "cd:here" {
                                    crate::channels::telegram::keyboards::ack_callback(&bot, &query, "cd here").await;
                                    // Confirm directory — set as working dir
                                    let session_id = resolve_callback_session(&query, &state, &shared_session).await;
                                    if let Some(sid) = session_id {
                                        // Move THIS chat's own cwd handle (#703). Writing the
                                        // global instead left the chat that ran /cd on its old
                                        // directory — its handle already existed — while every
                                        // other chat and the TUI silently moved.
                                        agent.set_session_only_working_directory(
                                            sid,
                                            std::path::PathBuf::from(&current_path),
                                        );
                                        // Persist to session DB
                                        let svc = crate::services::SessionService::new(agent.context().clone());
                                        if let Err(e) = svc.update_session_working_directory(sid, Some(current_path.clone())).await {
                                            tracing::warn!(error = %e, "failed to update session working directory");
                                        }
                                    }
                                    state.clear_dir_browser(chat_id, topic_id).await;
                                    // Edit the message to confirm
                                    if let Some(msg) = &query.message {
                                        let confirm_text = format!("✅ Working directory set to:\n<code>{}</code>", current_path);
                                        super::edit_retry::edit_text_ui(
                                            bot.clone(),
                                            msg.chat().id,
                                            msg.id(),
                                            confirm_text,
                                            true,
                                            Some(teloxide::types::InlineKeyboardMarkup::default()),
                                            "cd:here",
                                        );
                                    }
                                    return ResponseResult::Ok(());
                                } else if let Some(idx_str) = data.strip_prefix("cd:sel:") {
                                    // Select entry by index from full listing
                                    let idx: usize = idx_str.parse().unwrap_or(0);
                                    let all_entries = crate::channels::commands::read_dir_entries(
                                        &std::path::PathBuf::from(&current_path),
                                        current_filter.as_deref(),
                                    ).0;
                                    if let Some(entry) = all_entries.get(idx) {
                                        if entry.is_dir {
                                            crate::channels::telegram::keyboards::ack_callback(&bot, &query, "cd dir").await;
                                            // Navigate into directory
                                            let new_path = std::path::PathBuf::from(&current_path)
                                                .join(&entry.name)
                                                .to_string_lossy()
                                                .to_string();
                                            Some(crate::channels::commands::rebuild_cd_browser(&new_path, 0, current_filter.as_deref()))
                                        } else {
                                            // File selected — just show info, stay in same dir
                                            let file_path = std::path::PathBuf::from(&current_path)
                                                .join(&entry.name);
                                            // Answer with a popup showing the file path
                                            if let Err(e) = bot
                                                .answer_callback_query(query.id.clone())
                                                .text(format!("📄 {}", file_path.display()))
                                                .show_alert(true)
                                                .await
                                            {
                                                tracing::warn!(
                                                    target: "telegram::send",
                                                    query_id = %query.id,
                                                    error = %e,
                                                    "file popup ack failed"
                                                );
                                            }
                                            Some(crate::channels::commands::rebuild_cd_browser(&current_path, 0, current_filter.as_deref()))
                                        }
                                    } else {
                                        None
                                    }
                                } else if let Some(pg_str) = data.strip_prefix("cd:pg:") {
                                    crate::channels::telegram::keyboards::ack_callback(&bot, &query, "cd page").await;
                                    // Page navigation
                                    let page: usize = pg_str.parse().unwrap_or(0);
                                    Some(crate::channels::commands::rebuild_cd_browser(&current_path, page, current_filter.as_deref()))
                                } else {
                                    crate::channels::telegram::keyboards::ack_callback(&bot, &query, "cd fallback").await;
                                    None
                                };

                                if let Some(resp) = new_state {
                                    state.set_dir_browser(chat_id, topic_id, resp.current_path.clone(), resp.filter.clone()).await;
                                    let rows = crate::channels::telegram::handler::build_cd_keyboard(&resp);
                                    let keyboard = teloxide::types::InlineKeyboardMarkup::new(rows);
                                    if let Some(msg) = &query.message {
                                        let html = crate::channels::telegram::markdown::md_to_html(&resp.text);
                                        super::edit_retry::edit_text_ui(
                                            bot.clone(),
                                            msg.chat().id,
                                            msg.id(),
                                            html,
                                            true,
                                            Some(keyboard),
                                            "cd:navigate",
                                        );
                                    }
                                }
                                return ResponseResult::Ok(());
                            }

                            // Profile manager callbacks: prof:sel:{name}, prof:create,
                            // prof:migrate:{name}, prof:del:{name}, prof:confirm_migrate:{name},
                            // prof:confirm_del:{name}, prof:back
                            if data.starts_with("prof:") {
                                use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
                                let chat_id = query.message.as_ref().map(|m| m.chat().id.0).unwrap_or(0);
                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "profile picker").await;

                                if let Some(name) = data.strip_prefix("prof:sel:") {
                                    // Show profile detail view with action buttons
                                    let profiles = crate::config::profile::list_profiles().unwrap_or_default();
                                    let active = crate::config::profile::active_profile().unwrap_or("default");
                                    if let Some(entry) = profiles.iter().find(|p| p.name == name) {
                                        let is_active = entry.name == active;
                                        let mut text = format!("👤 *Profile:* `{}`\n", entry.name);
                                        if let Some(desc) = &entry.description {
                                            text.push_str(&format!("📝 {}\n", desc));
                                        }
                                        text.push_str(&format!("📅 Created: {}\n", entry.created_at));
                                        if let Some(used) = &entry.last_used {
                                            text.push_str(&format!("⏰ Last used: {}\n", used));
                                        }
                                        if is_active {
                                            text.push_str("\n✅ *This is the active profile*");
                                        }

                                        let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
                                        if !is_active {
                                            rows.push(vec![InlineKeyboardButton::callback(
                                                "🔄 Migrate from current",
                                                format!("prof:migrate:{}", entry.name),
                                            )]);
                                        }
                                        if entry.name != "default" && !is_active {
                                            rows.push(vec![InlineKeyboardButton::callback(
                                                "🗑️ Delete",
                                                format!("prof:del:{}", entry.name),
                                            )]);
                                        }
                                        rows.push(vec![InlineKeyboardButton::callback(
                                            "◀️ Back to Profiles",
                                            "prof:back".to_string(),
                                        )]);

                                        let keyboard = InlineKeyboardMarkup::new(rows);
                                        if let Some(msg) = &query.message {
                                            let html = crate::channels::telegram::markdown::md_to_html(&text);
                                            super::edit_retry::edit_text_ui(
                                                bot.clone(),
                                                msg.chat().id,
                                                msg.id(),
                                                html,
                                                true,
                                                Some(keyboard),
                                                "prof:sel",
                                            );
                                        }
                                    }
                                    return ResponseResult::Ok(());
                                }

                                if data == "prof:create" {
                                    // Prompt user for new profile name
                                    // Store the create state for this chat
                                    state.set_prof_create(chat_id, true).await;
                                    if let Some(msg) = &query.message {
                                        use teloxide::payloads::SendMessageSetters;
                                        use teloxide::prelude::Requester;
                                        let prompt = "✏️ Send me the name for the new profile:\n\n\
                                            (Letters, numbers, hyphens, underscores. 1-64 chars.)";
                                        let sent = bot
                                            .send_message(msg.chat().id, prompt)
                                            .reply_markup(InlineKeyboardMarkup::new(vec![vec![
                                                InlineKeyboardButton::callback("❌ Cancel", "prof:back"),
                                            ]]))
                                            .await;
                                        if let Err(e) = sent {
                                            tracing::warn!("prof:create: failed to send prompt: {}", e);
                                        }
                                    }
                                    return ResponseResult::Ok(());
                                }

                                if let Some(name) = data.strip_prefix("prof:migrate:") {
                                    // Show migration confirmation
                                    let active = crate::config::profile::active_profile().unwrap_or("default");
                                    let text = format!(
                                        "🔄 *Migrate Profile*\n\n\
                                        This will copy all brain files, config, keys, and memory from \
                                        `{}` → `{}`.\n\n\
                                        Existing files in `{}` will be overwritten (force=true).\n\n\
                                        Confirm?",
                                        active, name, name
                                    );
                                    let keyboard = InlineKeyboardMarkup::new(vec![
                                        vec![InlineKeyboardButton::callback(
                                            "✅ Confirm migration",
                                            format!("prof:confirm_migrate:{}", name),
                                        )],
                                        vec![InlineKeyboardButton::callback(
                                            "❌ Cancel",
                                            format!("prof:sel:{}", name),
                                        )],
                                    ]);
                                    if let Some(msg) = &query.message {
                                        let html = crate::channels::telegram::markdown::md_to_html(&text);
                                        super::edit_retry::edit_text_ui(
                                            bot.clone(),
                                            msg.chat().id,
                                            msg.id(),
                                            html,
                                            true,
                                            Some(keyboard),
                                            "prof:migrate",
                                        );
                                    }
                                    return ResponseResult::Ok(());
                                }

                                if let Some(name) = data.strip_prefix("prof:confirm_migrate:") {
                                    let active = crate::config::profile::active_profile().unwrap_or("default");
                                    match crate::config::profile::migrate_profile(active, name, true) {
                                        Ok(files) => {
                                            let text = format!(
                                                "✅ Migration complete!\n\n\
                                                Copied {} files from `{}` → `{}`.\n\n\
                                                Restart with `-p {}` to switch profiles.",
                                                files.len(), active, name, name
                                            );
                                            if let Some(msg) = &query.message {
                                                let html = crate::channels::telegram::markdown::md_to_html(&text);
                                                super::edit_retry::edit_text_ui(
                                                    bot.clone(),
                                                    msg.chat().id,
                                                    msg.id(),
                                                    html,
                                                    true,
                                                    Some(InlineKeyboardMarkup::default()),
                                                    "callback UI update",
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            let text = format!("❌ Migration failed: {}", e);
                                            if let Some(msg) = &query.message {
                                                super::edit_retry::edit_text_ui(
                                                    bot.clone(),
                                                    msg.chat().id,
                                                    msg.id(),
                                                    text,
                                                    false,
                                                    Some(InlineKeyboardMarkup::default()),
                                                    "prof:confirm_migrate",
                                                );
                                            }
                                        }
                                    }
                                    return ResponseResult::Ok(());
                                }

                                if let Some(name) = data.strip_prefix("prof:del:") {
                                    let text = format!(
                                        "🗑️ *Delete Profile*\n\n\
                                        Are you sure you want to delete `{}`?\n\
                                        This removes all files in the profile directory.\n\n\
                                        This cannot be undone.",
                                        name
                                    );
                                    let keyboard = InlineKeyboardMarkup::new(vec![
                                        vec![InlineKeyboardButton::callback(
                                            "✅ Confirm delete",
                                            format!("prof:confirm_del:{}", name),
                                        )],
                                        vec![InlineKeyboardButton::callback(
                                            "❌ Cancel",
                                            "prof:back".to_string(),
                                        )],
                                    ]);
                                    if let Some(msg) = &query.message {
                                        let html = crate::channels::telegram::markdown::md_to_html(&text);
                                        super::edit_retry::edit_text_ui(
                                            bot.clone(),
                                            msg.chat().id,
                                            msg.id(),
                                            html,
                                            true,
                                            Some(keyboard),
                                            "prof:del",
                                        );
                                    }
                                    return ResponseResult::Ok(());
                                }

                                if let Some(name) = data.strip_prefix("prof:confirm_del:") {
                                    match crate::config::profile::delete_profile(name) {
                                        Ok(()) => {
                                            let text = format!("✅ Profile `{}` deleted.", name);
                                            if let Some(msg) = &query.message {
                                                let html = crate::channels::telegram::markdown::md_to_html(&text);
                                                super::edit_retry::edit_text_ui(
                                                    bot.clone(),
                                                    msg.chat().id,
                                                    msg.id(),
                                                    html,
                                                    true,
                                                    Some(InlineKeyboardMarkup::default()),
                                                    "callback UI update",
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            let text = format!("❌ Delete failed: {}", e);
                                            if let Some(msg) = &query.message {
                                                super::edit_retry::edit_text_ui(
                                                    bot.clone(),
                                                    msg.chat().id,
                                                    msg.id(),
                                                    text,
                                                    false,
                                                    Some(InlineKeyboardMarkup::default()),
                                                    "callback UI update",
                                                );
                                            }
                                        }
                                    }
                                    return ResponseResult::Ok(());
                                }

                                if data == "prof:back" {
                                    // Return to profiles list
                                    let resp = crate::channels::commands::format_profiles_browser().await;
                                    let rows = crate::channels::telegram::handler::build_profiles_keyboard(&resp);
                                    let keyboard = InlineKeyboardMarkup::new(rows);
                                    if let Some(msg) = &query.message {
                                        let html = crate::channels::telegram::markdown::md_to_html(&resp.text);
                                        super::edit_retry::edit_text_ui(
                                            bot.clone(),
                                            msg.chat().id,
                                            msg.id(),
                                            html,
                                            true,
                                            Some(keyboard),
                                            "prof:back",
                                        );
                                    }
                                    return ResponseResult::Ok(());
                                }

                                crate::channels::telegram::keyboards::ack_callback(&bot, &query, "callback").await;
                                return ResponseResult::Ok(());
                            }

                            // Plan Approve/Discard buttons (`plan:` prefix,
                            // deliberately distinct from tool-approval
                            // `approve:{id}`). Owner-only, same spirit as
                            // sensitive tool approval: the keyboard sits in
                            // the chat where any allowlisted member could tap
                            // it, so the tapper is re-checked here. Approve is
                            // FORBIDDEN while a turn runs (refuse, never
                            // queue); Discard cancels the turn first.
                            if data == "plan:ok" || data == "plan:no" {
                                let caller_is_owner = config_rx
                                    .borrow()
                                    .channels
                                    .telegram
                                    .is_owner(&query.from.id.0.to_string());
                                if !caller_is_owner {
                                    if let Err(e) = bot
                                        .answer_callback_query(query.id.clone())
                                        .text("🔒 Owner only")
                                        .show_alert(true)
                                        .await
                                    {
                                        tracing::warn!("Telegram: callback UI update failed: {e}");
                                    }
                                    return ResponseResult::Ok(());
                                }
                                let Some(session_id) =
                                    resolve_callback_session(&query, &state, &shared_session).await
                                else {
                                    if let Err(e) = bot
                                        .answer_callback_query(query.id.clone())
                                        .text("No session for this chat.")
                                        .await
                                    {
                                        tracing::warn!("Telegram: callback UI update failed: {e}");
                                    }
                                    return ResponseResult::Ok(());
                                };
                                let (chat_id, thread_id) = match query.message.as_ref() {
                                    Some(m) => (
                                        m.chat().id,
                                        m.regular_message().and_then(|r| r.thread_id),
                                    ),
                                    None => {
                                        crate::channels::telegram::keyboards::ack_callback(&bot, &query, "callback").await;
                                        return ResponseResult::Ok(());
                                    }
                                };
                                // Used buttons disappear: clear the markup on
                                // the message that carried them (the flow tick
                                // re-attaches when the state still wants one).
                                let kb_msg_id = query.message.as_ref().map(|m| m.id());

                                if data == "plan:no" {
                                    let cancelled = state.cancel_session(session_id).await;
                                    let mut reply =
                                        crate::utils::plan_mode::discard(session_id, agent.context())
                                            .await;
                                    if cancelled {
                                        reply =
                                            format!("⏹️ Cancelled the running turn. {reply}");
                                    }
                                    if let Err(e) = bot
                                        .answer_callback_query(query.id.clone())
                                        .text("Plan discarded")
                                        .await
                                    {
                                        tracing::warn!("Telegram: callback UI update failed: {e}");
                                    }
                                    // The Approve/Discard keyboard rides the plan
                                    // card (#580); discard removes the whole card.
                                    crate::channels::telegram::plan_card::remove_plan_card(
                                        &bot,
                                        chat_id,
                                        &state,
                                        session_id,
                                    )
                                    .await;
                                    crate::channels::telegram::send::best_effort_note(
                                        &bot, chat_id, thread_id, &reply,
                                        None,
                                        "system",
                                        "agent_plan_reply",
                                        "plan callback reply",
                                    )
                                    .await;
                                    return ResponseResult::Ok(());
                                }

                                // plan:ok — Approve / seed retry.
                                if state.is_turn_active(session_id) {
                                    if let Err(e) = bot
                                        .answer_callback_query(query.id.clone())
                                        .text(
                                            "⛔ A turn is running. Approve is refused while \
                                             busy; try again when it finishes.",
                                        )
                                        .show_alert(true)
                                        .await
                                    {
                                        tracing::warn!("Telegram: callback UI update failed: {e}");
                                    }
                                    return ResponseResult::Ok(());
                                }
                                match crate::utils::plan_mode::try_approve(
                                    session_id,
                                    crate::tui::plan::ApprovalSource::User,
                                )
                                .await
                                {
                                    crate::utils::plan_mode::ApproveOutcome::Refused(msg) => {
                                        let _ =
                                            bot.answer_callback_query(query.id.clone()).await;
                                        let _ =
                                            crate::channels::telegram::send::message_in_thread(
                                                &bot, chat_id, thread_id, msg,
                                            )
                                            .await;
                                    }
                                    crate::utils::plan_mode::ApproveOutcome::SeedTurn {
                                        prompt,
                                    } => {
                                        if let Err(e) = bot
                                            .answer_callback_query(query.id.clone())
                                            .text("✅ Plan approved")
                                            .await
                                        {
                                            tracing::warn!("Telegram: callback UI update failed: {e}");
                                        }
                                        if let Some(mid) = kb_msg_id {
                                            // Used-button strip: defer through the #68
                                            // machinery. A post-strip failure is benign —
                                            // the #1226 dead-keyboard sweep may have
                                            // stripped the same block mid-wait — so the
                                            // exhaustion path warns without alarm.
                                            let bot2 = bot.clone();
                                            match bot.edit_message_reply_markup(chat_id, mid).await {
                                                Ok(_) => {}
                                                Err(e) => match super::edit_retry::classify(&e) {
                                                    super::edit_retry::EditErr::RetryAfter(wait) => {
                                                        super::edit_retry::spawn_deferred(
                                                            wait,
                                                            move || {
                                                                let bot = bot2.clone();
                                                                async move {
                                                                    bot.edit_message_reply_markup(chat_id, mid).await.map(|_| ())
                                                                }
                                                            },
                                                            || async move {
                                                                tracing::warn!(
                                                                    "Telegram: plan-approve markup strip deferred retry failed (benign — sweep race)"
                                                                );
                                                            },
                                                        );
                                                    }
                                                    super::edit_retry::EditErr::Fatal(msg) => {
                                                        tracing::warn!("Telegram: callback UI update failed: {msg}");
                                                    }
                                                },
                                            }
                                        }
                                        crate::channels::telegram::send::best_effort_note(
                                            &bot,
                                            chat_id,
                                            thread_id,
                                            "✅ Plan approved — starting now…",
                                            None,
                                            "system",
                                            "plan_approved_notice",
                                            "plan approve notice",
                                        )
                                        .await;
                                        // Visible seed turn, spawned so the
                                        // callback answers fast. The turn guard
                                        // keeps concurrent messages from forking a
                                        // second turn.
                                        let bot2 = bot.clone();
                                        let agent2 = agent.clone();
                                        let state2 = state.clone();
                                        tokio::spawn(async move {
                                            let _guard =
                                                match state2.try_begin_turn(session_id) {
                                                    Some(g) => g,
                                                    None => return,
                                                };
                                            // Run the approval turn through the full streaming
                                            // pipeline — typing indicator, live flow chrome with
                                            // the checklist progressing, tool messages, final
                                            // delivery — the same surface /execute gets. Without
                                            // it the tool loop still ran but silently, so a long
                                            // execution looked dead after "starting now…".
                                            // resume_session is the shared full-streaming session
                                            // runner; a button tap triggers it exactly like a
                                            // resumed turn (no inbound message to react to).
                                            if let Err(e) =
                                                crate::channels::telegram::resume::resume_session_inner(
                                                    bot2,
                                                    chat_id,
                                                    thread_id,
                                                    session_id,
                                                    prompt,
                                                    agent2,
                                                    state2,
                                                    false, // user button tap, not a push wake (#12)
                                                )
                                                .await
                                            {
                                                tracing::warn!(
                                                    "Plan approval turn failed for session {session_id}: {e}"
                                                );
                                            }
                                        });
                                    }
                                }
                                return ResponseResult::Ok(());
                            }

                            let (approved, always, yolo, id) =
                                if let Some(id) = data.strip_prefix("approve:") {
                                    (true, false, false, id.to_string())
                                } else if let Some(id) = data.strip_prefix("always:") {
                                    (true, true, false, id.to_string())
                                } else if let Some(id) = data.strip_prefix("yolo:") {
                                    (true, true, true, id.to_string())
                                } else if let Some(id) = data.strip_prefix("deny:") {
                                    (false, false, false, id.to_string())
                                } else {
                                    // Route unrecognized callback data to the agent as a
                                    // synthetic message. This allows skills (like earbot)
                                    // to handle their own inline button taps.
                                    tracing::info!(
                                        "Telegram: routing unrecognized callback to agent: {}",
                                        data
                                    );
                                    crate::channels::telegram::keyboards::ack_callback(&bot, &query, "callback").await;

                                    // Echo the tapped button as a quoted message so there's
                                    // a visible record of the user's choice in the chat.
                                    let (cb_chat, cb_thread) = query
                                        .message
                                        .as_ref()
                                        .map(|m| {
                                            (
                                                m.chat().id,
                                                m.regular_message().and_then(|r| r.thread_id),
                                            )
                                        })
                                        .unwrap_or((teloxide::types::ChatId(0), None));
                                    let echo =
                                        crate::channels::telegram::markdown::md_to_html(
                                            &format!("> 🔘 {data}"),
                                        );
                                    let _ =
                                        crate::channels::telegram::send::message_in_thread(
                                            &bot, cb_chat, cb_thread, echo,
                                        )
                                        .parse_mode(teloxide::types::ParseMode::Html)
                                        .await;

                                    // Clone data for the spawned task (tokio::spawn requires 'static)
                                    let data_owned = data.to_string();
                                    // #878: resolve the originating session.
                                    // 1st: look up the callback_origins map (set by
                                    // telegram_send send_buttons) — this is the reliable
                                    // path because it records which session sent the buttons.
                                    // 2nd: try parsing a UUID from the callback_data
                                    // (format: <prefix>:<uuid>:<rest>).
                                    // 3rd: fall back to chat-context resolution (fragile).
                                    let sid = if let Some(uuid) = state.lookup_callback_origin(data) {
                                        tracing::info!(
                                            "Telegram: callback routing — origin map hit: \
                                             {data} → session {uuid}"
                                        );
                                        Some(uuid)
                                    } else if let Some(uuid) = data.find(':').and_then(|i| {
                                        data[i + 1..].find(':').map(|j| (i, j))
                                    }).and_then(|(i, j)| {
                                        uuid::Uuid::parse_str(&data[i + 1..i + 1 + j]).ok()
                                    }) {
                                        tracing::info!(
                                            "Telegram: callback routing — parsed session UUID \
                                             {uuid} from callback data"
                                        );
                                        Some(uuid)
                                    } else {
                                        tracing::warn!(
                                            "Telegram: callback routing — no origin for '{}', \
                                             falling back to chat context (may route to wrong \
                                             session — see #878)",
                                            data
                                        );
                                        resolve_callback_session(&query, &state, &shared_session)
                                            .await
                                    };
                                    if let Some(sid) = sid {
                                        let agent_cb = agent.clone();
                                        let bot_cb = bot.clone();
                                        let state_cb = state.clone();
                                        tokio::spawn(async move {
                                            // #869: route through the full streaming pipeline
                                            // instead of the stripped-down run_resume_turn.
                                            let _guard =
                                                match state_cb.try_begin_turn(sid) {
                                                    Some(g) => g,
                                                    None => {
                                                        tracing::warn!(
                                                            "Telegram: callback routing — \
                                                             session {sid} already mid-turn"
                                                        );
                                                        return;
                                                    }
                                                };
                                            if let Err(e) =
                                                crate::channels::telegram::resume::resume_session_inner(
                                                    bot_cb,
                                                    cb_chat,
                                                    cb_thread,
                                                    sid,
                                                    format!("[callback:{data_owned}]"),
                                                    agent_cb,
                                                    state_cb,
                                                    false, // user-initiated callback tap (#12)
                                                )
                                                .await
                                            {
                                                tracing::warn!(
                                                    "Telegram: callback routing \
                                                     resume_session failed for session \
                                                     {sid}: {e}"
                                                );
                                            }
                                        });
                                    } else {
                                        tracing::warn!(
                                            "Telegram: no session for chat {} — cannot route \
                                             callback",
                                            cb_chat.0
                                        );
                                    }

                                    return ResponseResult::Ok(());
                                };

                            // OC-01: the approval keyboard sits in the chat where
                            // any allowlisted user can tap it, so re-check the
                            // tapper is the owner before acting, same as cd:/plan:
                            // above. A non-owner tap otherwise runs the pending
                            // tool, and a YOLO tap persists auto-always instance-wide.
                            let caller_is_owner = config_rx
                                .borrow()
                                .channels
                                .telegram
                                .is_owner(&query.from.id.0.to_string());
                            if !caller_is_owner {
                                tracing::warn!(
                                    "Telegram: non-owner {} tapped '{}' — refused (OC-01)",
                                    query.from.id.0,
                                    data
                                );
                                if let Err(e) = bot
                                    .answer_callback_query(query.id.clone())
                                    .text("🔒 Owner only")
                                    .show_alert(true)
                                    .await
                                {
                                    tracing::warn!("Telegram: callback UI update failed: {e}");
                                }
                                return ResponseResult::Ok(());
                            }

                            // Persist YOLO (permanent) directly from callback
                            if yolo {
                                crate::utils::persist_auto_always_policy();
                            }

                            let resolved = state.resolve_pending_approval(&id, approved, always).await;
                            tracing::info!(
                                "Telegram approval resolved: id={}, approved={}, always={}, found_pending={}",
                                id, approved, always, resolved
                            );
                            if !resolved {
                                tracing::warn!(
                                    "Telegram: no pending approval found for id={} — may have timed out or already resolved",
                                    id
                                );
                            }
                            crate::channels::telegram::keyboards::ack_callback(&bot, &query, "callback").await;

                            // Edit the approval message: keep original context, append outcome, remove buttons
                            if let Some(msg) = &query.message {
                                let label = if yolo {
                                    "\n\n🔥 YOLO — always approved"
                                } else if always {
                                    "\n\n🔁 Always approved (session)"
                                } else if approved {
                                    "\n\n✅ Approved"
                                } else {
                                    "\n\n❌ Denied"
                                };
                                let original_text = match msg {
                                    teloxide::types::MaybeInaccessibleMessage::Regular(m) => {
                                        m.text().unwrap_or("").to_string()
                                    }
                                    _ => String::new(),
                                };
                                let updated = format!("{}{}", original_text, label);
                                super::edit_retry::edit_text_ui(
                                    bot.clone(),
                                    msg.chat().id,
                                    msg.id(),
                                    updated,
                                    false,
                                    Some(teloxide::types::InlineKeyboardMarkup::default()),
                                    "approval message",
                                );
                            } else {
                                tracing::warn!("Telegram: callback query has no message — cannot edit");
                            }
                        } else {
                            tracing::warn!("Telegram: callback query with no data");
                            crate::channels::telegram::keyboards::ack_callback(&bot, &query, "callback").await;
                        }
                        ResponseResult::Ok(())
                    }
                }
            });

            // Note: service messages (member joins/leaves) are regular Message
            // updates in teloxide 0.17+ — they flow through msg_handler and are
            // captured in handler.rs BEFORE the allowlist check so bot/user IDs
            // are logged even when the joining user isn't allowlisted yet.

            // Inbound reaction handler: user reacts on a bot message, bot may
            // react back or respond with text. See handle.rs for details.
            let reaction_handler = Update::filter_message_reaction_updated().endpoint({
                let deps = deps.clone();
                move |bot: Bot, reaction: teloxide::types::MessageReactionUpdated| {
                    let deps = deps.clone();
                    async move {
                        spawn_handle_reaction(bot, reaction, deps);
                        ResponseResult::Ok(())
                    }
                }
            });

            // #1155 lifecycle coverage: the bot's OWN membership status changed
            // (kicked, left, promoted, restricted). Nothing to serve here — the
            // value is (a) visibility in logs and (b) cache hygiene: a kicked
            // bot forgets the solo-owner evaluation so a re-add re-evaluates
            // fresh instead of trusting a stale decision.
            let my_chat_member_handler = Update::filter_my_chat_member().endpoint({
                let deps = deps.clone();
                move |update: teloxide::types::ChatMemberUpdated| {
                    let deps = deps.clone();
                    async move {
                        let chat_id = update.chat.id.0;
                        let new_status = update.new_chat_member.status();
                        tracing::info!(
                            "Telegram: my membership in chat {chat_id} changed to {new_status:?}"
                        );
                        deps.telegram_state.clear_solo_evaluated(chat_id).await;
                        ResponseResult::Ok(())
                    }
                }
            });

            let tree = dptree::entry()
                .branch(msg_handler)
                .branch(edited_handler)
                .branch(cb_handler)
                .branch(reaction_handler)
                .branch(my_chat_member_handler);

            // Guest queries (#2087) never reach the typed dispatcher: the
            // pinned teloxide-core 0.13.0 has no `guest_message` kind, so a
            // guest update parses into `UpdateKind::Error` and no branch
            // matches. The runner drives those turns off the raw update and
            // answers them with `answerGuestQuery` (telegram/guest.rs).
            let guest_runner = super::guest::GuestRunner::new(
                Arc::new(listener_token.clone()),
                deps.agent.clone(),
                deps.session_svc.clone(),
                deps.config_rx.clone(),
                deps.telegram_state.clone(),
            );

            // Retry loop: if the dispatcher exits (network hiccup, Telegram conflict
            // from another process using the same token, etc.), wait and reconnect.
            // Without this, daemon mode silently loses the Telegram connection forever.
            loop {
                tracing::info!("Telegram: starting dispatcher polling loop (raw-aware listener)");
                // Raw-aware listener (#354): fetches updates as raw JSON,
                // stashes each message's payload, and synthesizes readable
                // text for content types the Bot API client cannot decode
                // (forwarded rich messages), so they flow through the normal
                // pipeline. The typed parse goes through from_str ONLY —
                // from_value yields Error kinds for everything and took the
                // whole intake down once (see telegram_raw_update_parse_test).
                let listener = super::raw_updates::raw_polling_listener(
                    listener_token.clone(),
                    Some(guest_runner.clone()),
                );
                Dispatcher::builder(bot.clone(), tree.clone())
                    .build()
                    .dispatch_with_listener(
                        listener,
                        teloxide::error_handlers::LoggingErrorHandler::with_custom_text(
                            "Telegram raw update listener error",
                        ),
                    )
                    .await;
                tracing::warn!("Telegram: dispatcher exited unexpectedly — reconnecting in 5s");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        })
    }
}

/// How long a peer bot's streamed message must go without a further edit
/// before we treat it as complete and hand the final text to the agent.
/// Peer crab bots edit their reply roughly every 1.5s while streaming
/// (matching our own stream throttle), so ~2s of quiet means the stream
/// finished — we start processing about 2s after the peer stops, which
/// keeps a bot-to-bot exchange snappy without ever acting on a partial.
const BOT_STREAM_SETTLE: Duration = Duration::from_secs(2);

/// Pending peer-bot edit streams, keyed by (chat, message). The value is
/// `(generation, latest_message)`: each new frame bumps the generation so a
/// stale settle watcher knows it was superseded and bows out.
type PendingEdits = Arc<Mutex<HashMap<(ChatId, MessageId), (u64, Message)>>>;

/// Cloneable bundle of everything `handle_message` needs, so the message,
/// edited-message, and settle paths can dispatch without threading nine
/// arguments through each.
/// Re-fire a tap pick-record edit exactly as attempt 1 fired it (#68).
/// HEAD adaptation of fork's `TapRetry` plumbing: the `PickRewrite` payload is
/// already in scope at the failure site and is `Clone`, so the deferred retry
/// is byte-identical by construction — no parallel enum needed.
async fn refire_pick_edit(
    bot: &teloxide::Bot,
    (chat_id, mid, rewrite, kb): (
        teloxide::types::ChatId,
        teloxide::types::MessageId,
        super::suggest_options::PickRewrite,
        teloxide::types::InlineKeyboardMarkup,
    ),
) -> Result<(), String> {
    use teloxide::payloads::EditMessageTextSetters;
    use teloxide::prelude::Requester;
    match rewrite {
        super::suggest_options::PickRewrite::RichMarkdownHost(body) => {
            // Fence-safe (#98): the redraw must not re-soup a delivered diagram.
            super::suggest_options::edit_rich_md_fencesafe(
                bot.api_url().as_str(),
                bot.token(),
                chat_id.0,
                mid.0,
                &body,
                Some(&serde_json::json!(kb)),
                "turn",
                "-",
            )
            .await
        }
        super::suggest_options::PickRewrite::RichHost(body) => super::rich::api::edit_rich_html(
            bot.api_url().as_str(),
            bot.token(),
            chat_id.0,
            mid.0,
            &body,
            Some(&serde_json::json!(kb)),
            "turn",
            "-",
        )
        .await
        .map_err(|e| e.to_string()),
        super::suggest_options::PickRewrite::ClassicHost(body) => bot
            .edit_message_text(chat_id, mid, &body)
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(kb)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        super::suggest_options::PickRewrite::Standalone(body) => bot
            .edit_message_text(chat_id, mid, &body)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

#[derive(Clone)]
struct DispatchDeps {
    agent: Arc<AgentService>,
    session_svc: SessionService,
    bot_token: Arc<String>,
    shared_session: Arc<Mutex<Option<Uuid>>>,
    telegram_state: Arc<TelegramState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
    session_binding_repo: SessionBindingRepository,
}

/// Should this message be held until its edit stream settles? True only for
/// a text message sent by a bot in a group — the one case where the sender
/// streams its reply via progressive edits. Humans, DMs, and non-text
/// messages process immediately.
fn is_stream_candidate(msg: &Message) -> bool {
    !msg.chat.is_private() && msg.text().is_some() && msg.from.as_ref().is_some_and(|u| u.is_bot)
}

/// Dispatch a message to the agent in the background, isolating panics so a
/// single bad turn can't take down the dispatcher. The dispatcher stays free
/// to process callback queries (approval buttons) while the agent runs.
fn spawn_handle_message(bot: Bot, msg: Message, deps: DispatchDeps) {
    tokio::spawn(async move {
        // #1317: cheap skills-signature check per inbound message. If the
        // skills overlay changed since the menus were last published (new
        // symlinked skill, edited project skills), re-publish the scoped
        // command menus. Fire-and-forget so the reply path is never delayed.
        {
            let bot = bot.clone();
            let state = deps.telegram_state.clone();
            tokio::spawn(async move {
                super::menu_refresh::refresh_menus_if_skills_changed(&bot, &state).await;
            });
        }
        let result = tokio::task::spawn(async move {
            handle_message(
                bot,
                msg,
                deps.agent,
                deps.session_svc,
                deps.bot_token,
                deps.shared_session,
                deps.telegram_state,
                deps.config_rx,
                deps.channel_msg_repo,
                deps.session_binding_repo,
            )
            .await
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("Telegram handle_message error: {e}"),
            Err(panic_err) => {
                tracing::error!("Telegram handle_message panicked: {:?}", panic_err)
            }
        }
    });
}

/// Dispatch an inbound reaction to the agent in the background.
/// Same pattern as `spawn_handle_message`: isolate panics, keep dispatcher free.
fn spawn_handle_reaction(
    bot: Bot,
    reaction: teloxide::types::MessageReactionUpdated,
    deps: DispatchDeps,
) {
    tokio::spawn(async move {
        let result = tokio::task::spawn(async move {
            handle_reaction(
                bot,
                reaction,
                deps.agent,
                deps.telegram_state,
                deps.config_rx,
                deps.channel_msg_repo,
            )
            .await
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("Telegram handle_reaction error: {e}"),
            Err(panic_err) => {
                tracing::error!("Telegram handle_reaction panicked: {:?}", panic_err)
            }
        }
    });
}

/// Wait for a peer bot's edit stream to go quiet, then process the final
/// text. If a newer frame arrived while we waited (generation moved on) we
/// bow out: that frame's own watcher will fire. The matching watcher
/// removes the pending entry, so the map self-cleans.
fn spawn_settle_watcher(
    key: (ChatId, MessageId),
    generation: u64,
    pending_edits: PendingEdits,
    bot: Bot,
    deps: DispatchDeps,
) {
    tokio::spawn(async move {
        tokio::time::sleep(BOT_STREAM_SETTLE).await;
        let final_msg = {
            let mut map = pending_edits.lock().await;
            match map.get(&key) {
                Some((g, _)) if *g == generation => map.remove(&key).map(|(_, m)| m),
                _ => None,
            }
        };
        if let Some(final_msg) = final_msg {
            tracing::debug!(
                "Telegram: peer-bot message in chat {} settled after streaming — \
                 processing final text",
                key.0.0
            );
            spawn_handle_message(bot, final_msg, deps);
        }
    });
}

/// Resolve the correct session ID for a callback query.
///
/// Callbacks from inline buttons (e.g. `/models` picker) fire in the chat
/// where the button was pressed. We look up the session that's registered for
/// that chat — this is the session the message handler resolved. When no
/// mapping exists yet (a first-ever interaction, before any message was
/// processed) the shared session is used ONLY if it belongs to this same
/// chat; otherwise the callback resolves to nothing rather than acting on a
/// session that has no relationship to where the button was pressed.
async fn resolve_callback_session(
    query: &CallbackQuery,
    state: &super::TelegramState,
    shared_session: &tokio::sync::Mutex<Option<Uuid>>,
) -> Option<Uuid> {
    // Try to get the session registered for this chat, scoped to the forum
    // topic the button was pressed in (#215) so a callback inside a topic
    // resolves that topic's session, not the base one.
    //
    // #1248: this MUST compose the topic id exactly like message ingress —
    // through `session_topic_for_event`, which applies the #1220 General-topic
    // normalization. Calling `topic_session_id` alone resolved General to
    // `None` here while ingress bound the session under
    // `Some(GENERAL_TOPIC_ID)`, so button presses in a forum's General topic
    // acted on the stale base session instead of the live one.
    if let Some(msg) = &query.message {
        let chat_id = msg.chat().id.0;
        let known_forum = state.is_known_forum(chat_id).await;
        let topic_id = msg.regular_message().and_then(|m| {
            super::session_resolve::session_topic_for_event(
                m.is_topic_message,
                m.thread_id.map(|t| t.0.0),
                known_forum,
            )
        });
        if let Some(session_id) = state.chat_session(chat_id, topic_id).await {
            return Some(session_id);
        }
    }
    // The shared session is only usable when it genuinely belongs to THIS
    // chat. It is a process-global handle, so taken unconditionally it lets a
    // press in one chat act on whatever session happens to be current — the
    // same reach that put a group's reaction into an unrelated session
    // (#1131). The case the fallback exists for is an owner DM arriving before
    // any message handler registered the chat, and that case still resolves:
    // the shared session's own chat is this one.
    let candidate = (*shared_session.lock().await)?;
    let chat_id = query.message.as_ref().map(|m| m.chat().id.0)?;
    (state.session_chat(candidate).await == Some(chat_id)).then_some(candidate)
}

/// Register bot commands with Telegram so they appear in the `/` menu.
///
/// Builds the command list dynamically from:
/// 1. Built-in commands (hardcoded)
/// 2. User-defined commands from commands.toml
/// 3. Skills from SKILL.md files
///
/// Telegram constraints: max 100 commands, names must be lowercase with
/// underscores only (no hyphens), descriptions max 256 chars.
/// Build the full owner command catalog (#1155): built-ins, `commands.toml`
/// entries and skills, names sanitized to Telegram's rules, deduped, capped
/// at 100. Extracted from `register_bot_commands` so the solo-owner
/// auto-registration path (`menu_auto`) publishes the exact same menu
/// without config entries.
pub(crate) fn collect_command_catalog() -> Vec<teloxide::types::BotCommand> {
    use teloxide::types::BotCommand;

    let mut commands: Vec<BotCommand> = vec![
        BotCommand::new("new", "Start a new session"),
        BotCommand::new("cd", "Change working directory"),
        BotCommand::new("sessions", "List and switch sessions"),
        BotCommand::new("stop", "Cancel the current operation"),
        BotCommand::new("help", "Show available commands"),
        BotCommand::new("models", "Switch AI model or provider"),
        BotCommand::new("usage", "Session token and cost stats"),
        // Authored with the canonical hyphen; sanitized to `mission_control`
        // for the menu below (Telegram command names can't contain hyphens).
        // The dispatcher and /help keep the dash; the menu chip is underscore,
        // consistent with every other multi-word command/skill.
        BotCommand::new(
            "mission-control",
            "Mission control: analytics, activity, inbox & schedule",
        ),
        BotCommand::new("compact", "Compact conversation context"),
        BotCommand::new("clear", "Clear context here at no cost; history stays"),
        BotCommand::new("goal", "Set/track an autonomous goal"),
        BotCommand::new("profiles", "Manage profiles (create, switch, migrate)"),
        BotCommand::new(
            "cowork",
            "Create a cowork workspace with QR invite (Telegram only)",
        ),
        BotCommand::new("doctor", "Run connection health check"),
        BotCommand::new("evolve", "Check for updates"),
        BotCommand::new("rtk", "Show RTK token savings statistics"),
        BotCommand::new("respond_to", "Show/switch auto-mention mode"),
        BotCommand::new("redact", "Show/switch scoped secret redaction"),
        BotCommand::new("plan", "Enter Plan mode (design a plan for approval)"),
        BotCommand::new("show_plan", "Show the current plan state"),
        BotCommand::new(
            "execute",
            "Approve the design plan / retry the checklist seed",
        ),
        BotCommand::new("discard", "Discard the live plan (back to no plan)"),
    ];

    // Load user-defined commands from commands.toml
    let brain_path = crate::brain::BrainLoader::resolve_path();
    let loader = crate::brain::CommandLoader::from_brain_path(&brain_path);
    let user_commands = loader.load();
    for cmd in &user_commands {
        // Strip leading slash and convert to Telegram format.
        let name = cmd.name.strip_prefix('/').unwrap_or(&cmd.name);
        let name = sanitize_command_name(name);
        if name.is_empty() {
            continue;
        }
        let description = truncate_description(&cmd.description, 256);
        commands.push(BotCommand::new(name, description));
    }

    // Load skills and register them as commands.
    let skills = crate::brain::skills::load_all_skills();
    for skill in &skills {
        // Skills use hyphens (security-audit); convert to underscores.
        let name = sanitize_command_name(&skill.name);
        if name.is_empty() {
            continue;
        }
        let description = truncate_description(&skill.description, 256);
        commands.push(BotCommand::new(name, description));
    }

    // Normalize EVERY command name to Telegram's rules ([a-z0-9_], 1-32 chars).
    // Telegram rejects the ENTIRE setMyCommands call if a single name is
    // invalid, and it can't show hyphens, so the menu standard is the
    // underscore form: `mission-control` → `mission_control`, consistent with
    // every other multi-word command and skill. The canonical dash is kept in
    // /help and accepted (alongside the underscore) by the dispatcher.
    for c in &mut commands {
        c.command = sanitize_command_name(&c.command);
    }
    commands.retain(|c| !c.command.is_empty() && c.command.len() <= 32);

    // Dedup by normalized name — built-ins are listed first, so a commands.toml
    // entry or skill that shadows a built-in (e.g. a user-defined `/goal`) is
    // dropped instead of creating a duplicate menu chip. Telegram would render
    // two identical entries otherwise.
    let mut seen = std::collections::HashSet::new();
    commands.retain(|c| seen.insert(c.command.clone()));

    // Telegram limit: max 100 commands
    commands.truncate(100);

    trim_catalog_to_budget(commands)
}

/// Telegram silently caps the whole `setMyCommands` request body at ~7.8KB and
/// rejects bigger payloads with a misleading `BOT_COMMANDS_TOO_MUCH` — the
/// documented 100-command limit is not the binding constraint. Empirically
/// bisected (#1613): a 63-command body at 7750B publishes fine, 7876B is
/// rejected; the real 64-command catalog serialized to 8505B and failed on
/// every publish since 2026-09-08, freezing the menu without `/clear`.
///
/// Keep the serialized catalog comfortably under the measured line: shrink
/// descriptions in tiers (96 → 48 → 32 chars — menu chips visually truncate
/// far earlier anyway), and only then drop tail commands (skills are pushed
/// last, so built-ins and `commands.toml` entries survive longest). Budget is
/// on the compact-serialized command array; the scope wrapper adds ~90B.
const MENU_BUDGET_BYTES: usize = 7000;
const MENU_DESC_TIERS: [usize; 3] = [96, 48, 32];

/// Trim a command catalog to fit [`MENU_BUDGET_BYTES`]. Pure so the budget
/// behaviour is unit-testable without the live brain directory.
pub(crate) fn trim_catalog_to_budget(
    mut commands: Vec<teloxide::types::BotCommand>,
) -> Vec<teloxide::types::BotCommand> {
    use teloxide::types::BotCommand;

    let fits = |cmds: &[BotCommand]| -> bool {
        serde_json::to_string(cmds)
            .map(|s| s.len() <= MENU_BUDGET_BYTES)
            .unwrap_or(false)
    };

    let mut dropped = 0usize;
    for tier in MENU_DESC_TIERS {
        for c in &mut commands {
            c.description = truncate_description(&c.description, tier);
        }
        if fits(&commands) {
            return commands;
        }
    }
    // Even 32-char descriptions don't fit: sacrifice tail commands (skills
    // are pushed last, so built-ins and commands.toml entries survive).
    while !commands.is_empty() && !fits(&commands) {
        commands.pop();
        dropped += 1;
    }
    if dropped > 0 {
        tracing::warn!(
            "Telegram: command menu exceeded the ~7.8KB setMyCommands payload cap; \
             dropped {dropped} tail command(s) to fit"
        );
    }
    commands
}

/// Register the command menus for every configured audience (startup +
/// ConfigWatcher refresh).
pub(crate) async fn register_bot_commands(bot: &Bot) {
    let commands = collect_command_catalog();
    register_scoped_menus(bot, commands).await;
}

/// Publish the command menu per audience instead of one list for everyone.
///
/// Every command except `/start` is owner-gated at execution, so a single
/// global menu showed non-owners ~22 entries that all answer "🔒 Owner-only
/// command." `/start` had the mirror problem: offered to users already on the
/// allowlist, for whom registering again means nothing (#782).
///
/// - default: `/start` alone, the one command a stranger needs
/// - owner: the full list, in DMs and in every configured group
/// - allowed non-owner: nothing, because nothing there is theirs to run
///
/// Registration runs at startup and is re-run by the ConfigWatcher on every
/// config write and by `menu_refresh` whenever the skills dirs change
/// (#1317), so allowlist and skills edits are picked up without a restart.
async fn register_scoped_menus(bot: &Bot, commands: Vec<teloxide::types::BotCommand>) {
    use teloxide::payloads::SetMyCommandsSetters;
    use teloxide::types::{BotCommand, BotCommandScope, ChatId, Recipient, UserId};

    let Ok(cfg) = crate::config::Config::load() else {
        tracing::warn!("Telegram: no config for scoped command menus, skipping");
        return;
    };
    let tg = &cfg.channels.telegram;

    let start_only = vec![BotCommand::new(
        "start",
        "Get your user ID to start using the bot",
    )];
    if let Err(e) = bot
        .set_my_commands(start_only)
        .scope(BotCommandScope::Default)
        .await
    {
        tracing::warn!("Telegram: failed to set default command menu: {e}");
    }

    let owner_id: Option<u64> = tg.allowed_users.first().and_then(|s| s.parse().ok());
    let Some(owner_id) = owner_id else {
        tracing::info!("Telegram: no owner configured, only the default menu is set");
        return;
    };

    let count = commands.len();
    // Owner in DMs.
    if let Err(e) = bot
        .set_my_commands(commands.clone())
        .scope(BotCommandScope::Chat {
            chat_id: Recipient::Id(ChatId(owner_id as i64)),
        })
        .await
    {
        tracing::warn!("Telegram: failed to set owner DM menu: {e}");
    }

    for group_id in tg.groups.keys() {
        let Ok(chat) = group_id.parse::<i64>() else {
            continue;
        };
        // Owner inside the group.
        if let Err(e) = bot
            .set_my_commands(commands.clone())
            .scope(BotCommandScope::ChatMember {
                chat_id: Recipient::Id(ChatId(chat)),
                user_id: UserId(owner_id),
            })
            .await
        {
            // A basic group that upgraded to a supergroup has a new chat id, and
            // every call against the old one fails from then on. Follow it once
            // and move the config entry across; otherwise this warns per user
            // per refresh forever and the group never gets its menus (#946).
            if let Some(new_id) = super::menu_scope::migrated_to(&e.to_string()) {
                let from = format!("channels.telegram.groups.{group_id}");
                let to = format!("channels.telegram.groups.{new_id}");
                match crate::config::Config::rename_section(&from, &to) {
                    Ok(true) => tracing::info!(
                        "Telegram: group {group_id} migrated to supergroup {new_id} — config moved"
                    ),
                    Ok(false) => tracing::info!(
                        "Telegram: group {group_id} migrated to supergroup {new_id}, already registered"
                    ),
                    Err(err) => tracing::warn!(
                        "Telegram: group {group_id} migrated to {new_id} but the config move failed: {err}"
                    ),
                }
                // Every member call would fail the same way on the dead id.
                continue;
            }
            tracing::warn!("Telegram: failed to set owner menu in {group_id}: {e}");
        }
        // Everyone else already allowed there: empty, since every command is
        // owner-only. Leaving them the default would offer /start to someone
        // already registered.
        // The global admins are allowed in every group, so they belong here
        // alongside the group's own roster. Deduplicated because an admin
        // listed in both lists was otherwise called twice per group.
        let group = &tg.groups[group_id];
        let mut cleared: HashSet<u64> = HashSet::new();
        for uid in group.allowed_users.iter().chain(tg.allowed_users.iter()) {
            let Ok(uid) = uid.parse::<u64>() else {
                continue;
            };
            if uid == owner_id || !cleared.insert(uid) {
                continue;
            }
            if let Err(e) = bot
                .set_my_commands(Vec::<BotCommand>::new())
                .scope(BotCommandScope::ChatMember {
                    chat_id: Recipient::Id(ChatId(chat)),
                    user_id: UserId(uid),
                })
                .await
            {
                // An admin who is not in THIS group is the common case, not a
                // fault: there is no roster to consult and no cheaper probe,
                // so the failed call is the membership test (#839).
                if super::menu_scope::means_not_a_member(&e.to_string()) {
                    tracing::debug!("Telegram: {uid} is not in {group_id}, no menu to clear");
                } else {
                    tracing::warn!("Telegram: failed to clear menu for {uid} in {group_id}: {e}");
                }
            }
        }
    }
    tracing::info!("Telegram: registered {count} owner commands, /start for everyone else");
}

/// Sanitize a command name for Telegram: lowercase, underscores only.
/// Telegram only allows: a-z, 0-9, and underscores.
pub(crate) fn sanitize_command_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c == '-' { '_' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// Truncate description to max character length, adding ellipsis if needed.
/// Telegram limits descriptions by character count, not byte count.
pub(crate) fn truncate_description(desc: &str, max_chars: usize) -> String {
    let char_count = desc.chars().count();
    if char_count <= max_chars {
        desc.to_string()
    } else {
        let truncated: String = desc.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
