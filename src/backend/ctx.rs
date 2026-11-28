//! The crab an operation runs against: its ledger and, once booted, its
//! agent service.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use uuid::Uuid;

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
    turns: Arc<Mutex<HashSet<Uuid>>>,
}

impl Ctx {
    /// A crab reached by ledger path, with no service yet.
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            pool: None,
            service: OnceLock::new(),
            home: None,
            turns: Arc::default(),
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
    /// window's reads use. A failed connect reads `{e:#}`, the text the
    /// window's loads have always shown.
    pub async fn db(&self) -> Result<Database, BackendError> {
        self.database()
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }

    /// The same door as [`Ctx::db`], with the connect error untouched, for an
    /// operation whose moved body words its own failure (`clear: {e}`).
    pub async fn database(&self) -> anyhow::Result<Database> {
        if let Some(pool) = &self.pool {
            return Ok(Database {
                pool: pool.clone(),
                db_path: Some(self.db_path.to_string_lossy().into_owned()),
            });
        }
        Database::connect(&self.db_path).await
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

    /// Take `session` for one turn, or `Busy` when a turn already holds it.
    ///
    /// A turn is in flight when this crab already handed out a claim for the
    /// session, or when the channel that owns it says so through its turn
    /// probe (the gate `session/notify` reads). The claim lasts until the
    /// returned value drops. While it lives, a session no channel probes reads
    /// as mid-turn to that same gate, so a notify does not land inside it.
    pub fn claim_turn(&self, session: Uuid) -> Result<TurnClaim, BackendError> {
        use crate::brain::agent::service::session_routes::{register_turn_probe, turn_probe};

        let probe = turn_probe(session);
        if probe.as_ref().is_some_and(|mid_turn| mid_turn()) {
            return Err(BackendError::Busy);
        }
        if !lock_turns(&self.turns).insert(session) {
            return Err(BackendError::Busy);
        }
        if probe.is_none() {
            let turns = Arc::clone(&self.turns);
            register_turn_probe(
                session,
                Arc::new(move || lock_turns(&turns).contains(&session)),
            );
        }
        Ok(TurnClaim {
            turns: Arc::clone(&self.turns),
            session,
        })
    }

    /// Take `session` for one window turn, refusing only this crab's own.
    ///
    /// The same claim as [`Ctx::claim_turn`] without its read of another
    /// owner's turn probe: a channel turn mid-flight on `session` does not
    /// refuse it. `Busy` here means this crab already holds a turn on the
    /// session, and nothing else.
    ///
    /// It exists to keep the behaviour the window had before its turns moved
    /// into the crab: a window send ran alongside a channel turn on the same
    /// session. That collision is older than this door and is tracked on its
    /// own issue; until it is settled there, the window's turns must not start
    /// refusing what they used to run. The probe is registered exactly as
    /// `claim_turn` registers it, so the notify gate still reads the claim.
    pub fn claim_own_turn(&self, session: Uuid) -> Result<TurnClaim, BackendError> {
        use crate::brain::agent::service::session_routes::{register_turn_probe, turn_probe};

        if !lock_turns(&self.turns).insert(session) {
            return Err(BackendError::Busy);
        }
        if turn_probe(session).is_none() {
            let turns = Arc::clone(&self.turns);
            register_turn_probe(
                session,
                Arc::new(move || lock_turns(&turns).contains(&session)),
            );
        }
        Ok(TurnClaim {
            turns: Arc::clone(&self.turns),
            session,
        })
    }

    /// Whether this crab holds a turn on `session` right now.
    pub fn turn_in_flight(&self, session: Uuid) -> bool {
        lock_turns(&self.turns).contains(&session)
    }
}

/// A turn's hold on its session. Dropping it frees the session.
#[must_use = "the session is free again as soon as the claim drops"]
pub struct TurnClaim {
    turns: Arc<Mutex<HashSet<Uuid>>>,
    session: Uuid,
}

impl TurnClaim {
    /// The session this claim holds.
    pub fn session(&self) -> Uuid {
        self.session
    }
}

impl Drop for TurnClaim {
    fn drop(&mut self) {
        lock_turns(&self.turns).remove(&self.session);
    }
}

/// The in-flight set. A turn that panicked while holding the lock left the
/// set itself intact, so a poisoned lock is read through rather than wedging
/// every later turn on this crab.
fn lock_turns(turns: &Mutex<HashSet<Uuid>>) -> std::sync::MutexGuard<'_, HashSet<Uuid>> {
    turns.lock().unwrap_or_else(PoisonError::into_inner)
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

/// A crab on a fresh, migrated ledger in its own temp directory. Keep the
/// `TempDir`: dropping it deletes the ledger.
#[cfg(test)]
pub(crate) async fn temp_ctx() -> (tempfile::TempDir, Ctx) {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = Ctx::new(dir.path().join("backend.db"));
    ctx.db()
        .await
        .expect("connect the temp ledger")
        .run_migrations()
        .await
        .expect("migrate the temp ledger");
    (dir, ctx)
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

    #[test]
    fn a_second_turn_on_a_busy_session_is_refused() {
        let ctx = Ctx::new(PathBuf::from("unused.db"));
        let session = Uuid::new_v4();
        let first = ctx
            .claim_turn(session)
            .expect("an idle session takes a turn");
        assert!(ctx.turn_in_flight(session));
        assert!(matches!(ctx.claim_turn(session), Err(BackendError::Busy)));
        drop(first);
        assert!(!ctx.turn_in_flight(session));
        let again = ctx
            .claim_turn(session)
            .expect("a freed session takes a turn");
        assert_eq!(again.session(), session);
    }

    #[test]
    fn a_busy_session_leaves_the_others_free() {
        let ctx = Ctx::new(PathBuf::from("unused.db"));
        let _held = ctx.claim_turn(Uuid::new_v4()).expect("first session");
        assert!(ctx.claim_turn(Uuid::new_v4()).is_ok());
    }

    #[test]
    fn a_channel_mid_turn_makes_the_session_busy() {
        use crate::brain::agent::service::session_routes::register_turn_probe;

        let ctx = Ctx::new(PathBuf::from("unused.db"));
        let session = Uuid::new_v4();
        register_turn_probe(session, Arc::new(|| true));
        assert!(matches!(ctx.claim_turn(session), Err(BackendError::Busy)));
        assert!(!ctx.turn_in_flight(session));
    }

    #[test]
    fn a_claimed_session_reads_mid_turn_to_the_notify_gate() {
        use crate::brain::agent::service::session_routes::turn_probe;

        let ctx = Ctx::new(PathBuf::from("unused.db"));
        let session = Uuid::new_v4();
        let claim = ctx.claim_turn(session).expect("idle session");
        let probe = turn_probe(session).expect("the claim registered a probe");
        assert!(probe());
        drop(claim);
        assert!(!probe());
    }
}
