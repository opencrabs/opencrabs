//! Canvas background coverage (#1634).
//!
//! `/theme` left the canvas untouched because `ThemeColors` had no
//! background field at all: 43 roles, none of them the surface the UI is
//! drawn on. These tests pin the field that fixes it, and the decision
//! that it stays optional so a theme can decline to paint and leave the
//! terminal's own background, transparency and blur intact.
//!
//! Every hex is pinned to the UPSTREAM spec, matching the convention in
//! `tui_theme_presets_test.rs`: drift from the canonical source fails here.

use crate::tui::render::presets::{
    ALUCARD, CATPPUCCIN_LATTE, CATPPUCCIN_MOCHA, DRACULA, MONOKAI, SOLARIZED_DARK, SOLARIZED_LIGHT,
    built_ins,
};
use crate::tui::render::theme::{self, rgb_to_ansi256};
use crate::tui::render::user_themes::build_theme;
use crate::tui::theme_catalog::theme_pack::sources;
use ratatui::style::Color;

const fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// The decision this issue turned on: the default theme declares NO
/// canvas, so `crab-dark` renders exactly as it did before #1634 and a
/// transparent or blurred terminal keeps showing through. If this ever
/// becomes `Some`, every user running a see-through terminal loses it.
#[test]
fn crab_dark_declares_no_canvas() {
    assert_eq!(
        theme::CRAB_DARK.colors.background,
        None,
        "crab-dark must not paint a canvas: terminal transparency depends on it"
    );
    assert_eq!(theme::CRAB_DARK.colors.ansi.background, None);
}

/// Each built-in preset declares its real upstream background, the value
/// that was parsed and then thrown away before #1634.
#[test]
fn presets_declare_their_upstream_background() {
    let cases = [
        ("dracula", &DRACULA, 0x282A36u32),
        ("alucard", &ALUCARD, 0xFFFBEB),
        ("monokai", &MONOKAI, 0x272822),
        ("catppuccin-mocha", &CATPPUCCIN_MOCHA, 0x1E1E2E),
        ("catppuccin-latte", &CATPPUCCIN_LATTE, 0xEFF1F5),
        ("solarized-light", &SOLARIZED_LIGHT, 0xFDF6E3),
        ("solarized-dark", &SOLARIZED_DARK, 0x002B36),
    ];
    for (name, theme, hex) in cases {
        assert_eq!(
            theme.colors.background,
            Some(rgb(hex)),
            "{name} background drifted from its upstream spec"
        );
    }
}

/// The degraded tier has to answer too: a non-truecolor terminal still
/// needs a canvas index, and it is derived from the rgb rather than
/// hand-picked, matching the documented `AnsiColors` contract.
#[test]
fn preset_ansi_background_is_derived_from_the_rgb() {
    let cases = [
        (&DRACULA, 0x282A36u32),
        (&ALUCARD, 0xFFFBEB),
        (&MONOKAI, 0x272822),
        (&CATPPUCCIN_MOCHA, 0x1E1E2E),
        (&CATPPUCCIN_LATTE, 0xEFF1F5),
        (&SOLARIZED_LIGHT, 0xFDF6E3),
        (&SOLARIZED_DARK, 0x002B36),
    ];
    for (theme, hex) in cases {
        let (r, g, b) = (
            ((hex >> 16) & 0xFF) as u8,
            ((hex >> 8) & 0xFF) as u8,
            (hex & 0xFF) as u8,
        );
        assert_eq!(
            theme.colors.ansi.background,
            Some(rgb_to_ansi256(r, g, b)),
            "{} ansi background is not the quantized rgb",
            theme.name
        );
    }
}

/// A theme file written before the canvas existed must still load. This
/// is why the field is `Option` and `#[serde(default)]` rather than a
/// 44th required key: `deny_unknown_fields` would otherwise turn every
/// pre-existing drop-in into a parse error.
#[test]
fn user_theme_without_background_still_loads_and_declares_none() {
    let toml = pack_toml_without_background();
    let theme = build_theme("bgtest-absent", &toml).expect("legacy theme file must still load");
    assert_eq!(theme.colors.background, None);
    assert_eq!(theme.colors.ansi.background, None);
}

/// A theme file that declares a canvas gets it, quantized for the
/// degraded tier on the way through.
#[test]
fn user_theme_with_background_declares_it() {
    // The canvas is what the contrast floors are measured against, so a
    // declared background has to be the surface the palette was built for.
    // This fixture re-declares the theme's own ink, which keeps the test
    // about the field round-tripping rather than about a colour change.
    let base = pack_toml_without_background();
    let ink = base
        .lines()
        .find_map(|l| l.strip_prefix("ink = \"#"))
        .and_then(|v| v.strip_suffix('"'))
        .expect("fixture declares ink");
    let toml = format!("{base}\nbackground = \"#{ink}\"\n");
    let theme = build_theme("bgtest-present", &toml).expect("theme with background must load");
    let declared = u32::from_str_radix(ink, 16).expect("ink is hex");
    assert_eq!(theme.colors.background, Some(rgb(declared)));
    assert_eq!(
        theme.colors.ansi.background,
        Some(rgb_to_ansi256(
            (declared >> 16) as u8,
            (declared >> 8) as u8,
            declared as u8
        ))
    );
}

/// A malformed canvas is a parse error naming the key, not a silent
/// fallback to no background: a typo that renders "correctly" is how a
/// theme file drifts unnoticed.
#[test]
fn user_theme_with_invalid_background_is_rejected() {
    let toml = format!(
        "{}\nbackground = \"not-a-hex\"\n",
        pack_toml_without_background()
    );
    let err = build_theme("bgtest-bad", &toml).expect_err("invalid hex must be rejected");
    assert!(
        err.contains("background"),
        "error must name the offending key, got: {err}"
    );
}

/// Every curated pack theme declares a canvas, and it equals that file's
/// `ink`. Not a coincidence: `converter.rs` maps `(Ink, p.bg)`, so `ink`
/// IS the source's declared background, which is exactly what the
/// converter now emits as `background`. This pins the backfill against
/// what a regeneration from upstream would produce.
#[test]
fn every_pack_theme_declares_a_canvas_matching_its_ink() {
    assert!(!sources().is_empty(), "pack is empty");
    for (name, toml) in sources() {
        let ink =
            toml_value(toml, "ink").unwrap_or_else(|| panic!("pack theme {name} has no ink key"));
        let bg = toml_value(toml, "background")
            .unwrap_or_else(|| panic!("pack theme {name} declares no canvas"));
        assert_eq!(
            bg, ink,
            "pack theme {name}: canvas {bg} does not match its source background {ink}"
        );
    }
}

/// Nothing in the catalog may ship a canvas it cannot parse.
#[test]
fn every_pack_canvas_survives_the_validator() {
    for (name, toml) in sources() {
        let theme = build_theme(&format!("bgcheck-{name}"), toml)
            .unwrap_or_else(|e| panic!("pack theme {name} rejected: {e}"));
        assert!(
            theme.colors.background.is_some(),
            "pack theme {name} parsed with no canvas"
        );
    }
}

/// The built-in roster is mixed on purpose: `crab-dark` declines a canvas
/// and everything else declares one, so the optional field is genuinely
/// exercised by shipped themes rather than only by tests.
#[test]
fn built_ins_are_mixed_on_canvas_declaration() {
    let mut with = 0usize;
    let mut without: Vec<&str> = Vec::new();
    for t in built_ins() {
        if t.colors.background.is_some() {
            with += 1;
        } else {
            without.push(t.name);
        }
    }
    assert!(with > 0, "no built-in declares a canvas");
    assert_eq!(
        without,
        vec!["crab-dark"],
        "crab-dark must be the only built-in without a canvas"
    );
}

/// First `key = "value"` line for `key`, matching the converter's flat
/// emission format. Deliberately not a toml parse: this asserts on the
/// generated text, which is what ships.
fn toml_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key} = ")))
        .map(|v| v.trim().trim_matches('"').to_string())
}

/// A real pack file with its `background` line stripped: the shape every
/// user theme file had before #1634.
fn pack_toml_without_background() -> String {
    let (_, toml) = sources().first().expect("pack is empty");
    toml.lines()
        .filter(|l| !l.starts_with("background = "))
        .collect::<Vec<_>>()
        .join("\n")
}
