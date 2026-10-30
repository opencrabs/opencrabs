//! The WhatsApp session turn slot and the mid-turn follow-up queue (#2179).
//!
//! Telegram's `active_turns` twin (#501) and Discord's `turns.rs` (#1990).
//! WhatsApp shipped with neither half of the pair. `store_cancel_token`
//! cancelled the token it found, so a second inbound message on a busy
//! session killed the live turn: three messages in six seconds produced one
//! answer, to the last one, and the two turns it killed left a compaction
//! summary of one turn's context applied to another.
//!
//! Removing that cancel alone moves the failure instead of fixing it. With
//! no gate the second message forks a second tool loop onto the same
//! session: two turns append to one transcript, only the last stored token
//! is reachable by /stop, and the overlap is exactly what produced the
//! mismatched compaction snapshot. So the slot and the queue ship together,
//! the same way they did on Discord.

use std::sync::Arc;

use uuid::Uuid;

use super::WhatsAppState;
use crate::brain::agent::AgentService;
use crate::brain::agent::{MessageQueueCallback, QueuedUserMessage};

/// Keeps a session marked "turn active" for its lifetime (#2179). Held for
/// the whole inbound turn and released on drop, so the next claimer (a later
/// inbound message, or the flush of this turn's own leftovers) can take the
/// slot. RAII, so every exit path clears it, including an early return.
pub(crate) struct ActiveTurnGuard {
    state: Arc<WhatsAppState>,
    session_id: Uuid,
}

impl Drop for ActiveTurnGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = self.state.active_turns.lock() {
            set.remove(&self.session_id);
        }
    }
}

impl WhatsAppState {
    /// Atomically begin a turn for `session_id` (#2179). Under ONE lock:
    /// `Some(guard)` and the session is marked active when no turn runs,
    /// `None` when one already does (the caller then queues its message as a
    /// follow-up instead of forking a second loop). This closes the
    /// check-then-act window a separate "is it idle?" read would leave.
    pub(crate) fn try_begin_turn(self: &Arc<Self>, session_id: Uuid) -> Option<ActiveTurnGuard> {
        let mut set = self.active_turns.lock().ok()?;
        if !set.insert(session_id) {
            // Already present: a turn is in flight for this session.
            return None;
        }
        Some(ActiveTurnGuard {
            state: self.clone(),
            session_id,
        })
    }

    /// Queue a follow-up for a session whose slot is held (#2179).
    pub(crate) fn enqueue_turn_message(&self, session_id: Uuid, msg: QueuedUserMessage) {
        // Durable twin (#111): the in-memory queue dies with the process, the
        // row survives it. Best-effort, the same rail Telegram and Discord
        // ride; a no-runtime context (sync tests) skips the row.
        crate::brain::agent::service::notify_queue::persist(session_id, &msg);
        match self.turn_queue.lock() {
            Ok(mut map) => map.entry(session_id).or_default().push_back(msg),
            Err(e) => {
                // The message is gone at this point, so say so loudly rather
                // than dropping a user's message without a trace.
                tracing::error!(
                    "WhatsApp: could not queue a follow-up for session {session_id}, \
                     it is dropped: {e}"
                );
            }
        }
    }

    /// Take EVERYTHING queued for `session_id` (FIFO), folded into ONE joined
    /// message (#1837's rule: one round-end answers a whole burst, never N
    /// injections for N rapid follow-ups). Returns `None` when nothing is
    /// queued; removing the entry up front means an empty queue leaves no
    /// husk.
    pub(crate) fn drain_turn_messages(&self, session_id: Uuid) -> Option<QueuedUserMessage> {
        let mut map = self.turn_queue.lock().ok()?;
        let queue = map.remove(&session_id)?;
        // Delivered into a turn (#111): the durable twin is redundant from
        // here, for EVERY item. A failed clear costs a next-boot duplicate,
        // never a loss.
        for msg in &queue {
            crate::brain::agent::service::notify_queue::clear_on_delivery(session_id, msg);
        }
        let msgs: Vec<QueuedUserMessage> = queue.into_iter().collect();
        QueuedUserMessage::join(&msgs)
    }

    /// True when something is queued for `session_id`. One lock check, used by
    /// the end-of-turn flush to skip spawning on the common empty case.
    pub(crate) fn has_queued_turn_message(&self, session_id: Uuid) -> bool {
        self.turn_queue
            .lock()
            .map(|map| map.get(&session_id).is_some_and(|q| !q.is_empty()))
            .unwrap_or(false)
    }

    /// A [`MessageQueueCallback`] that drains this state's follow-up queue,
    /// keyed per session. Wired into the WhatsApp `AgentService`
    /// (manager.rs, #2179) so the tool loop injects what arrived mid-turn
    /// between rounds, the same rail Telegram has run since #302.
    pub(crate) fn followup_queue_callback(self: &Arc<Self>) -> MessageQueueCallback {
        let state = self.clone();
        Arc::new(move |session_id: Uuid| {
            let state = state.clone();
            Box::pin(async move { state.drain_turn_messages(session_id) })
        })
    }
}

/// Flush anything that landed during the FINAL round of a finished turn into
/// a fresh turn (#2179).
///
/// The between-rounds drain is the primary path; this covers the message that
/// arrives after the last tool round, which no further drain follows. Empty is
/// the common case and costs one lock check.
///
/// The caller must have dropped the turn guard first, so this fresh turn can
/// claim the slot. Spawned, so the finished turn returns immediately instead
/// of blocking on a whole new inference.
pub(crate) fn flush_after_turn(
    state: Arc<WhatsAppState>,
    agent: Arc<AgentService>,
    session_id: Uuid,
) {
    if !state.has_queued_turn_message(session_id) {
        return;
    }
    tokio::spawn(async move {
        let Some(joined) = state.drain_turn_messages(session_id) else {
            return;
        };
        // Someone else may have claimed the slot between the guard dropping
        // and this task running: put the message back rather than forking.
        let Some(guard) = state.try_begin_turn(session_id) else {
            state.enqueue_turn_message(session_id, joined);
            return;
        };
        let Some(jid_str) = state.session_jid(session_id).await else {
            tracing::warn!(
                "WhatsApp: queued follow-up for session {session_id} has no chat jid; dropping"
            );
            return;
        };
        let Ok(jid) = jid_str.parse::<wacore_binary::jid::Jid>() else {
            tracing::warn!("WhatsApp: bad jid '{jid_str}' for queued follow-up; dropping");
            return;
        };
        let context_text = joined.context_text;
        let result = agent
            .send_push_turn(
                session_id,
                context_text,
                None,
                None,
                None,
                None,
                "whatsapp",
                Some(&jid_str),
                None,
            )
            .await;
        drop(guard);
        match result {
            Ok(response) => {
                let Some(client) = state.client().await else {
                    tracing::warn!(
                        "WhatsApp: flushed follow-up answered but the transport is down; \
                         the answer for session {session_id} was not delivered"
                    );
                    return;
                };
                let (text, _imgs) = crate::utils::extract_img_markers(&response.content);
                // Outbound text goes through the paired strip+redact, not the
                // bare artifact strip: an answer can echo a key it read (#2196).
                let text = crate::utils::sanitize::sanitize_outbound(&text);
                if text.trim().is_empty() {
                    return;
                }
                for chunk in super::handler::split_message(&text, 4000) {
                    let out = waproto::whatsapp::Message {
                        conversation: Some(chunk.to_string()),
                        ..Default::default()
                    };
                    if let Err(e) = client.send_message(jid.clone(), out).await {
                        tracing::warn!(
                            "WhatsApp: failed to deliver the flushed follow-up answer: {e}"
                        );
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "WhatsApp: flushed follow-up turn failed for session {session_id}: {e}"
                );
            }
        }
    });
}
