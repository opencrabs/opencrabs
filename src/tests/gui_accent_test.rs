//! The theme modal's accent maths, tested without a window (#2619).
//!
//! The wheel is a pure function of a pointer's offset from its own painted
//! box, and the tracks are a pure function of an x. Everything the modal does
//! with a drag is therefore decided here, in the library, and these tests pin
//! it: the box to hue, the track to a value, the pick to a colour that can
//! still be seen on the base it lands on.

use crate::config::GuiTheme;
use crate::config::gui_theme::{
    DEFAULT_ACCENT, LIGHT, MIN_ACCENT_CONTRAST, ORGN, contrast_ratio, format_accent, inside,
    legible_accent, parse_accent, resolve, slider_value, wheel_drag, wheel_hit,
};

/// A wheel hundred pixels square, so its centre is (150, 150) and its radius
/// is 50: the arithmetic in the assertions below reads off that directly.
const WHEEL: [f32; 4] = [100.0, 100.0, 200.0, 200.0];

/// A track two hundred pixels wide on row 300.
const TRACK: [f32; 4] = [40.0, 300.0, 240.0, 310.0];

fn assert_hue_eq(actual: f32, expected: f32) {
    let diff = (actual - expected).abs();
    assert!(
        diff < 0.01 || (360.0 - diff) < 0.01,
        "hue {actual:.2} is not {expected:.2}"
    );
}

/// The four cardinal points of the ring are the four the eye expects: up is
/// red, right is a quarter turn, down is half, left is three quarters.
#[test]
fn the_cardinals_of_the_wheel_are_the_cardinal_hues() {
    let top = wheel_hit(WHEEL, 150.0, 100.0).expect("the rim is on the wheel");
    assert_hue_eq(top.hue, 0.0);
    assert!(
        (top.saturation - 1.0).abs() < 0.001,
        "the rim is full saturation"
    );

    let right = wheel_hit(WHEEL, 200.0, 150.0).expect("the rim is on the wheel");
    assert_hue_eq(right.hue, 90.0);

    let bottom = wheel_hit(WHEEL, 150.0, 200.0).expect("the rim is on the wheel");
    assert_hue_eq(bottom.hue, 180.0);

    let left = wheel_hit(WHEEL, 100.0, 150.0).expect("the rim is on the wheel");
    assert_hue_eq(left.hue, 270.0);
}

/// The centre of the wheel has no angle, so it answers red at no saturation
/// rather than dividing by zero or jumping about.
#[test]
fn the_centre_is_grey_and_reads_as_red() {
    let centre = wheel_hit(WHEEL, 150.0, 150.0).expect("the centre is on the wheel");
    assert_hue_eq(centre.hue, 0.0);
    assert_eq!(centre.saturation, 0.0);
}

/// Halfway to the rim is half the saturation, which is what makes a drag read
/// as depth rather than as a jump between two colours.
#[test]
fn the_distance_from_the_centre_is_the_saturation() {
    let half = wheel_hit(WHEEL, 175.0, 150.0).expect("inside the rim");
    assert!(
        (half.saturation - 0.5).abs() < 0.001,
        "half way, half saturated"
    );
    assert_hue_eq(half.hue, 90.0);
}

/// A pointer past the grab ring is not a press on the wheel: the modal must
/// not drag the colour because a click landed on the card around it.
#[test]
fn a_pointer_off_the_wheel_is_not_a_press_on_it() {
    assert!(wheel_hit(WHEEL, 400.0, 400.0).is_none(), "far away");
    assert!(
        wheel_hit(WHEEL, 150.0, 260.0).is_none(),
        "past the grab ring"
    );
    // The grab ring is deliberate: a press a little outside still drags, and
    // pins to the rim instead of wrapping.
    let edge = wheel_hit(WHEEL, 150.0, 205.0).expect("within the grab ring");
    assert_eq!(edge.saturation, 1.0, "past the rim the saturation pins");
    assert_hue_eq(edge.hue, 180.0);
}

/// Once a drag holds the wheel, a pointer that runs past the rim keeps the
/// angle it is pointing at and pins the saturation, so a fast flick does not
/// stall the colour halfway.
#[test]
fn a_drag_past_the_rim_keeps_its_angle() {
    assert!(
        wheel_hit(WHEEL, 150.0, 400.0).is_none(),
        "a press there is a miss"
    );
    let drag = wheel_drag(WHEEL, 150.0, 400.0).expect("a drag reads it anyway");
    assert_eq!(drag.saturation, 1.0, "the saturation pins at the rim");
    assert_hue_eq(drag.hue, 180.0);
    assert!(
        wheel_drag([0.0, 0.0, 0.0, 0.0], 0.0, 0.0).is_none(),
        "no box, no read"
    );
}

/// A box that has not been painted, or has no area, cannot produce a hue.
#[test]
fn a_wheel_with_no_area_answers_nothing() {
    assert!(wheel_hit([10.0, 10.0, 10.0, 10.0], 10.0, 10.0).is_none());
    assert!(wheel_hit([10.0, 10.0, 8.0, 10.0], 9.0, 10.0).is_none());
}

/// The tracks read left to right and clamp at both ends, so a drag that runs
/// off the end of a slider holds its last value instead of wrapping.
#[test]
fn a_track_reads_left_to_right_and_clamps() {
    assert_eq!(slider_value(TRACK, 40.0), 0.0);
    assert_eq!(slider_value(TRACK, 240.0), 1.0);
    assert!((slider_value(TRACK, 140.0) - 0.5).abs() < 0.0001);
    assert_eq!(slider_value(TRACK, 0.0), 0.0, "a pointer left of the track");
    assert_eq!(slider_value(TRACK, 999.0), 1.0, "a pointer right of it");
}

/// A track nobody painted has no width, and a value cannot be invented from
/// it: the answer is 0.0 rather than a division by zero.
#[test]
fn a_track_with_no_width_is_zero() {
    assert_eq!(slider_value([50.0, 10.0, 50.0, 20.0], 50.0), 0.0);
}

/// A press is inside a control with a few pixels of grace, so a drag along an
/// edge keeps the control it started on.
#[test]
fn a_press_keeps_a_little_grace_around_a_control() {
    assert!(inside(TRACK, 140.0, 305.0, 4.0));
    assert!(
        inside(TRACK, 38.0, 305.0, 4.0),
        "four pixels left of the track"
    );
    assert!(!inside(TRACK, 30.0, 305.0, 4.0), "ten pixels left is miss");
    assert!(!inside(TRACK, 140.0, 330.0, 4.0), "a row below is a miss");
}

/// The default accent already reads on the base it ships with, so a window
/// with no `custom_accent` looks exactly like the palette says it does.
#[test]
fn the_default_accent_is_left_alone_on_its_own_base() {
    assert_eq!(legible_accent(DEFAULT_ACCENT, ORGN), DEFAULT_ACCENT);
    assert!(contrast_ratio(DEFAULT_ACCENT, ORGN.bg) >= MIN_ACCENT_CONTRAST);
}

/// A pick that already reads is stored and painted as the same colour, so the
/// swatch, the hex and the ring cannot disagree.
#[test]
fn a_pick_that_reads_on_the_base_is_not_moved() {
    for (picked, base) in [(0xff3366u32, ORGN), (0x2244aa, LIGHT), (0x00ff00, ORGN)] {
        assert_eq!(
            legible_accent(picked, base),
            picked,
            "0x{picked:06x} reads on its base"
        );
    }
}

/// The two extremes the wheel can produce: black on the dark base and white
/// on the light one would both vanish, so both are moved until they can be
/// seen, and both are moved towards the base's opposite, never away from it.
#[test]
fn black_and_white_are_moved_until_they_can_be_seen() {
    let black = legible_accent(0x000000, ORGN);
    assert!(
        contrast_ratio(black, ORGN.bg) >= MIN_ACCENT_CONTRAST,
        "a black pick on the dark base is lifted: 0x{black:06x}"
    );
    let white = legible_accent(0x00ffffff, LIGHT);
    assert!(
        contrast_ratio(white, LIGHT.bg) >= MIN_ACCENT_CONTRAST,
        "a white pick on the light base is darkened: 0x{white:06x}"
    );
}

/// Every colour the wheel can paint stays visible on both bases once it is
/// resolved, which is the promise the derivation makes: a wheel that can pick
/// an invisible accent is a broken control.
#[test]
fn every_wheel_colour_resolves_to_a_visible_accent() {
    for base in [ORGN, LIGHT] {
        for hue in (0..360).step_by(15) {
            for (saturation, value) in [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.2)] {
                let picked = crate::config::gui_theme::hsv_to_rgb(hue as f32, saturation, value);
                let shown = legible_accent(picked, base);
                assert!(
                    contrast_ratio(shown, base.bg) >= MIN_ACCENT_CONTRAST,
                    "0x{picked:06x} on 0x{:06x} resolved to 0x{shown:06x}",
                    base.bg
                );
            }
        }
    }
}

/// Resolving `custom` keeps the pick in the store and shows the legible one.
/// The config keeps exactly what the wheel produced; only the painted accent
/// moves, and only when it has to.
#[test]
fn custom_stores_the_pick_and_paints_a_legible_one() {
    let picked = 0x101010u32;
    let hex = format_accent(picked);
    assert_eq!(parse_accent(&hex), Some(picked), "the hex round trips");
    let shown = resolve(GuiTheme::Custom, true, Some(&hex));
    assert_eq!(shown.bg, ORGN.bg, "the base is still the dark one");
    assert_ne!(shown.accent, picked, "the painted accent was lifted");
    assert!(contrast_ratio(shown.accent, shown.bg) >= MIN_ACCENT_CONTRAST);
}

/// A malformed accent is ignored rather than fatal: the window falls back to
/// the palette the mode names instead of drawing a colour it could not parse.
#[test]
fn a_malformed_accent_falls_back_to_the_mode() {
    for bad in ["", "#", "red", "#12345", "#1234567", "#gggggg", "# 123456"] {
        assert_eq!(
            parse_accent(bad),
            None,
            "{bad:?} is not an accent, so nothing is parsed from it"
        );
    }
    assert_eq!(
        parse_accent(" #123456 "),
        Some(0x123456),
        "spaces are trimmed"
    );
    let shown = resolve(GuiTheme::Custom, false, Some("not a colour"));
    assert_eq!(shown, LIGHT, "an unparseable accent leaves the base alone");
}
