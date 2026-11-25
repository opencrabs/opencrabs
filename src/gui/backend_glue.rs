//! The window's handle on a crab.
//!
//! The window never opens the ledger or holds the agent service by itself:
//! it keeps a [`Backend`] and asks it. Either this process's own crab, or
//! another one over its gateway (`backend_remote.rs`). The callers do not
//! change between the two, because both answer the same [`Op`] values.

use std::path::PathBuf;
use std::sync::Arc;

use opencrabs::backend::{BackendError, Ctx, Facts, Op};
use uuid::Uuid;

use crate::agent_bridge::EventSink;

#[path = "backend_remote.rs"]
mod remote;

pub use remote::{RemoteCrab, Watch};

/// Which crab the window talks to.
#[derive(Clone)]
pub enum Backend {
    /// This process's own crab.
    Local(Arc<Ctx>),
    /// Another crab, reached through its gateway.
    #[allow(dead_code)] // the instance rail (C3) builds it when a crab is picked
    Remote(Arc<RemoteCrab>),
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
    ///
    /// A remote crab's ledger is not on this machine, so a remote backend
    /// answers with the window's own: right for what belongs to the window
    /// (dismissed hints), and for the crab's data the reason those callers
    /// still have to move behind an `Op`.
    pub fn local_db_path(&self) -> PathBuf {
        match self {
            Backend::Local(ctx) => ctx.db_path().to_path_buf(),
            Backend::Remote(crab) => crab.window_ledger().to_path_buf(),
        }
    }

    /// Run `op` and wait for its reply.
    ///
    /// Locally the op runs on its own lane of `spawn_worker`, named after the
    /// op: a 4 MB stack and a `current_thread` runtime built before the job
    /// is sent, so every ledger connect the op makes has a reactor (#2973,
    /// #3033). Remotely it is one `gui/call` on the transport's runtime. The
    /// window only awaits a oneshot, which needs no reactor.
    pub async fn call<O: Op>(&self, op: O) -> Result<O::Reply, BackendError> {
        let (tx, rx) = futures::channel::oneshot::channel();
        self.spawn(op, move |reply| {
            let _ = tx.send(reply);
        })
        .map_err(BackendError::Failed)?;
        rx.await
            .map_err(|_| BackendError::Failed(format!("{} worker dropped its result", O::NAME)))?
    }

    /// Run `op` off the window's thread and hand its reply to `done`, on the
    /// same lane `call` uses. For the `spawn_*` doors that report through the
    /// event sink instead of awaiting. `Err` means the job never reached a
    /// worker, so `done` will not run: the caller reports that itself, as it
    /// did for a failed thread spawn.
    pub fn spawn<O: Op>(
        &self,
        op: O,
        done: impl FnOnce(Result<O::Reply, BackendError>) + Send + 'static,
    ) -> Result<(), String> {
        match self {
            Backend::Local(ctx) => {
                let ctx = ctx.clone();
                crate::agent_bridge::spawn_worker(O::NAME, move |runtime| {
                    done(runtime.block_on(op.run(&ctx)));
                })
            }
            Backend::Remote(crab) => remote::spawn(crab.clone(), op, done),
        }
    }

    /// The per-frame facts for `session`, or `None` while the crab is still
    /// booting. Synchronous: the window reads it while it renders.
    #[allow(dead_code)] // no window caller yet: the footer still reads the service
    pub fn facts(&self, session: Option<Uuid>) -> Option<Facts> {
        match self {
            Backend::Local(ctx) => opencrabs::backend::facts_for(ctx, session).ok(),
            // The window reads facts every frame and a remote read is a round
            // trip, so a remote crab answers from a cache (C4). Until that
            // cache exists it has none, which reads as "still booting".
            Backend::Remote(_) => None,
        }
    }

    /// Feed `session`'s events into `sink` until the returned [`Watch`]
    /// drops. A local crab has nothing to open: its agent service already
    /// forwards every event into the window's sink, so it answers `None`.
    /// `Err` means the reader's thread never started.
    #[allow(dead_code)] // the instance rail (C3) opens one with a remote session
    pub fn watch(&self, session: Uuid, sink: EventSink) -> Result<Option<Watch>, String> {
        match self {
            Backend::Local(_) => Ok(None),
            Backend::Remote(crab) => remote::watch(crab.clone(), session, sink).map(Some),
        }
    }
}
