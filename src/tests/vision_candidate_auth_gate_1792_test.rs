//! #1792 item 2: a vision candidate that cannot satisfy vision auth must be
//! excluded at resolution, not attempted.
//!
//! The reported shape: `[providers.fallback].vision` ended on an OpenCode Go
//! entry, `analyze_image` walked the chain to it, and the gateway answered
//! `400`. Because the entry was configured with a `vision_model` and no key,
//! `custom_vision_candidate` handed the chain an empty credential and the call
//! went out anyway as `Authorization: Bearer ` -- a request that can only fail,
//! one per configured name, and the merged error blamed the whole chain.
//!
//! `builtin_vision_candidate` already refused exactly this. These pin that the
//! custom path refuses it too, and that the refusal did not widen: keyless
//! LOCAL endpoints stay in (Ollama, llama.cpp, LM Studio), and `enabled` is
//! still NOT a vision gate (#401 -- it gates chat only).

use crate::brain::provider::factory::{vision_candidates, vision_candidates_for};
use crate::config::{Config, FallbackProviderConfig, ProviderConfig, ProviderConfigs};
use std::collections::BTreeMap;

/// The shape from the report: an OpenCode Go custom with a `vision_model`,
/// `enabled = false`, and no key anywhere.
fn opencode_go(key: Option<&str>) -> ProviderConfig {
    ProviderConfig {
        enabled: false,
        api_key: key.map(|k| k.to_string()),
        base_url: Some("https://opencode.ai/zen/go/v1".into()),
        default_model: Some("kimi-k3".into()),
        models: vec![],
        vision_model: Some("kimi-k3".into()),
        ..Default::default()
    }
}

fn config_with_custom(name: &str, cfg: ProviderConfig, chain: Vec<String>) -> Config {
    let mut custom = BTreeMap::new();
    custom.insert(name.to_string(), cfg);
    Config {
        providers: ProviderConfigs {
            custom: Some(custom),
            ..Default::default()
        },
        ..Default::default()
    }
    .with_chain(chain)
}

trait WithChain {
    fn with_chain(self, chain: Vec<String>) -> Self;
}

impl WithChain for Config {
    fn with_chain(mut self, chain: Vec<String>) -> Self {
        self.providers.fallback = Some(FallbackProviderConfig {
            generation: vec![],
            enabled: true,
            vision: chain,
            ..Default::default()
        });
        self
    }
}

/// The bug: a keyless remote custom must never be offered as a candidate.
#[test]
fn keyless_remote_custom_is_not_a_vision_candidate() {
    let config = config_with_custom(
        "opencode-go",
        opencode_go(None),
        vec!["opencode-go".to_string()],
    );
    let cands = vision_candidates(&config);
    assert!(
        cands.is_empty(),
        "keyless remote entry must be excluded, got: {cands:?}"
    );
}

/// Same entry, with the credential it needs: back in the chain. The gate is
/// about auth, not about the provider's name.
#[test]
fn keyed_remote_custom_is_a_vision_candidate() {
    let config = config_with_custom(
        "opencode-go",
        opencode_go(Some("go-key")),
        vec!["opencode-go".to_string()],
    );
    let cands = vision_candidates(&config);
    assert_eq!(
        cands.len(),
        1,
        "keyed entry must survive the gate: {cands:?}"
    );
    assert_eq!(cands[0].api_key, "go-key");
    assert!(
        cands[0]
            .base_url
            .starts_with("https://opencode.ai/zen/go/v1/chat/completions"),
        "url normalized as before: {}",
        cands[0].base_url
    );
}

/// An empty string is not a credential. Without the filter this became
/// `Some("")` and sailed past the guard.
#[test]
fn empty_string_key_is_no_key() {
    let config = config_with_custom(
        "opencode-go",
        opencode_go(Some("")),
        vec!["opencode-go".to_string()],
    );
    assert!(
        vision_candidates(&config).is_empty(),
        "api_key = \"\" must not count as auth"
    );
}

/// The gate must not widen past the builtin rule: a keyless LOCAL endpoint is
/// a legitimate candidate (Ollama, llama.cpp, LM Studio) and the tool has to
/// keep offering it.
#[test]
fn keyless_local_custom_is_still_a_candidate() {
    let mut local = opencode_go(None);
    local.base_url = Some("http://localhost:11434/v1".into());
    local.vision_model = Some("llava".into());
    let config = config_with_custom("ollama-local", local, vec!["ollama-local".to_string()]);
    let cands = vision_candidates(&config);
    assert_eq!(
        cands.len(),
        1,
        "keyless local endpoint must stay a candidate: {cands:?}"
    );
    assert!(
        cands[0].api_key.is_empty(),
        "local stays keyless: {:?}",
        cands[0].api_key
    );
}

/// #401 stands: `enabled` gates chat, not vision. A disabled-but-keyed
/// provider is still a usable pair of (key, endpoint), so the gate must not
/// grow an `enabled` check on the way to fixing auth.
#[test]
fn disabled_but_keyed_custom_is_still_a_candidate() {
    assert!(
        !opencode_go(None).enabled,
        "fixture must be disabled to prove the point"
    );
    let config = config_with_custom(
        "opencode-go",
        opencode_go(Some("go-key")),
        vec!["opencode-go".to_string()],
    );
    assert_eq!(
        vision_candidates(&config).len(),
        1,
        "enabled = false must not be a vision gate (#401)"
    );
}

/// The reported session shape: the session is RUNNING on the keyless custom,
/// so step 1 of resolution (session provider first) used to inject it. With
/// the gate the chain resolves to nothing instead of to a doomed request.
#[test]
fn session_provider_candidate_is_gated_too() {
    let config = config_with_custom("opencode-go", opencode_go(None), vec![]);
    let cands = vision_candidates_for(&config, Some("opencode-go"));
    assert!(
        cands.is_empty(),
        "the session provider must not bypass the auth gate: {cands:?}"
    );
}

/// The complaint was "every call burns the full chain". A chain of nothing but
/// unauthable entries must resolve to zero candidates, which is the difference
/// between N round trips and none.
#[test]
fn all_keyless_chain_burns_no_round_trips() {
    let mut custom = BTreeMap::new();
    custom.insert("opencode-go".to_string(), opencode_go(None));
    let mut second = opencode_go(None);
    second.base_url = Some("https://api.example-remote.test/v1".into());
    second.vision_model = Some("other-vision".into());
    custom.insert("other-remote".to_string(), second);

    let config = Config {
        providers: ProviderConfigs {
            custom: Some(custom),
            ..Default::default()
        },
        ..Default::default()
    }
    .with_chain(vec!["opencode-go".to_string(), "other-remote".to_string()]);

    let cands = vision_candidates(&config);
    assert!(
        cands.is_empty(),
        "no entry in this chain can authenticate, so none may be attempted: {cands:?}"
    );
}
