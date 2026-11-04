//! The shared card shape: one radius token and the glass fill (#2497).
//!
//! Every chat card surface reads [`CARD_RADIUS`] and paints [`glass_fill`],
//! so the chat reads as one family the way the Seeker card already does
//! (#2505). The glass keeps the card's own panel tone: the hue comes from
//! the theme, the gradient applies transparency only, so light and dark
//! keep their exact colors (owner, 2026-11-03: "no color change on chat,
//! just simply bare styling"). Chips, buttons and the Seeker pill are not
//! cards and keep their small radii.

use gpui::{Background, Hsla, linear_color_stop, linear_gradient, rgba};

/// The one radius, in px, every chat card surface uses (#2497).
/// The Seeker has its own rounder token, so changing its Spotlight shape
/// cannot resize every chat card with it (#2532).
pub const CARD_RADIUS: f32 = 24.0;

/// The Seeker results card, in px. Its rows derive their nesting curve from
/// this token rather than the chat card radius (#2532).
pub const SEEKER_RADIUS: f32 = 32.0;

/// The glass's top alpha, 0xf2 (about 0.95): nearly solid where the
/// content starts.
pub const GLASS_ALPHA_TOP: u32 = 0xf2;

/// The glass's bottom alpha, 0xcc (about 0.80): the transparency that
/// makes the surface read as glass.
pub const GLASS_ALPHA_BOTTOM: u32 = 0xcc;

/// The two glass stops for a card whose panel tone is `base` (0xRRGGBB),
/// top then bottom, as plain colors.
///
/// This is the testable unit: `Background` keeps its fields private, so
/// the tests pin these stops and [`glass_fill`] only assembles them.
pub fn glass_stops(base: u32) -> [Hsla; 2] {
    [
        Hsla::from(rgba((base << 8) | GLASS_ALPHA_TOP)),
        Hsla::from(rgba((base << 8) | GLASS_ALPHA_BOTTOM)),
    ]
}

/// The glass fill for a card whose panel tone is `base` (0xRRGGBB).
///
/// Two stops, 180 degrees: the card's own tone at [`GLASS_ALPHA_TOP`] on
/// top and [`GLASS_ALPHA_BOTTOM`] at the bottom. The gradient shape is the
/// Seeker's recipe (src/gui/command_palette.rs:144), pointed at the
/// theme's tone instead of a fixed dark hex, so no theme changes color.
pub fn glass_fill(base: u32) -> Background {
    let [top, bottom] = glass_stops(base);
    linear_gradient(
        180.0,
        linear_color_stop(top, 0.0),
        linear_color_stop(bottom, 1.0),
    )
}

/// The concentric rule (#2513): a card nested `edge_inset` px inside a
/// parent card carries `CARD_RADIUS - edge_inset`, so the curves nest
/// instead of poking out. Derived here, never hardcoded per site.
pub fn concentric_radius(edge_inset: f32) -> f32 {
    CARD_RADIUS - edge_inset
}

/// The same nesting rule inside the Seeker's dedicated results card (#2532).
pub fn seeker_concentric_radius(edge_inset: f32) -> f32 {
    SEEKER_RADIUS - edge_inset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeker_radius_is_dedicated_to_the_results_card() {
        assert_eq!(CARD_RADIUS, 24.0);
        assert_eq!(SEEKER_RADIUS, 32.0);
        const { assert!(SEEKER_RADIUS > CARD_RADIUS) };
    }

    #[test]
    fn the_glass_stops_carry_the_given_tone() {
        let [top, bottom] = glass_stops(0x1a1a24);
        let want_top: Hsla = rgba((0x1a1a24 << 8) | GLASS_ALPHA_TOP).into();
        let want_bottom: Hsla = rgba((0x1a1a24 << 8) | GLASS_ALPHA_BOTTOM).into();
        assert_eq!(top, want_top);
        assert_eq!(bottom, want_bottom);
    }

    #[test]
    fn the_glass_keeps_the_hue_in_both_themes() {
        // A dark panel tone and a light panel tone: the gradient must
        // carry the tone it is given, changing alpha only.
        for base in [0x1a1a24, 0xf4f2ee] {
            let [top, bottom] = glass_stops(base);
            assert_eq!(
                top,
                Hsla::from(rgba((base << 8) | GLASS_ALPHA_TOP)),
                "top stop of {base:#x}"
            );
            assert_eq!(
                bottom,
                Hsla::from(rgba((base << 8) | GLASS_ALPHA_BOTTOM)),
                "bottom stop of {base:#x}"
            );
        }
    }

    #[test]
    fn the_fill_fades_downward() {
        // Top stop more opaque than bottom: the transparency is at the
        // bottom edge, matching the Seeker's fade.
        let [top, bottom] = glass_stops(0x1a1a24);
        assert!(top.a > bottom.a, "top.a={} bottom.a={}", top.a, bottom.a);
        // glass_fill must assemble from exactly these stops: compiles
        // and runs against the pinned values.
        let _ = glass_fill(0x1a1a24);
    }

    #[test]
    fn concentric_radius_subtracts_the_edge_inset() {
        // Chat cards keep their established 24px curve (#2497).
        assert_eq!(concentric_radius(12.0), 12.0);
        assert_eq!(concentric_radius(4.0), 20.0);
        assert_eq!(concentric_radius(0.0), CARD_RADIUS);
    }

    #[test]
    fn seeker_rows_nest_inside_the_dedicated_radius() {
        assert_eq!(seeker_concentric_radius(12.0), SEEKER_RADIUS - 12.0);
        assert_eq!(seeker_concentric_radius(4.0), SEEKER_RADIUS - 4.0);
        assert_eq!(seeker_concentric_radius(0.0), SEEKER_RADIUS);
    }
}
