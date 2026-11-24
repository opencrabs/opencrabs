//! The window's handle on a crab.
//!
//! The window never opens the ledger or holds the agent service by itself:
//! it keeps a [`Backend`] and asks it. Today the only backend is this
//! process's own crab. A remote one lands next to it without the callers
//! changing, because both answer the same [`Op`] values.

use std::path::PathBuf;
use std::sync::Arc;

use opencrabs::backend::{BackendError, Ctx, Facts, Op};
use uuid::Uuid;

/// Which crab the window talks to.
#[derive(Clone)]
pub enum Backend {
    /// This process's own crab.
    Local(Arc<Ctx>),
}

impl Backend {
    /// A local backend on the ledger at `db_path`, with no service yet and
    /// not recorded as the primary: the helper tests reach a ledger through.
    #[cfg(test)]
    pub fn local(db_path: PathBuf) -> Self {
        Backend::Local(Arc::new(Ctx::new(db_path)))
    }

    /// The ledger file of a local crab.
    ///
    /// A bridge for functions that still open the ledger themselves: each
    /// one takes a `Backend` and reads its path on the first line. The count
    /// of callers only goes down as those functions move behind an `Op`.
    pub fn local_db_path(&self) -> PathBuf {
        match self {
            Backend::Local(ctx) => ctx.db_path().to_path_buf(),
        }
    }

    /// Run `op` and wait for its reply.
    ///
    /// Locally the op runs on its own lane of `spawn_worker`, named after the
    /// op: a 4 MB stack and a `current_thread` runtime built before the job
    /// is sent, so every ledger connect the op makes has a reactor (#2973,
    /// #3033). The window only awaits a oneshot, which needs no reactor.
    #[allow(dead_code)] // the first caller is the LoadSessions example
    pub async fn call<O: Op>(&self, op: O) -> Result<O::Reply, BackendError> {
        match self {
            Backend::Local(ctx) => {
                let ctx = ctx.clone();
                let (tx, rx) = futures::channel::oneshot::channel();
                crate::agent_bridge::spawn_worker(O::NAME, move |runtime| {
                    let reply = runtime.block_on(op.run(&ctx));
                    let _ = tx.send(reply);
                })
                .map_err(BackendError::Failed)?;
                rx.await.map_err(|_| {
                    BackendError::Failed(format!("{} worker dropped its result", O::NAME))
                })?
            }
        }
    }

    /// The per-frame facts for `session`, or `None` while the crab is still
    /// booting. Synchronous: the window reads it while it renders.
    #[allow(dead_code)] // no window caller yet: the footer still reads the service
    pub fn facts(&self, session: Option<Uuid>) -> Option<Facts> {
        match self {
            Backend::Local(ctx) => opencrabs::backend::facts_for(ctx, session).ok(),
        }
    }
}
