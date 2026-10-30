use crate::config::Config;

/// The `/models` picker reads its catalogue from the embedded example, so the
/// built-in provider's model has to be listed there and default to disabled.
#[test]
fn example_config_lists_the_built_in_model_disabled_by_default() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert!(!section.enabled, "built-in provider ships disabled");
    assert_eq!(
        section.default_model.as_deref(),
        Some("deepseek-v4.1-flash")
    );
    assert_eq!(section.models, vec!["deepseek-v4.1-flash".to_string()]);
    // The built-in endpoint is free and fixed at 200k, so the key is not
    // offered in the example and is not honoured if a user adds it (#2066).
    assert_eq!(section.context_window, None);
}

/// Image generation ships on day 0 through the built-in provider, on its own
/// model id (served only on the image route, never as chat).
#[test]
fn example_config_sets_the_built_in_image_model() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert_eq!(
        section.generation_model.as_deref(),
        Some("gemini-2.5-flash-image")
    );
    assert!(
        !section.models.iter().any(|m| m == "gemini-2.5-flash-image"),
        "image model must not appear in the chat model list"
    );
}

/// Transcription ships on day 0 through the built-in provider, but on the chat
/// route rather than `/v1/audio/transcriptions`: the endpoint takes the audio as
/// an `input_audio` part on `v1/chat/completions` and answers every
/// `/v1/audio/*` path with a 404 (#2107).
///
/// The id must be the alias the proxy publishes (`opencrabs-proxy/src/alias.rs`),
/// never a publisher-prefixed routing id: those stay in the proxy's rotation
/// and the client never names them.
#[test]
fn example_config_sets_the_built_in_transcription_model() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert_eq!(
        section.stt_model.as_deref(),
        Some("qwen3.8-omni-flash"),
        "the shipped example must carry the published audio alias"
    );
    assert!(
        !section
            .stt_model
            .as_deref()
            .is_some_and(|m| m.contains('/')),
        "a publisher-prefixed routing id must never reach a client config"
    );
}

/// Speech is NOT served by the built-in provider: it answers no
/// `/v1/audio/speech` route in any shape, so the shipped example must leave
/// `tts_model` unset. A set field would make a dead route the primary
/// synthesis candidate ahead of `[providers.tts.*]`, so a working engine waits
/// behind a failed round trip on every spoken reply (#2105, #2107).
#[test]
fn example_config_leaves_the_built_in_speech_model_unset() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert!(
        section.tts_model.is_none(),
        "no synthesis route is served here, got: {:?}",
        section.tts_model
    );
}
