//! `Config::voice_config()` derives the runtime voice view from the
//! `[providers.stt.*]` / `[providers.tts.*]` engine blocks. A disabled block
//! must contribute nothing (#1399): the dispatcher chooses providers by the
//! presence of the derived fields, so a switched-off openai_compatible entry
//! that still carried its base_url was dispatched first on every voice note
//! and failed every time, while the enabled provider waited behind it.

use crate::config::Config;

fn cfg(toml: &str) -> Config {
    toml::from_str(toml).expect("config toml parses")
}

#[test]
fn disabled_openai_compatible_tts_contributes_no_base_url_or_key() {
    let vc = cfg(r#"
[providers.tts.openai_compatible]
enabled = false
base_url = "http://localhost:11434"
model = "tts-1"
api_key = "compat-key"
[providers.tts.openai]
enabled = true
api_key = "openai-key"
"#)
    .voice_config();
    assert_eq!(vc.tts_base_url, None, "disabled block leaked its base_url");
    assert_eq!(vc.tts_api_key.as_deref(), Some("openai-key"));
    assert_eq!(
        vc.tts_provider.as_ref().and_then(|p| p.api_key.as_deref()),
        Some("openai-key")
    );
    assert!(vc.tts_enabled);
}

#[test]
fn enabled_openai_compatible_tts_is_the_derived_endpoint() {
    let vc = cfg(r#"
[providers.tts.openai_compatible]
enabled = true
base_url = "http://localhost:11434"
model = "tts-1"
api_key = "compat-key"
"#)
    .voice_config();
    assert_eq!(vc.tts_base_url.as_deref(), Some("http://localhost:11434"));
    assert_eq!(vc.tts_api_key.as_deref(), Some("compat-key"));
    assert_eq!(vc.tts_model, "tts-1");
}

#[test]
fn disabled_openai_tts_with_a_stored_key_is_not_a_provider() {
    let vc = cfg(r#"
[providers.tts.openai]
enabled = false
api_key = "openai-key"
"#)
    .voice_config();
    assert!(
        vc.tts_provider.is_none(),
        "disabled openai must not be a candidate"
    );
    assert_eq!(vc.tts_api_key, None);
    assert_eq!(vc.tts_base_url, None, "no synthetic api.openai.com URL");
    assert!(!vc.tts_enabled);
}

#[test]
fn openai_tts_alone_does_not_plant_a_base_url() {
    // The OpenAI kind never reads tts_base_url; planting api.openai.com
    // there made the dispatcher label real OpenAI calls openai_compatible.
    let vc = cfg(r#"
[providers.tts.openai]
enabled = true
api_key = "openai-key"
"#)
    .voice_config();
    assert_eq!(vc.tts_base_url, None);
    assert!(vc.tts_provider.is_some());
}

#[test]
fn disabled_groq_stt_with_a_stored_key_is_not_a_provider() {
    let vc = cfg(r#"
[providers.stt.groq]
enabled = false
api_key = "groq-key"
"#)
    .voice_config();
    assert!(vc.stt_provider.is_none());
    assert_eq!(vc.stt_api_key, None);
    assert_eq!(vc.stt_base_url, None, "no synthetic groq URL");
}

#[test]
fn disabled_openai_compatible_stt_contributes_nothing_next_to_enabled_groq() {
    let vc = cfg(r#"
[providers.stt.openai_compatible]
enabled = false
base_url = "http://localhost:11434"
model = "whisper-large-v3-turbo"
api_key = "compat-key"
[providers.stt.groq]
enabled = true
api_key = "groq-key"
"#)
    .voice_config();
    assert_eq!(vc.stt_base_url, None);
    assert_eq!(vc.stt_api_key.as_deref(), Some("groq-key"));
    assert!(vc.stt_provider.is_some());
}

#[test]
fn chains_and_display_preferences_survive_regardless_of_enabled() {
    // The voice/model the wizard seeds its fields from are preferences,
    // not candidates; they stay readable while the engine is off.
    let vc = cfg(r#"
[providers.tts]
fallback_chain = ["openai", "local"]
[providers.tts.openai]
enabled = false
voice = "echo"
model = "gpt-4o-mini-tts"
[providers.stt]
fallback_chain = ["local", "groq"]
"#)
    .voice_config();
    assert_eq!(vc.tts_fallback_chain, vec!["openai", "local"]);
    assert_eq!(vc.stt_fallback_chain, vec!["local", "groq"]);
    assert_eq!(vc.tts_voice, "echo");
    assert_eq!(vc.tts_model, "gpt-4o-mini-tts");
}

#[test]
fn active_provider_audio_models_turn_voice_on_with_no_stt_blocks() {
    // Out of the box: the session's provider carries `stt_model` /
    // `tts_model`, exactly as it carries `vision_model` and
    // `generation_model`. No `[providers.stt.*]` block is written, and
    // voice still works. The provider here is a normal [OI]-compatible
    // endpoint that serves both audio routes, which is what the mechanism
    // is about; the built-in endpoint is covered separately below.
    let vc = cfg(r#"
[providers.custom.lm-studio]
enabled = true
base_url = "http://localhost:1234/v1"
api_key = "local-key"
default_model = "some-chat-model"
stt_model = "whisper-large-v3-turbo"
tts_model = "tts-1"
"#)
    .voice_config();
    assert!(
        vc.stt_enabled,
        "provider stt_model must enable transcription"
    );
    assert!(vc.tts_enabled, "provider tts_model must enable synthesis");
    assert_eq!(vc.stt_base_url.as_deref(), Some("http://localhost:1234"));
    assert_eq!(vc.tts_base_url.as_deref(), Some("http://localhost:1234"));
    assert_eq!(vc.stt_model.as_deref(), Some("whisper-large-v3-turbo"));
    assert_eq!(vc.tts_model, "tts-1");
    assert_eq!(vc.stt_api_key.as_deref(), Some("local-key"));
    assert_eq!(vc.tts_api_key.as_deref(), Some("local-key"));
}

/// The built-in endpoint serves transcription on the chat route and no speech
/// route at all, so its `stt_model` must turn voice input on while its
/// `tts_model` must not turn synthesis on (#2107).
#[test]
fn built_in_provider_enables_transcription_but_never_synthesis() {
    let vc = cfg(r#"
[providers.opencrabs]
enabled = true
api_key = "ocp_test_token"
default_model = "deepseek-v4.1-flash"
stt_model = "qwen3.8-omni-flash"
tts_model = "qwen3.8-omni-flash"
"#)
    .voice_config();
    assert!(
        vc.stt_enabled,
        "the built-in provider transcribes on stt_model"
    );
    // Host root: the [OI] audio client appends `v1/chat/completions` itself.
    assert_eq!(
        vc.stt_base_url.as_deref(),
        Some("https://inference.opencrabs.com")
    );
    assert_eq!(vc.stt_model.as_deref(), Some("qwen3.8-omni-flash"));
    assert_eq!(vc.stt_api_key.as_deref(), Some("ocp_test_token"));

    assert!(
        !vc.tts_enabled,
        "the endpoint answers no /v1/audio/speech route, so a tts_model here must not enable synthesis"
    );
    assert_eq!(vc.tts_base_url, None);
}

#[test]
fn provider_audio_models_win_over_a_user_fallback_chain() {
    // The active provider's own route is the primary one even when the
    // user set a chain: `generation_model` has the same precedence over
    // the global `image.generation.model`. The chain itself survives, so
    // the dispatcher can still fall back to it.
    let vc = cfg(r#"
[providers.custom.lm-studio]
enabled = true
base_url = "http://localhost:1234/v1"
api_key = "local-key"
default_model = "some-chat-model"
stt_model = "whisper-large-v3-turbo"
tts_model = "tts-1"
[providers.stt]
fallback_chain = ["groq"]
[providers.tts]
fallback_chain = ["openai"]
"#)
    .voice_config();
    assert_eq!(vc.stt_base_url.as_deref(), Some("http://localhost:1234"));
    assert_eq!(vc.tts_base_url.as_deref(), Some("http://localhost:1234"));
    assert_eq!(vc.stt_fallback_chain, vec!["groq"]);
    assert_eq!(vc.tts_fallback_chain, vec!["openai"]);
}

#[test]
fn keyless_provider_audio_models_do_not_enable_voice() {
    // Same #1792 gate as the vision candidates: a remote endpoint we hold
    // no key for is a guaranteed 401, not a working out-of-the-box path.
    let vc = cfg(r#"
[providers.custom.remote]
enabled = true
base_url = "https://api.example.com/v1"
default_model = "some-chat-model"
stt_model = "whisper-large-v3-turbo"
tts_model = "tts-1"
"#)
    .voice_config();
    assert!(
        !vc.stt_enabled,
        "keyless remote must not enable transcription"
    );
    assert!(!vc.tts_enabled, "keyless remote must not enable synthesis");
    assert_eq!(vc.stt_base_url, None);
    assert_eq!(vc.tts_base_url, None);
}

/// #2185: an empty `stt_model` on the active provider is an ABSENT model, not
/// a declared one. It used to win the precedence chain (`Some("")` is `Some`),
/// route audio to the active provider with an empty model name, and leave the
/// enabled `[providers.stt.openai_compatible]` block unread. This is the exact
/// shape from the report: a local parakeet shim plus a cleared field.
#[test]
fn an_empty_stt_model_falls_through_to_the_openai_compatible_block() {
    let vc = cfg(r#"
[providers.stt.openai_compatible]
enabled = true
base_url = "http://127.0.0.1:2246"
model = "parakeet"
api_key = "local-shim-key"
[providers.custom.local]
enabled = true
base_url = "http://127.0.0.1:2246/v1"
api_key = "local-shim-key"
default_model = "some-chat-model"
stt_model = ""
"#)
    .voice_config();
    assert_eq!(
        vc.stt_base_url.as_deref(),
        Some("http://127.0.0.1:2246"),
        "the empty provider model must not claim the audio route"
    );
    assert_eq!(
        vc.stt_model.as_deref(),
        Some("parakeet"),
        "the configured provider's model must win when the field is blank"
    );
    assert_eq!(vc.stt_api_key.as_deref(), Some("local-shim-key"));
    assert!(vc.stt_enabled);
}

/// The same rule for a whitespace-only value: a field holding spaces is a
/// cleared field, not a model named " ".
#[test]
fn a_whitespace_only_stt_model_is_treated_as_absent() {
    let vc = cfg(r#"
[providers.stt.openai_compatible]
enabled = true
base_url = "http://127.0.0.1:2246"
model = "parakeet"
api_key = "local-shim-key"
[providers.custom.local]
enabled = true
base_url = "http://127.0.0.1:2246/v1"
api_key = "local-shim-key"
default_model = "some-chat-model"
stt_model = "   "
"#)
    .voice_config();
    assert_eq!(vc.stt_base_url.as_deref(), Some("http://127.0.0.1:2246"));
    assert_eq!(vc.stt_model.as_deref(), Some("parakeet"));
}

/// With nothing behind it, a cleared model must not invent a working route:
/// no `[providers.stt.*]` block means transcription stays off rather than
/// posting an empty model at the active provider.
#[test]
fn an_empty_stt_model_with_no_stt_block_does_not_enable_transcription() {
    let vc = cfg(r#"
[providers.custom.local]
enabled = true
base_url = "http://127.0.0.1:1234/v1"
api_key = "local-key"
default_model = "some-chat-model"
stt_model = ""
"#)
    .voice_config();
    assert!(
        !vc.stt_enabled,
        "a cleared stt_model must not enable transcription on its own"
    );
    assert_eq!(vc.stt_base_url, None);
}

/// The synthesis twin rides the same shared candidate builder: a cleared
/// `tts_model` falls through instead of claiming the speech route.
#[test]
fn an_empty_tts_model_does_not_claim_the_speech_route() {
    let vc = cfg(r#"
[providers.tts.openai_compatible]
enabled = true
base_url = "http://127.0.0.1:8880"
model = "kokoro"
api_key = "local-tts-key"
[providers.custom.local]
enabled = true
base_url = "http://127.0.0.1:1234/v1"
api_key = "local-key"
default_model = "some-chat-model"
tts_model = ""
"#)
    .voice_config();
    assert_eq!(
        vc.tts_base_url.as_deref(),
        Some("http://127.0.0.1:8880"),
        "the empty provider model must not claim the speech route"
    );
    assert_eq!(vc.tts_model, "kokoro");
}
