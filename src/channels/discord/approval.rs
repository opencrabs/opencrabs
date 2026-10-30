//! Per-approval oneshot register so a button interaction can resolve the
//! tool-approval request that is blocking the agent loop.
//!
//! The sender carries `(approved, always)`, mirroring the TUI's Yes /
//! Always / No choices.

use tokio::sync::oneshot;

use super::DiscordState;

impl DiscordState {
    /// Register a pending approval oneshot channel.
    pub async fn register_pending_approval(&self, id: String, tx: oneshot::Sender<(bool, bool)>) {
        self.pending_approvals.lock().await.insert(id, tx);
    }

    /// Resolve a pending approval. Returns true if one existed.
    pub async fn resolve_pending_approval(&self, id: &str, approved: bool, always: bool) -> bool {
        if let Some(tx) = self.pending_approvals.lock().await.remove(id) {
            let _ = tx.send((approved, always));
            true
        } else {
            false
        }
    }

    /// Drop a registration without answering it (#2194).
    ///
    /// `resolve_pending_approval` only runs when someone clicks a button, so
    /// every approval that timed out or lost its channel left its sender in
    /// the map forever. The waiter calls this on the way out of every path,
    /// click or not. Returns true when the entry was still there, which is the
    /// normal case for a timeout.
    pub async fn drop_pending_approval(&self, id: &str) -> bool {
        self.pending_approvals.lock().await.remove(id).is_some()
    }
}
