//! The platform mark a channel-backed row leads with (#2766).
//!
//! A channel card used to print the word `Telegram:` in front of the chat name
//! and again on every topic chip under it, so one card said the same word as
//! many times as it had topics. The mark says it once, in the slot the eye
//! already reads first.
//!
//! The vectors are embedded with `include_bytes!`, the same contract as the
//! brand mark in `icon.rs`: the binary carries its icons and never reads the
//! checkout it was built from.

use gpui::{
    AnyElement, Image, ImageFormat, IntoElement, ObjectFit, Styled, StyledImage, img, rems,
};
use std::sync::Arc;

/// The literal every mark carries where the brand colour goes. One asset can
/// then be drawn in any colour without a second file.
pub const PLACEHOLDER: &str = "#8a8a8a";

/// Standalone channel marks keep their brand colours. A badge overlaid on a
/// stored picture may use the theme accent so the two images read as one.
pub const TELEGRAM_BLUE: u32 = 0x2aabee;
pub const SLACK_PURPLE: u32 = 0x4a154b;
pub const DISCORD_BLURPLE: u32 = 0x5865f2;

const TELEGRAM_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/assets/telegram.svg"
));
const SLACK_SVG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/slack.svg"));
const DISCORD_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/assets/discord.svg"
));

/// The embedded vector for a platform, when we carry one.
fn platform_svg(platform: &str) -> Option<&'static [u8]> {
    match platform {
        "Telegram" => Some(TELEGRAM_SVG),
        "Slack" => Some(SLACK_SVG),
        "Discord" => Some(DISCORD_SVG),
        _ => None,
    }
}

/// The brand colour for a platform mark.
pub fn platform_colour(platform: &str) -> Option<u32> {
    match platform {
        "Telegram" => Some(TELEGRAM_BLUE),
        "Slack" => Some(SLACK_PURPLE),
        "Discord" => Some(DISCORD_BLURPLE),
        _ => None,
    }
}

/// A platform's standalone mark, recoloured to its brand colour.
pub fn platform_icon(platform: &str, size: f32) -> Option<AnyElement> {
    platform_icon_with_colour(platform, size, platform_colour(platform)?)
}

/// A platform mark in an explicit colour, drawn at `size` rems. The rail uses
/// this for the small accent badge over a stored session picture (#2800).
///
/// `None` when no mark is embedded for that platform: the caller then paints
/// the name alone rather than inventing a picture for it.
pub fn platform_icon_with_colour(platform: &str, size: f32, colour: u32) -> Option<AnyElement> {
    let svg = platform_svg(platform)?;
    // GPUI rasterizes a vector from its intrinsic size, so the source carries
    // 96px the way `topic-orientation.svg` does and stays crisp at 16px.
    let recoloured = String::from_utf8_lossy(svg)
        .replace(PLACEHOLDER, &format!("#{colour:06x}"))
        .into_bytes();
    Some(
        img(Arc::new(Image::from_bytes(ImageFormat::Svg, recoloured)))
            .w(rems(size))
            .h(rems(size))
            .flex_none()
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        DISCORD_BLURPLE, DISCORD_SVG, PLACEHOLDER, SLACK_PURPLE, SLACK_SVG, TELEGRAM_BLUE,
        TELEGRAM_SVG, platform_colour, platform_svg,
    };

    /// An embedded file that is not a vector would draw a blank box in the
    /// slot the name used to occupy, so the shape is checked here.
    #[test]
    fn the_telegram_mark_is_an_embedded_vector() {
        let svg = std::str::from_utf8(TELEGRAM_SVG).expect("telegram SVG is UTF-8");
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("width=\"96\" height=\"96\""));
        assert!(svg.contains("viewBox=\"0 0 24 24\""));
        assert!(svg.contains("<path"));
        // The recolour is a string replacement: without the placeholder the
        // mark would paint in whatever literal the asset shipped with.
        assert!(svg.contains(PLACEHOLDER));
        assert_eq!(platform_colour("Telegram"), Some(TELEGRAM_BLUE));
        assert!(platform_svg("Telegram").is_some());
    }

    #[test]
    fn slack_and_discord_cards_have_embedded_brand_marks() {
        for (platform, bytes, colour) in [
            ("Slack", SLACK_SVG, SLACK_PURPLE),
            ("Discord", DISCORD_SVG, DISCORD_BLURPLE),
        ] {
            let svg = std::str::from_utf8(bytes).expect("platform SVG is UTF-8");
            assert!(svg.starts_with("<svg"));
            assert!(svg.contains("width=\"96\" height=\"96\""));
            assert!(svg.contains("viewBox=\"0 0 24 24\""));
            assert!(svg.contains("<path"));
            assert!(svg.contains(PLACEHOLDER));
            assert_eq!(platform_colour(platform), Some(colour));
            assert!(platform_svg(platform).is_some());
        }
    }

    /// No mark, no picture: a platform we carry no vector for falls back to its
    /// name instead of borrowing another brand's icon.
    #[test]
    fn a_platform_without_a_mark_gets_none() {
        assert_eq!(platform_colour("Trello"), None);
        assert!(platform_svg("Trello").is_none());
    }
}
