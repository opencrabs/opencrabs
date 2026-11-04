//! What the first chat can still ask the user about.
//!
//! The quick-start welcome names what is already configured (the LLM and its
//! model) and asks only about what is missing: the profile rows in USER.md,
//! speech to text, text to speech and image generation. The check itself is
//! pure so it can be tested without touching the live home. Only
//! `read_setup_gaps` reads disk.

use std::io::ErrorKind;

use crate::config::Config;

/// One thing the first chat can still ask about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupGap {
    Name,
    Location,
    Timezone,
    Title,
    SpeechToText,
    TextToSpeech,
    ImageGeneration,
}

impl SetupGap {
    /// Plain-language label the welcome prompt shows the agent.
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name (USER.md)",
            Self::Location => "location (USER.md)",
            Self::Timezone => "timezone (USER.md)",
            Self::Title => "role (USER.md)",
            Self::SpeechToText => "speech to text",
            Self::TextToSpeech => "text to speech",
            Self::ImageGeneration => "image generation",
        }
    }
}

/// USER.md rows that must be filled, in the order they are asked about.
const PROFILE_FIELDS: [(&str, SetupGap); 4] = [
    ("Name", SetupGap::Name),
    ("Location", SetupGap::Location),
    ("Timezone", SetupGap::Timezone),
    ("Title", SetupGap::Title),
];

/// Value of the `| **Key** | value |` row in USER.md. `None` when no such row
/// exists.
fn row_value<'a>(user_md: &'a str, key: &str) -> Option<&'a str> {
    user_md.lines().find_map(|line| {
        let mut cells = line.trim().trim_matches('|').split('|').map(str::trim);
        let label = cells.next()?.trim_matches('*').trim();
        if !label.eq_ignore_ascii_case(key) {
            return None;
        }
        cells.next()
    })
}

/// A value is empty when it is blank or still the template's `*(...)*` hint.
fn is_placeholder(value: &str) -> bool {
    value.is_empty() || value.starts_with("*(")
}

/// Profile fields that USER.md does not fill in yet.
pub fn profile_gaps(user_md: &str) -> Vec<SetupGap> {
    PROFILE_FIELDS
        .iter()
        .filter(|(key, _)| row_value(user_md, key).is_none_or(is_placeholder))
        .map(|(_, gap)| *gap)
        .collect()
}

/// Every gap in the order the welcome asks about it: profile first, then the
/// optional capabilities that the advanced wizard would have configured.
pub fn collect_gaps(user_md: &str, stt: bool, tts: bool, image: bool) -> Vec<SetupGap> {
    let mut gaps = profile_gaps(user_md);
    if !stt {
        gaps.push(SetupGap::SpeechToText);
    }
    if !tts {
        gaps.push(SetupGap::TextToSpeech);
    }
    if !image {
        gaps.push(SetupGap::ImageGeneration);
    }
    gaps
}

/// Comma-separated list for the welcome prompt.
pub fn render_gaps(gaps: &[SetupGap]) -> String {
    if gaps.is_empty() {
        return "nothing, everything is set".to_string();
    }
    gaps.iter()
        .map(|gap| gap.label())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether image generation has a route this install can use.
///
/// Delegates to the tool's own resolver (#2106). The global
/// `[image.generation]` block is only ONE leg: a built-in install serves image
/// generation through the provider chain while that block ships disabled, so
/// the hand-rolled check told every fresh user a capability they already had
/// was missing. `plan_candidates` walks the provider chain and then the global
/// leg, so it is a strict superset of the old test and an install configured
/// the old way keeps the same answer.
pub fn image_generation_ready(config: &Config) -> bool {
    !crate::brain::tools::generate_image::GenerateImageTool::plan_candidates(config, None)
        .is_empty()
}

/// Read USER.md and the config from disk and report what is still missing.
///
/// A missing USER.md counts as an empty profile. A config that fails to load
/// counts voice and image as unset, which only means the welcome asks about
/// them again.
pub fn read_setup_gaps() -> Vec<SetupGap> {
    let path = crate::config::opencrabs_home().join("USER.md");
    let user_md = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            if e.kind() != ErrorKind::NotFound {
                tracing::warn!(error = %e, "setup gaps: could not read USER.md");
            }
            String::new()
        }
    };
    let (stt, tts, image) = match Config::load() {
        Ok(config) => {
            let voice = config.voice_config();
            (
                voice.stt_enabled,
                voice.tts_enabled,
                image_generation_ready(&config),
            )
        }
        Err(e) => {
            tracing::warn!(error = %e, "setup gaps: config did not load, treating voice and image as unset");
            (false, false, false)
        }
    };
    collect_gaps(&user_md, stt, tts, image)
}
