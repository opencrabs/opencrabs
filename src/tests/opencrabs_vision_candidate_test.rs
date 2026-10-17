//! The built-in OpenCrabs provider serves images on day 0: its
//! `vision_model` must resolve to a candidate once the install token is
//! stored, and never before (a keyless remote endpoint is not a candidate).

use crate::brain::provider::factory::any_provider_vision;
use crate::brain::provider::opencrabs_install::OPENCRABS_BASE_URL;
use crate::config::{Config, ProviderConfig, ProviderConfigs};

fn opencrabs_config(api_key: Option<&str>) -> Config {
    Config {
        providers: ProviderConfigs {
            opencrabs: Some(ProviderConfig {
                enabled: false,
                api_key: api_key.map(|k| k.to_string()),
                default_model: Some("cb/deepseek-v4.1-flash".into()),
                vision_model: Some("cb/deepseek-v4.1-flash".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn opencrabs_with_token_is_a_vision_candidate() {
    let cands = any_provider_vision(&opencrabs_config(Some("ocp_test_token")));
    assert_eq!(
        cands,
        vec![(
            "ocp_test_token".to_string(),
            format!("{OPENCRABS_BASE_URL}/chat/completions"),
            "cb/deepseek-v4.1-flash".to_string(),
        )]
    );
}

#[test]
fn opencrabs_without_token_is_not_a_vision_candidate() {
    let cands = any_provider_vision(&opencrabs_config(None));
    assert!(
        cands.is_empty(),
        "keyless remote entry must be excluded, got: {cands:?}"
    );
}

#[test]
fn shipped_example_enables_vision_for_opencrabs() {
    let example = include_str!("../../config.toml.example");
    let parsed: toml::Value = toml::from_str(example).expect("example config parses");
    let vision = parsed["providers"]["opencrabs"]["vision_model"]
        .as_str()
        .expect("example sets providers.opencrabs.vision_model");
    assert_eq!(vision, "cb/deepseek-v4.1-flash");
}
