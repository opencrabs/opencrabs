//! Telegram forum topics.
//!
//! Operations land here as their GUI doors move behind the contract.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{BackendError, Ctx, Op, Scope};

/// Create a forum topic when `parent` is a Telegram forum (#2603).
///
/// `Ok(Some(id))` is the new topic row. `Ok(None)` means this parent is not a
/// Telegram forum (a local conversation, a DM, a plain group), so the caller
/// keeps its ordinary local topic behaviour. `Err` means it IS a forum and the
/// create failed: the caller shows it and never falls back to a local row.
///
/// The library's forum-topic path calls the Bot API first and keys the row on
/// the thread id Telegram returns. A crab built without the Telegram channel
/// has no forum, so it answers `Ok(None)` without opening the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateForumTopic {
    /// The conversation the topic goes under.
    pub parent: Uuid,
    /// The name the user typed.
    pub name: String,
}

impl Op for CreateForumTopic {
    const NAME: &'static str = "telegram_topics.create_forum_topic";
    const SCOPE: Scope = Scope::Manage;
    type Reply = Option<Uuid>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        #[cfg(feature = "telegram")]
        {
            let db = ctx.db().await?;
            crate::channels::telegram::topic_create::create_forum_topic_session(
                db.pool(),
                self.parent,
                &self.name,
            )
            .await
            .map(|created| created.map(|topic| topic.session))
            .map_err(BackendError::Failed)
        }
        #[cfg(not(feature = "telegram"))]
        {
            let _ = (ctx, self);
            Ok(None)
        }
    }
}

ops! { CreateForumTopic }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};
    use crate::db::models::Session;
    use crate::db::repository::SessionRepository;

    #[tokio::test]
    async fn create_forum_topic_has_parity() {
        // A plain local conversation is no forum: both doors answer `None`
        // and neither writes a row or reaches the Bot API.
        let gateway = Gateway::start().await;
        let db = gateway.ctx.db().await.expect("db");
        let repo = SessionRepository::new(db.pool().clone());
        let parent = Session::new(Some("local chat".into()), None, None);
        repo.create(&parent).await.expect("create");

        let answer = parity!(
            gateway,
            CreateForumTopic {
                parent: parent.id,
                name: "plans".into(),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": null }));

        forbidden!(
            gateway,
            CreateForumTopic {
                parent: parent.id,
                name: "refused".into(),
            }
        );
    }
}
