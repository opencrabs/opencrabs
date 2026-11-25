//! The few values a surface reads off the agent service on every frame.
//!
//! The window asks for the working directory, the context limit and the
//! provider while it renders. Doing an operation per frame would be absurd,
//! so they travel together as [`Facts`]: one read, cached by the surface,
//! refreshed when an event says they moved.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{BackendError, Ctx, Op, Scope};

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
    /// The model the session's provider actually sends, read with the
    /// provider as one coherent pair (#2264). A sticky fallback that does not
    /// carry the session's pick reports the model it remapped to here, while
    /// `model` still names the pick. With no session, the crab-wide model.
    pub served_model: String,
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
            served_model: service.provider_pair_for_session(id).1,
            base_context_tokens: service.base_context_tokens(),
        },
        None => Facts {
            working_directory: crate::utils::cwd::launch_cwd(),
            context_limit: service.context_limit(),
            provider: service.provider_name(),
            model: service.provider_model(),
            served_model: service.provider_model(),
            base_context_tokens: service.base_context_tokens(),
        },
    };
    Ok(facts)
}

/// What a `/clear` leaves behind: the system prompt and the tool schemas,
/// in tokens.
///
/// Not a field of [`Facts`]: counting it tokenises every tool schema, so the
/// window asks only while the context rung is lit, and the other rungs never
/// pay for it. Answers `NotReady` while the crab is still booting, the moment
/// the window used to find no service and printed 0.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextBase {}

impl ContextBase {
    /// The count, now. Synchronous, for the frame that draws the hint row.
    pub fn answer(&self, ctx: &Ctx) -> Result<u32, BackendError> {
        Ok(ctx.service()?.base_context_tokens_raw())
    }
}

impl Op for ContextBase {
    const NAME: &'static str = "facts.context_base";
    const SCOPE: Scope = Scope::Read;
    type Reply = u32;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        self.answer(ctx)
    }
}

ops! { ContextBase }

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
        assert_eq!(
            (facts.provider.clone(), facts.served_model.clone()),
            service.provider_pair_for_session(session)
        );
        assert_eq!(facts.base_context_tokens, service.base_context_tokens());
    }

    /// With no conversation open the footer names the launch directory.
    #[tokio::test]
    async fn without_a_session_the_directory_is_the_launch_one() {
        let ctx = booted_ctx().await;
        let facts = facts_for(&ctx, None).expect("facts");
        assert_eq!(facts.working_directory, crate::utils::cwd::launch_cwd());
    }

    #[tokio::test]
    async fn context_base_is_the_services_raw_floor() {
        let ctx = booted_ctx().await;
        let service = ctx.service().expect("booted");
        assert_eq!(
            ContextBase {}.answer(&ctx),
            Ok(service.base_context_tokens_raw())
        );
        let unbooted = Ctx::new(PathBuf::from("unused.db"));
        assert_eq!(ContextBase {}.answer(&unbooted), Err(BackendError::NotReady));
    }

    #[tokio::test]
    async fn context_base_has_parity() {
        use crate::backend::parity::{Gateway, forbidden, parity};

        // The gateway crab has no agent service: both doors answer
        // `NotReady`, the state in which the window prints 0.
        let gateway = Gateway::start().await;
        let answer = parity!(gateway, ContextBase {});
        assert_eq!(answer, serde_json::json!({ "Err": { "kind": "not_ready" } }));
        forbidden!(gateway, ContextBase {});
    }

    #[test]
    fn facts_survive_the_wire() {
        let facts = Facts {
            working_directory: PathBuf::from("/home/crab/project"),
            context_limit: 200_000,
            provider: "anthropic".into(),
            model: "claude-x".into(),
            served_model: "claude-x".into(),
            base_context_tokens: 0,
        };
        let json = serde_json::to_string(&facts).expect("encode");
        let back: Facts = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, facts, "{json}");
    }
}
