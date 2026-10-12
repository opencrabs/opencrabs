//! A 429 that declares its own wait parks the provider for exactly that long.
//!
//! The rule, in one line: the provider goes out of rotation until
//! `now + declared`, requests during the window are served by the next entry in
//! the chain, and the provider returns on its own when the window lapses. The
//! session keeps its model. Nothing is disabled and no config key is added.
//!
//! The clamp is the bug these tests pin: `RetryableError::retry_after()` caps
//! the hint at 30 s so the in-place retry loop cannot stall a turn, which is
//! right for a per-minute throttle and wrong for a provider that said it is
//! closed for hours. The uncapped number lives in `declared_cooldown()` and is
//! what drives the cooldown.
//!
//! The one non-obvious assertion is `cooldown_skip_does_not_promote`: a
//! fallback that covers a parked provider must NOT become sticky, or the chain
//! would never route back to the primary once its window lapses.

mod cooldown_runtime {
    use crate::brain::provider::{
        FallbackProvider, LLMRequest, LLMResponse, Provider, ProviderError, ProviderStream,
        cooldown::CooldownRegistry,
    };
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// A provider that returns `err` for its first `fail_first` calls and
    /// succeeds afterwards, counting every call it receives.
    struct ScriptedProvider {
        name: String,
        calls: AtomicUsize,
        fail_first: usize,
        err: Box<dyn Fn() -> ProviderError + Send + Sync>,
    }

    impl ScriptedProvider {
        fn new(
            name: &str,
            fail_first: usize,
            err: impl Fn() -> ProviderError + Send + Sync + 'static,
        ) -> Self {
            Self {
                name: name.to_string(),
                calls: AtomicUsize::new(0),
                fail_first,
                err: Box::new(err),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        async fn complete(
            &self,
            _request: LLMRequest,
        ) -> crate::brain::provider::error::Result<LLMResponse> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_first {
                return Err((self.err)());
            }
            Ok(LLMResponse {
                id: format!("{}-response", self.name),
                model: "mock-model".into(),
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
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_first {
                return Err((self.err)());
            }
            Ok(Box::pin(futures::stream::empty()))
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn default_model(&self) -> &str {
            "mock-model"
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["mock-model".into()]
        }

        fn context_window(&self, _model: &str) -> Option<u32> {
            Some(4096)
        }

        fn calculate_cost(&self, _model: &str, _input: u32, _output: u32) -> f64 {
            0.0
        }
    }

    fn mock_request() -> LLMRequest {
        LLMRequest {
            model: "mock-model".into(),
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

    /// The shape the operator's box saw: a 429 naming a wait, no quota phrase.
    fn declared_429(secs: u64) -> ProviderError {
        ProviderError::RateLimitExceeded(format!(
            "Too many requests, retry after {secs} s (rate_limit_exceeded)"
        ))
    }

    // -----------------------------------------------------------------
    // 1. The clamp split
    // -----------------------------------------------------------------

    #[test]
    fn declared_cooldown_is_uncapped() {
        // A monthly quota that names its wait. The in-place hint stays clamped
        // so a turn cannot stall; the cooldown takes the number literally.
        let err = declared_429(157_000);
        assert_eq!(err.declared_cooldown(), Some(Duration::from_secs(157_000)));
        assert_eq!(
            crate::utils::retry::RetryableError::retry_after(&err),
            Some(Duration::from_secs(30)),
            "the in-place hint stays clamped at 30s"
        );
    }

    #[test]
    fn long_declared_wait_is_not_retryable_in_place() {
        let err = declared_429(157_000);
        assert!(
            !err.is_retryable(),
            "retrying 4 x 30s against an endpoint closed for 43 hours is the mute"
        );
        assert!(
            crate::brain::provider::error::should_try_next_provider(&err),
            "and the chain must still walk, since the next provider bills \
             a different account"
        );
    }

    #[test]
    fn short_declared_wait_is_unchanged() {
        // A per-minute throttle is exactly what the in-place retry is for.
        let err = declared_429(5);
        assert!(err.is_retryable());
        assert_eq!(
            crate::utils::retry::RetryableError::retry_after(&err),
            Some(Duration::from_secs(5))
        );
        assert_eq!(err.declared_cooldown(), Some(Duration::from_secs(5)));
    }

    // -----------------------------------------------------------------
    // 2. The registry
    // -----------------------------------------------------------------

    #[test]
    fn a_second_429_extends_the_window() {
        let reg = CooldownRegistry::default();
        reg.park("p", Instant::now() + Duration::from_secs(10));
        reg.park("p", Instant::now() + Duration::from_secs(30));
        let left = reg.remaining("p").expect("still parked");
        assert!(
            left > Duration::from_secs(25),
            "the later deadline wins, got {left:?}"
        );

        // And a shorter one arriving afterwards cannot bring it back early.
        reg.park("p", Instant::now() + Duration::from_secs(1));
        assert!(reg.remaining("p").unwrap() > Duration::from_secs(25));
    }

    #[test]
    fn an_expired_window_is_dropped_on_read() {
        let reg = CooldownRegistry::default();
        reg.park("p", Instant::now() + Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(60));
        assert!(reg.remaining("p").is_none());
        assert!(!reg.is_cooling("p"));
    }

    // -----------------------------------------------------------------
    // 3. The chain
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn parked_provider_is_skipped_and_recovers() {
        let primary = Arc::new(ScriptedProvider::new("primary", 1, || declared_429(1)));
        let fallback = Arc::new(ScriptedProvider::new(
            "fallback1",
            0,
            || declared_429(1), // never used
        ));
        let provider = FallbackProvider::new(primary.clone(), vec![fallback.clone()]);

        // Turn 1: the primary names a 1 s wait, so it is parked and the chain
        // serves the fallback.
        let first = provider.complete(mock_request()).await.unwrap();
        assert_eq!(first.id, "fallback1-response");
        assert_eq!(primary.calls(), 1);

        // The window is a real window, not a no-op: a second turn inside it
        // does not touch the primary at all.
        let second = provider.complete(mock_request()).await.unwrap();
        assert_eq!(second.id, "fallback1-response");
        assert_eq!(primary.calls(), 1, "still parked, so not even tried");

        // Once it lapses the primary re-enters on its own, with nothing having
        // had to clear it.
        std::thread::sleep(Duration::from_millis(1_100));
        let third = provider.complete(mock_request()).await.unwrap();
        assert_eq!(
            third.id, "primary-response",
            "the parked provider must return once its window lapses"
        );
    }

    #[tokio::test]
    async fn cooldown_skip_does_not_promote() {
        let primary = Arc::new(ScriptedProvider::new("primary", 1, || declared_429(60)));
        let fallback = Arc::new(ScriptedProvider::new("fallback1", 0, || declared_429(1)));
        let provider = FallbackProvider::new(primary.clone(), vec![fallback.clone()]);

        let resp = provider.complete(mock_request()).await.unwrap();
        assert_eq!(resp.id, "fallback1-response");
        assert_eq!(
            provider.active_subprovider_name(),
            None,
            "a fallback covering a PARKED primary is not a swap: if it went \
             sticky the session would never route back to the primary"
        );
        assert!(
            provider.take_swap_event().is_none(),
            "and no swap should be announced to the user"
        );
    }

    #[tokio::test]
    async fn no_declared_wait_keeps_today_behaviour() {
        // A 429 with no number in it: nothing to park, so the chain behaves
        // exactly as it did before this change, including the sticky
        // promotion.
        let primary = Arc::new(ScriptedProvider::new("primary", 1, || {
            ProviderError::RateLimitExceeded("Too many requests, slow down".into())
        }));
        let fallback = Arc::new(ScriptedProvider::new("fallback1", 0, || declared_429(1)));
        let provider = FallbackProvider::new(primary.clone(), vec![fallback.clone()]);

        let first = provider.complete(mock_request()).await.unwrap();
        assert_eq!(first.id, "fallback1-response");
        assert_eq!(
            provider.active_subprovider_name(),
            Some("fallback1".to_string()),
            "an undeclared 429 still promotes, as it always has"
        );

        // Sticky: the next turn starts at the fallback and never touches the
        // dead primary again.
        let second = provider.complete(mock_request()).await.unwrap();
        assert_eq!(second.id, "fallback1-response");
        assert_eq!(primary.calls(), 1);

        // A plain 500 walks the chain the same way.
        let primary_500 = Arc::new(ScriptedProvider::new("primary", 1, || {
            ProviderError::ApiError {
                status: 500,
                message: "Internal Server Error".into(),
                error_type: None,
            }
        }));
        let fb = Arc::new(ScriptedProvider::new("fallback1", 0, || declared_429(1)));
        let chain = FallbackProvider::new(primary_500.clone(), vec![fb]);
        let resp = chain.complete(mock_request()).await.unwrap();
        assert_eq!(resp.id, "fallback1-response");
    }

    #[tokio::test]
    async fn a_declared_wait_parks_the_provider_that_named_it() {
        // Two fallbacks, each declaring its own window. The warm one must be
        // preferred over a parked one, whichever position it sits in.
        let primary = Arc::new(ScriptedProvider::new("primary", 1, || declared_429(60)));
        let fb1 = Arc::new(ScriptedProvider::new("fallback1", 1, || declared_429(60)));
        let fb2 = Arc::new(ScriptedProvider::new("fallback2", 0, || declared_429(1)));
        let provider = FallbackProvider::new(primary.clone(), vec![fb1.clone(), fb2.clone()]);

        // Turn 1: primary and fb1 both name 60 s and park themselves; fb2
        // answers.
        let first = provider.complete(mock_request()).await.unwrap();
        assert_eq!(first.id, "fallback2-response");
        assert_eq!(primary.calls(), 1);
        assert_eq!(fb1.calls(), 1);

        // Turn 2: both parked entries are skipped, so neither is tried again.
        let second = provider.complete(mock_request()).await.unwrap();
        assert_eq!(second.id, "fallback2-response");
        assert_eq!(primary.calls(), 1, "parked, so not tried");
        assert_eq!(fb1.calls(), 1, "parked, so not tried");
    }
}
