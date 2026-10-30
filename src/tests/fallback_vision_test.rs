//! Fallback Provider Chain & Vision Model Tests
//!
//! Tests for the fallback provider chain configuration, runtime fallback
//! behavior, and per-provider vision model swapping.

// --- Fallback chain config ---

mod fallback_chain {
    use crate::brain::provider::factory::fallback_chain;
    use crate::config::FallbackProviderConfig;

    #[test]
    fn empty_config_returns_empty_chain() {
        let cfg = FallbackProviderConfig::default();
        assert!(fallback_chain(&cfg).is_empty());
    }

    #[test]
    fn generation_chain_parses_and_defaults_empty() {
        // #1672: the ordered image-generation provider chain.
        let cfg: FallbackProviderConfig =
            toml::from_str("generation = [\"qwen-image\", \"openrouter\"]")
                .expect("generation chain must parse");
        assert_eq!(
            cfg.generation,
            ["qwen-image".to_string(), "openrouter".to_string()]
        );
        let bare = FallbackProviderConfig::default();
        assert!(bare.generation.is_empty());
    }

    #[test]
    fn legacy_single_provider() {
        let cfg = FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            provider: Some("openrouter".into()),
            providers: vec![],
            vision: vec![],
        };
        assert_eq!(fallback_chain(&cfg), vec!["openrouter"]);
    }

    #[test]
    fn providers_array_only() {
        let cfg = FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            provider: None,
            providers: vec!["anthropic".into(), "openai".into()],
            vision: vec![],
        };
        assert_eq!(fallback_chain(&cfg), vec!["anthropic", "openai"]);
    }

    #[test]
    fn array_plus_legacy_appended() {
        let cfg = FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            provider: Some("gemini".into()),
            providers: vec!["anthropic".into(), "openai".into()],
            vision: vec![],
        };
        assert_eq!(fallback_chain(&cfg), vec!["anthropic", "openai", "gemini"]);
    }

    #[test]
    fn legacy_deduped_if_already_in_array() {
        let cfg = FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            provider: Some("anthropic".into()),
            providers: vec!["anthropic".into(), "openai".into()],
            vision: vec![],
        };
        // "anthropic" already in array — should NOT be appended again
        assert_eq!(fallback_chain(&cfg), vec!["anthropic", "openai"]);
    }

    #[test]
    fn single_provider_in_array() {
        let cfg = FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            provider: None,
            providers: vec!["minimax".into()],
            vision: vec![],
        };
        assert_eq!(fallback_chain(&cfg), vec!["minimax"]);
    }

    #[test]
    fn deserialization_from_toml_array() {
        let toml_str = r#"
enabled = true
providers = ["openrouter", "anthropic"]
"#;
        let cfg: FallbackProviderConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.providers, vec!["openrouter", "anthropic"]);
        assert!(cfg.provider.is_none());
    }

    #[test]
    fn deserialization_from_toml_legacy() {
        let toml_str = r#"
enabled = true
provider = "openrouter"
"#;
        let cfg: FallbackProviderConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.provider, Some("openrouter".into()));
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn deserialization_from_toml_both() {
        let toml_str = r#"
enabled = true
provider = "gemini"
providers = ["anthropic", "openai"]
"#;
        let cfg: FallbackProviderConfig = toml::from_str(toml_str).unwrap();
        let chain = fallback_chain(&cfg);
        assert_eq!(chain, vec!["anthropic", "openai", "gemini"]);
    }
}

// --- Fallback provider runtime ---

mod fallback_runtime {
    use crate::brain::provider::{
        FallbackProvider, LLMRequest, LLMResponse, Provider, ProviderError, ProviderStream,
    };
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mock provider that fails N times, then succeeds.
    struct MockProvider {
        name: String,
        fail_count: AtomicUsize,
        max_failures: usize,
    }

    impl MockProvider {
        fn always_fail(name: &str) -> Self {
            Self {
                name: name.to_string(),
                fail_count: AtomicUsize::new(0),
                max_failures: usize::MAX,
            }
        }

        fn always_succeed(name: &str) -> Self {
            Self {
                name: name.to_string(),
                fail_count: AtomicUsize::new(0),
                max_failures: 0,
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn complete(
            &self,
            _request: LLMRequest,
        ) -> crate::brain::provider::error::Result<LLMResponse> {
            let count = self.fail_count.fetch_add(1, Ordering::SeqCst);
            if count < self.max_failures {
                // Use RateLimitExceeded so FallbackProvider's should_try_next
                // guard (is_retryable) falls through to the next provider.
                // Internal errors are intentionally treated as hard failures.
                Err(ProviderError::RateLimitExceeded(format!(
                    "{} mock failure #{}",
                    self.name,
                    count + 1
                )))
            } else {
                Ok(LLMResponse {
                    id: format!("{}-response", self.name),
                    model: "mock-model".into(),
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
        }

        async fn stream(
            &self,
            _request: LLMRequest,
        ) -> crate::brain::provider::error::Result<ProviderStream> {
            let count = self.fail_count.fetch_add(1, Ordering::SeqCst);
            if count < self.max_failures {
                Err(ProviderError::RateLimitExceeded(format!(
                    "{} stream mock failure #{}",
                    self.name,
                    count + 1
                )))
            } else {
                Ok(Box::pin(futures::stream::empty()))
            }
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

    #[tokio::test]
    async fn primary_succeeds_no_fallback_tried() {
        let primary = Arc::new(MockProvider::always_succeed("primary"));
        let fallback = Arc::new(MockProvider::always_succeed("fallback"));
        let provider = FallbackProvider::new(primary, vec![fallback.clone()]);

        let resp = provider.complete(mock_request()).await.unwrap();
        assert_eq!(resp.id, "primary-response");
        // Fallback should not have been called
        assert_eq!(fallback.fail_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn primary_fails_first_fallback_succeeds() {
        let primary = Arc::new(MockProvider::always_fail("primary"));
        let fb1 = Arc::new(MockProvider::always_succeed("fallback1"));
        let provider = FallbackProvider::new(primary, vec![fb1]);

        let resp = provider.complete(mock_request()).await.unwrap();
        assert_eq!(resp.id, "fallback1-response");
    }

    #[tokio::test]
    async fn primary_fails_first_fallback_fails_second_succeeds() {
        let primary = Arc::new(MockProvider::always_fail("primary"));
        let fb1 = Arc::new(MockProvider::always_fail("fallback1"));
        let fb2 = Arc::new(MockProvider::always_succeed("fallback2"));
        let provider = FallbackProvider::new(primary, vec![fb1, fb2]);

        let resp = provider.complete(mock_request()).await.unwrap();
        assert_eq!(resp.id, "fallback2-response");
    }

    #[tokio::test]
    async fn all_fail_returns_last_error() {
        let primary = Arc::new(MockProvider::always_fail("primary"));
        let fb1 = Arc::new(MockProvider::always_fail("fallback1"));
        let fb2 = Arc::new(MockProvider::always_fail("fallback2"));
        let provider = FallbackProvider::new(primary, vec![fb1, fb2]);

        let err = provider.complete(mock_request()).await.unwrap_err();
        // Sticky fallback tries in order; when all fail, the last error
        // (from the final fallback tried) is surfaced.
        assert!(err.to_string().contains("fallback2"));
    }

    #[tokio::test]
    async fn no_fallbacks_primary_error_propagated() {
        let primary = Arc::new(MockProvider::always_fail("primary"));
        let provider = FallbackProvider::new(primary, vec![]);

        let err = provider.complete(mock_request()).await.unwrap_err();
        assert!(err.to_string().contains("primary"));
    }

    #[tokio::test]
    async fn stream_primary_fails_fallback_succeeds() {
        let primary = Arc::new(MockProvider::always_fail("primary"));
        let fb1 = Arc::new(MockProvider::always_succeed("fallback1"));
        let provider = FallbackProvider::new(primary, vec![fb1]);

        // The assertion is the unwrap: the fallback stream must succeed. The
        // stream itself is must_use, so discard it explicitly.
        let _ = provider.stream(mock_request()).await.unwrap();
    }

    #[tokio::test]
    async fn stream_all_fail() {
        let primary = Arc::new(MockProvider::always_fail("primary"));
        let fb1 = Arc::new(MockProvider::always_fail("fallback1"));
        let provider = FallbackProvider::new(primary, vec![fb1]);

        match provider.stream(mock_request()).await {
            Ok(_) => panic!("Expected error when all providers fail"),
            Err(e) => assert!(e.to_string().contains("fallback1")),
        }
    }

    #[tokio::test]
    async fn delegates_name_to_primary() {
        let primary = Arc::new(MockProvider::always_succeed("my-primary"));
        let provider = FallbackProvider::new(primary, vec![]);
        assert_eq!(provider.name(), "my-primary");
    }

    #[tokio::test]
    async fn delegates_default_model_to_primary() {
        let primary = Arc::new(MockProvider::always_succeed("p"));
        let provider = FallbackProvider::new(primary, vec![]);
        assert_eq!(provider.default_model(), "mock-model");
    }
}

// --- Vision model ---

mod vision_model {
    use crate::brain::provider::Provider;
    use crate::brain::provider::custom_openai_compatible::OpenAIProvider;

    #[test]
    fn no_vision_model_by_default() {
        let provider = OpenAIProvider::new("test-key".into());
        assert!(!provider.supports_vision());
    }

    #[test]
    fn with_vision_model_enables_vision() {
        let provider =
            OpenAIProvider::new("test-key".into()).with_vision_model("gpt-5-nano".into());
        assert!(provider.supports_vision());
    }

    #[test]
    fn vision_model_accessor() {
        let provider =
            OpenAIProvider::new("test-key".into()).with_vision_model("gpt-5-nano".into());
        assert_eq!(provider.vision_model(), Some("gpt-5-nano"));

        let no_vision = OpenAIProvider::new("test-key".into());
        assert_eq!(no_vision.vision_model(), None);
    }

    #[test]
    fn vision_model_config_roundtrip() {
        let toml_str = r#"
enabled = true
api_key = "test"
default_model = "MiniMax-M2.7"
vision_model = "MiniMax-Text-01"
"#;
        let cfg: crate::config::ProviderConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.vision_model, Some("MiniMax-Text-01".into()));
        assert_eq!(cfg.default_model, Some("MiniMax-M2.7".into()));
    }

    #[test]
    fn vision_model_absent_in_config() {
        let toml_str = r#"
enabled = true
api_key = "test"
default_model = "gpt-4"
"#;
        let cfg: crate::config::ProviderConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.vision_model.is_none());
    }

    #[tokio::test]
    async fn factory_config_wires_vision_model() {
        use crate::config::{Config, ProviderConfig, ProviderConfigs};

        let config = Config {
            providers: ProviderConfigs {
                openai: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("test-key".into()),
                    base_url: None,
                    default_model: Some("gpt-4".into()),
                    models: vec![],
                    vision_model: Some("gpt-5-nano".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let provider = crate::brain::provider::factory::create_provider(&config)
            .await
            .unwrap();
        assert!(provider.supports_vision());
    }

    #[tokio::test]
    async fn factory_config_no_vision_model() {
        use crate::config::{Config, ProviderConfig, ProviderConfigs};

        let config = Config {
            providers: ProviderConfigs {
                openai: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("test-key".into()),
                    base_url: None,
                    default_model: Some("gpt-4".into()),
                    models: vec![],
                    vision_model: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let provider = crate::brain::provider::factory::create_provider(&config)
            .await
            .unwrap();
        assert!(!provider.supports_vision());
    }
}

// --- Factory fallback wiring ---

mod factory_fallback {
    use crate::config::{Config, FallbackProviderConfig, ProviderConfig, ProviderConfigs};

    #[tokio::test]
    async fn no_fallback_returns_primary_directly() {
        let config = Config {
            providers: ProviderConfigs {
                openai: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("test-key".into()),
                    base_url: None,
                    default_model: None,
                    models: vec![],
                    vision_model: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let provider = crate::brain::provider::factory::create_provider(&config)
            .await
            .unwrap();
        assert_eq!(provider.name(), "openai");
    }

    #[tokio::test]
    async fn fallback_disabled_returns_primary_directly() {
        let config = Config {
            providers: ProviderConfigs {
                openai: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("test-key".into()),
                    base_url: None,
                    default_model: None,
                    models: vec![],
                    vision_model: None,
                    ..Default::default()
                }),
                fallback: Some(FallbackProviderConfig {
                    generation: vec![],
                    enabled: false,
                    provider: Some("anthropic".into()),
                    providers: vec![],
                    vision: vec![],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let provider = crate::brain::provider::factory::create_provider(&config)
            .await
            .unwrap();
        // Should be plain openai, not wrapped in fallback
        assert_eq!(provider.name(), "openai");
    }

    #[tokio::test]
    async fn no_provider_no_fallback_returns_placeholder() {
        let config = Config {
            providers: ProviderConfigs::default(),
            ..Default::default()
        };

        let provider = crate::brain::provider::factory::create_provider(&config)
            .await
            .unwrap();
        assert_eq!(provider.name(), "none");
    }

    #[tokio::test]
    async fn fallback_with_unconfigured_providers_skipped() {
        // Fallback lists providers that don't have API keys — should skip them gracefully
        let config = Config {
            providers: ProviderConfigs {
                fallback: Some(FallbackProviderConfig {
                    generation: vec![],
                    enabled: true,
                    provider: None,
                    providers: vec!["anthropic".into(), "openai".into()],
                    vision: vec![],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        // No providers configured at all — should end up with placeholder
        let provider = crate::brain::provider::factory::create_provider(&config)
            .await
            .unwrap();
        assert_eq!(provider.name(), "none");
    }
}

// --- Active provider vision discovery ---

mod active_provider_vision {
    use crate::brain::provider::factory::active_provider_vision;
    use crate::config::{Config, ProviderConfig, ProviderConfigs};

    #[test]
    fn returns_none_when_no_providers() {
        let config = Config::default();
        assert!(active_provider_vision(&config).is_none());
    }

    #[test]
    fn returns_none_when_no_vision_model() {
        let config = Config {
            providers: ProviderConfigs {
                openai: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("key".into()),
                    base_url: None,
                    default_model: None,
                    models: vec![],
                    vision_model: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(active_provider_vision(&config).is_none());
    }

    #[test]
    fn returns_vision_model_from_active_provider() {
        let config = Config {
            providers: ProviderConfigs {
                minimax: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("minimax-key".into()),
                    base_url: Some("https://api.minimax.io/v1".into()),
                    default_model: Some("MiniMax-M2.7".into()),
                    models: vec![],
                    vision_model: Some("MiniMax-Text-01".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let result = active_provider_vision(&config);
        assert!(result.is_some());
        let (api_key, base_url, vision_model) = result.unwrap();
        assert_eq!(api_key, "minimax-key");
        assert!(base_url.contains("minimax"));
        assert_eq!(vision_model, "MiniMax-Text-01");
    }

    #[test]
    fn disabled_provider_with_vision_model_resolves() {
        // #401: enabled gates CHAT only. A provider at enabled = false with
        // a vision_model and key is a valid vision backend — requiring
        // enabled = true here was the regression.
        let config = Config {
            providers: ProviderConfigs {
                minimax: Some(ProviderConfig {
                    enabled: false,
                    api_key: Some("key".into()),
                    base_url: Some("https://api.minimax.io/v1".into()),
                    default_model: None,
                    models: vec![],
                    vision_model: Some("MiniMax-Text-01".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let (_key, base_url, vision_model) =
            active_provider_vision(&config).expect("disabled + vision_model resolves");
        assert!(base_url.contains("minimax"));
        assert_eq!(vision_model, "MiniMax-Text-01");
    }

    #[test]
    fn registers_keyless_provider_with_vision_model() {
        // A keyless / local provider (Ollama, llama.cpp, LM Studio) has no API
        // key but still serves vision. Modelled as a custom provider, which is
        // how a local endpoint is configured, and which becomes active without
        // a key. The gate is `vision_model`, so this resolves with an empty
        // Bearer.
        let mut custom = std::collections::BTreeMap::new();
        custom.insert(
            "localllm".to_string(),
            ProviderConfig {
                enabled: true,
                api_key: None,
                base_url: Some("http://localhost:11434/v1".into()),
                vision_model: Some("llava".into()),
                ..Default::default()
            },
        );
        let config = Config {
            providers: ProviderConfigs {
                custom: Some(custom),
                ..Default::default()
            },
            ..Default::default()
        };
        let (api_key, _, vision_model) = active_provider_vision(&config)
            .expect("keyless provider with vision_model must resolve");
        assert_eq!(api_key, "");
        assert_eq!(vision_model, "llava");
    }

    #[test]
    fn picks_first_provider_with_vision_by_priority() {
        // REGISTRATIONS order: OpenAI (pos 9) before Minimax (pos 13).
        // When both have vision_model, the one earlier in REGISTRATIONS wins.
        let config = Config {
            providers: ProviderConfigs {
                minimax: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("minimax-key".into()),
                    base_url: Some("https://api.minimax.io/v1".into()),
                    default_model: None,
                    models: vec![],
                    vision_model: Some("MiniMax-Text-01".into()),
                    ..Default::default()
                }),
                openai: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("openai-key".into()),
                    base_url: None,
                    default_model: None,
                    models: vec![],
                    vision_model: Some("gpt-5-nano".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let (api_key, _, vision_model) = active_provider_vision(&config).unwrap();
        // OpenAI is earlier in REGISTRATIONS priority order
        assert_eq!(api_key, "openai-key");
        assert_eq!(vision_model, "gpt-5-nano");
    }

    #[test]
    fn issue_253_active_provider_without_vision_falls_through() {
        // Reproduces #253: opencode (requires_api_key=false) is the active
        // provider because it's registered before minimax, but opencode has
        // no vision_model. The scan should continue past it and find minimax.
        let config = Config {
            providers: ProviderConfigs {
                opencode: Some(ProviderConfig {
                    enabled: true,
                    api_key: None,
                    default_model: Some("opencode-model".into()),
                    vision_model: None,
                    ..Default::default()
                }),
                minimax: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("minimax-key".into()),
                    base_url: Some("https://api.minimax.io/v1".into()),
                    default_model: Some("MiniMax-M2.7".into()),
                    vision_model: Some("MiniMax-Text-01".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        // opencode is the active provider (first enabled in provider_registry),
        // but has no vision_model — minimax's should still be found
        let (api_key, _, vision_model) = active_provider_vision(&config).unwrap();
        assert_eq!(api_key, "minimax-key");
        assert_eq!(vision_model, "MiniMax-Text-01");
    }
}

// --- Vision fallback chain ([providers.fallback].vision) ---

mod vision_fallback_chain {
    use crate::brain::provider::factory::{
        active_provider_vision, vision_candidates, vision_candidates_for,
    };
    use crate::config::{Config, FallbackProviderConfig, ProviderConfig, ProviderConfigs};
    use std::collections::BTreeMap;

    /// Helper: config with two built-in providers. openai is earlier in
    /// REGISTRATIONS, minimax is later.
    fn two_providers_config() -> Config {
        Config {
            providers: ProviderConfigs {
                openai: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("openai-key".into()),
                    base_url: None,
                    default_model: None,
                    models: vec![],
                    vision_model: Some("gpt-5-nano".into()),
                    ..Default::default()
                }),
                minimax: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("minimax-key".into()),
                    base_url: Some("https://api.minimax.io/v1".into()),
                    default_model: None,
                    models: vec![],
                    vision_model: Some("MiniMax-Text-01".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn empty_chain_falls_through_to_scan() {
        // Empty vision chain = no override, scan-all still works.
        let mut config = two_providers_config();
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec![],
            ..Default::default()
        });

        let (_, _, vision_model) = active_provider_vision(&config).unwrap();
        // openai is earlier in REGISTRATIONS
        assert_eq!(vision_model, "gpt-5-nano");
    }

    #[test]
    fn no_fallback_at_all_falls_through_to_scan() {
        // No fallback configured at all — scan-all still works.
        let config = two_providers_config();
        assert!(config.providers.fallback.is_none());

        let (_, _, vision_model) = active_provider_vision(&config).unwrap();
        assert_eq!(vision_model, "gpt-5-nano");
    }

    #[test]
    fn the_chain_decides_the_order_not_the_scan() {
        // Inverted deliberately (#1318). The scan used to run FIRST and,
        // because dedup keeps the first occurrence, a provider it found held
        // its scan position — so a configured chain could never reorder
        // anything and only contributed entries the scan had missed. Measured
        // cost: four candidates tried and failed on every image while
        // providers named in the chain were never reached.
        //
        // `openai` here has a vision_model and would have won the old scan.
        // It is not in the chain, so it must not be tried.
        let mut config = two_providers_config();
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["minimax".into()],
            ..Default::default()
        });

        let cands = vision_candidates(&config);
        assert_eq!(cands.len(), 1, "chain only: {cands:?}");
        assert_eq!(cands[0].api_key, "minimax-key", "the configured entry wins");
    }

    #[test]
    fn the_session_provider_is_tried_before_the_chain() {
        // Owner contract (#1318): current provider, then chain, then Gemini.
        let mut config = two_providers_config();
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["minimax".into()],
            ..Default::default()
        });

        let cands = vision_candidates_for(&config, Some("openai"));
        assert_eq!(cands.len(), 2, "current provider + chain: {cands:?}");
        assert_eq!(
            cands[0].api_key, "openai-key",
            "the session's provider is first"
        );
        assert_eq!(cands[1].api_key, "minimax-key");
    }

    #[test]
    fn a_session_provider_already_in_the_chain_is_not_tried_twice() {
        let mut config = two_providers_config();
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["minimax".into(), "openai".into()],
            ..Default::default()
        });

        let cands = vision_candidates_for(&config, Some("openai"));
        assert_eq!(cands.len(), 2, "deduped: {cands:?}");
        assert_eq!(
            cands[0].api_key, "openai-key",
            "and it keeps the CURRENT-provider position, not its chain one"
        );
    }

    #[test]
    fn a_session_provider_without_vision_just_falls_to_the_chain() {
        let mut config = two_providers_config();
        config.providers.openai.as_mut().unwrap().vision_model = None;
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["minimax".into()],
            ..Default::default()
        });

        let cands = vision_candidates_for(&config, Some("openai"));
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].api_key, "minimax-key");
    }

    #[test]
    fn roll_through_covers_the_chain_while_capability_still_sees_everything() {
        // Two different questions, and conflating them was the bug (#1318).
        // "What do we TRY, in order?" is the chain. "Can we see AT ALL?" is
        // the scan, which registration gates and file_extract rely on.
        let config = two_providers_config();

        assert!(
            vision_candidates(&config).is_empty(),
            "nothing configured to try: no chain, no session provider"
        );

        let scanned = crate::brain::provider::factory::any_provider_vision(&config);
        let capable: Vec<&str> = scanned.iter().map(|c| c.2.as_str()).collect();
        assert_eq!(
            capable,
            vec!["gpt-5-nano", "MiniMax-Text-01"],
            "capability still sees every provider that can serve vision"
        );
    }

    #[test]
    fn provider_without_vision_model_never_a_candidate() {
        // openai has no vision_model: not a candidate via scan OR chain.
        let mut config = two_providers_config();
        config.providers.openai.as_mut().unwrap().vision_model = None;
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["openai".into(), "minimax".into()],
            ..Default::default()
        });

        let cands = vision_candidates(&config);
        assert_eq!(cands.len(), 1);
        let (api_key, _, vision_model) = active_provider_vision(&config).unwrap();
        assert_eq!(api_key, "minimax-key");
        assert_eq!(vision_model, "MiniMax-Text-01");
    }

    #[test]
    fn chain_disabled_entry_still_serves_vision() {
        // A chain entry at enabled = false with a vision_model RESOLVES:
        // enabled gates chat only (#401). Requiring enabled here was the
        // regression that broke vision for keyed-but-disabled providers.
        let mut config = two_providers_config();
        config.providers.minimax.as_mut().unwrap().enabled = false;
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["minimax".into()],
            ..Default::default()
        });

        let cands = vision_candidates(&config);
        assert!(
            cands
                .iter()
                .any(|c| c.api_key == "minimax-key" && c.model == "MiniMax-Text-01"),
            "disabled provider with vision_model stays a candidate: {cands:?}"
        );
    }

    #[test]
    fn chain_entry_no_vision_model_skipped() {
        // Chain entry has no vision_model — should be skipped.
        let mut config = two_providers_config();
        config.providers.openai.as_mut().unwrap().vision_model = None;
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["openai".into()],
            ..Default::default()
        });

        // openai has no vision_model in chain, falls through to scan-all
        // where minimax wins (only one with vision_model).
        let (api_key, _, vision_model) = active_provider_vision(&config).unwrap();
        assert_eq!(api_key, "minimax-key");
        assert_eq!(vision_model, "MiniMax-Text-01");
    }

    #[test]
    fn chain_nonexistent_provider_skipped() {
        // Chain entry references a provider that doesn't exist in config.
        let mut config = two_providers_config();
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["nonexistent".into()],
            ..Default::default()
        });

        // nonexistent is skipped, falls through to scan-all where openai wins.
        let (_, _, vision_model) = active_provider_vision(&config).unwrap();
        assert_eq!(vision_model, "gpt-5-nano");
    }

    #[test]
    fn xiaomi_token_plan_endpoint_derived_not_openai() {
        // THE #430 regression: xiaomi with endpoint_type = "token-plan"
        // and no base_url was resolved to api.openai.com, 401ing every
        // vision call. The endpoint must derive like the chat factory.
        let config = Config {
            providers: ProviderConfigs {
                xiaomi: Some(ProviderConfig {
                    enabled: false,
                    api_key: Some("tp-test-key".into()),
                    base_url: None,
                    endpoint_type: Some("token-plan".into()),
                    vision_model: Some("mimo-v2.5".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let (api_key, base_url, vision_model) = active_provider_vision(&config).unwrap();
        assert_eq!(api_key, "tp-test-key");
        assert!(
            base_url.contains("token-plan-ams.xiaomimimo.com"),
            "token-plan endpoint, got {base_url}"
        );
        assert!(!base_url.contains("openai.com"), "never guess OpenAI");
        assert_eq!(vision_model, "mimo-v2.5");
    }

    #[test]
    fn keyless_remote_builtin_skipped() {
        // A remote built-in with vision_model but NO key cannot serve
        // vision (the gate is vision_model + usable key): skipped instead
        // of shipping a doomed candidate.
        let mut config = two_providers_config();
        config.providers.openai.as_mut().unwrap().api_key = None;

        let cands = crate::brain::provider::factory::any_provider_vision(&config);
        assert_eq!(cands.len(), 1, "only minimax remains: {cands:?}");
        assert_eq!(cands[0].0, "minimax-key");
    }

    #[test]
    fn builtin_without_base_url_or_known_default_skipped() {
        // zhipu has vision_model + key but no base_url and no known
        // OpenAI-compatible default in the vision resolver: skipped,
        // NEVER pointed at api.openai.com (#430).
        let config = Config {
            providers: ProviderConfigs {
                zai: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("zhipu-key".into()),
                    base_url: None,
                    vision_model: Some("glm-4.6v".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(vision_candidates(&config).is_empty());
    }

    #[test]
    fn scan_all_wins_when_all_chain_entries_fail() {
        // All chain entries fail (no vision_model, nonexistent) — falls
        // through to scan-all, where nothing has a vision_model either.
        // NOTE: disabled is NOT a failure mode (#401): the only vision
        // gate, whole app lifetime, is a configured vision_model.
        let mut config = two_providers_config();
        config.providers.openai.as_mut().unwrap().vision_model = None;
        config.providers.minimax.as_mut().unwrap().vision_model = None;
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["nonexistent".into(), "openai".into(), "minimax".into()],
            ..Default::default()
        });

        assert!(active_provider_vision(&config).is_none());
    }

    #[test]
    fn chain_custom_provider() {
        // Custom provider in the fallback chain.
        let mut custom = BTreeMap::new();
        custom.insert(
            "localllm".to_string(),
            ProviderConfig {
                enabled: true,
                api_key: None,
                base_url: Some("http://localhost:11434/v1".into()),
                vision_model: Some("llava".into()),
                ..Default::default()
            },
        );

        let mut config = two_providers_config();
        config.providers.custom = Some(custom);
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["localllm".into()],
            ..Default::default()
        });

        let cands = vision_candidates(&config);
        assert!(
            cands
                .iter()
                .any(|c| c.api_key.is_empty() && c.model == "llava"),
            "keyless local custom is a candidate: {cands:?}"
        );
    }

    #[test]
    fn chain_custom_with_prefix() {
        // custom:name prefix convention.
        let mut custom = BTreeMap::new();
        custom.insert(
            "myvision".to_string(),
            ProviderConfig {
                enabled: true,
                api_key: Some("custom-key".into()),
                base_url: Some("http://localhost:8080/v1".into()),
                vision_model: Some("my-model".into()),
                ..Default::default()
            },
        );

        let mut config = two_providers_config();
        config.providers.custom = Some(custom);
        config.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: vec!["custom:myvision".into()],
            ..Default::default()
        });

        let cands = vision_candidates(&config);
        assert!(
            cands
                .iter()
                .any(|c| c.api_key == "custom-key" && c.model == "my-model"),
            "custom: prefixed chain entry resolves: {cands:?}"
        );
    }

    #[test]
    fn toml_deserialization_with_vision_field() {
        let toml_str = r#"
enabled = true
providers = ["openrouter", "anthropic"]
vision = ["minimax", "anthropic"]
"#;
        let cfg: FallbackProviderConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.providers, vec!["openrouter", "anthropic"]);
        assert_eq!(cfg.vision, vec!["minimax", "anthropic"]);
    }

    #[test]
    fn toml_deserialization_without_vision_field() {
        // Old configs without vision field should still work (default empty).
        let toml_str = r#"
enabled = true
providers = ["openrouter", "anthropic"]
"#;
        let cfg: FallbackProviderConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.vision.is_empty());
    }
}
// Mirrors the vision-helper suite above. `generation_model` is the
// 2026-05-18 follow-up so binary users can override the image-gen
// model without leaving the TUI.
mod active_provider_generation {
    use crate::brain::provider::factory::{active_provider_generation, effective_generation_model};
    use crate::config::{Config, ProviderConfig, ProviderConfigs};

    #[test]
    fn returns_none_when_no_generation_model_set() {
        let config = Config::default();
        assert!(active_provider_generation(&config).is_none());
    }

    #[test]
    fn returns_override_from_active_provider() {
        let config = Config {
            providers: ProviderConfigs {
                gemini: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("gemini-key".into()),
                    base_url: Some("https://generativelanguage.googleapis.com/v1beta".into()),
                    default_model: Some("gemini-3.6-flash".into()),
                    generation_model: Some("imagen-4.0-generate-001".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let (api_key, _, model) = active_provider_generation(&config).expect("must resolve");
        assert_eq!(api_key, "gemini-key");
        assert_eq!(model, "imagen-4.0-generate-001");
    }

    #[test]
    fn registers_keyless_provider_with_generation_model() {
        // Mirror of the vision path: a keyless / local custom provider with a
        // `generation_model` set still resolves, with an empty Bearer.
        let mut custom = std::collections::BTreeMap::new();
        custom.insert(
            "localllm".to_string(),
            ProviderConfig {
                enabled: true,
                api_key: None,
                base_url: Some("http://localhost:11434/v1".into()),
                generation_model: Some("sd-xl".into()),
                ..Default::default()
            },
        );
        let config = Config {
            providers: ProviderConfigs {
                custom: Some(custom),
                ..Default::default()
            },
            ..Default::default()
        };
        let (api_key, _, model) = active_provider_generation(&config)
            .expect("keyless provider with generation_model must resolve");
        assert_eq!(api_key, "");
        assert_eq!(model, "sd-xl");
    }

    #[test]
    fn effective_falls_back_to_global_when_no_override() {
        // Provider has no `generation_model` → fall back to the global
        // `image.generation.model` (whose default is the seeded Gemini
        // value from /onboard).
        let config = Config::default();
        let fallback = effective_generation_model(&config);
        assert_eq!(fallback, config.image.generation.model);
    }

    #[test]
    fn effective_prefers_provider_override_over_global() {
        let config = Config {
            providers: ProviderConfigs {
                gemini: Some(ProviderConfig {
                    enabled: true,
                    api_key: Some("gemini-key".into()),
                    base_url: Some("https://generativelanguage.googleapis.com/v1beta".into()),
                    default_model: Some("gemini-3.6-flash".into()),
                    generation_model: Some("imagen-4.0-generate-001".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            effective_generation_model(&config),
            "imagen-4.0-generate-001"
        );
    }
}

// --- Normalised chains (#1355) ---

mod normalized_chain {
    use crate::brain::provider::factory::{normalized_fallback_chain, normalized_vision_chain};
    use crate::config::Config;

    fn config(json: &str) -> Config {
        serde_json::from_str(json).expect("fixture config")
    }

    #[test]
    fn entries_are_normalised_and_deduped_in_order() {
        let cfg = config(
            r#"{
                "providers": {
                    "fallback": {
                        "enabled": true,
                        "provider": "custom:myprovider",
                        "providers": ["custom:myprovider", "minimax/MiniMax-M2", "anthropic"]
                    },
                    "custom": { "myprovider": { "base_url": "http://localhost:1/v1", "api_key": "k" } },
                    "minimax": { "enabled": true, "api_key": "k" }
                }
            }"#,
        );
        assert_eq!(
            normalized_fallback_chain(&cfg),
            vec!["myprovider", "minimax", "anthropic"],
            "custom: dropped, provider/model split to its provider, legacy duplicate collapsed"
        );
    }

    #[test]
    fn bare_names_pass_through_unchanged() {
        let cfg = config(
            r#"{
                "providers": {
                    "fallback": { "enabled": true, "providers": ["anthropic", "openai"], "vision": ["minimax"] },
                    "minimax": { "enabled": true, "api_key": "k" }
                }
            }"#,
        );
        assert_eq!(normalized_fallback_chain(&cfg), vec!["anthropic", "openai"]);
        assert_eq!(normalized_vision_chain(&cfg), vec!["minimax"]);
    }

    #[test]
    fn the_vision_list_follows_the_same_rule() {
        let cfg = config(
            r#"{
                "providers": {
                    "fallback": { "enabled": true, "vision": ["custom:myprovider", "myprovider/some-vision"] },
                    "custom": { "myprovider": { "base_url": "http://localhost:1/v1", "api_key": "k" } }
                }
            }"#,
        );
        assert_eq!(normalized_vision_chain(&cfg), vec!["myprovider"]);
    }

    #[test]
    fn no_fallback_section_means_empty_chains() {
        let cfg = config(r#"{ "providers": {} }"#);
        assert!(normalized_fallback_chain(&cfg).is_empty());
        assert!(normalized_vision_chain(&cfg).is_empty());
    }
}
