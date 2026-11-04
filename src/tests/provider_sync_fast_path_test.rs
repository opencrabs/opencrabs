//! Regression (#2184, #2188): the per-session provider sync rebuilt the
//! provider on every inbound channel message.
//!
//! `sync_provider_for_session` decides "is this session already on its own
//! provider?" by comparing the session's stored pair against the agent's. It
//! asked `provider_name_for_session`, which reports the STICKY FALLBACK entry
//! while a chain is working around a failed primary. That answer can never
//! equal the pinned name, so for any session with a fallback chain configured
//! the fast path was unreachable: every message paid a full
//! `create_provider_by_name` (0.6-1.9s measured, #2184) and installed a brand
//! new chain, which also discarded the cooldown record and made the failed
//! primary get retried and re-promoted on the very next message.
//!
//! The sync now compares against the CONFIGURED primary. A promoted fallback is
//! a temporary state, not a new configuration, so the chain is left alone.

use crate::brain::provider::{LLMRequest, LLMResponse, Provider, ProviderError, ProviderStream};
use crate::config::{Config, ProviderConfig};
use crate::tests::agent_service_mocks::create_test_service_with_provider;
use async_trait::async_trait;
use std::sync::Arc;

/// The provider the session is pinned to. Fails with a retryable error so the
/// chain promotes its fallback, which is exactly the state the bug needed.
struct PinnedProvider;

#[async_trait]
impl Provider for PinnedProvider {
    async fn complete(
        &self,
        _request: LLMRequest,
    ) -> crate::brain::provider::error::Result<LLMResponse> {
        Err(ProviderError::RateLimitExceeded(
            "pinned mock: daily quota spent".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: LLMRequest,
    ) -> crate::brain::provider::error::Result<ProviderStream> {
        Err(ProviderError::RateLimitExceeded(
            "pinned mock: daily quota spent".to_string(),
        ))
    }

    fn name(&self) -> &'static str {
        "pinned"
    }

    fn default_model(&self) -> &'static str {
        "pinned-model"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["pinned-model".to_string()]
    }

    fn context_window(&self, _model: &str) -> Option<u32> {
        Some(4096)
    }

    fn calculate_cost(&self, _model: &str, _input: u32, _output: u32) -> f64 {
        0.0
    }
}

fn request() -> LLMRequest {
    LLMRequest {
        model: "pinned-model".to_string(),
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

/// A config that can actually build a provider named `pinned`, so a rebuild is
/// observable instead of failing on an unknown name.
fn config_with_pinned() -> Config {
    let mut cfg = Config::default();
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        "pinned".to_string(),
        ProviderConfig {
            enabled: true,
            api_key: Some("test-key-not-a-real-one".to_string()),
            base_url: Some("http://127.0.0.1:1/v1".to_string()),
            default_model: Some("pinned-model".to_string()),
            ..Default::default()
        },
    );
    cfg.providers.custom = Some(map);
    cfg
}

/// Put the session on its pinned provider, wrapped in a chain with `mock` as
/// the fallback, then drive one turn so the failed primary promotes the
/// fallback and the chain goes sticky.
async fn sticky_on_fallback() -> (crate::brain::agent::AgentService, uuid::Uuid) {
    let (svc, sid) = create_test_service_with_provider(Arc::new(PinnedProvider)).await;
    svc.set_fallback_providers_for_test(vec![Arc::new(
        crate::tests::agent_service_mocks::MockProvider,
    )]);
    svc.swap_provider_for_session(sid, Arc::new(PinnedProvider), "pinned-model");

    let chain = svc.provider_for_session(sid);
    assert!(
        chain.is_fallback_chain(),
        "the pinned provider must be wrapped in a chain for this test to mean anything"
    );
    chain
        .complete(request())
        .await
        .expect("the mock fallback answers, so the chain must promote it");
    (svc, sid)
}

// `Config::current()` is one process-wide mirror, so a test that swaps it has
// to serialize against every other swapper in the suite, not just against its
// neighbours in this file. Without the guard the two bodies below race:
// whichever restores first strips the custom providers the other is still
// resolving, and the swap lands on the wrong provider (observed as
// `left: "pinned", right: "other"` in a default-features run that passed under
// `--all-features` on scheduling luck). Same cross-suite lock the governor
// suites hold around their own mirror swaps (#2012).
#[tokio::test]
async fn fast_path_skips_provider_rebuild() {
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    let previous = Config::current();
    Config::set_current(config_with_pinned());

    let (svc, sid) = sticky_on_fallback().await;

    // Precondition: the chain is serving its fallback, so the session's LIVE
    // provider name is the fallback while its CONFIGURED provider is still the
    // pinned one.
    assert_eq!(
        svc.provider_name_for_session(sid),
        "mock",
        "the chain should be sticky on the fallback after the pinned primary failed"
    );
    assert_eq!(
        svc.provider_for_session(sid).name(),
        "pinned",
        "the chain must still report the configured primary"
    );

    crate::channels::commands::sync_provider_for_session(
        &svc,
        sid,
        Some("pinned"),
        Some("pinned-model"),
    )
    .await;

    // A rebuild installs a fresh chain with no sticky entry, so the live name
    // would snap back to the pinned provider. The session did not change
    // provider, so the sync must have left the chain alone.
    assert_eq!(
        svc.provider_name_for_session(sid),
        "mock",
        "sync rebuilt the provider chain for a session that had not changed \
         provider: the fast path compared against the sticky fallback instead of \
         the configured primary (#2184), so every inbound message paid a full \
         provider rebuild and threw away the chain's cooldown record"
    );

    Config::set_current((*previous).clone());
}

#[tokio::test]
async fn a_changed_provider_still_swaps() {
    // Same mirror, same cross-suite lock: see the note above.
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    // The fast path must stay a fast path, not a "never sync" path: when the
    // session's stored provider genuinely differs, the swap still happens.
    let previous = Config::current();
    let mut cfg = config_with_pinned();
    cfg.providers
        .custom
        .as_mut()
        .expect("config_with_pinned sets customs")
        .insert(
            "other".to_string(),
            ProviderConfig {
                enabled: true,
                api_key: Some("test-key-not-a-real-one".to_string()),
                base_url: Some("http://127.0.0.1:2/v1".to_string()),
                default_model: Some("other-model".to_string()),
                ..Default::default()
            },
        );
    Config::set_current(cfg);

    let (svc, sid) = sticky_on_fallback().await;
    assert_eq!(svc.provider_for_session(sid).name(), "pinned");

    crate::channels::commands::sync_provider_for_session(
        &svc,
        sid,
        Some("other"),
        Some("other-model"),
    )
    .await;

    assert_eq!(
        svc.provider_for_session(sid).name(),
        "other",
        "a genuine provider change must still be restored, not skipped by the \
         fast path"
    );
    assert_eq!(
        svc.provider_name_for_session(sid),
        "other",
        "the restored chain must start on the new primary, not stay sticky on \
         the previous fallback"
    );

    Config::set_current((*previous).clone());
}
