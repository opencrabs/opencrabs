//! Tests for the setup-gap check that drives the quick-start welcome.
//!
//! The check is pure: USER.md text and the three capability flags go in,
//! the list of gaps comes out. Nothing here reads the live home.

use crate::config::{Config, ProviderConfig, ProviderConfigs};
use crate::tui::onboarding::SetupGap;
use crate::tui::onboarding::setup_gaps::{
    collect_gaps, image_generation_ready, profile_gaps, render_gaps,
};

const TEMPLATE_USER_MD: &str = include_str!("../docs/reference/templates/USER.md");

const FILLED_USER_MD: &str = "\
# USER.md - About Your Human

| Field | Value |
|-------|-------|
| **Name** | Adolfo |
| **Location** | Coimbra, Portugal |
| **Timezone** | UTC+1 |
| **Availability** | 10AM - 4AM |

## Professional

| Field | Value |
|-------|-------|
| **Title** | Senior Engineer |
";

#[test]
fn untouched_template_leaves_every_profile_field_missing() {
    assert_eq!(
        profile_gaps(TEMPLATE_USER_MD),
        vec![
            SetupGap::Name,
            SetupGap::Location,
            SetupGap::Timezone,
            SetupGap::Title,
        ]
    );
}

#[test]
fn filled_profile_leaves_no_profile_gaps() {
    assert!(profile_gaps(FILLED_USER_MD).is_empty());
}

#[test]
fn only_the_unfilled_rows_are_reported() {
    let partial = "| **Name** | Adolfo |\n| **Location** | *(city, country)* |\n";
    assert_eq!(
        profile_gaps(partial),
        vec![SetupGap::Location, SetupGap::Timezone, SetupGap::Title]
    );
}

#[test]
fn a_missing_user_md_counts_as_an_empty_profile() {
    assert_eq!(profile_gaps("").len(), 4);
}

#[test]
fn a_blank_value_counts_as_missing() {
    let blank = "| **Name** |  |\n";
    assert!(profile_gaps(blank).contains(&SetupGap::Name));
}

#[test]
fn profile_gaps_come_first_then_the_capabilities() {
    assert_eq!(
        collect_gaps("", false, false, false),
        vec![
            SetupGap::Name,
            SetupGap::Location,
            SetupGap::Timezone,
            SetupGap::Title,
            SetupGap::SpeechToText,
            SetupGap::TextToSpeech,
            SetupGap::ImageGeneration,
        ]
    );
}

#[test]
fn configured_capabilities_are_not_asked_about() {
    assert_eq!(
        collect_gaps(FILLED_USER_MD, true, true, false),
        vec![SetupGap::ImageGeneration]
    );
}

#[test]
fn everything_set_gives_no_gaps() {
    assert!(collect_gaps(FILLED_USER_MD, true, true, true).is_empty());
}

#[test]
fn render_names_each_gap_and_handles_none() {
    assert_eq!(
        render_gaps(&[SetupGap::SpeechToText, SetupGap::TextToSpeech]),
        "speech to text, text to speech"
    );
    assert_eq!(render_gaps(&[]), "nothing, everything is set");
}

#[test]
fn gap_labels_have_no_em_dash() {
    for gap in collect_gaps("", false, false, false) {
        assert!(!gap.label().contains('\u{2014}'));
    }
}

// ── #2106: the image leg asks the tool's own resolver ────────────────

/// The shape onboarding actually saves for a built-in install: the provider
/// enabled, the install token stored, the image model pinned.
fn builtin_install(token: Option<&str>) -> Config {
    Config {
        providers: ProviderConfigs {
            opencrabs: Some(ProviderConfig {
                enabled: true,
                api_key: token.map(str::to_string),
                default_model: Some("deepseek-v4.1-flash".into()),
                generation_model: Some("gemini-2.5-flash-image".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn a_built_in_install_is_not_told_image_generation_is_missing() {
    let config = builtin_install(Some("ocp_test_token"));
    assert!(
        image_generation_ready(&config),
        "the built-in provider serves image generation on day 0"
    );
    let gaps = collect_gaps(FILLED_USER_MD, true, true, image_generation_ready(&config));
    assert!(
        !gaps.contains(&SetupGap::ImageGeneration),
        "a capability the install already has must not be listed as missing: {gaps:?}"
    );
}

#[test]
fn a_config_with_no_generation_route_still_reports_the_gap() {
    let config = Config::default();
    assert!(!image_generation_ready(&config));
    let gaps = collect_gaps(FILLED_USER_MD, true, true, image_generation_ready(&config));
    assert!(gaps.contains(&SetupGap::ImageGeneration));
}

#[test]
fn the_global_gemini_leg_alone_still_counts() {
    let mut config = Config::default();
    config.image.generation.enabled = true;
    config.image.generation.api_key = Some("test-gemini-key".into());
    assert!(
        image_generation_ready(&config),
        "an install configured the old way keeps the same answer"
    );
}

#[test]
fn a_built_in_section_without_a_token_is_not_ready() {
    let config = builtin_install(None);
    assert!(
        !image_generation_ready(&config),
        "a keyless section is not a route; the gap is honest"
    );
}
