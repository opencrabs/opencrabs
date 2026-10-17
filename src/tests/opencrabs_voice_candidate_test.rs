//! The built-in OpenCrabs provider serves transcription and synthesis once
//! its `stt_model` / `tts_model` are set and an install token is stored.
//!
//! The endpoint handed back is the proxy HOST ROOT, not the `/v1` base: the
//! [OI] audio clients join `v1/audio/...` onto it themselves
//! (`openai_tts::build_endpoint_url`), so a `.../v1` base would produce
//! `.../v1/v1/audio/speech`. A keyless section must not become a candidate
//! either — a remote endpoint with no credential is a guaranteed 401
//! (#1792), the same gate the vision candidates use.

use crate::brain::provider::factory::{active_provider_stt, active_provider_tts};
use crate::config::Config;

/// The proxy root with `/v1` stripped, which is what the audio clients want.
const OPENCRABS_HOST_ROOT: &str = "https://inference.opencrabs.com";

const AUDIO_MODEL: &str = "qwen3.8-omni-flash";

fn cfg(toml: &str) -> Config {
    toml::from_str(toml).expect("config toml parses")
}

fn opencrabs_toml(api_key: Option<&str>, with_audio_models: bool) -> String {
    let key_line = match api_key {
        Some(k) => format!("api_key = \"{k}\""),
        None => String::new(),
    };
    let audio_lines = if with_audio_models {
        format!("stt_model = \"{AUDIO_MODEL}\"\ntts_model = \"{AUDIO_MODEL}\"\n")
    } else {
        String::new()
    };
    format!(
        r#"
[providers.opencrabs]
enabled = true
{key_line}
default_model = "cb/deepseek-v4.1-flash"
{audio_lines}"#
    )
}

#[test]
fn opencrabs_with_token_is_an_stt_candidate_at_the_host_root() {
    let config = cfg(&opencrabs_toml(Some("ocp_test_token"), true));
    let cand = active_provider_stt(&config).expect("built-in provider must be an STT candidate");
    assert_eq!(
        cand.0, "ocp_test_token",
        "install token must be the credential"
    );
    assert_eq!(
        cand.1, OPENCRABS_HOST_ROOT,
        "base URL must be the host root; the client appends v1/audio/..."
    );
    assert_eq!(cand.2, AUDIO_MODEL);
}

#[test]
fn opencrabs_with_token_is_a_tts_candidate_at_the_host_root() {
    let config = cfg(&opencrabs_toml(Some("ocp_test_token"), true));
    let cand = active_provider_tts(&config).expect("built-in provider must be a TTS candidate");
    assert_eq!(cand.0, "ocp_test_token");
    assert_eq!(cand.1, OPENCRABS_HOST_ROOT);
    assert_eq!(cand.2, AUDIO_MODEL);
}

#[test]
fn opencrabs_without_token_is_not_an_audio_candidate() {
    let config = cfg(&opencrabs_toml(None, true));
    assert!(
        active_provider_stt(&config).is_none(),
        "keyless remote endpoint must be excluded (#1792)"
    );
    assert!(
        active_provider_tts(&config).is_none(),
        "keyless remote endpoint must be excluded (#1792)"
    );
}

#[test]
fn opencrabs_without_audio_models_is_not_an_audio_candidate() {
    // A token alone is not enough: the provider must declare the model,
    // exactly as `generation_model` gates image generation.
    let config = cfg(&opencrabs_toml(Some("ocp_test_token"), false));
    assert!(active_provider_stt(&config).is_none());
    assert!(active_provider_tts(&config).is_none());
}

#[test]
fn opencrabs_audio_base_url_is_reduced_from_the_v1_default() {
    // The registered default base is `.../v1`; the candidate must come back
    // with that segment stripped so the client does not double it.
    let config = cfg(&opencrabs_toml(Some("ocp_test_token"), true));
    let (_, url, _) = active_provider_stt(&config).expect("candidate");
    assert!(
        !url.ends_with("/v1"),
        "trailing /v1 must be stripped, got: {url}"
    );
    assert_eq!(url, OPENCRABS_HOST_ROOT);
}
