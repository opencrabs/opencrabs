//! Per-session commands: create, delete, clear, model, cd, undo.
//!
//! Operations land here as their GUI doors move behind the contract.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::services::{MessageService, ServiceContext};

use super::{BackendError, Ctx, Op, Scope};

/// Delete one session's messages, as the TUI's ctrl+k does. The session row
/// stays, so the conversation is kept and only its history clears (#2430).
///
/// The service example: the work goes through a crab service built on the
/// ledger, not through a repository by hand. Its failures keep the exact
/// `clear: {e}` text the window showed, which is why it opens the ledger
/// through [`Ctx::database`] rather than [`Ctx::db`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearMessages {
    /// The session whose messages go.
    pub session_id: Uuid,
}

impl Op for ClearMessages {
    const NAME: &'static str = "session_ops.clear_messages";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("clear: {e}")))?;
        let service = MessageService::new(ServiceContext::new(db.pool().clone()));
        service
            .delete_messages_for_session(self.session_id)
            .await
            .map_err(|e| BackendError::Failed(format!("clear: {e}")))
    }
}

ops! { ClearMessages }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ctx::temp_ctx;
    use crate::db::models::{Message, Session};
    use crate::db::repository::{MessageRepository, SessionRepository};

    #[tokio::test]
    async fn clear_messages_empties_the_transcript_and_keeps_the_session() {
        let (_dir, ctx) = temp_ctx().await;
        let db = ctx.db().await.expect("db");
        let sessions = SessionRepository::new(db.pool().clone());
        let messages = MessageRepository::new(db.pool().clone());
        let session = Session::new(Some("chat".into()), None, None);
        sessions.create(&session).await.expect("create session");
        let message = Message::new(session.id, "user".into(), "hello".into(), 1);
        messages.create(&message).await.expect("create message");

        ClearMessages {
            session_id: session.id,
        }
        .run(&ctx)
        .await
        .expect("clear");

        let left = messages
            .find_by_session(session.id)
            .await
            .expect("read back");
        assert!(left.is_empty(), "{} messages left", left.len());
        assert!(
            sessions
                .find_by_id(session.id)
                .await
                .expect("read session")
                .is_some(),
            "the session row stays"
        );
    }
}
