//! Shared attachment chrome. Every surface reads the active GUI palette so
//! dark, light, and custom themes keep the same geometry without frozen colors.

use gpui::{Background, Hsla, linear_color_stop, linear_gradient, rgb};

use crate::palette::Palette;

/// Attachment cards and their nested media use one restrained corner shape.
pub const TILE_RADIUS: f32 = 12.0;
pub const MEDIA_RADIUS: f32 = 8.0;
pub const PREVIEW_RADIUS: f32 = 16.0;

/// The outer ring follows the theme's existing separator role.
pub fn ring(palette: Palette) -> Hsla {
    rgb(palette.track).into()
}

/// A small vertical highlight gives flat attachment surfaces some depth.
pub fn surface(palette: Palette) -> Background {
    linear_gradient(
        180.0,
        linear_color_stop(rgb(palette.panel_hi), 0.0),
        linear_color_stop(rgb(palette.panel), 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette;

    #[test]
    fn attachment_style_colors_follow_the_supplied_theme() {
        let custom = Palette {
            panel: 0x112233,
            panel_hi: 0x445566,
            track: 0x778899,
            ..palette::DARK
        };
        let expected_ring: Hsla = rgb(0x778899).into();
        assert_eq!(ring(custom), expected_ring);
        assert_eq!(
            surface(custom),
            linear_gradient(
                180.0,
                linear_color_stop(rgb(0x445566), 0.0),
                linear_color_stop(rgb(0x112233), 1.0),
            )
        );
        assert_ne!(surface(custom), surface(palette::DARK));
    }

    #[test]
    fn attachment_style_transcript_integrates_the_caption_without_growing_the_plan() {
        let src = include_str!("transcript.rs");
        let cell = src
            .split("fn attachment_cell(")
            .nth(1)
            .expect("attachment cell")
            .split("/// The last cell")
            .next()
            .expect("attachment cell body");
        assert!(cell.contains("attachment_style::TILE_RADIUS"));
        assert!(cell.contains("attachment_style::ring(theme)"));
        assert!(cell.contains("attachment_style::surface(theme)"));
        assert!(cell.contains("attachment_view::CAPTION"));
        assert!(cell.contains(".shadow_sm()"));

        let fold = src
            .split("fn fold_cell(")
            .nth(1)
            .expect("fold cell")
            .split("/// A thinking block")
            .next()
            .expect("fold cell body");
        assert!(fold.contains("attachment_style::TILE_RADIUS"));
        assert!(fold.contains("attachment_style::surface(theme)"));
    }

    #[test]
    fn attachment_style_composer_and_preview_share_the_same_chrome() {
        let editor = include_str!("editor.rs");
        let strip = editor
            .split("fn attachment_strip(")
            .nth(1)
            .expect("composer attachment strip")
            .split("/// The paperclip")
            .next()
            .expect("composer attachment strip body");
        assert!(strip.contains("attachment_style::TILE_RADIUS"));
        assert!(strip.contains("attachment_style::MEDIA_RADIUS"));
        assert!(strip.contains("attachment_style::ring(theme)"));
        assert!(strip.contains("attachment_style::surface(theme)"));
        assert!(strip.contains(".shadow_sm()"));
        assert!(strip.matches("px(34.0)").count() >= 2);

        let preview = include_str!("preview.rs");
        assert!(preview.matches("attachment_style::MEDIA_RADIUS").count() >= 2);
        let card = preview
            .split(".id(\"preview-card\")")
            .nth(1)
            .expect("preview card")
            .split(".child(body)")
            .next()
            .expect("preview card body");
        assert!(card.contains("attachment_style::PREVIEW_RADIUS"));
        assert!(card.contains("attachment_style::ring(theme)"));
        assert!(card.contains("attachment_style::surface(theme)"));
        assert!(card.contains(".shadow_md()"));
    }
}
