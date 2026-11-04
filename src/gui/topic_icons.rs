//! The topic icons and the composer's crab set, embedded at compile time
//! (#2478).
//!
//! Two sets, two jobs. The owner keeps them apart on purpose:
//!
//! - [`TOPIC_ICONS`] are the illustrated crab badges under
//!   `src/assets/emojis/topic-icons/`. They belong to topic creation: a topic
//!   tab draws one before its name, the way Telegram's topic bar does.
//! - [`CRAB_ICONS`] are the reaction-style crabs under
//!   `src/assets/emojis/`. They are the input's emoji set, and they are
//!   embedded here so nothing has to load a file at run time.
//!
//! Every byte is `include_bytes!`, so the binary carries its icons and never
//! depends on the checkout it was built from, the same contract as the brand
//! mark in `icon.rs`.

use gpui::{
    AnyElement, Image, ImageFormat, IntoElement, ObjectFit, Styled, StyledImage, img, rems,
};
use std::sync::Arc;

/// The icon a topic tab shows, as the picker offers it (#2414, #2478).
///
/// The stored value is [`TOPIC_PREFIX`] plus the name here, so a row that
/// already holds a plain emoji still reads as text and nothing in an existing
/// database has to be rewritten.
pub const TOPIC_ICONS: &[(&str, &[u8])] = &[
    (
        "general",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-general.png"
        )),
    ),
    (
        "development",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-development.png"
        )),
    ),
    (
        "ideas",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-ideas.png"
        )),
    ),
    (
        "bug-reports",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-bug-reports.png"
        )),
    ),
    (
        "releases",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-releases.png"
        )),
    ),
    (
        "showcase",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-showcase.png"
        )),
    ),
    (
        "ops",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-ops.png"
        )),
    ),
    (
        "off-topic",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-off-topic.png"
        )),
    ),
];

/// The composer's crab set: one picture per shortcode, the way the terminal
/// prints `:ok:` as a glyph.
///
/// The input itself writes plain characters, so the set is embedded for the
/// surfaces that draw a picture: a tab resolves either set by name, and the
/// GUI reactions (owner-deferred, a later feature) read this table directly.
pub const CRAB_ICONS: &[(&str, &[u8])] = &[
    (
        "100",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_100.png"
        )),
    ),
    (
        "bolt",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_bolt.png"
        )),
    ),
    (
        "bug",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_bug.png"
        )),
    ),
    (
        "clap",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_clap.png"
        )),
    ),
    (
        "coffee",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_coffee.png"
        )),
    ),
    (
        "cry",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_cry.png"
        )),
    ),
    (
        "dead",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_dead.png"
        )),
    ),
    (
        "eyes",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_eyes.png"
        )),
    ),
    (
        "fire",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_fire.png"
        )),
    ),
    (
        "gg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_gg.png"
        )),
    ),
    (
        "laugh",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_laugh.png"
        )),
    ),
    (
        "loading",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_loading.png"
        )),
    ),
    (
        "merge",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_merge.png"
        )),
    ),
    (
        "mindblow",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_mindblow.png"
        )),
    ),
    (
        "no",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_no.png"
        )),
    ),
    (
        "ok",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_ok.png"
        )),
    ),
    (
        "party",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_party.png"
        )),
    ),
    (
        "please",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_please.png"
        )),
    ),
    (
        "rocket",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_rocket.png"
        )),
    ),
    (
        "salute",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_salute.png"
        )),
    ),
    (
        "shrug",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_shrug.png"
        )),
    ),
    (
        "smug",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_smug.png"
        )),
    ),
    (
        "sweat",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_sweat.png"
        )),
    ),
    (
        "think",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_think.png"
        )),
    ),
    (
        "trophy",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_trophy.png"
        )),
    ),
    (
        "yes",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/crab_yes.png"
        )),
    ),
];

/// The prefix the topic picker writes in front of an icon's name (#2478).
///
/// It keeps a stored picture apart from a stored emoji: a row written before
/// the picture set existed holds a glyph, and one written now holds
/// `topic:development`. Both render, and neither is guessed at.
pub const TOPIC_PREFIX: &str = "topic:";

/// The stored value for a topic icon name, e.g. `topic:development`.
pub fn store_name(name: &str) -> String {
    format!("{TOPIC_PREFIX}{name}")
}

/// The bytes for a topic icon, by name, with or without [`TOPIC_PREFIX`].
pub fn topic_icon(name: &str) -> Option<&'static [u8]> {
    let name = name.strip_prefix(TOPIC_PREFIX).unwrap_or(name);
    TOPIC_ICONS
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, bytes)| *bytes)
}

/// The bytes for a crab, by shortcode.
pub fn crab_icon(name: &str) -> Option<&'static [u8]> {
    CRAB_ICONS
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, bytes)| *bytes)
}

/// A stored topic icon resolved to a picture, or `None` when it is a plain
/// emoji and has to stay text (#2478).
///
/// Both embedded sets are consulted: a tab draws whatever it holds, so moving
/// a row from the crab set to the topic set, or back, needs no migration.
pub fn tab_icon(stored: &str) -> Option<AnyElement> {
    let bytes = topic_icon(stored).or_else(|| crab_icon(stored))?;
    Some(picture(bytes, 1.15).into_any_element())
}

/// A picture for a tab, a picker cell or the badge in the topic prompt.
/// Contained, never stretched, with the same rounding the banner logo uses.
pub fn picture(bytes: &'static [u8], size: f32) -> impl IntoElement {
    img(Arc::new(Image::from_bytes(
        ImageFormat::Png,
        bytes.to_vec(),
    )))
    .w(rems(size))
    .h(rems(size))
    .flex_none()
    .rounded(gpui::px(size * 2.0))
    .object_fit(ObjectFit::Contain)
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG always starts with these four bytes, so an embedded file that is
    /// not one (a stray `.DS_Store`, a renamed JPG) fails here instead of
    /// drawing a blank box in the bar.
    fn is_png(bytes: &[u8]) -> bool {
        bytes.starts_with(&[0x89, b'P', b'N', b'G'])
    }

    #[test]
    fn every_topic_icon_resolves_by_name_and_by_stored_value() {
        for (name, bytes) in TOPIC_ICONS {
            assert!(is_png(bytes), "{name} is not a PNG");
            assert_eq!(topic_icon(name), Some(*bytes), "{name} by name");
            let stored = store_name(name);
            assert_eq!(topic_icon(&stored), Some(*bytes), "{stored} as stored");
        }
    }

    #[test]
    fn the_picker_offers_the_eight_topic_badges() {
        let mut names: Vec<&str> = TOPIC_ICONS.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "bug-reports",
                "development",
                "general",
                "ideas",
                "off-topic",
                "ops",
                "releases",
                "showcase"
            ]
        );
    }

    #[test]
    fn the_crab_set_matches_its_shortcodes() {
        assert_eq!(CRAB_ICONS.len(), 26);
        for (name, bytes) in CRAB_ICONS {
            assert!(is_png(bytes), "{name} is not a PNG");
            assert_eq!(crab_icon(name), Some(*bytes), "{name} by shortcode");
        }
        assert_eq!(crab_icon("nope"), None);
    }

    /// A row written before the pictures existed holds an emoji, and it has to
    /// keep drawing as text rather than resolve to a wrong icon (#2478).
    #[test]
    fn a_stored_emoji_is_not_a_picture() {
        assert_eq!(topic_icon("💬"), None);
        assert_eq!(crab_icon("💬"), None);
    }

    #[test]
    fn the_prefix_is_stripped_so_both_forms_find_the_same_picture() {
        let stored = store_name("ops");
        assert_eq!(stored, "topic:ops");
        assert_eq!(topic_icon(&stored), topic_icon("ops"));
        assert_eq!(topic_icon("topic:nope"), None);
    }
}
