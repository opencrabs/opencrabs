//! The Discord session turn slot and the mid-turn follow-up queue (#1990).
//!
//! Telegram's `active_turns` / `pending_reactions` twin (#501, #1213, #1837).
//! Two facts made this necessary: `store_cancel_token` used to CANCEL the
//! token it found (`cancel.rs`), so a second inbound message on a busy
//! session killed the live or recovered turn; and the follow-up had nowhere
//! to queue, so it forked a second concurrent tool loop on the same session,
//! whose answer read the first turn's prompt as "another task in flight" and
//! said so (#1990). The store no longer cancels (it only overwrites, #652's
//! Discord twin), but the slot is still what makes the claim atomic: without
//! it two turns run at once and only the last token stored is reachable by
//! /stop. The queue gives the loser somewhere to wait: injected between tool
//! rounds by the queue callback, flushed into a fresh tracked turn by whoever
//! held the slot last.

use std::sync::Arc;
use uuid::Uuid;

use super::DiscordState;
use crate::brain::agent::{MessageQueueCallback, QueuedUserMessage};

/// Keeps a session marked "turn active" for its lifetime (#1990). Held for
/// the whole turn, ingress-born, background-born or boot-born, and released
/// on drop so the next claimer (an inbound message, or the flush of this
/// turn's own leftovers) can take the slot.
pub(crate) struct ActiveTurnGuard {
    state: Arc<DiscordState>,
    session_id: Uuid,
}

impl Drop for ActiveTurnGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = self.state.active_turns.lock() {
            set.remove(&self.session_id);
        }
    }
}

impl DiscordState {
    /// Atomically begin a turn for `session_id` (#501's rule, ported to
    /// Discord by #1990). Under ONE lock: `Some(guard)` and the session is
    /// marked active when no turn runs, `None` when one already does (the
    /// caller then queues its message as a follow-up instead of forking a
    /// second loop). This closes the check-then-act window a separate
    /// "is it idle?" read would leave: the slot could flip busy between
    /// the read and the claim.
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

    /// Queue a follow-up for a session whose slot is held: an inbound
    /// message, or a background completion that lost the claim (#1990).
    pub(crate) fn enqueue_followup(&self, session_id: Uuid, msg: QueuedUserMessage) {
        // Durable twin (#111): the in-memory queue dies with the process,
        // the row survives it. Best-effort, same rail Telegram's enqueue
        // rides; a no-runtime context (sync tests) skips the row and the
        // queue alone holds the message.
        crate::brain::agent::service::notify_queue::persist(session_id, &msg);
        match self.pending_followups.lock() {
            Ok(mut map) => map.entry(session_id).or_default().push_back(msg),
            Err(e) => {
                // The message is gone at this point, and for a background
                // completion that means a result nobody will ever see.
                tracing::error!(
                    "Discord: could not queue a follow-up for session {session_id}, \
                     it is dropped: {e}"
                );
            }
        }
    }

    /// Take EVERYTHING queued for `session_id` (FIFO), folded into ONE
    /// joined message (#1837's rule: one round-end answers a whole burst,
    /// never N injections for N rapid follow-ups). Returns `None` when
    /// nothing is queued; removing the entry up front means an empty queue
    /// leaves no husk.
    pub(crate) fn drain_followups(&self, session_id: Uuid) -> Option<QueuedUserMessage> {
        let mut map = self.pending_followups.lock().ok()?;
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

    /// A [`MessageQueueCallback`] that drains this state's follow-up queue,
    /// keyed per session. Wired into the Discord `AgentService`
    /// (manager.rs, #1990) so the tool loop injects what arrived mid-turn
    /// between rounds, the same rail Telegram has run since #302.
    pub(crate) fn followup_queue_callback(self: &Arc<Self>) -> MessageQueueCallback {
        let state = self.clone();
        Arc::new(move |session_id: Uuid| {
            let state = state.clone();
            Box::pin(async move { state.drain_followups(session_id) })
        })
    }
}
