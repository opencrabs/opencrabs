//! Regression tests for built-in theme presets (#1364).
//!
//! Every hex here is pinned to the UPSTREAM spec, not to our code —
//! if a preset value drifts from the canonical source, these tests
//! fail. Sources are named per preset; the audit trail lives in
//! ~/.opencrabs/projects/opencrabs/research (verification receipts).

use crate::tui::render::palette;
use crate::tui::render::presets::{
    self, ALUCARD, CATPPUCCIN_LATTE, CATPPUCCIN_MOCHA, DRACULA, MONOKAI, SOLARIZED_DARK,
    SOLARIZED_LIGHT,
};
use crate::tui::render::theme::{self, Role};
use ratatui::style::Color;

const fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// crab-dark must be byte-identical to the post-S1 palette: every role
/// resolves to its palette const. This is the #1363 no-regression
/// guarantee, enforced.
#[test]
fn crab_dark_is_byte_identical_to_palette() {
    let t = &theme::CRAB_DARK;
    let c = &t.colors;
    assert_eq!(c.accent, palette::ORANGE);
    assert_eq!(c.accent_teal, palette::TEAL);
    assert_eq!(c.accent_soft, palette::WHITE);
    assert_eq!(c.text_primary, palette::TEXT_PRIMARY);
    assert_eq!(c.text_secondary, palette::TEXT_SECONDARY);
    assert_eq!(c.text_muted, palette::TEXT_MUTED);
    assert_eq!(c.text_dim, palette::TEXT_DIM);
    assert_eq!(c.gray, palette::GRAY);
    assert_eq!(c.gray_mid, palette::GRAY_MID);
    assert_eq!(c.gray_detail, palette::GRAY_DETAIL);
    assert_eq!(c.gray_dim, palette::GRAY_DIM);
    assert_eq!(c.gray_dark, palette::GRAY_DARK);
    assert_eq!(c.gray_base, palette::GRAY_BASE);
    assert_eq!(c.gray_light, palette::GRAY_LIGHT);
    assert_eq!(c.gray_soft, palette::GRAY_SOFT);
    assert_eq!(c.gray_muted, palette::GRAY_MUTED);
    assert_eq!(c.success, palette::SUCCESS);
    assert_eq!(c.analytics_green, palette::ANALYTICS_GREEN);
    assert_eq!(c.green_check, palette::GREEN_CHECK);
    assert_eq!(c.error, palette::ERROR);
    assert_eq!(c.error_soft, palette::ERROR_SOFT);
    assert_eq!(c.error_faded, palette::ERROR_FADED);
    assert_eq!(c.warning, palette::WARNING);
    assert_eq!(c.warning_muted, palette::WARNING_MUTED);
    assert_eq!(c.amber_muted, palette::AMBER_MUTED);
    assert_eq!(c.teal_vivid, palette::TEAL_VIVID);
    assert_eq!(c.teal_bright, palette::TEAL_BRIGHT);
    assert_eq!(c.teal_muted, palette::TEAL_MUTED);
    assert_eq!(c.teal_calm, palette::TEAL_CALM);
    assert_eq!(c.blue_slate, palette::BLUE_SLATE);
    assert_eq!(c.blue_steel, palette::BLUE_STEEL);
    assert_eq!(c.blue_link, palette::BLUE_LINK);
    assert_eq!(c.blue_sky, palette::BLUE_SKY);
    assert_eq!(c.blue_soft, palette::BLUE_SOFT);
    assert_eq!(c.blue_vivid, palette::BLUE_VIVID);
    assert_eq!(c.blue_code, palette::BLUE_CODE);
    assert_eq!(c.selection_bg, palette::SELECTION_BG);
    assert_eq!(c.surface_panel, palette::SURFACE_PANEL);
    assert_eq!(c.surface_qr, palette::SURFACE_QR);
    assert_eq!(c.surface_code, palette::SURFACE_CODE);
    assert_eq!(c.surface_code_alt, palette::SURFACE_CODE_ALT);
    assert_eq!(c.ink, palette::INK);
    assert_eq!(c.purple_soft, palette::PURPLE_SOFT);
}

/// https://draculatheme.com/spec — Dracula Classic.
#[test]
fn dracula_matches_upstream_spec() {
    let c = &DRACULA.colors;
    assert_eq!(c.surface_panel, rgb(0x282A36), "bg");
    assert_eq!(c.text_primary, rgb(0xF8F8F2), "fg");
    assert_eq!(c.accent, rgb(0xFFB86C), "orange");
    assert_eq!(c.error, rgb(0xFF5555), "red");
    assert_eq!(c.success, rgb(0x50FA7B), "green");
    assert_eq!(c.warning, rgb(0xF1FA8C), "yellow");
    assert_eq!(c.purple_soft, rgb(0xBD93F9), "purple");
    assert_eq!(c.accent_teal, rgb(0x8BE9FD), "cyan");
    assert_eq!(c.text_secondary, rgb(0x6272A4), "comment");
    assert_eq!(c.selection_bg, rgb(0x44475A), "current line");
}

/// https://draculatheme.com/spec — Alucard, the official light
/// companion. Canonical bg is #FFFBEB (NOT third-party #FCF6E3).
#[test]
fn alucard_matches_upstream_spec() {
    let c = &ALUCARD.colors;
    assert_eq!(
        c.surface_panel,
        rgb(0xFFFBEB),
        "canonical bg, not the ported #FCF6E3"
    );
    assert_eq!(c.text_primary, rgb(0x1F1F1F), "fg");
    assert_eq!(c.accent, rgb(0xA34D14), "orange");
    assert_eq!(c.error, rgb(0xCB3A2A), "red");
    assert_eq!(c.success, rgb(0x14710A), "green");
    assert_eq!(c.purple_soft, rgb(0x644AC9), "purple");
    assert_eq!(c.surface_qr, rgb(0xEFEDDC), "floating");
    assert_eq!(c.selection_bg, rgb(0xDEDCCF), "bg-light");
    // `bg-dark` (#CECCC0) is the plate under this canvas, so as a
    // foreground it measured 1.56:1: the dim tiers drew a background
    // colour as text. They derive from `bg-dark` toward the ink instead,
    // which is a deliberate drift from the upstream spec (#2492).
    assert_eq!(c.gray_dark, rgb(0xCECCC0), "bg-dark, still the plate");
    assert_eq!(c.gray_mid, rgb(0xA3A097), "bg-dark toward ink, 2.52:1");
    assert_eq!(c.gray_detail, rgb(0x88867F), "bg-dark toward ink, 3.51:1");
}

/// tomasr/molokai — classic variant (#272822 bg, NOT Monokai Pro, NOT
/// the darker #1B1D1E default variant).
#[test]
fn monokai_matches_upstream_spec() {
    let c = &MONOKAI.colors;
    assert_eq!(c.surface_panel, rgb(0x272822), "classic bg");
    assert_eq!(c.accent, rgb(0xFD971F), "orange");
    assert_eq!(c.error, rgb(0xF92672), "pink");
    assert_eq!(c.success, rgb(0xA6E22E), "green");
    assert_eq!(c.accent_teal, rgb(0x66D9EF), "cyan");
    assert_eq!(c.warning, rgb(0xE6DB74), "yellow");
    assert_eq!(c.purple_soft, rgb(0xAE81FF), "purple");
    assert_eq!(c.text_secondary, rgb(0x75715E), "comment");
}

/// https://ethanschoonover.com/solarized — one spec, two modes; light
/// = bg base3 / fg base00, dark = bg base03 / fg base0.
#[test]
fn solarized_light_matches_upstream_spec() {
    let c = &SOLARIZED_LIGHT.colors;
    assert_eq!(c.surface_panel, rgb(0xFDF6E3), "base3 bg");
    assert_eq!(c.text_primary, rgb(0x657B83), "base00 fg");
    assert_eq!(c.accent, rgb(0xCB4B16), "orange");
    assert_eq!(c.error, rgb(0xDC322F), "red");
    assert_eq!(c.success, rgb(0x859900), "green");
    assert_eq!(c.warning, rgb(0xB58900), "yellow");
    assert_eq!(c.blue_steel, rgb(0x268BD2), "blue");
    assert_eq!(c.purple_soft, rgb(0x6C71C4), "violet");
}

#[test]
fn solarized_dark_matches_upstream_spec() {
    let c = &SOLARIZED_DARK.colors;
    assert_eq!(c.surface_panel, rgb(0x002B36), "base03 bg");
    assert_eq!(c.text_primary, rgb(0x839496), "base0 fg");
    assert_eq!(c.accent, rgb(0xCB4B16), "orange");
    assert_eq!(c.ink, rgb(0x002B36), "ink on-light");
}

/// https://github.com/catppuccin/palette — mocha.
#[test]
fn catppuccin_mocha_matches_upstream_spec() {
    let c = &CATPPUCCIN_MOCHA.colors;
    assert_eq!(c.surface_panel, rgb(0x1E1E2E), "base");
    assert_eq!(c.surface_qr, rgb(0x181825), "mantle");
    assert_eq!(c.text_primary, rgb(0xCDD6F4), "text");
    assert_eq!(c.text_muted, rgb(0xA6ADC8), "subtext0");
    assert_eq!(c.accent, rgb(0xFAB387), "orange");
    assert_eq!(c.error, rgb(0xF38BA8), "red");
    assert_eq!(c.success, rgb(0xA6E3A1), "green");
    assert_eq!(c.warning, rgb(0xF9E2AF), "yellow");
    assert_eq!(c.blue_steel, rgb(0x89B4FA), "blue");
    assert_eq!(c.purple_soft, rgb(0xCBA6F7), "mauve");
    assert_eq!(c.accent_teal, rgb(0x94E2D5), "teal");
}

/// https://github.com/catppuccin/palette — latte.
#[test]
fn catppuccin_latte_matches_upstream_spec() {
    let c = &CATPPUCCIN_LATTE.colors;
    assert_eq!(c.surface_panel, rgb(0xEFF1F5), "base");
    assert_eq!(c.text_primary, rgb(0x4C4F69), "text");
    assert_eq!(c.accent, rgb(0xFE640B), "orange");
    assert_eq!(c.error, rgb(0xD20F39), "red");
    assert_eq!(c.success, rgb(0x40A02B), "green");
    // Yellow is a fill in the upstream palette: #DF8E1D measures 2.31:1 as
    // text on base, under the 2.5:1 floor every theme role has to clear, so
    // the shipped value is that hue darkened to 2.70:1 (#2492).
    assert_eq!(c.warning, rgb(0xC98528), "yellow darkened for text");
    assert_eq!(c.warning_muted, rgb(0xDF8E1D), "upstream yellow, fill only");
    assert_eq!(c.blue_steel, rgb(0x1E66F5), "blue");
    assert_eq!(c.purple_soft, rgb(0x8839EF), "mauve");
    assert_eq!(c.accent_teal, rgb(0x179299), "teal");
}

/// Roster contract: the 8 hand-built presets first in pinned order
/// (crab-dark default at the head), then the embedded curated pack
/// (#1461); names unique across the full roster.
#[test]
fn roster_is_complete_and_ordered() {
    let roster = presets::built_ins();
    let expected = 8 + crate::tui::theme_catalog::theme_pack::sources().len();
    assert_eq!(roster.len(), expected, "roster = hand-built + pack");
    assert_eq!(roster[0].name, "crab-dark", "default first");
    assert_eq!(roster[1].name, "dracula", "hand-built order preserved");
    let mut names: Vec<&str> = roster.iter().map(|t| t.name).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), before, "names unique");
}

/// `/theme set` lookup: case-insensitive, exact-name miss returns None.
#[test]
fn by_name_is_case_insensitive() {
    assert!(presets::by_name("Dracula").is_some());
    assert!(presets::by_name("CATPPUCCIN-MOCHA").is_some());
    assert!(presets::by_name("nope").is_none());
    assert_eq!(presets::by_name("alucard").unwrap().name, "alucard");
}

/// Runtime switching: set() redirects role(), reset() restores palette
/// defaults. Runs last-ish; resets at the end so other tests that read
/// CRAB_DARK directly are unaffected either way (they read the static,
/// not the active slot).
#[test]
fn set_and_reset_switch_active_theme() {
    // The only writer of the global theme slot in the suite, and it bumps the
    // generation counter eleven times. `tui_theme_cache_invalidation_test`
    // reads that counter, so the two must not overlap under the parallel
    // harness; see `theme_global_lock`.
    let _guard = crate::tests::theme_global_lock::lock();

    assert_eq!(theme::role(Role::Accent), palette::ORANGE);
    theme::set(&DRACULA);
    assert_eq!(theme::role(Role::Accent), rgb(0xFFB86C));
    theme::set(&SOLARIZED_LIGHT);
    assert_eq!(theme::role(Role::SurfacePanel), rgb(0xFDF6E3));
    theme::reset();
    assert_eq!(theme::role(Role::Accent), palette::ORANGE);

    // The canvas rides the same active slot (#1634). Asserted here rather
    // than in its own test on purpose: `set` mutates process-global state,
    // so a second test doing it would race this one under the parallel
    // harness. One mutator, one place.
    assert_eq!(
        theme::background(),
        None,
        "crab-dark declares no canvas, so the terminal's own shows through"
    );
    theme::set(&DRACULA);
    assert_eq!(theme::background(), Some(rgb(0x282A36)));
    theme::set(&SOLARIZED_LIGHT);
    assert_eq!(theme::background(), Some(rgb(0xFDF6E3)));
    theme::reset();
    assert_eq!(theme::background(), None);

    // Body prose follows the active palette (#1634). Folded over a bare
    // `Style::default()` this span shipped `fg: None` and rendered in the
    // terminal's own foreground under every theme. Lives here for the same
    // reason the canvas assertions do: one mutator of the global slot.
    let prose = |markdown: &str| {
        crate::tui::markdown::parse_markdown(markdown, 80)
            .into_iter()
            .flat_map(|l| l.spans)
            .find(|s| s.content.contains("ordinary"))
            .expect("prose span")
            .style
            .fg
    };
    theme::set(&DRACULA);
    assert_eq!(prose("ordinary prose"), Some(rgb(0xF8F8F2)));
    theme::set(&SOLARIZED_LIGHT);
    assert_eq!(prose("ordinary prose"), Some(rgb(0x657B83)));
    theme::reset();
    assert_eq!(prose("ordinary prose"), Some(palette::TEXT_PRIMARY));

    // Every switch bumps the generation the render cache stamps itself with.
    // Only monotonicity is asserted: the counter is process-wide, so an
    // absolute value would depend on how many tests ran before this one.
    let before = theme::generation();
    theme::set(&DRACULA);
    let after_set = theme::generation();
    assert!(
        after_set > before,
        "set did not bump the generation, so the render cache keeps stale colours"
    );
    theme::reset();
    assert!(
        theme::generation() > after_set,
        "reset did not bump the generation"
    );
}

/// F1: RGB→ANSI-256 quantizer — the degraded-tier backbone.
///
/// Reference targets:
///  - Pure black #000000 → 16 (cube corner)
///  - Pure white #ffffff → 231 (cube corner)
///  - Pure red #ff0000 → 196  (16 + 36*5 + 0 + 0)
///  - Pure green #00ff00 → 46 (16 + 0 + 6*5 + 0)
///  - Pure blue #0000ff → 21  (16 + 0 + 0 + 5)
///  - Dracula orange #ffb86c → 216 (cube nearest; saturated wins over gray)
///  - Neutral gray #808080 → 244 (grayscale ramp beats the saturated cube
///    neighbor — the whole reason the gray branch exists)
///  - Terminal.app Sequoia failure case: any RGB should land on a defined
///    index in 16..=255, never out of range (crash guard)
#[test]
fn rgb_to_ansi256_corners() {
    use crate::tui::render::theme::rgb_to_ansi256;
    assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
    assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
    assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
    assert_eq!(rgb_to_ansi256(0, 255, 0), 46);
    assert_eq!(rgb_to_ansi256(0, 0, 255), 21);
}

#[test]
fn rgb_to_ansi256_dracula_orange_stays_saturated() {
    use crate::tui::render::theme::rgb_to_ansi256;
    let idx = rgb_to_ansi256(0xFF, 0xB8, 0x6C);
    assert!(
        (16..=231).contains(&idx),
        "Dracula orange must land in the color cube, got {idx}"
    );
}

#[test]
fn rgb_to_ansi256_neutral_gray_picks_ramp_over_cube() {
    use crate::tui::render::theme::rgb_to_ansi256;
    let idx = rgb_to_ansi256(0x80, 0x80, 0x80);
    assert!(
        (232..=255).contains(&idx),
        "Neutral gray must prefer the grayscale ramp, got {idx}"
    );
}

#[test]
fn rgb_to_ansi256_output_always_in_range() {
    use crate::tui::render::theme::rgb_to_ansi256;
    // Spot-check a spread of RGB values — every output must be a valid
    // ANSI-256 index (16..=255). The 0..15 range is reserved for the
    // terminal's basic 16 colors and never emitted by the quantizer.
    for r in [0u8, 64, 128, 192, 255] {
        for g in [0u8, 64, 128, 192, 255] {
            for b in [0u8, 64, 128, 192, 255] {
                let idx = rgb_to_ansi256(r, g, b);
                assert!(
                    (16..=255).contains(&idx),
                    "out-of-range ANSI index {idx} from ({r},{g},{b})"
                );
            }
        }
    }
}
