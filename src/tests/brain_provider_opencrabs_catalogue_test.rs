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
        Some("cb/deepseek-v4.1-flash")
    );
    assert_eq!(section.models, vec!["cb/deepseek-v4.1-flash".to_string()]);
    // The built-in endpoint is free and fixed at 200k, so the key is not
    // offered in the example and is not honoured if a user adds it (#2066).
    assert_eq!(section.context_window, None);
}

/// Image generation ships on day 0 through the built-in provider, on its own
/// model id (served only on the proxy's image routes, never as chat).
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
        Some("cb/gemini-2.5-flash-image")
    );
    assert!(
        !section
            .models
            .iter()
            .any(|m| m == "cb/gemini-2.5-flash-image"),
        "image model must not appear in the chat model list"
    );
}

/// Voice is deliberately NOT preset on the built-in provider. The endpoint
/// serves no `/v1/audio/transcriptions` or `/v1/audio/speech` route, so a
/// shipped default id could only produce a refusal on every voice note. Voice
/// stays unconfigured until the user sets `stt_model` / `tts_model` on a
/// provider whose endpoint actually serves audio.
#[test]
fn example_config_sets_no_built_in_voice_models() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert_eq!(
        section.stt_model, None,
        "no STT default: the built-in endpoint has no audio route to serve it"
    );
    assert_eq!(
        section.tts_model, None,
        "no TTS default: the built-in endpoint has no audio route to serve it"
    );
}
