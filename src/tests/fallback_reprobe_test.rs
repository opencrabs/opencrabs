//! The sticky chain re-probes the primary (#3081).
//!
//! A hard failure (DNS, connect, 502) declares no `Retry-After` window, so it
//! never created a cooldown record, and `candidate_order` admits an entry
//! before the sticky index only when one exists. One DNS hiccup therefore
//! demoted the primary for the life of the process, silently, while the tool
//! loop told the user "primary will be tried again next turn".
//!
//! These tests pin both halves of the fix: the primary is skipped while the
//! re-probe window is open, and it is tried again once the window lapses,
//! with a successful probe moving the chain back to index 0.

use crate::brain::agent::service::helpers::failure_attribution_name;
use crate::brain::provider::{
    FallbackProvider, LLMRequest, LLMResponse, Provider, ProviderError, ProviderStream,
};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A chain entry that records how many times it was called and either always
/// fails with a hard, window-less transport error or always answers.
struct CountingMock {
    name: String,
    fails: bool,
    calls: AtomicUsize,
}

impl CountingMock {
    fn failing(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            fails: true,
            calls: AtomicUsize::new(0),
        })
    }

    fn answering(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            fails: false,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

/// A real `reqwest::Error` of the connect kind, without leaving the machine.
/// Nothing listens on port 1, so the connection is refused immediately, which
/// is the window-less transport failure class from the issue.
async fn transport_error() -> reqwest::Error {
    reqwest::Client::new()
        .get("http://127.0.0.1:1/")
        .send()
        .await
        .expect_err("nothing listens on port 1")
}

#[async_trait]
impl Provider for CountingMock {
    async fn complete(
        &self,
        request: LLMRequest,
    ) -> crate::brain::provider::error::Result<LLMResponse> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.fails {
            // The transport shape from the issue: retryable (the chain
            // advances), no declared cooldown (so no record is ever written).
            return Err(ProviderError::HttpError(transport_error().await));
        }
        Ok(LLMResponse {
            id: format!("{}-response", self.name),
            model: request.model,
            content: vec![],
            stop_reason: None,
            usage: crate::brain::provider::TokenUsage::default(),
            streaming_active_secs: None,
            tool_text_leak: false,
        })
    }

    async fn stream(
        &self,
        _request: LLMRequest,
    ) -> crate::brain::provider::error::Result<ProviderStream> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.fails {
            return Err(ProviderError::HttpError(transport_error().await));
        }
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn default_model(&self) -> &str {
        "mock-default"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["mock-default".into()]
    }

    fn context_window(&self, _model: &str) -> Option<u32> {
        Some(4096)
    }

    fn calculate_cost(&self, _model: &str, _input: u32, _output: u32) -> f64 {
        0.0
    }
}

fn request() -> LLMRequest {
    LLMRequest::new("mock-default", vec![])
}

/// A window wide enough that no probe happens inside the test.
fn closed_window() -> Duration {
    Duration::from_secs(3600)
}

/// A window that has always already lapsed.
fn open_window() -> Duration {
    Duration::ZERO
}

#[tokio::test]
async fn a_sticky_chain_skips_the_primary_while_the_window_is_open() {
    let primary = CountingMock::failing("alpha");
    let fallback = CountingMock::answering("beta");
    let chain = FallbackProvider::new(primary.clone(), vec![fallback.clone()])
        .with_reprobe_ttl(closed_window());

    for _ in 0..4 {
        chain.complete(request()).await.expect("fallback answers");
    }

    // One attempt demoted it, and the open window keeps it demoted.
    assert_eq!(
        primary.calls(),
        1,
        "the primary must be skipped while the re-probe window is open"
    );
    assert_eq!(fallback.calls(), 4);
    assert_eq!(chain.active_subprovider_name().as_deref(), Some("beta"));
}

#[tokio::test]
async fn a_lapsed_window_tries_the_primary_again() {
    let primary = CountingMock::failing("alpha");
    let fallback = CountingMock::answering("beta");
    let chain = FallbackProvider::new(primary.clone(), vec![fallback.clone()])
        .with_reprobe_ttl(open_window());

    chain.complete(request()).await.expect("fallback answers");
    assert_eq!(primary.calls(), 1);

    chain.complete(request()).await.expect("fallback answers");
    assert_eq!(
        primary.calls(),
        2,
        "a lapsed window must put the primary back in front of the chain"
    );
}

#[tokio::test]
async fn a_successful_probe_moves_the_chain_back_to_the_primary() {
    // Fails once, then answers: the probe is what recovers the session.
    struct FlakyOnce {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for FlakyOnce {
        async fn complete(
            &self,
            request: LLMRequest,
        ) -> crate::brain::provider::error::Result<LLMResponse> {
            if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
                return Err(ProviderError::HttpError(transport_error().await));
            }
            Ok(LLMResponse {
                id: "flaky-response".into(),
                model: request.model,
                content: vec![],
                stop_reason: None,
                usage: crate::brain::provider::TokenUsage::default(),
                streaming_active_secs: None,
                tool_text_leak: false,
            })
        }

        async fn stream(
            &self,
            _request: LLMRequest,
        ) -> crate::brain::provider::error::Result<ProviderStream> {
            Ok(Box::pin(futures::stream::empty()))
        }

        fn name(&self) -> &str {
            "alpha"
        }

        fn default_model(&self) -> &str {
            "mock-default"
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["mock-default".into()]
        }

        fn context_window(&self, _model: &str) -> Option<u32> {
            Some(4096)
        }

        fn calculate_cost(&self, _model: &str, _input: u32, _output: u32) -> f64 {
            0.0
        }
    }

    let primary = Arc::new(FlakyOnce {
        calls: AtomicUsize::new(0),
    });
    let fallback = CountingMock::answering("beta");
    let chain = FallbackProvider::new(primary.clone(), vec![fallback.clone()])
        .with_reprobe_ttl(open_window());

    chain.complete(request()).await.expect("fallback answers");
    assert_eq!(chain.active_subprovider_name().as_deref(), Some("beta"));

    chain
        .complete(request())
        .await
        .expect("the probed primary answers");
    assert_eq!(
        chain.active_subprovider_name(),
        None,
        "a successful probe must leave the chain on the primary"
    );
    assert_eq!(chain.provenance_label(), "primary 'alpha'");
    assert_eq!(primary.calls.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn a_chain_forced_onto_a_fallback_still_probes_the_primary() {
    // `force_next_fallback` (tool-loop escalation) never attempted the
    // primary, so there is no attempt to wait out: the very next call must
    // try it, and a healthy primary takes the session back.
    let primary = CountingMock::answering("alpha");
    let fallback = CountingMock::answering("beta");
    let chain = FallbackProvider::new(primary.clone(), vec![fallback.clone()])
        .with_reprobe_ttl(closed_window());

    assert!(chain.force_next_fallback("test", "mock-default"));
    assert_eq!(chain.active_subprovider_name().as_deref(), Some("beta"));

    chain.complete(request()).await.expect("primary answers");
    assert_eq!(primary.calls(), 1);
    assert_eq!(fallback.calls(), 0, "the healthy primary serves the call");
    assert_eq!(chain.active_subprovider_name(), None);
}

#[test]
fn a_failure_is_attributed_to_the_entry_that_failed() {
    // The chain's `name()` stays the configured primary for persistence, so
    // keying health records on it filed a fallback's DNS failure against the
    // primary and poisoned every later "is the primary healthy?" decision.
    let chain = FallbackProvider::new(
        CountingMock::answering("alpha"),
        vec![CountingMock::answering("beta")],
    );
    assert_eq!(failure_attribution_name(&chain), "alpha");

    assert!(chain.force_next_fallback("test", "mock-default"));
    assert_eq!(failure_attribution_name(&chain), "beta");

    // A plain provider keeps its own name.
    let plain = CountingMock::answering("gamma");
    assert_eq!(failure_attribution_name(&*plain), "gamma");
}
