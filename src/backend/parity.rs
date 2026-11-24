//! The parity harness: one operation, two doors, one answer.
//!
//! [`Gateway`] boots a crab in process on a fresh, migrated ledger under a
//! temp home (never the real `~/.opencrabs`). `parity!` runs an operation by
//! the local door (`Op::run` on the `Ctx`) and by the remote door, and fails
//! unless both answer the same JSON. `forbidden!` proves the remote door
//! refuses a caller that lacks the operation's scope.
//!
//! In F4 the remote door is [`dispatch_as`] with the params and the answer
//! each encoded to JSON text and decoded back, which is everything the wire
//! does to a value. When the HTTP transport lands it replaces
//! [`Gateway::remote`] and the macros stay as they are.

use std::future::Future;
use std::path::Path;

use serde_json::Value;

use super::{BackendError, Ctx, Op, Scope, dispatch_as};

/// A crab running in this process, for parity tests.
pub(crate) struct Gateway {
    home: tempfile::TempDir,
    pub(crate) ctx: Ctx,
}

impl Gateway {
    /// A crab on its own temp home with a migrated ledger inside it.
    pub(crate) async fn start() -> Self {
        let home = tempfile::tempdir().expect("temp home");
        let ctx = Ctx::new(home.path().join("opencrabs.db")).with_home(home.path().to_path_buf());
        ctx.db()
            .await
            .expect("connect the gateway ledger")
            .run_migrations()
            .await
            .expect("migrate the gateway ledger");
        Self { home, ctx }
    }

    /// This crab's home.
    pub(crate) fn home(&self) -> &Path {
        self.home.path()
    }

    /// Run `fut` with this crab's home as the profile home, so anything an
    /// operation resolves through `opencrabs_home()` lands in the temp dir.
    pub(crate) async fn within<T>(&self, fut: impl Future<Output = T>) -> T {
        crate::config::profile::with_home_override_async(self.home().to_path_buf(), fut).await
    }

    /// The local door: the operation run directly on the crab.
    pub(crate) async fn local<O: Op>(&self, op: O) -> Result<O::Reply, BackendError> {
        self.within(op.run(&self.ctx)).await
    }

    /// The remote door: params and answer cross as JSON text, and the
    /// caller holds only `granted`.
    pub(crate) async fn remote<O: Op>(
        &self,
        op: O,
        granted: &[Scope],
    ) -> Result<O::Reply, BackendError> {
        let wire = serde_json::to_string(&op).expect("params encode");
        let params: Value = serde_json::from_str(&wire).expect("params decode");
        let answer = self
            .within(dispatch_as(&self.ctx, granted, O::NAME, params))
            .await;
        let wire = serde_json::to_string(&answer).expect("answer encode");
        let answer: Result<Value, BackendError> =
            serde_json::from_str(&wire).expect("answer decode");
        answer.and_then(|value| {
            serde_json::from_value(value)
                .map_err(|e| BackendError::BadParams(format!("{}: reply decode: {e}", O::NAME)))
        })
    }

    /// Run `op` by both doors and fail unless they answer the same JSON.
    /// Returns that answer for the test to look at. Run against one crab in
    /// turn, so a write must answer the same when repeated.
    pub(crate) async fn assert_parity<O: Op + Clone>(&self, op: O) -> Value {
        let local = self.local(op.clone()).await;
        let remote = self.remote(op, Scope::ALL).await;
        let local = serde_json::to_value(&local).expect("local answer encode");
        let remote = serde_json::to_value(&remote).expect("remote answer encode");
        assert_eq!(
            local,
            remote,
            "{}: the local and the remote door disagree",
            O::NAME
        );
        local
    }

    /// Fail unless the remote door refuses `op` to a caller holding no scope,
    /// and to one holding every scope but the one it needs.
    pub(crate) async fn assert_forbidden<O: Op + Clone>(&self, op: O) {
        let all_but: Vec<Scope> = Scope::ALL
            .iter()
            .copied()
            .filter(|scope| *scope != O::SCOPE)
            .collect();
        for granted in [Vec::new(), all_but] {
            let answer = self.remote(op.clone(), &granted).await;
            assert_eq!(
                answer.err(),
                Some(BackendError::Forbidden { scope: O::SCOPE }),
                "{} ran for a caller holding {granted:?}",
                O::NAME
            );
        }
    }
}

/// `parity!(gateway, op)`: the local and the remote door give one answer.
macro_rules! parity {
    ($gateway:expr, $op:expr $(,)?) => {
        $gateway.assert_parity($op).await
    };
}

/// `forbidden!(gateway, op)`: without the op's scope the remote door refuses.
macro_rules! forbidden {
    ($gateway:expr, $op:expr $(,)?) => {
        $gateway.assert_forbidden($op).await
    };
}

#[allow(unused_imports)]
pub(crate) use {forbidden, parity};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::session_ops::ClearMessages;
    use crate::backend::sessions::{LoadSessions, RenameSession};
    use crate::db::models::{Message, Session};
    use crate::db::repository::{MessageRepository, SessionRepository};

    async fn sessions(gateway: &Gateway) -> SessionRepository {
        let db = gateway.ctx.db().await.expect("db");
        SessionRepository::new(db.pool().clone())
    }

    #[tokio::test]
    async fn the_gateway_never_uses_the_real_home() {
        let gateway = Gateway::start().await;
        let home = gateway
            .within(async { crate::config::opencrabs_home() })
            .await;
        assert_eq!(home, gateway.home());
        assert!(gateway.ctx.db_path().starts_with(gateway.home()));
        assert_eq!(gateway.ctx.home(), Some(gateway.home()));
    }

    #[tokio::test]
    async fn load_sessions_has_parity() {
        let gateway = Gateway::start().await;
        let repo = sessions(&gateway).await;
        for title in ["one", "two"] {
            repo.create(&Session::new(Some(title.into()), None, None))
                .await
                .expect("create");
        }
        let answer = parity!(gateway, LoadSessions {});
        let rows = answer["Ok"].as_array().expect("a list of sessions");
        assert_eq!(rows.len(), 2, "{answer}");
        forbidden!(gateway, LoadSessions {});
    }

    #[tokio::test]
    async fn rename_session_has_parity() {
        let gateway = Gateway::start().await;
        let repo = sessions(&gateway).await;
        let session = Session::new(Some("before".into()), None, None);
        repo.create(&session).await.expect("create");

        parity!(
            gateway,
            RenameSession {
                id: session.id,
                title: "after".into(),
            }
        );
        let title = |repo: &SessionRepository| {
            let repo = repo.clone();
            async move {
                repo.find_by_id(session.id)
                    .await
                    .expect("read back")
                    .expect("the row")
                    .title
            }
        };
        assert_eq!(title(&repo).await.as_deref(), Some("after"));

        forbidden!(
            gateway,
            RenameSession {
                id: session.id,
                title: "refused".into(),
            }
        );
        assert_eq!(
            title(&repo).await.as_deref(),
            Some("after"),
            "a refused rename wrote nothing"
        );
    }

    #[tokio::test]
    async fn clear_messages_has_parity() {
        let gateway = Gateway::start().await;
        let repo = sessions(&gateway).await;
        let db = gateway.ctx.db().await.expect("db");
        let messages = MessageRepository::new(db.pool().clone());
        let session = Session::new(Some("chat".into()), None, None);
        repo.create(&session).await.expect("create");
        let seed = || Message::new(session.id, "user".into(), "hello".into(), 1);
        messages.create(&seed()).await.expect("message");

        parity!(
            gateway,
            ClearMessages {
                session_id: session.id,
            }
        );
        assert!(
            messages
                .find_by_session(session.id)
                .await
                .expect("read back")
                .is_empty()
        );

        messages.create(&seed()).await.expect("message again");
        forbidden!(
            gateway,
            ClearMessages {
                session_id: session.id,
            }
        );
        assert_eq!(
            messages
                .find_by_session(session.id)
                .await
                .expect("read back")
                .len(),
            1,
            "a refused clear deleted nothing"
        );
    }

    #[tokio::test]
    async fn params_that_do_not_decode_are_bad_params() {
        let gateway = Gateway::start().await;
        let err = super::super::dispatch(
            &gateway.ctx,
            RenameSession::NAME,
            serde_json::json!({ "id": "not a uuid" }),
        )
        .await
        .expect_err("the params are not a RenameSession");
        assert!(matches!(err, BackendError::BadParams(_)), "{err:?}");
    }
}
