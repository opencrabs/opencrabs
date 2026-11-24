//! Regression guards for #1689: the two native families — `anthropic` and
//! `gemini` — consume both timeout keys through the same three-tier chain the
//! OpenAI-compatible family got in #1688.
//!
//! Before this, neither family read `timeout_secs` or
//! `stream_idle_timeout_secs` at *any* tier: the values were parsed into
//! `ProviderConfig`, stored, and dropped on the floor, while the README showed
//! `[providers.anthropic] timeout_secs = 120` as its headline example. A
//! documented no-op is worse than an undocumented one, because nobody thinks to
//! check.
//!
//! These go through `create_provider` rather than calling the setters directly,
//! because the defect was in the wiring: the setters existed in spirit, the
//! factory just never reached for them.

use crate::brain::provider::anthropic::AnthropicProvider;
use crate::brain::provider::factory::create_provider;
use crate::brain::provider::gemini::GeminiProvider;
use crate::brain::provider::r#trait::Provider;
use crate::config::{Config, ProviderConfig, ProviderConfigs};
use std::path::Path;
use std::time::Duration;

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

/// A config with exactly one native family configured, keyed, and enabled.
fn config_with(family: &str, provider: ProviderConfig) -> Config {
    let mut config = Config::default();
    let anthropic = (family == "anthropic").then(|| provider.clone());
    let gemini = (family == "gemini").then(|| provider.clone());
    config.providers = ProviderConfigs {
        anthropic,
        gemini,
        ..Default::default()
    };
    config
}

fn native(provider_timeout: Option<u64>, provider_idle: Option<u64>) -> ProviderConfig {
    ProviderConfig {
        enabled: true,
        api_key: Some("test-key".to_string()),
        timeout_secs: provider_timeout,
        stream_idle_timeout_secs: provider_idle,
        ..Default::default()
    }
}

// ------------------------------------------------------------ anthropic

#[tokio::test]
async fn anthropic_reads_the_global_timeout_tier() {
    let mut config = config_with("anthropic", native(None, None));
    config.agent.timeout_secs = Some(120);
    config.agent.stream_idle_timeout_secs = Some(45);

    let provider = create_provider(&config).await.expect("anthropic builds");
    assert_eq!(provider.name(), "anthropic");
    assert_eq!(
        provider.request_timeout(),
        Some(Duration::from_secs(120)),
        "`[agent] timeout_secs` still does not reach the anthropic family (#1689)"
    );
    assert_eq!(
        provider.stream_idle_timeout(),
        Some(Duration::from_secs(45)),
        "`[agent] stream_idle_timeout_secs` still does not reach the anthropic family (#1689)"
    );
}

#[tokio::test]
async fn anthropic_prefers_its_own_tier_over_the_global_one() {
    let mut config = config_with("anthropic", native(Some(30), Some(10)));
    config.agent.timeout_secs = Some(120);
    config.agent.stream_idle_timeout_secs = Some(45);

    let provider = create_provider(&config).await.expect("anthropic builds");
    assert_eq!(provider.request_timeout(), Some(Duration::from_secs(30)));
    assert_eq!(
        provider.stream_idle_timeout(),
        Some(Duration::from_secs(10))
    );
}

// --------------------------------------------------------------- gemini

#[tokio::test]
async fn gemini_reads_the_global_timeout_tier() {
    let mut config = config_with("gemini", native(None, None));
    config.agent.timeout_secs = Some(120);
    config.agent.stream_idle_timeout_secs = Some(45);

    let provider = create_provider(&config).await.expect("gemini builds");
    assert_eq!(provider.name(), "gemini");
    assert_eq!(
        provider.request_timeout(),
        Some(Duration::from_secs(120)),
        "`[agent] timeout_secs` still does not reach the gemini family (#1689)"
    );
    assert_eq!(
        provider.stream_idle_timeout(),
        Some(Duration::from_secs(45)),
        "`[agent] stream_idle_timeout_secs` still does not reach the gemini family (#1689)"
    );
}

#[tokio::test]
async fn gemini_prefers_its_own_tier_over_the_global_one() {
    let mut config = config_with("gemini", native(Some(30), Some(10)));
    config.agent.timeout_secs = Some(120);
    config.agent.stream_idle_timeout_secs = Some(45);

    let provider = create_provider(&config).await.expect("gemini builds");
    assert_eq!(provider.request_timeout(), Some(Duration::from_secs(30)));
    assert_eq!(
        provider.stream_idle_timeout(),
        Some(Duration::from_secs(10))
    );
}

// ------------------------------------------------------------- defaults

#[tokio::test]
async fn unset_timeouts_are_reported_as_no_override_not_as_a_number() {
    // The accessors answer "what did the user ask for?", not "what is in force".
    // The request ceiling's in-force default is the family's compiled
    // `DEFAULT_TIMEOUT`, already on the client `new()` built; the idle
    // default is picked per-request in `helpers.rs` (3600s local/CLI, 20s
    // remote). Reporting either as a number here would make the
    // runtime table look like an override it never was.
    for family in ["anthropic", "gemini"] {
        let config = config_with(family, native(None, None));
        let provider = create_provider(&config)
            .await
            .unwrap_or_else(|e| panic!("{family} builds: {e}"));
        assert_eq!(
            provider.request_timeout(),
            None,
            "{family} reported a request-timeout override that nobody configured"
        );
        assert_eq!(
            provider.stream_idle_timeout(),
            None,
            "{family} reported a stream-idle override that nobody configured, \
             which would shadow the helpers.rs runtime table"
        );
    }
}

#[tokio::test]
async fn a_zero_at_the_global_tier_is_skipped_not_honoured() {
    // `0` seconds is an instant deadline, not "no timer". It must fall through
    // to the compiled default rather than reach reqwest.
    for family in ["anthropic", "gemini"] {
        let mut config = config_with(family, native(None, None));
        config.agent.timeout_secs = Some(0);
        config.agent.stream_idle_timeout_secs = Some(0);

        let provider = create_provider(&config)
            .await
            .unwrap_or_else(|e| panic!("{family} builds: {e}"));
        assert_eq!(
            provider.request_timeout(),
            None,
            "{family} honoured a zero-second timeout instead of skipping it"
        );
        assert_eq!(provider.stream_idle_timeout(), None);
    }
}

// -------------------------------------------------- the setters, directly

/// The factory tests prove the wiring; these prove the two builders themselves
/// behave, so a failure can be attributed to the right layer.
#[test]
fn the_native_setters_record_what_they_apply() {
    let anthropic = AnthropicProvider::new("k".to_string())
        .with_timeout(Duration::from_secs(120))
        .with_stream_idle_timeout(Duration::from_secs(45));
    assert_eq!(anthropic.request_timeout(), Some(Duration::from_secs(120)));
    assert_eq!(
        anthropic.stream_idle_timeout(),
        Some(Duration::from_secs(45))
    );

    let gemini = GeminiProvider::new("k".to_string())
        .with_timeout(Duration::from_secs(120))
        .with_stream_idle_timeout(Duration::from_secs(45));
    assert_eq!(gemini.request_timeout(), Some(Duration::from_secs(120)));
    assert_eq!(gemini.stream_idle_timeout(), Some(Duration::from_secs(45)));
}

#[test]
fn a_fresh_native_provider_reports_no_override() {
    // `new()` puts the compiled 300s on the client and reports `None` through
    // the trait: the accessor answers "did the user configure this?", and the
    // answer must not be the family's own floor.
    assert_eq!(
        AnthropicProvider::new("k".to_string()).request_timeout(),
        None
    );
    assert_eq!(
        AnthropicProvider::new("k".to_string()).stream_idle_timeout(),
        None
    );
    assert_eq!(GeminiProvider::new("k".to_string()).request_timeout(), None);
    assert_eq!(
        GeminiProvider::new("k".to_string()).stream_idle_timeout(),
        None
    );
}

// ------------------------------------------------------- the #1687 seam
/// The whole point of #1687 was that the total wall clock must live on the
/// non-streaming client and nowhere near the stream path. A `with_timeout` that
/// rebuilt both clients would silently re-create that defect behind a config
/// key that looks harmless.
#[test]
fn with_timeout_touches_only_the_request_client() {
    for (rel, family) in [
        ("src/brain/provider/anthropic.rs", "anthropic"),
        ("src/brain/provider/gemini.rs", "gemini"),
    ] {
        let src = source(rel);
        let start = src
            .find("pub fn with_timeout(")
            .unwrap_or_else(|| panic!("{family}: `with_timeout()` is gone (#1689)"));
        let rest = &src[start..];
        let body = &rest[..rest.find("\n    }").unwrap_or(rest.len())];

        assert!(
            body.contains("self.client = crate::utils::http::buffered_transport("),
            "{family}: `with_timeout()` no longer swaps the buffered client, \
             so the key is a no-op again (#1689)"
        );
        assert!(
            !body.contains("stream_client"),
            "{family}: `with_timeout()` touches `stream_client` — that is the \
             #1687 wall clock going back onto every SSE body:\n{body}"
        );
    }
}

#[test]
fn both_native_families_carry_the_idle_timeout_accessor() {
    for (rel, family) in [
        ("src/brain/provider/anthropic.rs", "anthropic"),
        ("src/brain/provider/gemini.rs", "gemini"),
    ] {
        let src = source(rel);
        for needle in [
            "pub fn with_stream_idle_timeout(",
            "fn request_timeout(&self) -> Option<Duration>",
            "fn stream_idle_timeout(&self) -> Option<Duration>",
        ] {
            assert!(
                src.contains(needle),
                "{family}: {needle} is missing — the family stopped exposing a \
                 timeout to the trait (#1689)"
            );
        }
    }
}

/// Setters that exist but are never called are the exact shape of #1689. This
/// scans the factory so a future refactor that drops the wiring fails here
/// rather than in a user's production turn.
#[test]
fn the_factory_wires_the_chain_into_every_family() {
    let src = source("src/brain/provider/factory.rs");
    assert!(
        src.matches("report_timeout_chain(").count() >= 3,
        "expected report_timeout_chain() in the compat, anthropic and gemini \
         families, found {}",
        src.matches("report_timeout_chain(").count()
    );
    for needle in [
        "super::anthropic::DEFAULT_TIMEOUT",
        "super::gemini::DEFAULT_TIMEOUT",
        "super::custom_openai_compatible::DEFAULT_TIMEOUT",
    ] {
        assert!(
            src.contains(needle),
            "factory.rs no longer names {needle} as a compiled floor — a \
             family's default dropped out of the chain (#1689)"
        );
    }
}

/// The compiled floors must agree: they were all 300s before this change, and a
/// family drifting from that is a silent behavioural split again.
#[test]
fn the_native_families_share_the_compiled_ceiling() {
    use crate::brain::provider::custom_openai_compatible::DEFAULT_TIMEOUT as COMPAT;
    assert_eq!(
        crate::brain::provider::anthropic::DEFAULT_TIMEOUT,
        crate::brain::provider::gemini::DEFAULT_TIMEOUT
    );
    assert_eq!(
        crate::brain::provider::anthropic::DEFAULT_TIMEOUT,
        COMPAT,
        "the three families no longer agree on the compiled non-streaming \
         ceiling — #1635 was exactly this split"
    );
    assert_eq!(
        crate::brain::provider::anthropic::DEFAULT_TIMEOUT.as_secs(),
        300
    );
}
