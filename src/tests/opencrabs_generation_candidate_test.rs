//! The built-in OpenCrabs provider serves image generation once its
//! `generation_model` is set and an install token is stored. The endpoint
//! must be derived (never skipped as unknown), and a keyless section must
//! not become a candidate.

use crate::brain::provider::factory::generation_candidates_for;
use crate::brain::provider::opencrabs_install::OPENCRABS_BASE_URL;
use crate::config::{Config, ProviderConfig, ProviderConfigs};

const IMAGE_MODEL: &str = "cb/gemini-2.5-flash-image";

fn opencrabs_config(api_key: Option<&str>) -> Config {
    Config {
        providers: ProviderConfigs {
            opencrabs: Some(ProviderConfig {
                enabled: false,
                api_key: api_key.map(|k| k.to_string()),
                default_model: Some("deepseek-v4.1-flash".into()),
                generation_model: Some(IMAGE_MODEL.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn opencrabs_with_token_is_a_generation_candidate() {
    let cands =
        generation_candidates_for(&opencrabs_config(Some("ocp_test_token")), Some("opencrabs"));
    assert!(
        cands.iter().any(|(key, url, model)| key == "ocp_test_token"
            && url == OPENCRABS_BASE_URL
            && model == IMAGE_MODEL),
        "built-in provider must be a generation candidate, got: {cands:?}"
    );
}

#[test]
fn opencrabs_without_token_is_not_a_generation_candidate() {
    let cands = generation_candidates_for(&opencrabs_config(None), Some("opencrabs"));
    assert!(
        !cands.iter().any(|(_, url, _)| url == OPENCRABS_BASE_URL),
        "keyless built-in entry must be excluded, got: {cands:?}"
    );
}
