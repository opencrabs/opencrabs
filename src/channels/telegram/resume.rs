//! Crash-recovery resume: replays an interrupted Telegram turn on startup
//! with full streaming (typing, tool messages, edit loop, final delivery).
//!
//! Moved VERBATIM out of handler.rs (#471 phase 1, pure decomposition —
//! the handler glob re-export keeps every existing call site stable).

use super::TelegramState;
#[allow(unused_imports)]
use super::handler::*;
use super::send::{best_effort_delete, fire_chat_action};
use crate::a2a::handler::notify::CLI_SENDER_PREFIX;
use crate::brain::agent::service::background_tasks;
use crate::brain::agent::{AgentService, ProgressCallback, ProgressEvent};
use crate::config::Config;
use crate::db::repository::pending_request::PendingRequest;
use crate::db::{ChannelMessageRepository, PendingRequestRepository};
use crate::utils::echo_budget::{ECHO_BODY_CAP_CHARS, ECHO_BODY_CAP_CHARS_RICH};
use futures::future::BoxFuture;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, MessageId};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Build the background-task enqueue producer for Telegram (#722).
///
/// When a detached long command finishes, this resumes the session by delivering
/// a turn to its chat via `resume_session` (full streaming pipeline). The agent
/// handle can't be captured at service-creation time (it's being built), so a
/// weak holder filled afterwards breaks the cycle; on call we upgrade it.
pub(crate) fn build_enqueue_callback(
    state: Arc<TelegramState>,
    agent_holder: Arc<std::sync::Mutex<Option<std::sync::Weak<AgentService>>>>,
) -> crate::brain::agent::service::MessageEnqueueCallback {
    Arc::new(move |session_id, msg| {
        let state = state.clone();
        let agent_holder = agent_holder.clone();
        tokio::spawn(async move {
            let Some(chat_id) = state.session_chat(session_id).await else {
                tracing::warn!("[bg-resume] telegram: no chat for session {session_id}; dropping");
                return;
            };
            // Channel-ownership guard (fork #17): this callback is ALSO
            // reached by paths that bypass deliver_to_session's gate —
            // background-task completions resolve their route directly
            // (background_tasks.rs) — so the choke point checks too. A
            // session replaced on its chat/topic must never be woken into
            // the successor's conversation; refuse the wake. (Port seam:
            // the fork parks the message here; upstream has no channel
            // park primitive in this callback, so the completion is dropped
            // with a loud warn — the gate's contract is refusing the wake,
            // not preserving the message.)
            if let crate::brain::agent::service::session_routes::ChannelOwnership::Occupied {
                occupant,
            } = state.channel_ownership_of(session_id)
            {
                tracing::warn!(
                    "[bg-resume] telegram: session {session_id} no longer owns chat {chat_id} — \
                     occupied by session {occupant}; refusing to wake it into the successor's \
                     conversation"
                );
                return;
            }
            // Bounded wait, not a drop (#1242). At boot this callback and
            // the bot's own connect run concurrently with nothing ordering
            // them, so "not connected" here usually means "not connected
            // yet" — and answering it with a return lost the wake forever.
            let Some(bot) =
                crate::channels::transport_ready::await_transport("telegram", session_id, || {
                    state.bot()
                })
                .await
            else {
                return;
            };
            let Some(agent) = agent_holder
                .lock()
                .ok()
                .and_then(|g| g.as_ref().and_then(|w| w.upgrade()))
            else {
                tracing::warn!("[bg-resume] telegram: agent gone; dropping resume");
                return;
            };
            // The topic that OWNS the session, not whichever one spoke last
            // (#1200, #1319). Sessions are per-topic since #215, and
            // `register_session_chat` records the topic, but this path asked
            // the chat instead: in a forum, any traffic in another topic while
            // a detached command ran sent the result there. Both background
            // completions and sub-agent results share this callback, so they
            // were both misrouted.
            //
            // The resolution is THREE-state, and `session_topic` cannot
            // express it: its `None` covers both "bound to General / a DM" — a
            // definite no-thread address — and "not bound at all", which is
            // the only case the chat-wide lookup is for. Resolving through it
            // sent the General topic's hourly digest into whichever topic had
            // spoken last (measured 2026-09-21: session ef52656d, whose
            // durable binding is `thread_id = NULL` on this chat, was
            // delivered as msg 16795 into the Голубицкая topic).
            // `session_push_thread` keeps the two arms apart and reaches the
            // durable `session_bindings` row when the in-memory maps are still
            // empty.
            let thread_id = super::send::session_push_thread(&state, session_id, chat_id).await;

            // #1221: announce WHAT arrived before anything else happens — an
            // expandable blockquote echoing the completion output (rich format
            // preserved), so the user sees why the session woke up even when
            // the resumed answer restates it. Fires on BOTH delivery paths:
            // before the guard branch below means idle-wakes AND results that
            // get queued into an in-flight turn are announced alike. Covers
            // background-task completions AND session_notify pushes (#1221
            // notify lane); sub-agent pushes stay silent.
            if matches!(
                msg.origin,
                crate::brain::agent::PushOrigin::BackgroundTask
                    | crate::brain::agent::PushOrigin::SessionNotify
            ) {
                // #1377: a BARE background-task ack (typed bg_meta card) folds
                // into the session's settled flow card — one line in the
                // collapsible block, header counter re-stamped from the live
                // registry — instead of a standalone bubble. Notify pushes
                // keep their N4 document card (prose/tables ARE the content);
                // no-meta redeliveries (#1242) and cardless sessions keep the
                // bubble path below.
                let folded = if msg.origin == crate::brain::agent::PushOrigin::BackgroundTask {
                    match msg.bg_meta.clone() {
                        Some(meta) => {
                            fold_bg_ack_into_flow_card(
                                &bot, chat_id, &state, &agent, session_id, &meta,
                            )
                            .await
                        }
                        None => false,
                    }
                } else {
                    false
                };
                // #61: a notify landing while THIS session's flow roll is live
                // folds a compact line INTO the block instead of posting the
                // standalone bubble below — the bubble visually detached the
                // announcement from the block the notify may have caused. The
                // fold is best-effort (idle wake, bg completion, dead roll, or
                // unparseable sender → `false`) and every false case falls
                // through to the receipt card, byte-unchanged. BackgroundTask
                // pushes never hit this: the #1377 settled-card fold above
                // already owns them.
                let notify_folded = if state.is_turn_active(session_id)
                    && msg.origin == crate::brain::agent::PushOrigin::SessionNotify
                {
                    fold_notify_into_roll(
                        &state,
                        &bot,
                        session_id,
                        chat_id,
                        thread_id,
                        &msg.context_text,
                    )
                    .await
                } else {
                    false
                };
                if !folded && !notify_folded {
                    // #15: receipt cards. A bg completion carries the typed
                    // payload (icon/label/duration/tail) in `bg_meta` — the card
                    // renders from it, never by parsing the `[System: ...]`
                    // context text. Notify pushes carry no meta: the card is
                    // built from the sender label + markdown body (N4 shape).
                    // #1225: session_notify pushes carry a mechanical
                    // `[session-notify from=<uuid>]` header — the raw id is
                    // replaced with a human label (topic name for same-chat
                    // pushes, chat name / chat+topic for cross-chat, per
                    // Alexey's rule). The CLI lane (#1258) stamps
                    // `from=cli:<label>` instead — no sender session exists,
                    // so the carried label renders verbatim, zero lookups.
                    let (wire, classic_html) = if let Some(meta) = msg.bg_meta.clone() {
                        build_bg_receipt_card(&meta)
                    } else {
                        let (sender, body) = split_bg_echo_parts(&msg.context_text);
                        match sender {
                            Some(NotifySender::Session(s)) => {
                                let label = sender_label(&state, &bot, s, chat_id).await;
                                build_notify_receipt_card(&label, &body).await
                            }
                            Some(NotifySender::CliTooling(label)) => {
                                build_notify_receipt_card(label, &body).await
                            }
                            // Defensive: a BackgroundTask push without meta
                            // (parked by an older binary, #1242 redelivery) keeps
                            // the #1234 flat bold-title shape. Borrow note:
                            // `msg` is moved wholesale later in this callback.
                            None if msg.origin
                                == crate::brain::agent::PushOrigin::BackgroundTask =>
                            {
                                let title = background_task_title(&msg.display_text);
                                build_bg_echo_bubble(&body, &title)
                            }
                            None => build_bg_echo_bubble(&body, "⚙️ background task result"),
                        }
                    };
                    // #85 (re-landing #38 / 29b5731d): the wire is the card
                    // builder's decision. `<details>` chrome cards ride the HTML
                    // input mode, where the wrapper parses into a native
                    // RichBlockDetails collapsible — the markdown rich mode
                    // cannot express it (rich/api.rs dialect rule), and routing
                    // those cards through the #1234 markdown outbox is what
                    // leaked escaped tags into the chat (3x RICH_MESSAGE_EMPTY
                    // + visible wrapper text on 2026-08-29). Plain markdown
                    // bubbles keep the canonical outbox. On rich failure the
                    // classic blockquote below is the LIVE degradation:
                    // send_rich_html_id returns Err to this caller instead of
                    // being swallowed inside the outbox.
                    let rich_on = crate::config::Config::current()
                        .channels
                        .telegram
                        .rich_messages;
                    let sent_rich = match (&wire, rich_on) {
                        (BubbleWire::Html(html), true) => {
                            match super::rich::api::send_rich_html_id(
                                bot.api_url().as_str(),
                                bot.token(),
                                chat_id,
                                thread_id,
                                html,
                                None,
                                "bg-resume",
                                "-",
                            )
                            .await
                            {
                                Ok(_) => true,
                                Err(e) => {
                                    tracing::warn!(
                                        "[bg-resume] #38 rich HTML send failed, using HTML: {e}"
                                    );
                                    false
                                }
                            }
                        }
                        // Config gate: rich disabled → classic blockquote below.
                        (BubbleWire::Html(_), false) => false,
                        (BubbleWire::Markdown(md), _) => {
                            match super::send::send_markdown_outbox(
                                &bot,
                                teloxide::types::ChatId(chat_id),
                                thread_id,
                                md,
                                "bg-resume",
                                "-",
                                None,
                            )
                            .await
                            {
                                Ok(_) => true,
                                Err(e) => {
                                    tracing::warn!(
                                        "[bg-resume] #1234 rich echo failed, using HTML: {e}"
                                    );
                                    false
                                }
                            }
                        }
                    };
                    if !sent_rich {
                        let mut echo = bot
                            .send_message(teloxide::types::ChatId(chat_id), classic_html.clone())
                            .parse_mode(teloxide::types::ParseMode::Html);
                        // Forum topics address a thread; DMs and non-forum groups must
                        // omit the parameter entirely (E0308: not an unwrap decision).
                        if let Some(tid) = thread_id {
                            echo = echo.message_thread_id(tid);
                        }
                        // 429 discipline (#816): the raw send_message used to race
                        // the resumed stream + typing loop into the same chat and
                        // drop silently — 11/11 real echo sends failed that way on
                        // 2026-08-26 (per-chat flood windows, compiler batch log).
                        // Wait the window out (shared wait_out policy) and retry ONCE
                        // with fresh content, matching delivery.rs / flow.rs.
                        match echo.await {
                            Ok(_) => {}
                            Err(teloxide::RequestError::RetryAfter(secs)) => {
                                super::rate_limit::wait_out(
                                    "bg-resume echo",
                                    secs.duration(),
                                    " on first delivery, retrying once",
                                    Some(chat_id),
                                )
                                .await;
                                let mut retry = bot
                                    .send_message(teloxide::types::ChatId(chat_id), classic_html)
                                    .parse_mode(teloxide::types::ParseMode::Html);
                                if let Some(tid) = thread_id {
                                    retry = retry.message_thread_id(tid);
                                }
                                if let Err(e) = retry.await {
                                    tracing::warn!(
                                        "[bg-resume] #1221 echo bubble failed on 429 retry: {e}"
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!("[bg-resume] #1221 echo bubble failed to send: {e}");
                            }
                        }
                    }
                } // #1377: end of the not-folded (standalone bubble) lane
            }

            // One streaming turn per session (#845). Several detached commands
            // finishing together used to spawn a resume turn each, all on this
            // session and all in this chat, so every one opened and edited its
            // own flow block: duplicated, overlapping output.
            //
            // The background path was the only one skipping this gate; user
            // messages and reactions have gone through it since #501.
            let Some(_turn_guard) = state.try_begin_turn(session_id) else {
                // A turn is already streaming. Hand the result to it instead of
                // forking a second one: the tool loop drains this queue between
                // rounds via reaction_queue_callback, so the result lands in the
                // block that is already open.
                tracing::info!(
                    "[bg-resume] telegram: session {session_id} already streaming — queuing the \
                     result for the in-flight turn instead of opening a second block"
                );
                // Tagged as detached work, not as a reaction (#1213). The
                // guard covers the whole delivery window, not just the tool
                // loop, so "already streaming" can mean the loop has finished
                // and only the final bubble is still going out — in which case
                // nothing will drain this between rounds and the end-of-turn
                // flush has to know it needs a real tool loop, not a single
                // toolless round.
                // Framing for mid-flight drains (fork #13): the queued push
                // arrives in the receiver's context as a bare user turn,
                // indistinguishable from a fresh instruction — the confusion
                // the interrupt gate's true-branch knowingly accepts. One
                // plain string tells the receiver to re-anchor after reading.
                let mut msg = msg;
                // Idempotent framing (#439/#366): a push that already carries
                // it — a row re-offered at boot, an item re-queued after a lost
                // turn race — must not accumulate a second wrapper. The literal
                // and its inverse live together in `notify_queue`.
                msg.context_text =
                    crate::brain::agent::service::notify_queue::wrap_busy_once(&msg.context_text);
                state.enqueue_detached_result(session_id, msg);
                return;
            };

            // The topic that OWNS the session, not whichever one spoke last
            // (#1200, #1319). Sessions are per-topic since #215, and
            // `register_session_chat` records the topic, but this path asked
            // the chat instead: in a forum, any traffic in another topic while
            // a detached command ran sent the result there. Both background
            // completions and sub-agent results share this callback, so they
            // were both misrouted.
            //
            // The resolution is THREE-state, and `session_topic` cannot
            // express it: its `None` covers both "bound to General / a DM" — a
            // definite no-thread address — and "not bound at all", which is
            // the only case the chat-wide lookup is for. Resolving through it
            // sent the General topic's hourly digest into whichever topic had
            // spoken last (measured 2026-09-21: session ef52656d, whose
            // durable binding is `thread_id = NULL` on this chat, was
            // delivered as msg 16795 into the Голубицкая topic).
            // `session_push_thread` keeps the two arms apart and reaches the
            // durable `session_bindings` row when the in-memory maps are still
            // empty.
            let thread_id = super::send::session_push_thread(&state, session_id, chat_id).await;
            if let Err(e) = resume_session_inner(
                bot,
                teloxide::types::ChatId(chat_id),
                thread_id,
                session_id,
                msg.context_text,
                agent,
                state,
                true, // push-initiated wake (#12): track for restart recovery
            )
            .await
            {
                tracing::warn!("[bg-resume] telegram resume_session failed: {e}");
            }
        });
    })
}

/// Public entry for resuming a session with the full streaming pipeline:
/// claims the session's turn slot (#1222), then drives `resume_session_inner`.
///
/// Callers that ALREADY hold the turn guard — the bg-resume enqueue callback
/// (resume.rs #845/#1213), the end-of-turn stranded flush (handler.rs #1213)
/// and the agent approval flows (agent.rs) — must call `resume_session_inner`
/// directly so the slot is not double-claimed.
///
/// Called from ui.rs on startup when pending Telegram requests are detected.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resume_session(
    bot: Bot,
    chat_id: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    session_id: Uuid,
    prompt: String,
    agent: Arc<AgentService>,
    telegram_state: Arc<TelegramState>,
    track_push_turn: bool,
) -> anyhow::Result<()> {
    // Claim the session's turn slot for the whole replay (#1222). A recovery
    // replay drives the SAME edit loop as an ingress turn but used to run
    // without holding TelegramState's active_turns flag, so any inbound
    // message arriving mid-replay read the session as idle and forked a
    // second concurrent tool loop on top of it — two interleaved streaming
    // blocks progressing in one topic. Holding the RAII guard for the whole
    // resumed turn makes ingress queue follow-ups (#501/#845) exactly as it
    // does for normal turns; resume-born loops already drain those queues.
    // If the slot is somehow taken, skipping is safe: the pending item was
    // already cleared from the repo and something else owns the turn.
    let _turn_guard = match telegram_state.try_begin_turn(session_id) {
        Some(guard) => guard,
        None => {
            tracing::warn!(
                "Telegram: resume_session {session_id} skipped — a turn is already active for this session"
            );
            return Ok(());
        }
    };
    let bot_for_flush = bot.clone();
    let agent_for_flush = agent.clone();
    let state_for_flush = telegram_state.clone();
    let result = resume_session_inner(
        bot,
        chat_id,
        thread_id,
        session_id,
        prompt,
        agent,
        telegram_state,
        track_push_turn,
    )
    .await;

    // #201: the tool loop has exited — an item enqueued during the final
    // generation round (after the last between-rounds drain) has no consumer
    // left on this path, so it sat until the NEXT turn drained it — an
    // operator nudge by any other name. Mirror the ingress end-of-turn
    // flush: drain and dispatch leftovers AFTER the guard drops, so the
    // follow-up turn the flush opens can take the slot. The reaction arm
    // awaits its turn to completion before this returns; the detached arm
    // spawns and does not block.
    drop(_turn_guard);
    flush_queued_after_turn(
        bot_for_flush,
        chat_id,
        thread_id,
        session_id,
        agent_for_flush,
        state_for_flush,
    )
    .await;

    result
}

/// End-of-turn flush of queued items (#201) — the extracted consumer of
/// `drain_queued_items`. Until #201 the ONLY production call site was the
/// tail of `handle_message`, so an item enqueued after the tool loop's last
/// between-rounds drain was stranded on push-initiated turns: the turn
/// ended with nothing left to drain and the item sat until the next turn
/// on the session. Call this at every turn path's end, AFTER the
/// active-turn guard drops so a follow-up turn can take the slot.
///
/// Split by origin first (#1213): a detached result gets a real tool loop
/// (spawned — this function never blocks on it, and the resumed turn is a
/// `resume_session` wrapper turn that flushes again at ITS end); reactions
/// and ingress leftovers keep the full streaming pipeline (#1213 — a
/// toolless single-shot would leave the model announcing work it has no
/// tools to execute) under a fresh turn guard, re-queued if another turn
/// won the race.
pub(crate) fn flush_queued_after_turn(
    bot: Bot,
    chat_id: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    session_id: Uuid,
    agent: Arc<AgentService>,
    telegram_state: Arc<TelegramState>,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        // Empty is the common case (one cheap lock check) — a real inference
        // only fires when something was queued.
        let (detached, leftover_reactions): (Vec<_>, Vec<_>) = telegram_state
            .drain_queued_items(session_id)
            .into_iter()
            .partition(|item| item.origin == super::state::QueuedOrigin::DetachedWork);

        if !detached.is_empty() {
            let combined = detached
                .iter()
                .map(|i| i.msg.context_text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            tracing::info!(
                "Telegram: {} detached result(s) landed during final delivery for session \
                 {session_id} — resuming with a full tool loop rather than the toolless flush",
                detached.len()
            );
            let bot_for_resume = bot.clone();
            let agent_for_resume = agent.clone();
            let state_for_resume = telegram_state.clone();
            let chat_for_resume = chat_id;
            let detached_for_clear: Vec<_> = detached
                .iter()
                .map(|item| (session_id, item.msg.clone()))
                .collect();
            // Spawned: the turn guard is already dropped above, so the resumed
            // turn can take it, and this caller must not block until that whole
            // turn finishes.
            tokio::spawn(async move {
                if let Err(e) = resume_session(
                    bot_for_resume,
                    chat_for_resume,
                    thread_id,
                    session_id,
                    combined,
                    agent_for_resume,
                    state_for_resume,
                    true, // end-of-turn detached flush is a push-initiated wake (#12)
                )
                .await
                {
                    tracing::warn!(
                        "Telegram: resumed turn for a detached result failed \
                         (session {session_id}): {e}"
                    );
                }
                // The resume turn delivering (or even failing) retires the
                // durable twin (#111): the retry context is gone with this run,
                // so replaying the row next boot would double-deliver. Clear
                // unconditionally — a lost in-memory retry costs a plain
                // undelivered push, the pre-#111 behavior; a surviving row
                // costs a duplicate.
                for (_sid, m) in &detached_for_clear {
                    crate::brain::agent::service::notify_queue::clear_on_delivery(session_id, m);
                }
            });
        }

        let leftover_reactions: Vec<_> = leftover_reactions.into_iter().map(|i| i.msg).collect();
        if !leftover_reactions.is_empty() {
            // #1213: leftovers can be background-task or subagent completions
            // that raced the final-bubble delivery — the resume callback found
            // the turn guard still held and queued them here, but no tool loop
            // remained to drain them between rounds. A toolless single-shot
            // reply leaves the model announcing follow-up work it has no tools
            // to execute, then the session stalls until the next inbound
            // message. Run them through the FULL streaming pipeline (tools
            // intact), under the turn gate so a completion resuming concurrently
            // cannot fork a second overlapping block (#845). If another turn won
            // the race, re-queue: that turn drains them between rounds as
            // designed.
            let combined = leftover_reactions
                .iter()
                .map(|m| m.context_text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            match telegram_state.try_begin_turn(session_id) {
                Some(_flush_turn_guard) => {
                    tracing::info!(
                        "Telegram: flushing {} stranded reaction(s) for session {session_id} via full pipeline (#1213)",
                        leftover_reactions.len()
                    );
                    // Guard held across the await: the resumed pipeline owns the
                    // session exclusively, same as any other turn.
                    let result = resume_session_inner(
                        bot.clone(),
                        chat_id,
                        thread_id,
                        session_id,
                        combined,
                        agent.clone(),
                        telegram_state.clone(),
                        true, // stranded-reaction flush is a push-initiated wake (#12)
                    )
                    .await;
                    // Retire the durable twin on resume ATTEMPT (#111): this
                    // run's retry context is gone either way, so a surviving row
                    // next boot would double-deliver. Failure direction is a
                    // plain undelivered push (pre-#111), never a duplicate.
                    for r in &leftover_reactions {
                        crate::brain::agent::service::notify_queue::clear_on_delivery(
                            session_id, r,
                        );
                    }
                    if let Err(e) = result {
                        tracing::warn!(
                            "Telegram: flushed-reaction full pipeline failed for session {session_id}: {e}"
                        );
                    }
                }
                None => {
                    for r in leftover_reactions {
                        telegram_state.enqueue_reaction(session_id, r);
                    }
                }
            }
        }
    })
}

/// Which bubble a resumed turn takes over from the process it replaces
/// (#2010).
///
/// A turn streams into two different surfaces, and they live in two
/// different `StreamingState` fields: the processing-log block
/// (`open_group_msg_id`, opened in flow.rs) and the response placeholder
/// (`msg_id`, opened in stream_loop.rs). The row only stores one id, so the
/// field it belongs to is stored with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CarriedBubble {
    /// The processing-log block: seed `open_group_msg_id`. The bool is
    /// `flow_rich` as the interrupted process had it: a block opened through
    /// the rich API must be edited through the rich API, and an HTML one
    /// through HTML, or the first refresh re-renders the wrong markup.
    Flow(MessageId, bool),
    /// The response placeholder: seed `msg_id`.
    Answer(MessageId),
}

/// Decide which bubble, if any, a resumed turn should adopt (#2010).
///
/// A SIGKILLed Telegram turn leaves its partially written bubble on screen,
/// and the resume used to open a second one for the same answer. The pending
/// row is the only state that survives the kill and the resume already reads
/// it, so the live bubble's id rides on it as `"<surface>:<message id>"`.
///
/// Everything that is not exactly this turn's own streaming bubble declines,
/// and the resume then does what it did before #2010 (open a fresh bubble):
/// another channel, a `system` push row (its text is re-delivered, never
/// re-streamed), a row from a different chat (Telegram ids are only unique
/// per chat, so the wrong chat would edit a stranger's message), an unknown
/// surface tag, or an id that does not parse.
pub(crate) fn streaming_bubble_to_adopt(
    row: &PendingRequest,
    chat_id: i64,
) -> Option<CarriedBubble> {
    if row.channel != "telegram" || row.origin != "user" {
        return None;
    }
    if row.channel_chat_id.as_deref() != Some(chat_id.to_string().as_str()) {
        return None;
    }
    let (surface, raw_id) = row.channel_message_id.as_deref()?.split_once(':')?;
    let id = raw_id.parse::<i32>().ok().map(MessageId)?;
    match surface {
        "flow" => Some(CarriedBubble::Flow(id, true)),
        // A block the interrupted process opened through the classic HTML
        // path because the rich API was off or had already failed.
        "flow_plain" => Some(CarriedBubble::Flow(id, false)),
        "answer" => Some(CarriedBubble::Answer(id)),
        _ => None,
    }
}

/// Unguarded core of `resume_session`. The turn-slot contract lives in the
/// caller: either hold an `ActiveTurnGuard` across the await, or go through
/// the public wrapper.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resume_session_inner(
    bot: Bot,
    chat_id: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    session_id: Uuid,
    prompt: String,
    agent: Arc<AgentService>,
    telegram_state: Arc<TelegramState>,
    track_push_turn: bool,
) -> anyhow::Result<()> {
    tracing::info!(
        "Telegram: resume_session {} with full streaming pipeline",
        session_id
    );

    // ── Typing indicator ────────────────────────────────────────────────────
    let typing_cancel = CancellationToken::new();
    let _typing_guard = TypingGuard(typing_cancel.clone());
    // Outlives the turn while the session still has detached work, so a long
    // background command does not leave the chat looking dead (#812).
    super::typing::spawn_typing(
        bot.clone(),
        chat_id,
        thread_id,
        typing_cancel.clone(),
        agent.background_manager(),
        agent.subagent_manager(),
        session_id,
    );

    // ── Streaming setup ────────────────────────────────────────────────────
    // #2010: take over the bubble the interrupted process was streaming into.
    // The pending row is the only state that survives the SIGKILL and the
    // resume already reads it, so the live surface's id rides on it. Seeding
    // it here makes the first refresh EDIT that bubble instead of sending a
    // new one beside the partial answer the dead process left on screen,
    // which is what showed a resumed turn as two bubbles in one topic. A row
    // with nothing adoptable keeps the old behaviour: the resume opens its own
    // bubble.
    let carried_bubble = PendingRequestRepository::new(agent.context().pool())
        .find_latest_for_session(session_id)
        .await
        .ok()
        .flatten()
        .and_then(|row| streaming_bubble_to_adopt(&row, chat_id.0));
    // The bubble being adopted is the response placeholder, which predates
    // this process. The cancel teardown below must not delete it.
    let adopted_answer = matches!(carried_bubble, Some(CarriedBubble::Answer(_)));
    let seed_msg_id = match carried_bubble {
        Some(CarriedBubble::Answer(id)) => Some(id),
        _ => None,
    };
    let seed_flow = match carried_bubble {
        Some(CarriedBubble::Flow(id, rich)) => Some((id, rich)),
        _ => None,
    };
    let streaming = Arc::new(std::sync::Mutex::new(StreamingState {
        // Telegram: positive chat id = private/DM, negative = group (#677).
        is_dm: chat_id.0 > 0,
        compacting: false,
        pending_suggestions: None,
        pending_trailer: None,
        delivered_files: Vec::new(),
        media_intermediates: Vec::new(),
        msg_id: seed_msg_id,
        thinking: String::new(),
        tool_msgs: Vec::new(),
        display_queue: Vec::new(),
        open_group_msg_id: seed_flow.map(|(id, _)| id),
        rich_transport_failures: 0,
        flow_entries: Vec::new(),
        flow_status: None,
        flow_rich: matches!(seed_flow, Some((_, true))),
        response: String::new(),
        final_bubble: None,
        dirty: false,
        recreate: false,
        header_preview: None,
        sections: Default::default(),
        retained_goal: None,
        applied_plan_kb: Default::default(),
        tool_round_count: 0,
        tools_started_at: Some(std::time::Instant::now()),
        turn_started_at: std::time::Instant::now(),
        flow_outcome: None,
        bg_indicator: None,
        bg_count: None,
        subagent_counts: Default::default(),
        sent_intermediates: Vec::new(),
        intermediate_bubbles: Vec::new(),
        voice_msg_ids: Vec::new(),
        processing: true,
        // Cap folded narration only for CLI providers (#532).
        is_cli: agent.provider_for_session(session_id).cli_handles_tools(),
    }));

    let edit_cancel = CancellationToken::new();

    // Edit loop — same as handle_message
    // Store JoinHandle to await after cancellation (prevents duplicate race).
    // The resumed turn drives the SAME streaming edit loop as handle_message
    // (#1086 seam 5). This file used to carry a second implementation that
    // drifted from the original: it missed the tool-message edit pass, the
    // settle-flow handling and the reasoning excerpt, while the original
    // missed the placeholder-send telemetry this one had. Both gaps close by
    // sharing one loop. A resumed turn has no inbound message, so there is
    // nothing to react to (#261).
    let edit_loop_handle = super::stream_loop::spawn_edit_loop(
        &bot,
        chat_id,
        None,
        thread_id,
        chat_id.0 > 0,
        &streaming,
        &edit_cancel,
        &telegram_state,
        &agent,
        session_id,
    );

    // Progress callback — same as handle_message
    let progress_cb: ProgressCallback = {
        let st = streaming.clone();
        let bot_typing = bot.clone();
        let chat_typing = chat_id;
        Arc::new(move |_sid, event| match event {
            // Auto-compaction silent window — immediate typing refresh plus
            // the visible start line in the flow body (#29).
            // See handle_message for the full rationale.
            ProgressEvent::Compacting {
                usage_pct,
                predicted,
            } => {
                let bot = bot_typing.clone();
                let chat = chat_typing;
                tokio::spawn(async move {
                    fire_chat_action(&bot, chat, thread_id, ChatAction::Typing, "resume typing")
                        .await;
                });
                if let Ok(mut s) = st.lock() {
                    // The whole compaction surface is opt-in chrome (#1686),
                    // header pin included. Leaving the pin ungated was
                    // justified by a staleness worry that does not exist:
                    // tick_flow_header advances flow_status from
                    // turn_started_at outside the `compacting` branch, so the
                    // clock keeps counting through the silent window and the
                    // header keeps its last real preview. With the flag off
                    // the pin is never set, so a compaction leaves no trace on
                    // the block at all. The CompactionSummary arm's lift stays
                    // unconditional, so a flag flip mid-window cannot strand
                    // a pin.
                    if Config::current().agent.compaction_notice {
                        s.compacting = true;
                        s.header_preview = Some(COMPACTING_HEADER_TEXT.to_string());
                        s.display_queue
                            .push(DisplayItem::Intermediate(compacting_flow_line(
                                usage_pct, predicted,
                            )));
                    }
                }
            }
            ProgressEvent::ReasoningChunk { text } => {
                if let Ok(mut s) = st.lock() {
                    s.thinking.push_str(&text);
                    s.dirty = true;
                }
            }
            ProgressEvent::StreamingChunk { text } => {
                if let Ok(mut s) = st.lock() {
                    if !s.thinking.is_empty() {
                        s.thinking.clear();
                    }
                    s.response.push_str(&text);
                    s.dirty = true;
                    s.processing = false;
                }
            }
            ProgressEvent::ToolStarted {
                tool_name,
                tool_input,
                ..
            } => {
                if let Ok(mut s) = st.lock() {
                    s.thinking.clear();
                    if s.tools_started_at.is_none() {
                        s.tools_started_at = Some(std::time::Instant::now());
                    }
                    let ctx = tool_context(&tool_name, &tool_input);
                    let raw_ctx = crate::utils::tool_status_source(&tool_name, &tool_input);
                    let idx = s.tool_msgs.len();
                    s.tool_msgs.push(ToolMsg {
                        msg_id: None,
                        name: tool_name,
                        context: ctx,
                        raw_context: raw_ctx,
                        completed: None,
                        dirty: true,
                        task_id: None,
                    });
                    s.display_queue.push(DisplayItem::NewTool(idx));
                }
            }
            ProgressEvent::ToolCompleted {
                tool_name, success, ..
            } => {
                if let Ok(mut s) = st.lock() {
                    s.tool_round_count += 1;
                    if let Some(tool) = s
                        .tool_msgs
                        .iter_mut()
                        .rev()
                        .find(|t| t.name == tool_name && t.completed.is_none())
                    {
                        tool.completed = Some(success);
                        tool.dirty = true;
                    }
                    // No recreate here (#299) — see the handle_message arm:
                    // completions edit the group in place, nothing new lands
                    // below the placeholder.
                }
            }
            ProgressEvent::QueuedUserMessage { .. } => {
                detach_flow_for_followup(&st);
            }
            ProgressEvent::IntermediateText { text, reasoning: _ } => {
                if let Ok(mut s) = st.lock() {
                    s.thinking.clear();
                    s.response.clear();
                    if s.msg_id.is_some() {
                        s.recreate = true;
                    }
                    // Never push reasoning as a standalone intermediate — it
                    // belongs in the streaming response's 💭 thinking block.
                    // Using reasoning as a fallback here causes duplicate
                    // messages on Telegram (reasoning intermediate + final
                    // response that doesn't contain the reasoning text, so
                    // dedup can't strip it).
                    if !text.is_empty() {
                        s.display_queue.push(DisplayItem::Intermediate(text));
                    }
                }
            }
            ProgressEvent::SelfHealingAlert { message } => {
                if let Ok(mut s) = st.lock() {
                    s.display_queue.push(DisplayItem::System(format!(
                        "🔧 {}",
                        crate::utils::sanitize::normalize_dashes(&message)
                    )));
                }
            }
            ProgressEvent::RetryAttempt {
                attempt,
                max,
                reason,
            } => {
                if let Ok(mut s) = st.lock() {
                    s.display_queue.push(DisplayItem::System(format!(
                        "⏳ Retry {}/{} — {}",
                        attempt, max, reason
                    )));
                }
            }
            ProgressEvent::ProviderSwitched {
                to_name, to_model, ..
            } => {
                if let Ok(mut s) = st.lock() {
                    s.display_queue.push(DisplayItem::System(format!(
                        "🔄 Now using {}/{}",
                        to_name, to_model
                    )));
                }
            }
            ProgressEvent::SuggestedOptions(options) => {
                // Stash only (#724): buttons must be the FINAL message in the
                // chat, so render_suggestions runs after deliver_final_response
                // below — where it can also merge into the answer bubble
                // (#tg-suggest-merge). The old mid-stream spawn posted the
                // keyboard ABOVE the final answer on resumed turns.
                if let Ok(mut s) = st.lock() {
                    s.pending_suggestions = Some(options);
                }
            }
            // Compaction finished — definitive completion receipt (#29).
            // See the handle_message progress callback for the rationale.
            ProgressEvent::CompactionSummary {
                before_pct,
                after_pct,
                elapsed,
                ..
            } => {
                if let Ok(mut s) = st.lock() {
                    s.compacting = false;
                    s.header_preview = None;
                    // Lifting the pin stays unconditional while setting it is
                    // gated (#1686): set-gated / lift-ungated means a flag flip
                    // inside the compaction window can never strand a pin that
                    // nothing will clear. The lift is what lets the next tick
                    // recompute the header from live data.
                    if Config::current().agent.compaction_notice {
                        s.display_queue
                            .push(DisplayItem::Intermediate(compacted_flow_line(
                                before_pct, after_pct, elapsed,
                            )));
                    }
                }
            }
            _ => {}
        })
    };

    // ── Agent call ──────────────────────────────────────────────────────────
    let cancel_token = CancellationToken::new();
    telegram_state
        .store_cancel_token(session_id, cancel_token.clone())
        .await;

    let chat_id_str = chat_id.0.to_string();
    let result = if track_push_turn {
        // Push-initiated wake (bg-resume completion, stranded flush, boot
        // re-delivery): tracked with origin `system` so a kill mid-tool
        // leaves a boot-visible row (#12). The prompt IS the original push
        // text for push wakes, so it persists correctly.
        agent
            .send_push_turn(
                session_id,
                prompt,
                None,
                Some(cancel_token.clone()),
                None, // no approval callback for resume
                Some(progress_cb),
                "telegram",
                Some(&chat_id_str),
                None, // boot re-delivery routing stays chat-level (#12)
            )
            .await
    } else {
        agent
            .resume_interrupted_turn(
                session_id,
                prompt,
                None,
                Some(cancel_token.clone()),
                None, // no approval callback for resume
                Some(progress_cb),
                "telegram",
                Some(&chat_id_str),
            )
            .await
    };

    telegram_state.remove_cancel_token(session_id).await;
    edit_cancel.cancel();
    // Await edit loop to prevent race where it sends a NEW message after
    // we grab streaming_msg_id (causes duplicate completion).
    if let Err(e) = edit_loop_handle.await {
        tracing::warn!(error = %e, "Telegram resume edit loop task panicked");
    }

    // ── Final delivery ─────────────────────────────────────────────────────
    let (streaming_msg_id, remaining_display) = {
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        let display: Vec<DisplayItem> = std::mem::take(&mut s.display_queue);
        (s.msg_id, display)
    };

    if cancel_token.is_cancelled() {
        tracing::info!(
            "Telegram: resume for session {} cancelled by new message",
            session_id
        );
        // Only delete the streaming placeholder — keep prior
        // intermediate + tool-call history visible. See the matching
        // block in handle_message() for rationale.
        if let Some(mid) = streaming_msg_id {
            if adopted_answer {
                // #2010: this bubble is the partial answer the killed process
                // left on screen, not something this resume created. Deleting
                // user-visible history that predates the process is the worse
                // failure: a stale bubble is recoverable by hand, a deleted one
                // is gone (the same keep-never-discard call #1462 made for
                // rows). Leave it, and let the next message answer it.
                tracing::info!(
                    "Telegram: resume {} cancelled; keeping adopted bubble {mid} from the interrupted turn",
                    session_id
                );
            } else {
                best_effort_delete(&bot, chat_id, mid, "streaming teardown").await;
            }
        }
        return Ok(());
    }

    // Send remaining display items through the ONE shared drain (#470).
    // Resume has no inbound message to react to.
    drain_remaining_display(
        &bot,
        chat_id,
        thread_id,
        &streaming,
        remaining_display,
        None,
    )
    .await;

    // ── Final response ────────────────────────────────────────────────────
    // ONE delivery path (#471 phase 3): resume rides the exact same
    // deliver_final_response as live turns. Deliberate unifications vs the
    // old copy, all previously hand-maintained drift:
    // - the ctx footer renders in <i> like live turns (was <sub> here)
    // - the intermediate dedup uses the shared normalized matching
    // - group history records the bot reply (the old copy skipped it)
    // inbound=None: the original message id is lost across restarts, so
    // reactions strip without firing and reply anchoring is skipped —
    // identical to the old resume behavior.
    let voice_config = Config::current().voice_config();
    let channel_msg_repo = ChannelMessageRepository::new(agent.context().pool().clone());
    let is_dm = chat_id.0 > 0;
    // Settled header outcome for the flow block (#480), same classification
    // as the main handler: computed before `result` moves into delivery,
    // stamped after so it renders on the block's final shape.
    let flow_outcome = match &result {
        Ok(_) => FlowOutcome::Finished,
        Err(e) => crate::channels::background_work::outcome_for_error(e),
    };
    if !super::handler::deliver_final_response(
        &bot,
        chat_id,
        None,
        thread_id,
        &streaming,
        session_id,
        &agent,
        &telegram_state,
        &channel_msg_repo,
        &voice_config,
        false,
        is_dm,
        "unknown",
        streaming_msg_id,
        result,
    )
    .await?
    {
        return Ok(());
    }

    // Resume parity with the main handler: settle the flow header once the
    // final delivery has left the block in its final shape.
    {
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        s.flow_outcome = Some(flow_outcome);
        let (bg_indicator, bg_count) = super::handler::bg_indicator_for(&agent, session_id);
        s.bg_indicator = bg_indicator;
        s.bg_count = bg_count;
        // Sub-agent counts ride the same settle stamp as the crash-resume
        // path's background tasks (#1183 parity with handle_message).
        s.subagent_counts = super::handler::subagent_counts_for(&agent, session_id);
    }
    // Recompute sections at settle so the plan Approve/Discard keyboard, which
    // attaches only at turn end (#571), materializes on the final render — the
    // main handler does the same right after stamping the outcome.
    super::flow_chrome::refresh_sections(&streaming, &agent, session_id).await;
    // Crash-resume settle render (#1211 G2 Final): queued, never dropped.
    refresh_flow(&bot, chat_id, &streaming, super::governor::EditClass::Final).await;
    // #1377: if a flow card survived to settle, register its state handle so
    // later background-task completions fold their acks into THIS card
    // instead of spraying standalone bubbles. Overwritten by the next
    // settle; cleared on teardown (delivery.rs).
    if streaming
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .open_group_msg_id
        .is_some()
    {
        telegram_state
            .register_flow_state(session_id, Arc::clone(&streaming))
            .await;
    }
    // Settle the plan card too (#580): final checklist state + the Approve/
    // Discard keyboard, which is now gated to turn end on the card.
    let plan_kb = {
        streaming
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sections
            .plan_kb
    };
    super::plan_card::refresh_plan_card(
        &bot,
        chat_id,
        thread_id,
        &telegram_state,
        &agent,
        session_id,
        plan_kb,
    )
    .await;

    // Render buffered follow-up suggestions LAST (#724), same as handle_message:
    // buttons must be the final message in the chat, and a tap always resolves
    // to a live entry (#723). The merge host comes from deliver_final_response's
    // capture (#tg-suggest-merge) — attaching the keyboard to the answer bubble
    // kills the separate suggestions bubble on resumed turns too.
    let suggestions = streaming
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_suggestions
        .take();
    if let Some(options) = suggestions {
        // #31: the sign-off trailer rides to render_suggestions with the
        // merge host — same last-out-of-the-flow ordering as handle_message.
        let (merge_host, trailer) = {
            let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
            (s.final_bubble.take(), s.pending_trailer.take())
        };
        super::suggest_options::render_suggestions(
            &bot,
            &telegram_state,
            session_id,
            chat_id,
            thread_id,
            options,
            merge_host,
            trailer,
        )
        .await;
    }

    Ok(())
}

/// Producer stamped into a `[session-notify from=…]` header.
///
/// Cross-session pushes name the real sender session uuid (the agent tool,
/// #1203) — the echo humanizes it via [`sender_label`]. The CLI lane (#23)
/// has NO sender session (the verb runs as a separate process), so it
/// stamps `cli:<label>` ([`CLI_SENDER_PREFIX`]); the echo renders the
/// carried label verbatim instead of a session lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotifySender<'a> {
    /// A real sender session (`from=<uuid>`).
    Session(Uuid),
    /// The CLI lane (`from=cli:<label>`, #23): the label renders verbatim.
    CliTooling(&'a str),
}

/// Split the mechanical `[session-notify from=<uuid>]` header (#1203) off a
/// push's context text. Absent or malformed header → `(None, whole text)`.
/// A `cli:`-prefixed sender (#23) yields [`NotifySender::CliTooling`]
/// carrying the label after the prefix; an EMPTY label is malformed and
/// falls through to `(None, whole text)`.
pub(crate) fn split_notify_header(context_text: &str) -> (Option<NotifySender<'_>>, &str) {
    let trimmed = context_text.trim_start();
    let Some(after_open) = trimmed.strip_prefix("[session-notify from=") else {
        return (None, trimmed);
    };
    let Some(close) = after_open.find(']') else {
        return (None, trimmed);
    };
    let rest = after_open[close + 1..].trim_start();
    let candidate = &after_open[..close];
    if let Some(label) = candidate.strip_prefix(CLI_SENDER_PREFIX) {
        let label = label.trim();
        if !label.is_empty() {
            return (Some(NotifySender::CliTooling(label)), rest);
        }
    }
    match Uuid::parse_str(candidate) {
        Ok(sender) => (Some(NotifySender::Session(sender)), rest),
        Err(_) => (None, trimmed),
    }
}

/// Strip the synthetic `[System: ...]` framing (constructors in
/// `brain/agent/service` terminate the block with `]`). Any other shape
/// passes through untouched.
#[cfg(test)]
pub(crate) fn strip_system_framing(text: &str) -> &str {
    let trimmed = text.trim();
    trimmed
        .strip_prefix("[System:")
        .and_then(|s| s.strip_suffix(']'))
        .map(str::trim)
        .unwrap_or(trimmed)
}

/// Split a push's text into `(sender, clean_body)`: notify header and System
/// framing both removed. `sender` is `None` for plain background-task pushes.
///
/// The push body is producer-controlled scaffolding around user-facing
/// content: a `[System: ...]` block opened at the start of the body is the
/// frame (often multi-line, ending in `]`), and everything after the block
/// is the real payload. Strip the wrapper, keep inner content + tail (#1225
/// leftover: the squash shipped tests asserting this shape but the old
/// whole-string-only strip could never satisfy them).
pub(crate) fn split_bg_echo_parts(context_text: &str) -> (Option<NotifySender<'_>>, String) {
    let (sender, rest) = split_notify_header(context_text);
    (sender, strip_push_scaffolding(rest))
}

/// Aggressive scaffolding strip for push bodies: if the text opens with a
/// `[System:` block that terminates with `]`, drop the wrapper and join the
/// inner content with whatever follows. Unterminated blocks pass through
/// whole (cannot tell frame from payload). Contrast [`strip_system_framing`],
/// the conservative variant that only unwraps a whole-string wrap.
fn strip_push_scaffolding(rest: &str) -> String {
    let trimmed = rest.trim_start();
    match trimmed.strip_prefix("[System:") {
        Some(s) => match s.find(']') {
            Some(end) => {
                let inner = s[..end].trim();
                let tail = s[end + 1..].trim();
                if tail.is_empty() {
                    inner.to_owned()
                } else {
                    format!("{inner}\n{tail}")
                }
            }
            None => rest.to_owned(),
        },
        None => rest.to_owned(),
    }
}

/// The wire a wake bubble's rich leg rides (#38, re-landed by #85).
///
/// `<details>` chrome cards MUST use the HTML input mode: the markdown rich
/// mode cannot express the collapsible (rich/api.rs dialect rule; the #421
/// revert proved that route ships flat), and routing those cards through
/// the #1234 markdown outbox is what leaked escaped tags into the chat
/// (issue #38, 2026-08-29 — the outbox's internal fallback re-escaped the
/// wrapper into visible `<details>` text). Plain markdown bubbles keep the
/// canonical outbox — tables and fences stay native there. Senders branch
/// on this enum; see the bg-resume call site.
pub(crate) enum BubbleWire {
    /// HTML input mode via `rich::api::send_rich_html_id` (#420 path A).
    Html(String),
    /// Canonical markdown outbox via `send::send_markdown_outbox` (#1234).
    Markdown(String),
}

/// Assemble the echo bubble from the clean body + title. Returns the
/// rich-capable markdown on the canonical markdown-outbox wire and the
/// classic HTML blockquote fallback. Raw text is truncated BEFORE
/// conversion so the wrapper tags stay well-formed — cutting rendered HTML
/// can split a tag and make Telegram strip the formatting entirely
/// (plan_card lesson).
pub(crate) fn build_bg_echo_bubble(body: &str, title: &str) -> (BubbleWire, String) {
    // #490: keep BOTH ends. The head-only cut dropped the tail, which is where
    // a reader's own blocks live; the marker it leaves names the character
    // count that went, so the ending is visibly missing rather than silently.
    let (body, dropped) =
        crate::utils::string::truncate_chars_tail_preserving(body, ECHO_BODY_CAP_CHARS);
    // Plain suffix, deliberately WITHOUT the count: the body already carries
    // `… (truncated N chars) …` at the cut itself, and repeating N here would
    // re-create the double-reporting #490 exists to remove. The count lives
    // once, adjacent to the material it describes; this is the headline nudge.
    let suffix = if dropped > 0 {
        " (truncated)".to_string()
    } else {
        String::new()
    };
    let markdown = format!("{title}{suffix}\n\n{body}");
    // The title is dynamic (sender label / task display line): escape it for
    // the HTML dialect so a `<` in a label can't corrupt the wrapper.
    let html = format!(
        "<blockquote expandable><b>{}{}</b>\n{}</blockquote>",
        super::markdown::escape_html(title),
        suffix,
        super::rich::markdown_to_html(&body),
    );
    (BubbleWire::Markdown(markdown), html)
}

/// The bg-receipt tail fence (#15): three backticks normally, but ONE MORE
/// than the longest backtick run when the tail itself carries a ``` run, so
/// raw output can never escape the code block and corrupt the card.
fn receipt_fence(tail: &str) -> String {
    let (mut longest, mut run) = (0usize, 0usize);
    for ch in tail.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(if longest >= 3 { longest + 1 } else { 3 })
}

/// Pure line builder for the folded ack (#1377): same shapes as the receipt
/// card summary — backtick-stripped label (empty → "background task"),
/// humanized duration, first-line tail preview. Empty tail drops the
/// preview suffix entirely.
pub(crate) fn bg_ack_line(meta: &crate::brain::agent::BgTaskMeta) -> String {
    let icon = if meta.success { "✅" } else { "❌" };
    let stripped = meta.label.replace('`', "");
    let label = stripped.trim();
    let label = if label.is_empty() {
        "background task"
    } else {
        label
    };
    let duration = background_tasks::format_elapsed(meta.elapsed_secs);
    let preview = first_line_preview(&meta.tail);
    if preview.is_empty() {
        format!("{icon} `{label}` 🕒 {duration}")
    } else {
        format!("{icon} `{label}` 🕒 {duration} · {preview}")
    }
}

/// Pure state mutation for the fold (#1377): append the ack line as system
/// chrome (never reclaimed as the turn's answer, #1253) and re-stamp the
/// header counters. Refuses (returns false, touches nothing) when the state
/// has no card — the caller then falls back to the standalone bubble lane.
pub(crate) fn apply_bg_ack_fold(
    s: &mut crate::channels::telegram::flow::StreamingState,
    line: String,
    bg_indicator: Option<String>,
    bg_count: Option<usize>,
) -> bool {
    if s.open_group_msg_id.is_none() {
        return false;
    }
    s.flow_entries
        .push(crate::channels::telegram::flow::FlowEntry::System(line));
    s.bg_indicator = bg_indicator;
    s.bg_count = bg_count;
    true
}

/// Fold a bare background-task ack into the session's settled flow card
/// (#1377). Appends ONE system line (`✅ \`label\` 🕒 duration · preview`) to
/// the collapsible block, re-stamps the header counters from the live
/// registry (so "⏳ Waiting for N background tasks" decrements and flips to
/// "✅ Finished" at zero), and re-renders the card in place via the normal
/// governor path (Status class: droppable under flood, self-healing next
/// tick). Returns false — caller falls back to the standalone bubble — when
/// the session has no registered settled card or the card was torn down.
pub(crate) async fn fold_bg_ack_into_flow_card(
    bot: &Bot,
    chat_id: i64,
    state: &Arc<TelegramState>,
    agent: &Arc<AgentService>,
    session_id: Uuid,
    meta: &crate::brain::agent::BgTaskMeta,
) -> bool {
    let Some(streaming) = state.flow_state_for(session_id).await else {
        return false;
    };
    let line = bg_ack_line(meta);
    let (bg_indicator, bg_count) = super::delivery::bg_indicator_for(agent, session_id);
    let folded = {
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        apply_bg_ack_fold(&mut s, line, bg_indicator, bg_count)
    };
    if !folded {
        return false;
    }
    super::flow::refresh_flow(
        bot,
        teloxide::types::ChatId(chat_id),
        &streaming,
        super::governor::EditClass::Status,
    )
    .await;
    true
}

/// Background-task receipt card (#15, owner-locked shape P3f): ONE collapsed
/// `<details>`. Summary = `<sub>{✅|❌} <code>{label}</code> 🕒 {duration}</sub>`
/// — icon by exit 0 / non-zero as the sole outcome signal (no exit code, no
/// wording), label = the roster `short_label` form in monospace. Body = the
/// output tail verbatim inside `<pre>`. Rich leg rides the HTML wire
/// ([`BubbleWire::Html`]: send_rich_html_id parses the wrapper into a
/// native collapsible — the markdown rich mode cannot, #38/#85), visually
/// identical to the owner-approved prototypes (topic 31847); classic
/// blockquote is the degraded leg.
pub(crate) fn build_bg_receipt_card(
    meta: &crate::brain::agent::BgTaskMeta,
) -> (BubbleWire, String) {
    let icon = if meta.success { "✅" } else { "❌" };
    // The label sits inside an inline-code span: backticks would escape the
    // span and break the summary, so they are stripped.
    let stripped = meta.label.replace('`', "");
    let label = stripped.trim();
    let label = if label.is_empty() {
        "background task"
    } else {
        label
    };
    let duration = background_tasks::format_elapsed(meta.elapsed_secs);
    let flat_title = format!("{icon} {label} 🕒 {duration}");

    // #38 empty-body guard: a whitespace-only tail leaves nothing inside
    // the <details> wrapper and Telegram rejects the whole card with 400
    // RICH_MESSAGE_EMPTY (3 events on 2026-08-29). Emit a flat one-line
    // card instead — nothing to reject, no wrapper to leak on any wire.
    if meta.tail.trim().is_empty() {
        let markdown = format!("{icon} `{label}` 🕒 {duration}");
        let classic = format!(
            "<b>{icon} {} 🕒 {duration}</b>",
            super::markdown::escape_html(label)
        );
        return (BubbleWire::Markdown(markdown), classic);
    }

    // Rich leg: HTML input mode, where <details><summary> parses into a
    // native RichBlockDetails. <pre> replaces the markdown fence —
    // containment comes from the tag, so receipt_fence's backtick
    // arms-race only survives on the classic leg below.
    let rich_html = format!(
        "<details><summary><sub>{icon} <code>{}</code> 🕒 {duration}</sub></summary>\n\
         <pre>{}</pre>\n</details>",
        super::markdown::escape_html(label),
        super::markdown::escape_html(&meta.tail)
    );
    // Degraded path: same content as a classic blockquote (non-collapsible
    // on this wire), computed up front and sent when the rich call fails —
    // LIVE since #38: send_rich_html_id returns Err to the caller instead
    // of being swallowed inside the outbox (#1234 fallback discipline).
    let fence = receipt_fence(&meta.tail);
    let fenced_body = format!("{fence}\n{tail}\n{fence}", tail = meta.tail);
    let (_, classic_html) = build_bg_echo_bubble(&fenced_body, &flat_title);
    (BubbleWire::Html(rich_html), classic_html)
}

/// The notify-card peek (#15 amendment): the body's first line, truncated
/// ~45 chars + ellipsis. Deterministic at compose time — no new metadata.
fn first_line_preview(body: &str) -> String {
    let line = body.lines().next().unwrap_or("").trim();
    if line.chars().count() > 45 {
        format!("{}…", crate::utils::string::truncate_chars(line, 45))
    } else {
        line.to_string()
    }
}

/// Session-notify receipt card (#15 amendment, owner-locked shape N4): ONE
/// collapsed `<details>`. Summary = `<sub>📨 From <b>{sender}</b>: {preview}</sub>`
/// — fixed 📨 (notifies carry no success/failure semantics), sender label in
/// bold, preview = truncated first line of the body. Body = the notify
/// content rendered from markdown (prose and pipe tables, no code fence —
/// a notify is a document, not a log) on the HTML wire ([`BubbleWire::Html`]),
/// where the wrapper parses into a native collapsible (#38/#85; the
/// markdown rich mode cannot).
pub(crate) async fn build_notify_receipt_card(
    sender_label: &str,
    body: &str,
) -> (BubbleWire, String) {
    // The sender sits inside a <b> tag: angle brackets are neutralized so a
    // label containing '<' cannot open a tag and corrupt the summary.
    let sanitized = sender_label.replace('<', "‹").replace('>', "›");
    let sender = sanitized.trim();
    let sender = if sender.is_empty() {
        "session notify"
    } else {
        sender
    };
    // #38 empty-body guard: whitespace-only body = an empty card inside the
    // wrapper = 400 RICH_MESSAGE_EMPTY (same defect class as the bg card,
    // issue #38). Flat one-line card instead — no wrapper to reject.
    if body.trim().is_empty() {
        let markdown = format!("📨 From **{sender}**");
        let classic = format!("📨 From <b>{sender}</b>");
        return (BubbleWire::Markdown(markdown), classic);
    }
    let preview = first_line_preview(body);
    // #490: this body renders onto the RICH Html wire below (Telegram's ~32K
    // `sendRichMessage`), so it takes the rich budget, not the classic one.
    // The classic fallback is built by `build_bg_echo_bubble`, which applies
    // the classic budget itself — that call is handed the body UNCAPPED so
    // each leg gets exactly one cut, on the leg that needs it.
    let (rich_body, rich_dropped) =
        crate::utils::string::truncate_chars_tail_preserving(body, ECHO_BODY_CAP_CHARS_RICH);
    let rich_suffix = if rich_dropped > 0 {
        " (truncated)".to_string()
    } else {
        String::new()
    };
    // Body rendered from markdown with <p> wrapping — the rich HTML dialect
    // chrome surfaces use (#1142); mermaid fences resolve exactly like the
    // final-reply path, gated so a fence-less body costs no HTTP.
    let body_html = super::rich::markdown_to_html_mermaid_p(&rich_body).await;
    // The preview is body-derived: escape it, a `<` in the source must not
    // open a tag inside the summary.
    let rich_html = format!(
        "<details><summary><sub>📨 From <b>{sender}</b>: {}</sub></summary>\n\n\
         {body_html}{rich_suffix}\n\n</details>",
        super::markdown::escape_html(&preview)
    );
    let flat_title = format!("📨 From {sender}: {preview}");
    // #490: the classic fallback takes the ORIGINAL body and applies the
    // classic budget itself (inside `build_bg_echo_bubble`) — so THIS leg
    // gets exactly one cut and one marker, instead of the old re-cap that
    // fed an already-cut body back through the same guard.
    let (_, classic_html) = build_bg_echo_bubble(body, &flat_title);
    (BubbleWire::Html(rich_html), classic_html)
}

/// #61: hard cap for the folded notify roll line (chars, not bytes) —
/// one long notify title must not blow out the roll block.
pub(crate) const NOTIFY_ROLL_LINE_MAX: usize = 160;

/// #61: the compact roll line for a folded notify —
/// `📨 notify from <label>: <first body line>`. The label gets the same
/// angle-bracket neutralization the receipt card applies (topic/chat names
/// are user data; roll chrome renders verbatim). The first body line
/// carries the gist — the FULL notify text still reaches the session via
/// the queue below; this line is only the announcement. Capped so one
/// long title cannot blow out the roll block.
/// Strip a leading self-echo from a notify body's first line: a sender
/// (or a hand-typed probe) that pastes its own "📨 notify from …:" header
/// into the payload would otherwise render the phrase twice — the envelope
/// already carries the label (Alexey 2026-09-05, r4 smoke duplication).
/// Strips the header-shaped prefix up to the first ':'; a matching line
/// with no ':' is left alone (can't tell header from prose).
fn strip_leading_notify_echo(first: &str) -> &str {
    let Some(rest) = first.strip_prefix("📨 notify from") else {
        return first;
    };
    match rest.find(':') {
        Some(idx) => rest[idx + 1..].trim_start(),
        None => first,
    }
}

pub(crate) fn build_notify_roll_line(label: &str, body: &str) -> String {
    let sanitized = label.replace('<', "‹").replace('>', "›");
    let first = body.lines().next().map(str::trim).unwrap_or("");
    let first = strip_leading_notify_echo(first);
    let line = format!("📨 notify from {sanitized}: {first}");
    if line.chars().count() > NOTIFY_ROLL_LINE_MAX {
        let mut cut: String = line.chars().take(NOTIFY_ROLL_LINE_MAX).collect();
        cut.push('…');
        cut
    } else {
        line
    }
}

/// #61 fold-dedupe fingerprint: tagged sender identity + the FULL body.
/// Two deliveries of the same notify produce the same fingerprint; any
/// body or sender difference produces a new one. The tag byte keeps a
/// session uuid apart from a CLI label spelling the same text.
/// DefaultHasher is deliberate: this is per-session display dedup, not a
/// security boundary.
pub(crate) fn notify_fingerprint(sender: &NotifySender<'_>, body: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match sender {
        NotifySender::Session(u) => {
            0u8.hash(&mut hasher);
            u.as_bytes().hash(&mut hasher);
        }
        NotifySender::CliTooling(label) => {
            1u8.hash(&mut hasher);
            label.hash(&mut hasher);
        }
    }
    body.hash(&mut hasher);
    hasher.finish()
}

/// #61: fold a session-notify announcement into the session's LIVE flow
/// roll. Returns `true` when the line was folded — the caller then skips
/// the standalone #1221/#1225 bubble. Deliberate fallbacks (all →
/// `false`, bubble unchanged): no parseable sender (the bubble's
/// defensive arm owns that), no registered live flow (session idle), or
/// the roll block no longer open (registration proves a turn is in
/// flight, not that the block survived — `open_group_msg_id` re-checked
/// under the streaming lock is the real gate; the lock is scoped and
/// never held across the fold await). The notify CONTENT is not consumed
/// here: the caller's normal queue/resume logic delivers it; this only
/// replaces the announcement. Fold-dedupe (Alexey 2026-09-06): a notify
/// already folded for this session within the TTL window also returns
/// `true` WITHOUT re-rendering — same bubble-skip effect, nothing new on
/// the roll (`TelegramState::note_notify_fold`).
pub(crate) async fn fold_notify_into_roll(
    state: &TelegramState,
    bot: &teloxide::Bot,
    session_id: Uuid,
    chat_id: i64,
    thread_id: Option<teloxide::types::ThreadId>,
    context_text: &str,
) -> bool {
    let (sender, body) = split_bg_echo_parts(context_text);
    let Some(sender) = sender else {
        return false;
    };
    let Some(handle) = state.live_flow(session_id) else {
        return false;
    };
    let roll_open = {
        let s = handle.streaming.lock().unwrap_or_else(|e| e.into_inner());
        s.open_group_msg_id
    };
    if roll_open.is_none() {
        return false;
    }
    // Fold-dedupe (Alexey 2026-09-06): the same notify delivered twice to
    // one session folds ONCE — the 776a1fe5 incident rendered one notify
    // in two concurrent rolls. Check-and-record is atomic under the state
    // lock, so racing folds can't both pass. A duplicate returns `true`
    // (keeps the caller's bubble gated off) without re-rendering; the
    // content still reaches the session via the normal queue path.
    let fingerprint = notify_fingerprint(&sender, &body);
    if state.note_notify_fold(session_id, fingerprint) {
        tracing::debug!(
            "[bg-resume] #61: duplicate notify fold suppressed for session {session_id}"
        );
        return true;
    }
    let label = match sender {
        NotifySender::Session(s) => sender_label(state, bot, s, chat_id).await,
        // CLI lane (#23): no sender session to humanize — carried label
        // renders verbatim (build_notify_roll_line neutralizes brackets).
        NotifySender::CliTooling(l) => l.to_string(),
    };
    let line = build_notify_roll_line(&label, &body);
    super::flow::append_system_to_flow(
        bot,
        teloxide::types::ChatId(chat_id),
        thread_id,
        &handle.streaming,
        &line,
    )
    .await;
    tracing::info!(
        "[bg-resume] #61: notify from session {session_id} folded into the live flow roll"
    );
    true
}

/// Bubble header for a background-task echo: reuse the producer's display
/// line, which already names the task ("🔧 background task finished:
/// <label>") — the generic "⚙️ background task result" said nothing about
/// which task woke the session (Alexey 2026-08-26). Blank display → generic
/// fallback; overlong labels are capped so the header stays readable.
pub(crate) fn background_task_title(display_text: &str) -> String {
    let t = display_text.trim();
    if t.is_empty() {
        "⚙️ background task result".to_owned()
    } else {
        crate::utils::string::truncate_chars(t, 120).to_owned()
    }
}

/// Human-readable sender label for a session_notify push (Alexey's rule):
/// sender chat == recipient chat → the sender's topic name; a DM session
/// (positive chat id) → the BOT's username from the startup get_me cache
/// (the reader IS the chat's human side — handing their own name back as
/// the sender is useless); a different non-forum chat → chat name; a
/// different forum → chat + topic name. The card summary renders it as
/// `From <name>:`. Topic names come from the local
/// `channel_messages.topic_name` store (the Bot API has no method to query
/// them), chat names from `getChat`. Best-effort: any lookup failure falls
/// back to the short session id (the same prefix `session_search list`
/// displays).
pub(crate) async fn sender_label(
    state: &TelegramState,
    bot: &teloxide::Bot,
    sender: Uuid,
    recipient_chat: i64,
) -> String {
    let sender_chat = state.session_chat(sender).await;
    let sender_topic = state.session_topic(sender).await;
    let api_url = bot.api_url().to_string();
    let token = bot.token().to_owned();
    let label = match sender_chat {
        None => None,
        Some(sc) if sc == recipient_chat => match sender_topic {
            Some(tid) => local_topic_name(sc, tid).await,
            None => None,
        },
        // DM with the bot (positive chat id = private chat): label the bot
        // itself from the startup get_me cache — zero network. An empty
        // cache (shouldn't happen post-boot) falls through to the short-id
        // fallback below.
        Some(sc) if sc > 0 => state.bot_username().await,
        Some(sc) => {
            let chat =
                super::titles::chat_title(&api_url, &token, teloxide::types::ChatId(sc)).await;
            match (chat, sender_topic) {
                (Some(c), Some(tid)) => match local_topic_name(sc, tid).await {
                    Some(t) => Some(format!("{c} / {t}")),
                    None => Some(c),
                },
                (Some(c), None) => Some(c),
                (None, _) => None,
            }
        }
    };
    label.unwrap_or_else(|| short_session_id(sender))
}

/// Forum-topic display name, resolved LOCALLY: the Bot API has no method to
/// query a topic's title (`getForumTopic` does not exist — bots can only
/// learn names passively; telegram-bot-api issues #634/#356). The handler
/// already stores every observed topic name in `channel_messages.topic_name`,
/// so the card builder reads the newest one for (chat, thread). Any miss —
/// DB unavailable, topic never observed, empty name — feeds the short-id
/// fallback in `sender_label` like every other lookup failure.
async fn local_topic_name(chat_id: i64, thread_id: i32) -> Option<String> {
    let pool = crate::db::global_pool()?;
    let repo = ChannelMessageRepository::new(pool.clone());
    repo.latest_topic_name("telegram", &chat_id.to_string(), &thread_id.to_string())
        .await
        .ok()
        .flatten()
}

/// Short session id: first 8 hex chars of the uuid (matches `session_search`
/// list's short-id prefix display).
pub(crate) fn short_session_id(uuid: Uuid) -> String {
    uuid.simple().to_string()[..8].to_owned()
}

/// How recently (seconds) a Telegram-bound session must have been active to
/// appear in the boot-time wake log (#1227, #244).
///
/// Window for recently active session recovery pass (60 minutes / 3600 seconds, #244).
/// Captures sessions across longer CI cycles, tool intervals, and daemon restarts.
pub const WAKE_RECENT_SECS: i64 = 3600;

/// Log every Telegram-bound session that was active shortly before boot but
/// is not currently being resumed, so the stranded set is auditable (#1227).
///
/// The on-disk journal only rescues turns that were literally mid-loop at the
/// kill instant; between-turn sessions have no row (#1224 re-registers their
/// delivery routes, which drains parked reports, but that happens quietly).
/// This pass names them in the log instead: it runs no turn, re-executes
/// nothing, and sends no message — the former "I'm back" bubble was removed
/// per #34 (owner direction 2026-08-29: remove the UX message, leave
/// logging), because it promised resumption the feature does not yet deliver
/// and was never persisted in `channel_messages`, so it could not be audited.
///
/// Sessions whose ids are in `already_resumed` are skipped: those were roused
/// by a full continuation prompt via `resume_session` already. Returns how
/// many sessions landed in the log.
pub async fn wake_recently_active(
    pool: crate::db::Pool,
    already_resumed: &std::collections::HashSet<Uuid>,
) -> usize {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let since_epoch = now.saturating_sub(WAKE_RECENT_SECS);
    let repo = crate::db::SessionBindingRepository::new(pool);
    let Ok(bindings) = repo.recent_for_channel("telegram", since_epoch).await else {
        tracing::warn!(target: "telegram", "Boot wake could not read recent session bindings");
        return 0;
    };

    let mut stranded: Vec<String> = Vec::new();
    for b in bindings {
        let Ok(sid) = Uuid::parse_str(&b.session_id) else {
            continue;
        };
        if already_resumed.contains(&sid) {
            continue;
        }
        stranded.push(short_session_id(sid));
    }

    let scheduled = stranded.len();
    if scheduled > 0 {
        tracing::info!(
            target: "telegram",
            "Boot wake pass (log-only, #34): recently-active sessions not resumed: [{}]",
            stranded.join(",")
        );
        tracing::info!(
            target: "telegram",
            "Scheduled boot wake for {scheduled} recently-active session(s) (#1227)"
        );
    }
    scheduled
}
