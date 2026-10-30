//! Regression (#2264): the display pair must stay coherent after a sticky
//! fallback. The footer read the provider name from the chain's active entry
//! and the model from the session's pick, producing cross pairs like
//! `zai / deepseek-v4.1-flash` where the pick belonged to the dead primary
//! and the fallback was serving its own default.
//!
//! [`AgentService::provider_pair_for_session`] resolves both sides from the
//! same source: the model goes through `Provider::served_model()`, so a
//! fallback that does not carry the pick reports the model it remapped to,
//! and one that does carry it keeps reporting the pick (#918 case).

use std::sync::Arc;

use async_trait::async_trait;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::{
    FallbackProvider, LLMRequest, LLMResponse, Provider, ProviderError, ProviderStream,
};
use crate::db::Database;
use crate::services::ServiceContext;

/// Chain mock with a configurable model catalogue: the primary can be made
/// to fail with a hard rate limit (no declared cooldown) so the chain
/// promotes the fallback, mirroring the sticky path.
struct ChainMock {
    name: String,
    default_model: String,
    supported: Vec<String>,
    fails: bool,
}

impl ChainMock {
    fn new(name: &str, default_model: &str, supported: &[&str], fails: bool) -> Self {
        Self {
            name: name.to_string(),
            default_model: default_model.to_string(),
            supported: supported.iter().map(|s| s.to_string()).collect(),
            fails,
        }
    }
}

#[async_trait]
impl Provider for ChainMock {
    async fn complete(
        &self,
        request: LLMRequest,
    ) -> crate::brain::provider::error::Result<LLMResponse> {
        if self.fails {
            return Err(ProviderError::RateLimitExceeded(format!(
                "{} mock failure",
                self.name
            )));
        }
        Ok(LLMResponse {
            id: format!("{}-response", self.name),
            model: request.model,
            content: vec![],
            stop_reason: None,
            usage: crate::brain::provider::TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                ..Default::default()
            },
            streaming_active_secs: None,
            tool_text_leak: false,
        })
    }

    async fn stream(
        &self,
        _request: LLMRequest,
    ) -> crate::brain::provider::error::Result<ProviderStream> {
        if self.fails {
            return Err(ProviderError::RateLimitExceeded(format!(
                "{} stream mock failure",
                self.name
            )));
        }
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn supported_models(&self) -> Vec<String> {
        self.supported.clone()
    }

    fn context_window(&self, _model: &str) -> Option<u32> {
        Some(4096)
    }

    fn calculate_cost(&self, _model: &str, _input: u32, _output: u32) -> f64 {
        0.0
    }
}

fn request_for(model: &str) -> LLMRequest {
    LLMRequest {
        model: model.into(),
        messages: vec![],
        system: None,
        system_suffix: None,
        max_tokens: None,
        temperature: None,
        tools: None,
        stream: false,
        metadata: None,
        working_directory: None,
        session_id: None,
    }
}

/// Session service with the chain stored as the session provider and the
/// user's pick pinned as the session model. Returns the service plus the
/// chain Arc so a test can drive a turn through it directly.
async fn service_with_chain(
    primary: ChainMock,
    fallbacks: Vec<ChainMock>,
    pick: &str,
) -> (AgentService, Arc<FallbackProvider>, uuid::Uuid) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let global: Arc<dyn Provider> = Arc::new(ChainMock::new("global", "global-model", &[], false));
    let service = AgentService::new_for_test(global, context).await;

    let chain = Arc::new(FallbackProvider::new(
        Arc::new(primary),
        fallbacks
            .into_iter()
            .map(|p| Arc::new(p) as Arc<dyn Provider>)
            .collect(),
    ));
    let sid = uuid::Uuid::new_v4();
    service.swap_provider_for_session(sid, chain.clone(), pick.to_string());
    (service, chain, sid)
}

/// The reported bug: the fallback does not carry the pick, so it remaps to
/// its own default. The display pair must show what actually serves
/// (`zai / glm-9`), never the fallback name over the dead primary's pick.
#[tokio::test]
async fn promoted_fallback_reports_served_model_not_stale_pick() {
    let (service, chain, sid) = service_with_chain(
        ChainMock::new("acme", "acme-default", &["acme-default"], true),
        vec![ChainMock::new("zai", "glm-9", &["glm-9"], false)],
        "acme-pick",
    )
    .await;

    // One turn: the primary hard-fails, the fallback answers and promotes.
    let resp = chain.complete(request_for("acme-pick")).await.unwrap();
    assert_eq!(resp.model, "glm-9");

    // The fix: name and model come from the same source.
    let (name, model) = service.provider_pair_for_session(sid);
    assert_eq!(name, "zai");
    assert_eq!(model, "glm-9");

    // The deliberate divergence: routing and the #705 refresh guard still
    // read the session's pick, not the served model.
    assert_eq!(service.provider_model_for_session(sid), "acme-pick");
}

/// A fallback that carries the pick in its catalogue. Both policies are
/// pinned here: the cascade turn runs the substitute's configured default
/// (#1374), and once promoted the sticky entry remaps capability-style, so
/// the next turn runs the pick again. The footer describes the live sticky
/// state, so it shows the pick under the fallback's name (#918 semantics
/// preserved on the steady state).
#[tokio::test]
async fn fallback_that_carries_the_pick_keeps_showing_it() {
    let (service, chain, sid) = service_with_chain(
        ChainMock::new("acme", "acme-default", &["acme-default"], true),
        vec![ChainMock::new(
            "zai",
            "glm-9",
            &["glm-9", "acme-pick"],
            false,
        )],
        "acme-pick",
    )
    .await;

    // Cascade turn: the substitute always runs its own default, carried
    // or not (#1374 substitute policy).
    let resp = chain.complete(request_for("acme-pick")).await.unwrap();
    assert_eq!(resp.model, "glm-9");

    // Sticky turn: start_idx is now the fallback itself, and the
    // capability guard passes the carried pick through untouched.
    let resp = chain.complete(request_for("acme-pick")).await.unwrap();
    assert_eq!(resp.model, "acme-pick");

    let (name, model) = service.provider_pair_for_session(sid);
    assert_eq!(name, "zai");
    assert_eq!(model, "acme-pick");
    assert_eq!(service.provider_model_for_session(sid), "acme-pick");
}

/// Healthy primary: no promotion, the pair is the session's own pick under
/// the primary's name, exactly as before this fix.
#[tokio::test]
async fn healthy_primary_reports_its_own_pair() {
    let (service, chain, sid) = service_with_chain(
        ChainMock::new(
            "acme",
            "acme-default",
            &["acme-default", "acme-pick"],
            false,
        ),
        vec![ChainMock::new("zai", "glm-9", &["glm-9"], false)],
        "acme-pick",
    )
    .await;

    chain.complete(request_for("acme-pick")).await.unwrap();

    let (name, model) = service.provider_pair_for_session(sid);
    assert_eq!(name, "acme");
    assert_eq!(model, "acme-pick");
}
