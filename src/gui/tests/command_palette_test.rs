//! The command palette's frost stand-in, read as source text.
//!
//! One test file per module it exercises, beside the module rather than at
//! the bottom of it (the tests/mod.rs convention).

/// The smoke is the frost's stand-in: gpui 0.2.2 cannot blur one element,
/// so the card wears a haze layer under its ink and rides its alpha high
/// to dim the sharp lines behind it (#3130's audit).
#[test]
fn the_card_wears_smoke_where_blur_cannot_go() {
    let src = include_str!("../command_palette.rs");
    assert!(
        src.contains("SMOKE_COOL") && src.contains("SMOKE_DEEP"),
        "the haze is its own pair of tones, not a stolen rim colour"
    );
    assert!(
        src.contains(".absolute()\n            .inset_0()\n            .rounded(px(SEEKER_RADIUS))\n            .bg(linear_gradient(\n                160.0,"),
        "the smoke is a full-card layer at 160 degrees, not a fill the ink paints over"
    );
    let glass = src.find("const GLASS_BOTTOM").expect("the floor tone exists");
    let line = src[glass..].lines().next().expect("one line");
    let alpha = line
        .trim_start_matches("const GLASS_BOTTOM: u32 = 0x08080b")
        .trim_end_matches(';');
    assert!(
        u8::from_str_radix(alpha, 16).unwrap() > 0xd8,
        "the floor rides its alpha high so the terminal behind dims into mush"
    );
}
