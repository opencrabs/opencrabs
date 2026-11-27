//! The command palette's frost stand-in, read as source text.
//!
//! One test file per module it exercises, beside the module rather than at
//! the bottom of it (the tests/mod.rs convention).

/// gpui 0.2.2 cannot blur one element, so the frost is near-opaque glass:
/// the ramp is carried by colour, never by a lowered alpha, and the smoke
/// layer is gone (2026-11-27: "actually we need less transparency").
#[test]
fn the_glass_is_near_opaque_and_the_smoke_is_gone() {
    let src = include_str!("../command_palette.rs");
    assert!(
        !src.contains("SMOKE_COOL") && !src.contains("SMOKE_DEEP"),
        "the smoke layer is deleted, the fill alone carries the frost"
    );
    assert!(
        !src.contains("| 0xb8"),
        "the idle card's depth is colour now, not a lowered alpha stop"
    );
    for (name, min) in [
        ("const GLASS_TOP: u32 = 0x14141a", 0xf2u8),
        ("const GLASS_BOTTOM: u32 = 0x0a0a0e", 0xf2),
        ("const PILL_FILL: u32 = 0x14141a", 0xf2),
        ("const PILL_CROWN: u32 = 0x06060a", 0xf2),
    ] {
        let at = src.find(name).expect("the tone exists");
        let line = src[at..].lines().next().expect("one line");
        let alpha = line
            .split("0x")
            .nth(1)
            .and_then(|tail| tail.get(6..8))
            .map(|pair| u8::from_str_radix(pair, 16).expect("hex"))
            .expect("an alpha digit pair");
        assert!(
            alpha >= min,
            "{name} rides its alpha near-opaque (>= {min:#04x}) so the bright terminal behind cannot bleed through"
        );
    }
}

/// The one continuous sheet: the pill hands its bottom edge off at the exact
/// tone the card's ceiling starts at, and both run darker downward, so the
/// seam reads as continuity rather than a step up (#3130).
#[test]
fn the_pill_floor_and_the_card_ceiling_are_one_tone() {
    let src = include_str!("../command_palette.rs");
    let floor = src
        .find("const PILL_FILL: u32 = 0x")
        .map(|at| &src[at..])
        .expect("the pill's floor tone exists");
    let ceiling = src
        .find("const GLASS_TOP: u32 = 0x")
        .map(|at| &src[at..])
        .expect("the card's ceiling tone exists");
    let tone_of = |line: &str| -> String {
        line.split("0x")
            .nth(1)
            .expect("hex")
            .get(..8)
            .expect("rgb+alpha")
            .get(..6)
            .expect("rgb")
            .to_string()
    };
    assert_eq!(
        tone_of(floor.trim_start_matches("const ")),
        tone_of(ceiling.trim_start_matches("const ")),
        "the pill's floor and the card's ceiling share one rgb, so the gradient continues across the seam"
    );
    let crown = src
        .find("const PILL_CROWN: u32 = 0x")
        .map(|at| &src[at..])
        .expect("the pill's crown tone exists");
    let luminance = |line: &str| -> u32 {
        let hex = line
            .split("0x")
            .nth(1)
            .expect("hex")
            .get(..6)
            .expect("rgb")
            .to_string();
        let r = u32::from_str_radix(&hex[0..2], 16).expect("hex");
        let g = u32::from_str_radix(&hex[2..4], 16).expect("hex");
        let b = u32::from_str_radix(&hex[4..6], 16).expect("hex");
        r + g + b
    };
    assert!(
        luminance(crown.trim_start_matches("const "))
            < luminance(floor.trim_start_matches("const ")),
        "the pill is darker at its crown than at its floor: the ramp hands brightness downward"
    );
}
