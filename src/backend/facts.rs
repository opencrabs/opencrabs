//! The few values a surface reads off the agent service on every frame.
//!
//! The window asks for the working directory, the context limit and the
//! provider while it renders. Doing an operation per frame would be absurd,
//! so they travel together as [`Facts`]: one read, cached by the surface,
//! refreshed when an event says they moved.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{BackendError, Ctx};

/// What a surface needs to draw a session's footer and pickers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    /// The folder the crab's tools run in for this session. With no session,
    /// the folder the crab was launched in.
    pub working_directory: PathBuf,
    /// The context-window budget for the session, or the global one.
    pub context_limit: u32,
    /// The provider serving the session, including a sticky fallback.
    pub provider: String,
    /// The model serving the session, including a sticky fallback.
    pub model: String,
    /// The footer floor before the first reply.
    pub base_context_tokens: u32,
}

/// The facts for `session`, or the crab-wide ones when there is none.
///
/// Answers `NotReady` while the crab is still booting, the same moment the
/// window used to find no service.
pub fn facts_for(ctx: &Ctx, session: Option<Uuid>) -> Result<Facts, BackendError> {
    let service = ctx.service()?;
    let facts = match session {
        Some(id) => Facts {
            working_directory: service.get_working_directory_for_session(id),
            context_limit: service.context_limit_for_session(id),
            provider: service.provider_name_for_session(id),
            model: service.provider_model_for_session(id),
            base_context_tokens: service.base_context_tokens(),
        },
        None => Facts {
            working_directory: crate::utils::cwd::launch_cwd(),
            context_limit: service.context_limit(),
            provider: service.provider_name(),
            model: service.provider_model(),
            base_context_tokens: service.base_context_tokens(),
        },
    };
    Ok(facts)
}

ops! {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_are_not_ready_before_boot() {
        let ctx = Ctx::new(std::path::PathBuf::from("unused.db"));
        assert_eq!(facts_for(&ctx, None), Err(BackendError::NotReady));
        assert_eq!(
            facts_for(&ctx, Some(Uuid::new_v4())),
            Err(BackendError::NotReady)
        );
    }

    async fn booted_ctx() -> Ctx {
        let db = crate::db::Database::connect_in_memory()
            .await
            .expect("in-memory ledger");
        db.run_migrations().await.expect("migrate");
        let context = crate::services::ServiceContext::new(db.pool().clone());
        let provider: std::sync::Arc<dyn crate::brain::provider::Provider> =
            std::sync::Arc::new(crate::tests::agent_service_mocks::MockProvider);
        let service = crate::brain::agent::AgentService::new_for_test(provider, context).await;
        let ctx = Ctx::new(PathBuf::from("unused.db"));
        ctx.set_service(std::sync::Arc::new(service));
        ctx
    }

    /// The fact example (`footer_place`): a session's directory is the one
    /// the service holds for that session, not the process's.
    #[tokio::test]
    async fn a_sessions_working_directory_is_its_own() {
        let ctx = booted_ctx().await;
        let service = ctx.service().expect("booted");
        let session = Uuid::new_v4();
        let own = PathBuf::from("/tmp/facts-session-own");
        service.set_session_only_working_directory(session, own.clone());

        let facts = facts_for(&ctx, Some(session)).expect("facts");
        assert_eq!(facts.working_directory, own);
        assert_eq!(
            facts.working_directory,
            service.get_working_directory_for_session(session)
        );
        assert_eq!(facts.context_limit, service.context_limit_for_session(session));
        assert_eq!(facts.provider, service.provider_name_for_session(session));
        assert_eq!(facts.model, service.provider_model_for_session(session));
        assert_eq!(facts.base_context_tokens, service.base_context_tokens());
    }

    /// With no conversation open the footer names the launch directory.
    #[tokio::test]
    async fn without_a_session_the_directory_is_the_launch_one() {
        let ctx = booted_ctx().await;
        let facts = facts_for(&ctx, None).expect("facts");
        assert_eq!(facts.working_directory, crate::utils::cwd::launch_cwd());
    }

    #[test]
    fn facts_survive_the_wire() {
        let facts = Facts {
            working_directory: PathBuf::from("/home/crab/project"),
            context_limit: 200_000,
            provider: "anthropic".into(),
            model: "claude-x".into(),
            base_context_tokens: 0,
        };
        let json = serde_json::to_string(&facts).expect("encode");
        let back: Facts = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, facts, "{json}");
    }
}
