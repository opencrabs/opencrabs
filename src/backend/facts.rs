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
