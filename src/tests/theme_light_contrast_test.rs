//! The light-canvas readability certificate (#2492).
//!
//! Every shipped theme is drawn on some canvas, and the contrast a role
//! needs depends on which one: the same dim fraction that reads as quiet on
//! `#1E1E2E` reads as invisible on `#FDF6E3`. Before this, only the eight
//! dark-era pairs were checked, at a 2.5:1 floor, and the light palettes
//! shipped with body text at 2.88:1 and dim tiers under 1:1 against their own
//! background.
//!
//! These tests certify the whole roster through the same functions the
//! validator uses, so a new preset, a new pack theme, or a hand-typed colour
//! that regresses a light palette fails here rather than in someone's eyes.

use ratatui::style::Color;

use crate::tui::render::presets::{ALUCARD, CATPPUCCIN_LATTE, SOLARIZED_LIGHT, built_ins};
use crate::tui::render::theme::{Role, Theme, rgb_to_ansi256};
use crate::tui::render::user_themes::{
    CONTRAST_FLOOR, CONTRAST_PAIRS, build_theme, canvas, contrast, contrast_pairs_for,
    is_light_canvas, reading_floor, relative_luminance,
};

/// Resolve a pair's background the way the validator does: `Ink` is the
/// canvas itself, not the chip colour a light preset happens to put there.
fn pair_ratio(theme: &Theme, fg: Role, bg: Role) -> f64 {
    let surface = canvas(&theme.colors);
    let bgc = if bg == Role::Ink {
        surface
    } else {
        theme.colors.get(bg)
    };
    contrast(theme.colors.get(fg), bgc)
}

/// Every light built-in theme clears every floor its own canvas implies.
/// This is the guarantee #2492 adds: pick any light theme in the picker and
/// its body text and dim tiers are readable on the surface they are drawn
/// on.
///
/// Dark themes are deliberately not re-certified here: the 2.5:1 floor was
/// calibrated on `crab-dark` alone, four hand-built dark presets never met
/// it for `gray_mid` on the panel, and tightening them is a separate
/// decision with its own issue. What this test does pin is that the dark
/// pair table itself is untouched (see
/// `dark_canvases_keep_the_historical_pairs`).
#[test]
fn every_light_built_in_theme_clears_its_canvas_floors() {
    let all = built_ins();
    assert!(
        all.len() >= 38,
        "expected the full roster, got {} themes",
        all.len()
    );
    let mut certified = 0;
    for theme in all {
        let surface = canvas(&theme.colors);
        if !is_light_canvas(surface) {
            continue;
        }
        certified += 1;
        let pairs = contrast_pairs_for(surface);
        assert!(
            pairs.len() > CONTRAST_PAIRS.len(),
            "{}: a light canvas must certify the dim tiers too",
            theme.name
        );
        for (fg, bg, floor) in pairs {
            let ratio = pair_ratio(theme, fg, bg);
            assert!(
                ratio >= floor,
                "{}: {fg:?} on {bg:?} is {ratio:.2}:1, needs {floor:.1}:1",
                theme.name
            );
        }
    }
    assert_eq!(
        certified, 5,
        "expected the five light themes (alucard, catppuccin-latte, gruvbox-light, \
         rosepine-dawn, solarized-light), certified {certified}"
    );
}

/// The five light themes are the ones the light floors apply to, and the
/// classification comes from the declared canvas rather than from a name.
#[test]
fn light_themes_are_identified_by_their_canvas() {
    let mut light: Vec<&str> = built_ins()
        .iter()
        .filter(|t| is_light_canvas(canvas(&t.colors)))
        .map(|t| t.name)
        .collect();
    light.sort_unstable();
    assert_eq!(
        light,
        vec![
            "alucard",
            "catppuccin-latte",
            "gruvbox-light",
            "rosepine-dawn",
            "solarized-light",
        ],
        "light roster changed: check the new theme's floors before updating this list"
    );
}

/// A light canvas certifies the dim text tiers as well, and a dark one does
/// not: those tiers are deliberately dim decoration on a dark palette.
#[test]
fn light_canvases_certify_the_dim_tiers() {
    let light = Color::Rgb(0xFD, 0xF6, 0xE3);
    let dark = Color::Rgb(0x1E, 0x1E, 0x2E);
    assert!(is_light_canvas(light));
    assert!(!is_light_canvas(dark));

    let floors = contrast_pairs_for(light);
    assert_eq!(floors.len(), CONTRAST_PAIRS.len() + 5);
    for (role, expected) in [
        (Role::TextMuted, 4.0),
        (Role::GrayMuted, 4.0),
        (Role::TextDim, 3.5),
        (Role::GrayDetail, 3.5),
        (Role::GrayDim, CONTRAST_FLOOR),
    ] {
        let found = floors
            .iter()
            .find(|(fg, _, _)| *fg == role)
            .unwrap_or_else(|| panic!("{role:?} missing from the light certificate"));
        assert_eq!(
            found.1,
            Role::Ink,
            "{role:?} must certify against the canvas"
        );
        assert_eq!(found.2, expected, "{role:?} floor");
    }
}

/// A dark canvas keeps exactly the eight historical pairs at the historical
/// floor. This is what lets every dark preset and pack theme keep certifying
/// and regenerating unchanged.
#[test]
fn dark_canvases_keep_the_historical_pairs() {
    let dark = Color::Rgb(0x28, 0x2A, 0x36);
    let floors = contrast_pairs_for(dark);
    assert_eq!(floors.len(), CONTRAST_PAIRS.len());
    for ((fg, bg, floor), &(want_fg, want_bg)) in floors.iter().zip(CONTRAST_PAIRS.iter()) {
        assert_eq!((*fg, *bg), (want_fg, want_bg));
        assert_eq!(*floor, CONTRAST_FLOOR);
    }
}

/// `reading_floor` answers per role and per canvas, and declines for
/// surfaces rather than inventing a floor for them.
#[test]
fn reading_floor_is_keyed_on_the_canvas() {
    let light = Color::Rgb(0xEF, 0xF1, 0xF5);
    let dark = Color::Rgb(0x1E, 0x1E, 0x2E);
    assert_eq!(reading_floor(Role::TextSecondary, light), Some(4.5));
    assert_eq!(reading_floor(Role::GraySoft, light), Some(4.5));
    assert_eq!(reading_floor(Role::TextMuted, light), Some(4.0));
    assert_eq!(reading_floor(Role::GrayMuted, light), Some(4.0));
    assert_eq!(reading_floor(Role::TextDim, light), Some(3.5));
    assert_eq!(reading_floor(Role::GrayDetail, light), Some(3.5));
    assert_eq!(reading_floor(Role::GrayDim, light), Some(CONTRAST_FLOOR));
    assert_eq!(reading_floor(Role::SurfaceCode, light), None);
    assert_eq!(reading_floor(Role::Ink, light), None);

    assert_eq!(
        reading_floor(Role::TextSecondary, dark),
        Some(CONTRAST_FLOOR)
    );
    assert_eq!(reading_floor(Role::TextMuted, dark), None);
    assert_eq!(reading_floor(Role::GrayDim, dark), None);
}

/// The light presets that shipped unreadable are fixed, measured on the
/// canvas each one declares rather than on a remembered number.
#[test]
fn the_fixed_light_presets_clear_body_text() {
    for (theme, want) in [
        (&CATPPUCCIN_LATTE, 4.5),
        (&SOLARIZED_LIGHT, 4.5),
        (&ALUCARD, 4.5),
    ] {
        let ratio = pair_ratio(theme, Role::TextSecondary, Role::Ink);
        assert!(
            ratio >= want,
            "{}: TextSecondary on the canvas is {ratio:.2}:1, needs {want:.1}:1",
            theme.name
        );
        // The dim tiers are the ones that were literally a background plate
        // reused as a foreground: base2 on base3 was 1.14:1.
        for role in [Role::GrayMuted, Role::GrayDetail, Role::GrayDim] {
            let floor = reading_floor(role, canvas(&theme.colors)).expect("light floor");
            let ratio = pair_ratio(theme, role, Role::Ink);
            assert!(
                ratio >= floor,
                "{}: {role:?} on the canvas is {ratio:.2}:1, needs {floor:.1}:1",
                theme.name
            );
        }
    }
}

/// The `ansi` table is hand-written per preset, so a colour edit that skips
/// it leaves the 256-colour tier drawing the old shade. Quantize the roles
/// these fixes touched and compare.
#[test]
fn touched_preset_colours_still_quantize_to_their_ansi_index() {
    let cases: [(&Theme, &[Role]); 3] = [
        (
            &CATPPUCCIN_LATTE,
            &[
                Role::TextDim,
                Role::GrayMid,
                Role::GrayDetail,
                Role::GrayDim,
                Role::GraySoft,
                Role::GrayMuted,
                Role::Warning,
            ],
        ),
        (
            &SOLARIZED_LIGHT,
            &[
                Role::TextSecondary,
                Role::TextMuted,
                Role::TextDim,
                Role::GrayMid,
                Role::GrayDetail,
                Role::GrayDim,
                Role::GraySoft,
                Role::GrayMuted,
            ],
        ),
        (&ALUCARD, &[Role::GrayMid, Role::GrayDetail, Role::GrayDim]),
    ];
    for (theme, roles) in cases {
        for &role in roles {
            let Color::Rgb(r, g, b) = theme.colors.get(role) else {
                panic!("{}: {role:?} is not an Rgb colour", theme.name);
            };
            let Color::Indexed(idx) = theme.colors.get_ansi(role) else {
                panic!("{}: {role:?} has no ansi index", theme.name);
            };
            assert_eq!(
                idx,
                rgb_to_ansi256(r, g, b),
                "{}: {role:?} ansi index does not match its rgb",
                theme.name
            );
        }
    }
}

/// The validator enforces the same floors, so a drop-in theme file with a
/// light canvas and a background-plate foreground is refused at load time
/// instead of rendering as a blank-looking tier.
#[test]
fn a_light_drop_in_with_unreadable_text_is_rejected() {
    let Some((name, toml)) = crate::tui::theme_catalog::theme_pack::sources()
        .iter()
        .find(|(n, _)| *n == "rosepine-dawn")
    else {
        panic!("rosepine-dawn missing from the pack");
    };
    // Sanity: the shipped file loads under the new floors.
    build_theme(&format!("certify-{name}"), toml).expect("shipped pack theme must load");

    // Push the body-text tier to the canvas colour: same-on-same is the
    // shape of the bug this certificate exists for. Both hexes are read out
    // of the file, so a regeneration cannot turn this into a no-op.
    let canvas = toml
        .lines()
        .find_map(|l| l.strip_prefix("ink = \"#"))
        .and_then(|v| v.strip_suffix('"'))
        .expect("pack theme declares ink");
    let current = toml
        .lines()
        .find_map(|l| l.strip_prefix("text_secondary = \"#"))
        .and_then(|v| v.strip_suffix('"'))
        .expect("pack theme declares text_secondary");
    let broken = toml.replace(
        &format!("text_secondary = \"#{current}\""),
        &format!("text_secondary = \"#{canvas}\""),
    );
    assert_ne!(broken, *toml, "fixture rewrite did not apply");
    let err = build_theme("certify-broken", &broken)
        .expect_err("unreadable light theme must be rejected");
    assert!(
        err.contains("text_secondary") && err.contains("below"),
        "rejection should name the role and the floor, got: {err}"
    );
}

/// A dark canvas keeps the historical validator behaviour: the same
/// same-on-same body text is still rejected, but the dim tiers are not
/// certified, so a deliberately dim dark theme still loads.
#[test]
fn dark_canvas_dim_tiers_are_still_allowed_to_be_dim() {
    let dark = Color::Rgb(0x1E, 0x1E, 0x2E);
    assert!(relative_luminance(dark) < 0.05);
    assert_eq!(reading_floor(Role::GrayDim, dark), None);
}
