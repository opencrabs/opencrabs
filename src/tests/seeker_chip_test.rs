//! Seeker kind-chip styling sentinel (#2533, #3066).

const SOURCE: &str = include_str!("../gui/command_palette.rs");

#[test]
fn seeker_chip_row_is_filled_not_outlined() {
    let start = SOURCE
        .find("for (ix, (label, kind)) in chip_defs()")
        .expect("the chip builder stays in the overlay");
    let end = SOURCE
        .get(start..)
        .expect("the chip builder starts on a glyph")
        .find("card = card.child(row);")
        .map(|offset| start + offset)
        .expect("the chip row ends at the card attachment");
    let row = SOURCE
        .get(start..end)
        .expect("the chip row ends on a glyph");

    assert!(
        !row.contains(".border_1()") && !row.contains(".border_color("),
        "Spotlight chips are filled capsules, not bordered labels"
    );
    for expected in [
        ".rounded_full()",
        ".bg(rgba(CHIP_FILL))",
        ".bg(rgba(CHIP_ACTIVE_FILL))",
        ".text_color(rgb(INK))",
        ".bg(rgba(0xffffff33))",
        ".bg(rgba(0xffffff28))",
    ] {
        assert!(
            row.contains(expected),
            "the chip row lost `{expected}` from its filled active, inactive, or hover state"
        );
    }
    assert!(
        SOURCE.contains("const CHIP_FILL: u32 = 0xffffff14;"),
        "the inactive fill stays a named translucent white token"
    );
    // The active capsule is a neutral lift, not the theme accent (#3066): at
    // low alpha over the smoked card the accent sampled brown, so the fill is
    // a shade of the glass and stays a named token.
    assert!(
        SOURCE.contains("const CHIP_ACTIVE_FILL: u32 = 0xffffff26;"),
        "the active fill stays a named neutral token instead of the accent"
    );
}
