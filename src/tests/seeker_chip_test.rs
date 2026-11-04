//! Seeker kind-chip styling sentinel (#2533).

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
        ".bg(rgba((theme.accent << 8) | 0x2e))",
        ".text_color(rgb(theme.accent))",
        ".bg(rgba((theme.accent << 8) | 0x42))",
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
}
