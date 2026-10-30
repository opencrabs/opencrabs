//! The built-in OpenCrabs provider serves transcription once its `stt_model`
//! is set and an install token is stored. It serves no speech route in any
//! shape, so a `tts_model` on it must never become a synthesis candidate.
//!
//! The endpoint handed back is the proxy HOST ROOT, not the `/v1` base: the
//! [OI] audio clients join `v1/...` onto it themselves
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
default_model = "deepseek-v4.1-flash"
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
        "base URL must be the host root; the chat-route client appends v1/chat/completions"
    );
    assert_eq!(cand.2, AUDIO_MODEL);
}

#[test]
fn opencrabs_is_never_a_tts_candidate() {
    // The endpoint answers `/v1/audio/speech` with a 404 in every shape, so a
    // `tts_model` on it would guarantee one failed round trip per spoken
    // reply. Synthesis must fall through to the local voice, a
    // `[providers.tts.*]` block, or Voicebox instead.
    let config = cfg(&opencrabs_toml(Some("ocp_test_token"), true));
    assert!(
        active_provider_tts(&config).is_none(),
        "the built-in endpoint serves no speech route, so it cannot be a candidate"
    );
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

/// A custom provider on a normal [OI]-compatible host. Speech works there, so
/// it stays a synthesis candidate exactly as before.
const CUSTOM_TTS_TOML: &str = r#"
[providers.custom.lm-studio]
enabled = true
base_url = "http://localhost:1234/v1"
api_key = "local-key"
default_model = "some-chat-model"
tts_model = "tts-1"
"#;

#[test]
fn a_custom_provider_with_a_tts_model_stays_a_tts_candidate() {
    let config = cfg(CUSTOM_TTS_TOML);
    let cand = active_provider_tts(&config).expect("a real speech endpoint must stay a candidate");
    assert_eq!(cand.0, "local-key");
    assert_eq!(cand.1, "http://localhost:1234");
    assert_eq!(cand.2, "tts-1");
}

#[test]
fn a_custom_section_pointing_at_the_built_in_host_is_not_a_tts_candidate() {
    // The install token reaches the same endpoint under a different section
    // name. The exclusion keys off the host, not the section name, so this
    // copy is caught too.
    let config = cfg(r#"
[providers.custom.copied]
enabled = true
base_url = "https://inference.opencrabs.com/v1"
api_key = "ocp_test_token"
default_model = "deepseek-v4.1-flash"
tts_model = "qwen3.8-omni-flash"
"#);
    assert!(
        active_provider_tts(&config).is_none(),
        "a custom section on the built-in host serves no speech either"
    );
}

/// The candidate must carry the id the proxy publishes for voice, which is the
/// alias `qwen3.8-omni-flash`. A publisher-prefixed routing id (`mimo/...`,
/// `ocg/...`) stays inside the proxy's own rotation and must never reach a
/// client config, so shipping one here would ask the proxy for an id it does
/// not serve under that spelling.
#[test]
fn the_stt_candidate_carries_the_published_alias_not_a_routing_id() {
    let config = cfg(&opencrabs_toml(Some("ocp_test_token"), true));
    let (_, _, model) = active_provider_stt(&config).expect("candidate");
    assert_eq!(model, "qwen3.8-omni-flash");
    assert!(
        !model.contains('/'),
        "a routing id leaked into the client candidate: {model}"
    );
}

/// A `tts_model` with no `stt_model` is the case that used to look configured
/// and was not: speech has no route at all, so neither candidate may appear.
#[test]
fn a_speech_model_alone_is_neither_an_stt_nor_a_tts_candidate() {
    let config = cfg(r#"
[providers.opencrabs]
enabled = true
api_key = "ocp_test_token"
default_model = "deepseek-v4.1-flash"
tts_model = "qwen3.8-omni-flash"
"#);
    assert!(
        active_provider_stt(&config).is_none(),
        "transcription needs stt_model, not tts_model"
    );
    assert!(
        active_provider_tts(&config).is_none(),
        "the endpoint serves no speech route in any shape"
    );
}
