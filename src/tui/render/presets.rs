//! Built-in theme presets for the S2 theme runtime (#1364).
//!
//! Hex values are pinned to UPSTREAM specs, not third-party ports:
//! - dracula / alucard: https://draculatheme.com/spec (alucard is the
//!   official light companion, bg #FFFBEB; surface ladder is the spec's
//!   floating/bg-light/bg-dark)
//! - monokai classic: tomasr/molokai (classic variant, #272822 bg)
//! - solarized light/dark: https://ethanschoonover.com/solarized
//!   (one 16-color spec, two modes; light = bg base3 / fg base00)
//! - catppuccin mocha/latte: https://github.com/catppuccin/palette
//!
//! Roles the upstream spec does not distinguish are mapped onto the
//! spec's ladder (commented "derived"). The regression tests pin every
//! spec-listed value so silent drift fails CI.

use std::sync::OnceLock;

use ratatui::style::Color;

use super::theme::{AnsiColors, Theme, ThemeColors};

const fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

// ── Dracula (dark) ──────────────────────────────────────────────────────────
// Spec: bg #282A36 · fg #F8F8F2 · comment #6272A4 · current line #44475A ·
// red #FF5555 · green #50FA7B · orange #FFB86C · yellow #F1FA8C ·
// purple #BD93F9 · cyan #8BE9FD
pub static DRACULA: Theme = Theme {
    name: "dracula",
    colors: ThemeColors {
        accent: rgb(0xFFB86C),      // spec orange
        accent_teal: rgb(0x8BE9FD), // spec cyan
        accent_soft: rgb(0xF8F8F2), // spec fg
        text_primary: rgb(0xF8F8F2),
        text_secondary: rgb(0x6272A4), // spec comment (derived)
        text_muted: rgb(0x6272A4),
        text_dim: rgb(0x6272A4),
        gray: rgb(0x6272A4),     // ladder: comment (derived)
        gray_mid: rgb(0x44475A), // current line
        gray_detail: rgb(0x44475A),
        gray_dim: rgb(0x44475A),
        gray_dark: rgb(0x44475A),
        gray_base: rgb(0x282A36), // spec bg
        gray_light: rgb(0xF8F8F2),
        gray_soft: rgb(0x6272A4),
        gray_muted: rgb(0x6272A4),
        success: rgb(0x50FA7B),
        analytics_green: rgb(0x50FA7B),
        green_check: rgb(0x50FA7B),
        error: rgb(0xFF5555),
        error_soft: rgb(0xFF5555),
        error_faded: rgb(0xFF5555),
        warning: rgb(0xF1FA8C), // spec yellow
        warning_muted: rgb(0xF1FA8C),
        amber_muted: rgb(0xFFB86C),
        teal_vivid: rgb(0x8BE9FD),
        teal_bright: rgb(0x8BE9FD),
        teal_muted: rgb(0x8BE9FD),
        teal_calm: rgb(0x8BE9FD),
        blue_slate: rgb(0x6272A4),
        blue_steel: rgb(0xBD93F9), // spec purple (derived)
        blue_link: rgb(0x8BE9FD),
        blue_sky: rgb(0x8BE9FD),
        blue_soft: rgb(0x8BE9FD),
        blue_vivid: rgb(0xBD93F9),
        blue_code: rgb(0xBD93F9),
        selection_bg: rgb(0x44475A), // current line = selection
        surface_panel: rgb(0x282A36),
        surface_qr: rgb(0x282A36),
        surface_code: rgb(0x44475A),
        surface_code_alt: rgb(0x44475A),
        ink: rgb(0x282A36),
        purple_soft: rgb(0xBD93F9),
        border_modal: rgb(0xBD93F9),
        background: Some(rgb(0x282A36)), // spec bg
        ansi: AnsiColors {
            accent: 215,
            accent_teal: 117,
            accent_soft: 231,
            text_primary: 231,
            text_secondary: 61,
            text_muted: 61,
            text_dim: 61,
            gray: 61,
            gray_mid: 59,
            gray_detail: 59,
            gray_dim: 59,
            gray_dark: 59,
            gray_base: 17,
            gray_light: 231,
            gray_soft: 61,
            gray_muted: 61,
            success: 84,
            analytics_green: 84,
            green_check: 84,
            error: 203,
            error_soft: 203,
            error_faded: 203,
            warning: 228,
            warning_muted: 228,
            amber_muted: 215,
            teal_vivid: 117,
            teal_bright: 117,
            teal_muted: 117,
            teal_calm: 117,
            blue_slate: 61,
            blue_steel: 141,
            blue_link: 117,
            blue_sky: 117,
            blue_soft: 117,
            blue_vivid: 141,
            blue_code: 141,
            selection_bg: 59,
            surface_panel: 17,
            surface_qr: 17,
            surface_code: 59,
            surface_code_alt: 59,
            ink: 17,
            purple_soft: 141,
            border_modal: 141,
            background: Some(236),
        },
    },
};

// ── Alucard (light) ─────────────────────────────────────────────────────────
// Official Dracula light companion. Spec: bg #FFFBEB · fg #1F1F1F ·
// comment #6C664B · red #CB3A2A · green #14710A · orange #A34D14 ·
// purple #644AC9 · surface ladder: floating #EFEDDC · bg-light #DEDCCF ·
// bg-dark #CECCC0
pub static ALUCARD: Theme = Theme {
    name: "alucard",
    colors: ThemeColors {
        accent: rgb(0xA34D14),      // spec orange
        accent_teal: rgb(0x644AC9), // spec purple (derived: no cyan in spec)
        accent_soft: rgb(0x1F1F1F),
        text_primary: rgb(0x1F1F1F),
        text_secondary: rgb(0x6C664B), // spec comment
        text_muted: rgb(0x6C664B),
        text_dim: rgb(0x6C664B),
        gray: rgb(0x6C664B),
        // bg-dark (#CECCC0) is the plate under the canvas, 1.56:1 on it, so
        // as a foreground it was invisible. The tiers derive from it toward
        // the ink instead (#2492).
        gray_mid: rgb(0xA3A097),    // 2.52:1
        gray_detail: rgb(0x88867F), // 3.51:1
        gray_dim: rgb(0xA3A097),
        gray_dark: rgb(0xCECCC0), // spec bg-dark
        gray_base: rgb(0xCECCC0),
        gray_light: rgb(0x1F1F1F),
        gray_soft: rgb(0x6C664B),
        gray_muted: rgb(0x6C664B),
        success: rgb(0x14710A),
        analytics_green: rgb(0x14710A),
        green_check: rgb(0x14710A),
        error: rgb(0xCB3A2A),
        error_soft: rgb(0xCB3A2A),
        error_faded: rgb(0xCB3A2A),
        warning: rgb(0xA34D14), // derived: spec orange as amber
        warning_muted: rgb(0xA34D14),
        amber_muted: rgb(0xA34D14),
        teal_vivid: rgb(0x644AC9),
        teal_bright: rgb(0x644AC9),
        teal_muted: rgb(0x644AC9),
        teal_calm: rgb(0x644AC9),
        blue_slate: rgb(0x6C664B),
        blue_steel: rgb(0x644AC9),
        blue_link: rgb(0x644AC9),
        blue_sky: rgb(0x644AC9),
        blue_soft: rgb(0x644AC9),
        blue_vivid: rgb(0x644AC9),
        blue_code: rgb(0x644AC9),
        selection_bg: rgb(0xDEDCCF),  // spec bg-light
        surface_panel: rgb(0xFFFBEB), // spec bg
        surface_qr: rgb(0xEFEDDC),    // spec floating
        surface_code: rgb(0xDEDCCF),
        surface_code_alt: rgb(0xCECCC0),
        ink: rgb(0x1F1F1F),
        purple_soft: rgb(0x644AC9),
        border_modal: rgb(0x644AC9),
        background: Some(rgb(0xFFFBEB)), // spec bg
        ansi: AnsiColors {
            accent: 130,
            accent_teal: 62,
            accent_soft: 235,
            text_primary: 235,
            text_secondary: 59,
            text_muted: 59,
            text_dim: 59,
            gray: 59,
            gray_mid: 247,
            gray_detail: 102,
            gray_dim: 247,
            gray_dark: 187,
            gray_base: 187,
            gray_light: 235,
            gray_soft: 59,
            gray_muted: 59,
            success: 22,
            analytics_green: 22,
            green_check: 22,
            error: 166,
            error_soft: 166,
            error_faded: 166,
            warning: 130,
            warning_muted: 130,
            amber_muted: 130,
            teal_vivid: 62,
            teal_bright: 62,
            teal_muted: 62,
            teal_calm: 62,
            blue_slate: 59,
            blue_steel: 62,
            blue_link: 62,
            blue_sky: 62,
            blue_soft: 62,
            blue_vivid: 62,
            blue_code: 62,
            selection_bg: 188,
            surface_panel: 230,
            surface_qr: 230,
            surface_code: 188,
            surface_code_alt: 187,
            ink: 235,
            purple_soft: 62,
            border_modal: 62,
            background: Some(230),
        },
    },
};

// ── Monokai classic (dark) ──────────────────────────────────────────────────
// tomasr/molokai canonical: bg #272822 · fg #F8F8F2 · pink #F92672 ·
// green #A6E22E · orange #FD971F · cyan #66D9EF · yellow #E6DB74 ·
// purple #AE81FF · comment #75715E · selection #49483E
pub static MONOKAI: Theme = Theme {
    name: "monokai",
    colors: ThemeColors {
        accent: rgb(0xFD971F),      // orange
        accent_teal: rgb(0x66D9EF), // cyan
        accent_soft: rgb(0xF8F8F2),
        text_primary: rgb(0xF8F8F2),
        text_secondary: rgb(0x75715E),
        text_muted: rgb(0x75715E),
        text_dim: rgb(0x75715E),
        gray: rgb(0x75715E),
        gray_mid: rgb(0x49483E),
        gray_detail: rgb(0x49483E),
        gray_dim: rgb(0x49483E),
        gray_dark: rgb(0x49483E),
        gray_base: rgb(0x272822),
        gray_light: rgb(0xF8F8F2),
        gray_soft: rgb(0x75715E),
        gray_muted: rgb(0x75715E),
        success: rgb(0xA6E22E),
        analytics_green: rgb(0xA6E22E),
        green_check: rgb(0xA6E22E),
        error: rgb(0xF92672), // pink
        error_soft: rgb(0xF92672),
        error_faded: rgb(0xF92672),
        warning: rgb(0xE6DB74), // yellow
        warning_muted: rgb(0xE6DB74),
        amber_muted: rgb(0xFD971F),
        teal_vivid: rgb(0x66D9EF),
        teal_bright: rgb(0x66D9EF),
        teal_muted: rgb(0x66D9EF),
        teal_calm: rgb(0x66D9EF),
        blue_slate: rgb(0x75715E),
        blue_steel: rgb(0x66D9EF),
        blue_link: rgb(0x66D9EF),
        blue_sky: rgb(0x66D9EF),
        blue_soft: rgb(0x66D9EF),
        blue_vivid: rgb(0xAE81FF), // purple
        blue_code: rgb(0xAE81FF),
        selection_bg: rgb(0x49483E),
        surface_panel: rgb(0x272822),
        surface_qr: rgb(0x272822),
        surface_code: rgb(0x3E3D32),
        surface_code_alt: rgb(0x3E3D32),
        ink: rgb(0x272822),
        purple_soft: rgb(0xAE81FF),
        border_modal: rgb(0xAE81FF),
        background: Some(rgb(0x272822)), // classic bg
        ansi: AnsiColors {
            accent: 208,
            accent_teal: 81,
            accent_soft: 231,
            text_primary: 231,
            text_secondary: 95,
            text_muted: 95,
            text_dim: 95,
            gray: 95,
            gray_mid: 59,
            gray_detail: 59,
            gray_dim: 59,
            gray_dark: 59,
            gray_base: 16,
            gray_light: 231,
            gray_soft: 95,
            gray_muted: 95,
            success: 148,
            analytics_green: 148,
            green_check: 148,
            error: 197,
            error_soft: 197,
            error_faded: 197,
            warning: 186,
            warning_muted: 186,
            amber_muted: 208,
            teal_vivid: 81,
            teal_bright: 81,
            teal_muted: 81,
            teal_calm: 81,
            blue_slate: 95,
            blue_steel: 81,
            blue_link: 81,
            blue_sky: 81,
            blue_soft: 81,
            blue_vivid: 141,
            blue_code: 141,
            selection_bg: 59,
            surface_panel: 16,
            surface_qr: 16,
            surface_code: 59,
            surface_code_alt: 59,
            ink: 16,
            purple_soft: 141,
            border_modal: 141,
            background: Some(235),
        },
    },
};

// ── Catppuccin Mocha (dark) ─────────────────────────────────────────────────
// https://github.com/catppuccin/palette — semantic roles map 1:1:
// base #1E1E2E · mantle #181825 · text #CDD6F4 · subtext0 #A6ADC8 ·
// subtext1 #BAC2DE · overlay0 #6C7086 · overlay1 #7F849C · surface0
// #313244 · surface1 #45475A · surface2 #585B70 · red #F38BA8 · green
// #A6E3A1 · yellow #F9E2AF · orange #FAB387 · mauve #CBA6F7 · blue
// #89B4FA · teal #94E2D5 · red-dim #EBA0AC
pub static CATPPUCCIN_MOCHA: Theme = Theme {
    name: "catppuccin-mocha",
    colors: ThemeColors {
        accent: rgb(0xFAB387),      // orange
        accent_teal: rgb(0x94E2D5), // teal
        accent_soft: rgb(0xCDD6F4), // text
        text_primary: rgb(0xCDD6F4),
        text_secondary: rgb(0xBAC2DE), // subtext1
        text_muted: rgb(0xA6ADC8),     // subtext0
        text_dim: rgb(0x6C7086),       // overlay0
        gray: rgb(0xA6ADC8),
        gray_mid: rgb(0x585B70), // surface2
        gray_detail: rgb(0x585B70),
        gray_dim: rgb(0x45475A),  // surface1
        gray_dark: rgb(0x313244), // surface0
        gray_base: rgb(0x181825), // mantle
        gray_light: rgb(0xCDD6F4),
        gray_soft: rgb(0x7F849C), // overlay1
        gray_muted: rgb(0x6C7086),
        success: rgb(0xA6E3A1),
        analytics_green: rgb(0xA6E3A1),
        green_check: rgb(0xA6E3A1),
        error: rgb(0xF38BA8),
        error_soft: rgb(0xF38BA8),
        error_faded: rgb(0xEBA0AC), // red-dim
        warning: rgb(0xF9E2AF),
        warning_muted: rgb(0xF9E2AF),
        amber_muted: rgb(0xFAB387),
        teal_vivid: rgb(0x94E2D5),
        teal_bright: rgb(0x94E2D5),
        teal_muted: rgb(0x94E2D5),
        teal_calm: rgb(0x94E2D5),
        blue_slate: rgb(0x6C7086),
        blue_steel: rgb(0x89B4FA),
        blue_link: rgb(0x89B4FA),
        blue_sky: rgb(0x89B4FA),
        blue_soft: rgb(0x89B4FA),
        blue_vivid: rgb(0xCBA6F7), // mauve
        blue_code: rgb(0xCBA6F7),
        selection_bg: rgb(0x45475A),  // surface1
        surface_panel: rgb(0x1E1E2E), // base
        surface_qr: rgb(0x181825),    // mantle
        surface_code: rgb(0x313244),  // surface0
        surface_code_alt: rgb(0x45475A),
        ink: rgb(0x1E1E2E),
        purple_soft: rgb(0xCBA6F7),
        border_modal: rgb(0xCBA6F7),
        background: Some(rgb(0x1E1E2E)), // base
        ansi: AnsiColors {
            accent: 216,
            accent_teal: 116,
            accent_soft: 189,
            text_primary: 189,
            text_secondary: 146,
            text_muted: 146,
            text_dim: 60,
            gray: 146,
            gray_mid: 59,
            gray_detail: 59,
            gray_dim: 59,
            gray_dark: 59,
            gray_base: 16,
            gray_light: 189,
            gray_soft: 103,
            gray_muted: 60,
            success: 151,
            analytics_green: 151,
            green_check: 151,
            error: 211,
            error_soft: 211,
            error_faded: 181,
            warning: 223,
            warning_muted: 223,
            amber_muted: 216,
            teal_vivid: 116,
            teal_bright: 116,
            teal_muted: 116,
            teal_calm: 116,
            blue_slate: 60,
            blue_steel: 111,
            blue_link: 111,
            blue_sky: 111,
            blue_soft: 111,
            blue_vivid: 183,
            blue_code: 183,
            selection_bg: 59,
            surface_panel: 16,
            surface_qr: 16,
            surface_code: 59,
            surface_code_alt: 59,
            ink: 16,
            purple_soft: 183,
            border_modal: 183,
            background: Some(235),
        },
    },
};

// ── Catppuccin Latte (light) ────────────────────────────────────────────────
// Same spec, light mode: base #EFF1F5 · mantle #E6E9EF · text #4C4F69 ·
// subtext0 #6C6F85 · subtext1 #5C5F77 · overlay0 #9CA0B0 · overlay1
// #8C8FA1 · surface0 #CCD0DA · surface1 #BCC0CC · surface2 #ACADE3 ·
// red #D20F39 · green #40A02B · yellow #DF8E1D · orange #FE640B · mauve
// #8839EF · blue #1E66F5 · teal #179299 · red 30% #E64553
pub static CATPPUCCIN_LATTE: Theme = Theme {
    name: "catppuccin-latte",
    colors: ThemeColors {
        accent: rgb(0xFE640B),      // orange
        accent_teal: rgb(0x179299), // teal
        accent_soft: rgb(0x4C4F69),
        text_primary: rgb(0x4C4F69),
        text_secondary: rgb(0x5C5F77), // subtext1
        text_muted: rgb(0x6C6F85),     // subtext0
        // overlay0 (#9CA0B0) measures 2.30:1 on base; the light floors need
        // 3.5:1 for a dim tier, so the ladder starts at overlay2 (#7C7E92,
        // 3.53:1) and darkens from there (#2492).
        text_dim: rgb(0x7C7E92), // overlay2
        gray: rgb(0x6C6F85),
        // surface2 is a plate, not text: at 1.89:1 on base it vanished as a
        // foreground (dialogs, command palette). Derived from surface2 toward
        // the text ink until it clears the 2.5:1 floor.
        gray_mid: rgb(0x8F91BE),
        gray_detail: rgb(0x7C7E92), // overlay2, 3.53:1
        gray_dim: rgb(0x9699A9),    // surface2 -> ink, 2.50:1
        gray_dark: rgb(0xCCD0DA),   // surface0
        gray_base: rgb(0xE6E9EF),   // mantle
        gray_light: rgb(0x4C4F69),
        gray_soft: rgb(0x6A6D82),  // overlay1 darkened to 4.50:1
        gray_muted: rgb(0x73758A), // 4.00:1
        success: rgb(0x40A02B),
        analytics_green: rgb(0x40A02B),
        green_check: rgb(0x40A02B),
        error: rgb(0xD20F39),
        error_soft: rgb(0xD20F39),
        error_faded: rgb(0xE64553), // red 30% lightened
        // #DF8E1D is a fill colour: 2.31:1 as text on base. Darkened along
        // the same hue until it clears the 2.5:1 floor, 2.70:1 here (#2492).
        warning: rgb(0xC98528),
        warning_muted: rgb(0xDF8E1D),
        amber_muted: rgb(0xFE640B),
        teal_vivid: rgb(0x179299),
        teal_bright: rgb(0x179299),
        teal_muted: rgb(0x179299),
        teal_calm: rgb(0x179299),
        blue_slate: rgb(0x9CA0B0),
        blue_steel: rgb(0x1E66F5),
        blue_link: rgb(0x1E66F5),
        blue_sky: rgb(0x1E66F5),
        blue_soft: rgb(0x1E66F5),
        blue_vivid: rgb(0x8839EF), // mauve
        blue_code: rgb(0x8839EF),
        selection_bg: rgb(0xBCC0CC),  // surface1
        surface_panel: rgb(0xEFF1F5), // base
        surface_qr: rgb(0xE6E9EF),    // mantle
        surface_code: rgb(0xCCD0DA),  // surface0
        surface_code_alt: rgb(0xBCC0CC),
        ink: rgb(0x4C4F69),
        purple_soft: rgb(0x8839EF),
        border_modal: rgb(0x8839EF),
        background: Some(rgb(0xEFF1F5)), // base
        ansi: AnsiColors {
            accent: 202,
            accent_teal: 30,
            accent_soft: 59,
            text_primary: 59,
            text_secondary: 60,
            text_muted: 60,
            text_dim: 102,
            gray: 60,
            gray_mid: 103,
            gray_detail: 102,
            gray_dim: 247,
            gray_dark: 188,
            gray_base: 189,
            gray_light: 59,
            gray_soft: 60,
            gray_muted: 244,
            success: 70,
            analytics_green: 70,
            green_check: 70,
            error: 161,
            error_soft: 161,
            error_faded: 167,
            warning: 172,
            warning_muted: 172,
            amber_muted: 202,
            teal_vivid: 30,
            teal_bright: 30,
            teal_muted: 30,
            teal_calm: 30,
            blue_slate: 145,
            blue_steel: 27,
            blue_link: 27,
            blue_sky: 27,
            blue_soft: 27,
            blue_vivid: 99,
            blue_code: 99,
            selection_bg: 146,
            surface_panel: 231,
            surface_qr: 189,
            surface_code: 188,
            surface_code_alt: 146,
            ink: 59,
            purple_soft: 99,
            border_modal: 99,
            background: Some(255),
        },
    },
};

/// Built-in roster in `/theme list` order. `crab-dark` (from `theme.rs`)
/// is first because it is the default and byte-identical to pre-S2
/// colors by construction. The array is `static` so the returned slice
/// is `'static` — a literal `&[...]` would be a temporary.
/// The hand-built presets, in `/theme list` order. crab-dark (from
/// `theme.rs`) is first because it is the default.
static HAND_BUILT_INS: [&Theme; 8] = [
    &super::theme::CRAB_DARK,
    &DRACULA,
    &ALUCARD,
    &MONOKAI,
    &SOLARIZED_LIGHT,
    &SOLARIZED_DARK,
    &CATPPUCCIN_MOCHA,
    &CATPPUCCIN_LATTE,
];

/// Full built-in roster: hand-built presets first, then the embedded
/// curated pack (#1461). Pack themes are built once through the existing
/// user-theme validator; see `crate::tui::theme_catalog::theme_pack` for
/// the initialization-order invariants (the validator's collision check
/// re-enters `by_name`, so `by_name` must never force this).
pub fn built_ins() -> &'static [&'static Theme] {
    static ALL: OnceLock<Vec<&'static Theme>> = OnceLock::new();
    ALL.get_or_init(|| {
        HAND_BUILT_INS
            .iter()
            .copied()
            .chain(crate::tui::theme_catalog::theme_pack::themes().iter())
            .collect()
    })
}

/// Case-insensitive preset lookup by name (what `/theme set` feeds).
/// Scans the hand-built statics, then the embedded pack through its
/// NON-FORCING view: during pack initialization the validator's collision
/// check lands here, and forcing `built_ins()` (or `themes()`) from that
/// path would re-enter the `OnceLock` initializer.
pub fn by_name(name: &str) -> Option<&'static Theme> {
    let lowered = name.to_ascii_lowercase();
    HAND_BUILT_INS
        .iter()
        .find(|t| t.name.to_ascii_lowercase() == lowered)
        .copied()
        .or_else(|| {
            crate::tui::theme_catalog::theme_pack::themes_if_built()
                .and_then(|pack| pack.iter().find(|t| t.name.to_ascii_lowercase() == lowered))
        })
}

// ── Solarized Light / Dark ──────────────────────────────────────────────────
// One 16-color spec, two modes: light = bg base3 #FDF6E3 / fg base00
// #657B83; dark = bg base03 #002B36 / fg base0 #839496. Shared accents:
// yellow #B58900 · orange #CB4B16 · red #DC322F · green #859900 ·
// cyan #2AA198 · blue #268BD2 · violet #6C71C4. base2 #EEE8D5 ·
// base01 #586E75 · base02 #073642.
pub static SOLARIZED_LIGHT: Theme = Theme {
    name: "solarized-light",
    colors: ThemeColors {
        accent: rgb(0xCB4B16),      // orange
        accent_teal: rgb(0x2AA198), // cyan
        accent_soft: rgb(0x657B83),
        text_primary: rgb(0x657B83), // base00
        // base1 (#93A1A1) is a divider tone: 2.48:1 as text on base3. Each
        // reading tier is base01 mixed toward base1 until it clears its
        // floor (#2492).
        text_secondary: rgb(0x60757A), // 4.50:1
        text_muted: rgb(0x6A7D81),     // 4.00:1
        text_dim: rgb(0x768689),       // 3.51:1
        gray: rgb(0x93A1A1),
        // base2 is the plate, not the text: at 1.14:1 it was invisible as a
        // foreground.
        gray_mid: rgb(0x94A09D), // 2.50:1
        gray_detail: rgb(0x768689),
        gray_dim: rgb(0x94A09D),
        gray_dark: rgb(0xEEE8D5), // base2
        gray_base: rgb(0xEEE8D5),
        gray_light: rgb(0x073642), // base02: darkest text on light bg
        gray_soft: rgb(0x60757A),  // base01, 4.50:1
        gray_muted: rgb(0x6A7D81), // 4.00:1
        success: rgb(0x859900),
        analytics_green: rgb(0x859900),
        green_check: rgb(0x859900),
        error: rgb(0xDC322F),
        error_soft: rgb(0xDC322F),
        error_faded: rgb(0xDC322F),
        warning: rgb(0xB58900), // yellow
        warning_muted: rgb(0xB58900),
        amber_muted: rgb(0xB58900),
        teal_vivid: rgb(0x2AA198),
        teal_bright: rgb(0x2AA198),
        teal_muted: rgb(0x2AA198),
        teal_calm: rgb(0x2AA198),
        blue_slate: rgb(0x93A1A1),
        blue_steel: rgb(0x268BD2),
        blue_link: rgb(0x268BD2),
        blue_sky: rgb(0x2AA198),
        blue_soft: rgb(0x268BD2),
        blue_vivid: rgb(0x6C71C4), // violet
        blue_code: rgb(0x6C71C4),
        selection_bg: rgb(0xEEE8D5),  // base2
        surface_panel: rgb(0xFDF6E3), // base3
        surface_qr: rgb(0xEEE8D5),
        surface_code: rgb(0xEEE8D5),
        surface_code_alt: rgb(0xEEE8D5),
        ink: rgb(0x073642),
        purple_soft: rgb(0x6C71C4),
        border_modal: rgb(0x6C71C4),
        background: Some(rgb(0xFDF6E3)), // base3
        ansi: AnsiColors {
            accent: 166,
            accent_teal: 36,
            accent_soft: 66,
            text_primary: 66,
            text_secondary: 242,
            text_muted: 66,
            text_dim: 244,
            gray: 109,
            gray_mid: 247,
            gray_detail: 244,
            gray_dim: 247,
            gray_dark: 224,
            gray_base: 224,
            gray_light: 23,
            gray_soft: 242,
            gray_muted: 66,
            success: 100,
            analytics_green: 100,
            green_check: 100,
            error: 166,
            error_soft: 166,
            error_faded: 166,
            warning: 136,
            warning_muted: 136,
            amber_muted: 136,
            teal_vivid: 36,
            teal_bright: 36,
            teal_muted: 36,
            teal_calm: 36,
            blue_slate: 109,
            blue_steel: 32,
            blue_link: 32,
            blue_sky: 36,
            blue_soft: 32,
            blue_vivid: 62,
            blue_code: 62,
            selection_bg: 224,
            surface_panel: 230,
            surface_qr: 224,
            surface_code: 224,
            surface_code_alt: 224,
            ink: 23,
            purple_soft: 62,
            border_modal: 62,
            background: Some(230),
        },
    },
};

pub static SOLARIZED_DARK: Theme = Theme {
    name: "solarized-dark",
    colors: ThemeColors {
        accent: rgb(0xCB4B16),
        accent_teal: rgb(0x2AA198),
        accent_soft: rgb(0x839496),
        text_primary: rgb(0x839496),   // base0
        text_secondary: rgb(0x657B83), // base00
        text_muted: rgb(0x586E75),     // base01
        text_dim: rgb(0x586E75),
        gray: rgb(0x586E75),
        gray_mid: rgb(0x073642), // base02
        gray_detail: rgb(0x073642),
        gray_dim: rgb(0x073642),
        gray_dark: rgb(0x073642),
        gray_base: rgb(0x002B36),  // base03
        gray_light: rgb(0xEEE8D5), // base2: brightest text on dark bg
        gray_soft: rgb(0x839496),
        gray_muted: rgb(0x657B83),
        success: rgb(0x859900),
        analytics_green: rgb(0x859900),
        green_check: rgb(0x859900),
        error: rgb(0xDC322F),
        error_soft: rgb(0xDC322F),
        error_faded: rgb(0xDC322F),
        warning: rgb(0xB58900),
        warning_muted: rgb(0xB58900),
        amber_muted: rgb(0xB58900),
        teal_vivid: rgb(0x2AA198),
        teal_bright: rgb(0x2AA198),
        teal_muted: rgb(0x2AA198),
        teal_calm: rgb(0x2AA198),
        blue_slate: rgb(0x586E75),
        blue_steel: rgb(0x268BD2),
        blue_link: rgb(0x2AA198),
        blue_sky: rgb(0x2AA198),
        blue_soft: rgb(0x268BD2),
        blue_vivid: rgb(0x6C71C4),
        blue_code: rgb(0x6C71C4),
        selection_bg: rgb(0x073642),  // base02
        surface_panel: rgb(0x002B36), // base03
        surface_qr: rgb(0x002B36),
        surface_code: rgb(0x073642),
        surface_code_alt: rgb(0x073642),
        ink: rgb(0x002B36),
        purple_soft: rgb(0x6C71C4),
        border_modal: rgb(0x6C71C4),
        background: Some(rgb(0x002B36)), // base03
        ansi: AnsiColors {
            accent: 166,
            accent_teal: 36,
            accent_soft: 102,
            text_primary: 102,
            text_secondary: 66,
            text_muted: 60,
            text_dim: 60,
            gray: 60,
            gray_mid: 23,
            gray_detail: 23,
            gray_dim: 23,
            gray_dark: 23,
            gray_base: 17,
            gray_light: 224,
            gray_soft: 102,
            gray_muted: 66,
            success: 100,
            analytics_green: 100,
            green_check: 100,
            error: 166,
            error_soft: 166,
            error_faded: 166,
            warning: 136,
            warning_muted: 136,
            amber_muted: 136,
            teal_vivid: 36,
            teal_bright: 36,
            teal_muted: 36,
            teal_calm: 36,
            blue_slate: 60,
            blue_steel: 32,
            blue_link: 36,
            blue_sky: 36,
            blue_soft: 32,
            blue_vivid: 62,
            blue_code: 62,
            selection_bg: 23,
            surface_panel: 17,
            surface_qr: 17,
            surface_code: 23,
            surface_code_alt: 23,
            ink: 17,
            purple_soft: 62,
            border_modal: 62,
            background: Some(234),
        },
    },
};
