//! Per-session cancel tokens so `/stop` can abort an in-flight agent call.

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::WhatsAppState;

impl WhatsAppState {
    /// Store a cancel token for a session (before starting agent call).
    ///
    /// Overwrites any prior token WITHOUT cancelling it (#2179, #652's
    /// WhatsApp twin). A turn is already running for this session when a
    /// second message arrives, and `try_begin_turn` queues that message for
    /// the live turn instead of starting a second one, so the cancel here was
    /// redundant and harmful: it hard-killed the user's genuinely in-flight
    /// turn, dropping the running request and the answer with it. Genuine
    /// cancellation stays in `cancel_session` (`/stop`, a provider swap).
    pub async fn store_cancel_token(&self, session_id: Uuid, token: CancellationToken) {
        self.cancel_tokens.lock().await.insert(session_id, token);
    }

    /// Cancel and remove the token for a session. Returns true if a token existed.
    pub async fn cancel_session(&self, session_id: Uuid) -> bool {
        if let Some(token) = self.cancel_tokens.lock().await.remove(&session_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Remove the cancel token after the agent call completes (cleanup).
    /// Only removes if the stored token is already cancelled — prevents a
    /// finishing old call from removing a newer call's live token.
    pub async fn remove_cancel_token(&self, session_id: Uuid) {
        let mut tokens = self.cancel_tokens.lock().await;
        if let Some(token) = tokens.get(&session_id)
            && token.is_cancelled()
        {
            tokens.remove(&session_id);
        }
    }
}
