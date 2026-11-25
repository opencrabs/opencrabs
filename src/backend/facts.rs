//! The few values a surface reads off the agent service on every frame.
//!
//! The window asks for the working directory, the context limit and the
//! provider while it renders. Doing an operation per frame would be absurd,
//! so they travel together as [`Facts`]: one read, cached by the surface,
//! refreshed when an event says they moved.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::tasks::SessionTasks;
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

/// Read [`Facts`] as an operation: what a surface on another machine fills
/// its [`FactsCache`] with. `session: None` asks for the crab-wide facts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadFacts {
    /// The session to read, or none for the crab-wide facts.
    #[serde(default)]
    pub session: Option<Uuid>,
}

impl Op for LoadFacts {
    const NAME: &'static str = "facts.load";
    const SCOPE: Scope = Scope::Read;
    type Reply = Facts;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        facts_for(ctx, self.session)
    }
}

ops! { ContextBase, LoadFacts }

/// The per-frame values of a crab reached over the wire, kept on this side.
///
/// A local crab answers [`facts_for`] and `TasksSnapshot` from memory inside
/// a frame. A remote one would need a round trip, so the surface reads this
/// instead: every read is a lookup and nothing here can reach the network.
/// Whoever owns the cache fills it ([`store_facts`](Self::store_facts),
/// [`store_tasks`](Self::store_tasks)) when a session opens and when an
/// event says the values moved. A value never fetched reads as `None`, the
/// same answer a crab that is still booting gives.
#[derive(Debug, Default)]
pub struct FactsCache {
    inner: Mutex<Cached>,
}

#[derive(Debug, Default)]
struct Cached {
    /// The last facts read, by session (`None` is the crab-wide entry).
    facts: HashMap<Option<Uuid>, Facts>,
    /// The last running tasks read, by session. A session asked about and
    /// found idle is kept with no rows, so it is asked again next refresh.
    tasks: HashMap<Uuid, Vec<super::tasks::TaskRow>>,
}

impl FactsCache {
    /// The cached facts for `session`, or `None` when none were read yet.
    pub fn facts(&self, session: Option<Uuid>) -> Option<Facts> {
        self.inner
            .lock()
            .ok()
            .and_then(|cached| cached.facts.get(&session).cloned())
    }

    /// Keep `facts` as the answer for `session`. `true` when the answer a
    /// reader gets changed, so the surface knows to draw again.
    pub fn store_facts(&self, session: Option<Uuid>, facts: Facts) -> bool {
        let Ok(mut cached) = self.inner.lock() else {
            return false;
        };
        cached.facts.insert(session, facts.clone()) != Some(facts)
    }

    /// The sessions whose facts are cached, the crab-wide entry included.
    pub fn facts_sessions(&self) -> Vec<Option<Uuid>> {
        self.inner
            .lock()
            .map(|cached| cached.facts.keys().copied().collect())
            .unwrap_or_default()
    }

    /// The cached running tasks of `sessions`, in their order, with the
    /// shape `TasksSnapshot` answers: an idle or never read session is left
    /// out, so an empty answer means nothing is known to run.
    pub fn tasks(&self, sessions: &[Uuid]) -> Vec<SessionTasks> {
        let Ok(cached) = self.inner.lock() else {
            return Vec::new();
        };
        sessions
            .iter()
            .filter_map(|session| {
                let tasks = cached.tasks.get(session)?;
                (!tasks.is_empty()).then(|| SessionTasks {
                    session: *session,
                    tasks: tasks.clone(),
                })
            })
            .collect()
    }

    /// Keep the answer of a `TasksSnapshot` over `asked`. A session asked
    /// about and missing from `reply` is idle now. `true` when the rows a
    /// reader gets changed: a session read for the first time and found
    /// idle reads as nothing running before and after, so it is no change.
    pub fn store_tasks(&self, asked: &[Uuid], reply: Vec<SessionTasks>) -> bool {
        let Ok(mut cached) = self.inner.lock() else {
            return false;
        };
        let mut touched: Vec<Uuid> = asked.to_vec();
        touched.extend(reply.iter().map(|row| row.session));
        let before: Vec<Option<Vec<super::tasks::TaskRow>>> = touched
            .iter()
            .map(|session| cached.tasks.get(session).filter(|t| !t.is_empty()).cloned())
            .collect();
        for session in asked {
            cached.tasks.insert(*session, Vec::new());
        }
        for row in reply {
            cached.tasks.insert(row.session, row.tasks);
        }
        touched.iter().zip(before).any(|(session, was)| {
            cached.tasks.get(session).filter(|t| !t.is_empty()).cloned() != was
        })
    }

    /// The sessions whose tasks are cached, idle ones included.
    pub fn tasks_sessions(&self) -> Vec<Uuid> {
        self.inner
            .lock()
            .map(|cached| cached.tasks.keys().copied().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};
    use crate::backend::tasks::{TaskKind, TaskRow};

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
        assert_eq!(
            facts.context_limit,
            service.context_limit_for_session(session)
        );
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
        assert_eq!(
            ContextBase {}.answer(&unbooted),
            Err(BackendError::NotReady)
        );
    }

    #[tokio::test]
    async fn context_base_has_parity() {
        use crate::backend::parity::{Gateway, forbidden, parity};

        // The gateway crab has no agent service: both doors answer
        // `NotReady`, the state in which the window prints 0.
        let gateway = Gateway::start().await;
        let answer = parity!(gateway, ContextBase {});
        assert_eq!(
            answer,
            serde_json::json!({ "Err": { "kind": "not_ready" } })
        );
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

    fn sample(provider: &str) -> Facts {
        Facts {
            working_directory: PathBuf::from("/home/crab/project"),
            context_limit: 200_000,
            provider: provider.into(),
            model: "claude-x".into(),
            served_model: "claude-x".into(),
            base_context_tokens: 0,
        }
    }

    #[tokio::test]
    async fn load_facts_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(gateway, LoadFacts { session: None });
        assert_eq!(answer["Err"]["kind"], "not_ready", "{answer}");
        parity!(
            gateway,
            LoadFacts {
                session: Some(Uuid::new_v4()),
            }
        );
        forbidden!(gateway, LoadFacts { session: None });
    }

    #[test]
    fn load_facts_without_params_asks_for_the_crab_wide_ones() {
        let op: LoadFacts = serde_json::from_value(serde_json::json!({})).expect("decode");
        assert_eq!(op.session, None);
    }

    #[test]
    fn a_cache_never_filled_answers_none() {
        let cache = FactsCache::default();
        assert_eq!(cache.facts(None), None);
        assert_eq!(cache.facts(Some(Uuid::new_v4())), None);
        assert!(cache.tasks(&[Uuid::new_v4()]).is_empty());
    }

    #[test]
    fn cached_facts_are_kept_per_session() {
        let cache = FactsCache::default();
        let session = Uuid::new_v4();
        cache.store_facts(None, sample("crab-wide"));
        cache.store_facts(Some(session), sample("session"));
        assert_eq!(cache.facts(None), Some(sample("crab-wide")));
        assert_eq!(cache.facts(Some(session)), Some(sample("session")));
        assert_eq!(cache.facts(Some(Uuid::new_v4())), None);
        cache.store_facts(Some(session), sample("moved"));
        assert_eq!(cache.facts(Some(session)), Some(sample("moved")));
        let mut known = cache.facts_sessions();
        known.sort();
        let mut expected = vec![None, Some(session)];
        expected.sort();
        assert_eq!(known, expected);
    }

    #[test]
    fn a_task_snapshot_replaces_what_the_asked_sessions_ran() {
        let cache = FactsCache::default();
        let (busy, idle) = (Uuid::new_v4(), Uuid::new_v4());
        let row = TaskRow {
            task_id: Uuid::new_v4(),
            label: "cargo test".into(),
            kind: TaskKind::Detached,
            secs: 3,
            ours: true,
        };
        cache.store_tasks(
            &[busy, idle],
            vec![SessionTasks {
                session: busy,
                tasks: vec![row.clone()],
            }],
        );
        assert_eq!(
            cache.tasks(&[idle, busy]),
            vec![SessionTasks {
                session: busy,
                tasks: vec![row],
            }]
        );
        // The task ended: the next snapshot leaves the session out.
        cache.store_tasks(&[busy], Vec::new());
        assert!(cache.tasks(&[busy, idle]).is_empty());
        let mut known = cache.tasks_sessions();
        known.sort();
        let mut expected = vec![busy, idle];
        expected.sort();
        assert_eq!(known, expected);
    }

    /// A store says whether the surface has anything new to draw: the
    /// window repaints on that, and only on that.
    #[test]
    fn a_store_says_whether_the_answer_moved() {
        let cache = FactsCache::default();
        let session = Uuid::new_v4();
        assert!(cache.store_facts(Some(session), sample("first")));
        assert!(!cache.store_facts(Some(session), sample("first")));
        assert!(cache.store_facts(Some(session), sample("moved")));

        let row = TaskRow {
            task_id: Uuid::new_v4(),
            label: "cargo test".into(),
            kind: TaskKind::Detached,
            secs: 3,
            ours: true,
        };
        // Read for the first time and idle: nothing ran, nothing runs.
        assert!(!cache.store_tasks(&[session], Vec::new()));
        let busy = vec![SessionTasks {
            session,
            tasks: vec![row],
        }];
        assert!(cache.store_tasks(&[session], busy.clone()));
        assert!(!cache.store_tasks(&[session], busy));
        assert!(cache.store_tasks(&[session], Vec::new()));
    }
}
