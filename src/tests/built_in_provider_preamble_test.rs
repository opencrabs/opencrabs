//! The system preamble tells every install what the built-in provider serves.
//!
//! Two places describe the same capability: `BRAIN_PREAMBLE` (what the agent is
//! told on every install, every surface) and `config.toml.example` (what a
//! fresh install is actually configured with). They drifted once already: the
//! README and the old workspace note said the built-in provider serves no audio
//! while the shipped config had `stt_model` set and transcription worked out of
//! the box. The preamble carried the same gap.
//!
//! These tests bind the preamble to the shipped example, so changing one
//! without the other fails here instead of surfacing as a wrong answer to a
//! user asking what they get for free.

use crate::brain::prompt_builder::BRAIN_PREAMBLE;
use crate::config::Config;

fn shipped_built_in_section() -> crate::config::ProviderConfig {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example")
}

#[test]
fn the_preamble_states_the_built_in_provider_needs_no_account_or_key() {
    assert!(
        BRAIN_PREAMBLE.contains("BUILT-IN PROVIDER"),
        "the preamble must carry a built-in provider block"
    );
    assert!(
        BRAIN_PREAMBLE.contains("no account to create and no key to paste"),
        "the block must say a fresh install needs no account and no key"
    );
    assert!(
        BRAIN_PREAMBLE.contains("only provider that is free AND needs no setup"),
        "the block must state that this is the only free out-of-the-box route"
    );
}

/// Every model id the shipped config serves on day 0 has to be named in the
/// preamble. A user asking "do I have vision / images / transcription?" is
/// answered from the preamble, so a model swapped in the example without the
/// preamble (or the reverse) is a stale answer.
#[test]
fn the_preamble_names_every_day_zero_model_the_example_ships() {
    let section = shipped_built_in_section();
    for field in [
        section.default_model.as_deref(),
        section.vision_model.as_deref(),
        section.generation_model.as_deref(),
        section.stt_model.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        assert!(
            BRAIN_PREAMBLE.contains(field),
            "the preamble must name {field}, which the shipped config serves on day 0"
        );
    }
}

/// Speech is the one capability the built-in provider does not serve, so the
/// example leaves `tts_model` unset and the preamble has to say so. If the
/// endpoint ever starts serving `/v1/audio/speech`, both sides change together.
#[test]
fn the_preamble_says_speech_is_not_served_while_the_example_leaves_it_unset() {
    let section = shipped_built_in_section();
    assert!(
        section.tts_model.is_none(),
        "the built-in endpoint answers no /v1/audio/speech route, so the example must leave tts_model unset"
    );
    assert!(
        BRAIN_PREAMBLE.contains("Speech OUTPUT is not served yet"),
        "the preamble must say speech is not served, matching the unset tts_model"
    );
}

/// Transcription travels on the chat route for this endpoint, not on
/// `/v1/audio/transcriptions`. The preamble states the wire because a wrong
/// assumption here turns into a broken request.
#[test]
fn the_preamble_describes_the_chat_route_transcription_wire() {
    assert!(
        BRAIN_PREAMBLE.contains("input_audio"),
        "the preamble must name the input_audio part used for transcription"
    );
    assert!(
        BRAIN_PREAMBLE.contains("/v1/audio/*"),
        "the preamble must say the endpoint answers no /v1/audio/* route"
    );
}
