//! The custom accent's maths (#2619).
//!
//! The colour wheel is a pure function of a pointer's offset from its centre,
//! so the hue a drag lands on, and the colour that drag produces, are testable
//! without a window. These tests pin the round trip: wheel position to hue to
//! RGB and back.

use crate::config::gui_theme::{
    DEFAULT_ACCENT, format_accent, hsv_to_rgb, hue_from_unit, hue_to_unit, parse_accent, rgb_to_hsv,
};

/// A hue compares within a degree, because the round trip goes through
/// rounded 8-bit channels.
fn assert_hue_eq(actual: f32, expected: f32) {
    let diff = (actual - expected).abs();
    assert!(
        diff < 1.0 || (360.0 - diff) < 1.0,
        "hue {actual:.2} is not {expected:.2}"
    );
}

#[test]
fn the_primaries_land_on_their_six_hue_segments() {
    assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), 0xff0000);
    assert_eq!(hsv_to_rgb(60.0, 1.0, 1.0), 0xffff00);
    assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), 0x00ff00);
    assert_eq!(hsv_to_rgb(180.0, 1.0, 1.0), 0x00ffff);
    assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), 0x0000ff);
    assert_eq!(hsv_to_rgb(300.0, 1.0, 1.0), 0xff00ff);
    assert_eq!(hsv_to_rgb(360.0, 1.0, 1.0), 0xff0000, "hue wraps");
}

#[test]
fn zero_saturation_is_grey_and_zero_value_is_black() {
    assert_eq!(hsv_to_rgb(123.0, 0.0, 1.0), 0xffffff);
    assert_eq!(hsv_to_rgb(123.0, 0.0, 0.5), 0x808080);
    assert_eq!(hsv_to_rgb(123.0, 1.0, 0.0), 0x000000);
}

#[test]
fn out_of_range_input_is_clamped_rather_than_wrapped_in_colour() {
    assert_eq!(hsv_to_rgb(0.0, 2.0, 1.0), 0xff0000);
    assert_eq!(hsv_to_rgb(0.0, 1.0, 3.0), 0xff0000);
    assert_eq!(hsv_to_rgb(0.0, -1.0, 1.0), 0xffffff);
    assert_eq!(hsv_to_rgb(0.0, 1.0, -1.0), 0x000000);
    assert_eq!(hsv_to_rgb(-120.0, 1.0, 1.0), 0x0000ff, "negative hue wraps");
}

#[test]
fn hue_survives_the_round_trip_through_rgb() {
    for hue in [
        0.0, 30.0, 60.0, 90.0, 120.0, 180.0, 210.0, 240.0, 300.0, 330.0,
    ] {
        let rgb = hsv_to_rgb(hue, 1.0, 1.0);
        let (back, saturation, value) = rgb_to_hsv(rgb);
        assert_hue_eq(back, hue);
        assert!((saturation - 1.0).abs() < 0.01);
        assert!((value - 1.0).abs() < 0.01);
    }
}

#[test]
fn grey_has_no_hue_and_full_saturation_comes_back() {
    let (hue, saturation, value) = rgb_to_hsv(0x808080);
    assert_eq!(hue, 0.0);
    assert!(saturation.abs() < 0.01);
    assert!((value - 0x80 as f32 / 255.0).abs() < 0.01);
}

#[test]
fn the_wheel_starts_at_the_top_and_runs_clockwise() {
    let (x, y) = hue_to_unit(0.0);
    assert!(x.abs() < 1e-5 && (y + 1.0).abs() < 1e-5, "0 degrees is up");
    let (x, y) = hue_to_unit(90.0);
    assert!(
        (x - 1.0).abs() < 1e-5 && y.abs() < 1e-5,
        "90 degrees is right"
    );
    let (x, y) = hue_to_unit(180.0);
    assert!(x.abs() < 1e-5 && (y - 1.0).abs() < 1e-5, "180 is down");
    let (x, y) = hue_to_unit(270.0);
    assert!((x + 1.0).abs() < 1e-5 && y.abs() < 1e-5, "270 is left");
}

#[test]
fn the_wheel_position_and_the_hue_are_inverse() {
    for hue in [0.0, 15.0, 45.0, 90.0, 137.0, 200.0, 275.0, 359.0] {
        let (x, y) = hue_to_unit(hue);
        assert_hue_eq(hue_from_unit(x, y), hue);
    }
}

#[test]
fn a_point_on_the_wheel_still_answers_a_hue_when_it_is_off_the_rim() {
    // Only the angle matters, so a drag past the rim keeps the hue it went
    // out on instead of jumping.
    let (x, y) = hue_to_unit(45.0);
    assert_hue_eq(hue_from_unit(x * 4.0, y * 4.0), 45.0);
    assert_eq!(hue_from_unit(0.0, 0.0), 0.0, "the centre answers red");
}

#[test]
fn the_default_accent_is_the_palettes_own() {
    assert_eq!(format_accent(DEFAULT_ACCENT), "#d76414");
    assert_eq!(
        parse_accent(&format_accent(DEFAULT_ACCENT)),
        Some(DEFAULT_ACCENT)
    );
    let (hue, saturation, value) = rgb_to_hsv(DEFAULT_ACCENT);
    assert_hue_eq(hue, 24.47);
    assert!(saturation > 0.8 && value > 0.8);
}
