//! The session rail: listing, naming, topics, favourites and closing.
//!
//! Operations land here as their GUI doors move behind the contract.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::models::Session;
use crate::db::repository::session::{SessionListOptions, SessionRepository};

use super::{BackendError, Ctx, Op, Scope};

/// The hard cap so a long history cannot make the rail unbounded.
pub const RAIL_LIMIT: usize = 200;

/// The rail's session list, newest first, at most [`RAIL_LIMIT`] rows.
///
/// The read example every other load copies: open the ledger through
/// [`Ctx::db`] (pragmas only, never migrations, so a read cannot race the
/// daemon that owns the schema), ask the repository, and carry a failure as
/// its `{e:#}` text, which is what the window showed before.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadSessions {}

impl Op for LoadSessions {
    const NAME: &'static str = "sessions.load";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<Session>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.list(SessionListOptions {
            limit: Some(RAIL_LIMIT),
            ..Default::default()
        })
        .await
        .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// Rename a session or topic. The caller re-reads the rail afterwards.
///
/// The write example: same door as the read, a `Manage` scope because it
/// changes the other crab's ledger, and `()` as the reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameSession {
    /// The session or topic to rename.
    pub id: Uuid,
    /// Its new title, stored as given.
    pub title: String,
}

impl Op for RenameSession {
    const NAME: &'static str = "sessions.rename";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.rename(self.id, &self.title)
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

ops! { LoadSessions, RenameSession }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ctx::temp_ctx;

    #[tokio::test]
    async fn load_sessions_lists_what_the_ledger_holds() {
        let (_dir, ctx) = temp_ctx().await;
        let db = ctx.db().await.expect("db");
        let repo = SessionRepository::new(db.pool().clone());
        let first = Session::new(Some("first".into()), None, None);
        let second = Session::new(Some("second".into()), None, None);
        repo.create(&first).await.expect("create first");
        repo.create(&second).await.expect("create second");

        let rows = LoadSessions {}.run(&ctx).await.expect("load");
        let mut ids: Vec<_> = rows.iter().map(|s| s.id).collect();
        ids.sort();
        let mut want = vec![first.id, second.id];
        want.sort();
        assert_eq!(ids, want);
    }

    #[tokio::test]
    async fn rename_session_writes_the_new_title() {
        let (_dir, ctx) = temp_ctx().await;
        let db = ctx.db().await.expect("db");
        let repo = SessionRepository::new(db.pool().clone());
        let session = Session::new(Some("before".into()), None, None);
        repo.create(&session).await.expect("create");

        RenameSession {
            id: session.id,
            title: "after".into(),
        }
        .run(&ctx)
        .await
        .expect("rename");

        let stored = repo
            .find_by_id(session.id)
            .await
            .expect("read back")
            .expect("the row is still there");
        assert_eq!(stored.title.as_deref(), Some("after"));
    }

    #[tokio::test]
    async fn load_sessions_carries_a_failed_connect_as_text() {
        // A directory where the ledger file should be: the connect cannot
        // open it, and the failure arrives as `Failed`, never a panic.
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = Ctx::new(dir.path().to_path_buf());
        let err = LoadSessions {}.run(&ctx).await.expect_err("a directory is no ledger");
        assert!(matches!(err, BackendError::Failed(_)), "{err:?}");
    }
}
