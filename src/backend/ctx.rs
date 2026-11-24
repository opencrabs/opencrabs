//! The crab an operation runs against: its ledger and, once booted, its
//! agent service.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::brain::agent::AgentService;
use crate::db::{Database, Pool};

use super::BackendError;

/// Everything an operation may touch on this crab.
///
/// The ledger is reached in one of two ways. A `Ctx` built with a pool hands
/// out that pool. A `Ctx` built from a path alone connects by path on every
/// [`Ctx::db`], which is what the window's loads have always done on their own
/// thread and runtime: keeping that shape is what keeps a local call from
/// changing behaviour.
pub struct Ctx {
    db_path: PathBuf,
    pool: Option<Pool>,
    service: OnceLock<Arc<AgentService>>,
    home: Option<PathBuf>,
}

impl Ctx {
    /// A crab reached by ledger path, with no service yet.
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            pool: None,
            service: OnceLock::new(),
            home: None,
        }
    }

    /// Share an open pool instead of connecting per call.
    pub fn with_pool(mut self, pool: Pool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Pin the home directory this crab reads its files from.
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    /// The ledger file.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// The home this crab was pinned to, when it was.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// The ledger. The shared pool when there is one, otherwise a fresh
    /// connection by path: pragmas only, never migrations, the same door the
    /// window's reads use.
    pub async fn db(&self) -> Result<Database, BackendError> {
        if let Some(pool) = &self.pool {
            return Ok(Database {
                pool: pool.clone(),
                db_path: Some(self.db_path.to_string_lossy().into_owned()),
            });
        }
        Database::connect(&self.db_path)
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }

    /// Hand the booted agent service to this crab. The first one wins: a
    /// crab has one service for its whole life.
    pub fn set_service(&self, service: Arc<AgentService>) {
        let _ = self.service.set(service);
    }

    /// The agent service, or `NotReady` before boot finished.
    pub fn service(&self) -> Result<&Arc<AgentService>, BackendError> {
        self.service.get().ok_or(BackendError::NotReady)
    }
}

static PRIMARY: OnceLock<Arc<Ctx>> = OnceLock::new();

/// Record this process's own crab. Set once at boot; a later call is ignored
/// so nothing can swap the ledger under a running window.
pub fn set_primary(ctx: Arc<Ctx>) {
    let _ = PRIMARY.set(ctx);
}

/// This process's own crab, once boot recorded it.
pub fn primary() -> Option<Arc<Ctx>> {
    PRIMARY.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_is_not_ready_before_boot() {
        let ctx = Ctx::new(PathBuf::from("unused.db"));
        assert!(matches!(ctx.service(), Err(BackendError::NotReady)));
    }

    #[tokio::test]
    async fn a_path_ctx_connects_to_its_own_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ctx.db");
        let ctx = Ctx::new(path.clone());
        assert_eq!(ctx.db_path(), path.as_path());
        let db = ctx.db().await.expect("a fresh file connects");
        // The pool opens lazily: the file appears with the first connection,
        // not with `connect`.
        let conn = db.pool().get().await.expect("a pooled connection");
        drop(conn);
        assert!(
            path.exists(),
            "the first connection created the ledger file"
        );
    }
}
