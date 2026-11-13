//! The desktop window's theme model (#2619).
//!
//! Four themes only: `orgn`, `light`, `custom`, `auto`. These tests pin the
//! config shape, the parsing, and the pure resolution: which base a mode
//! picks, whether it follows the system, and how a custom accent lands.

use crate::config::gui_theme::{
    LIGHT, MAX_WINDOW_TRANSPARENCY, ORGN, base_for, clamp_window_transparency, contrast_ratio,
    derive, follows_system, format_accent, parse_accent, resolve, transparency_from_slider,
    window_opacity,
};
use crate::config::types::{GuiConfig, GuiTheme};

#[test]
fn the_theme_defaults_to_auto_so_a_first_install_follows_the_system() {
    let config = GuiConfig::default();
    assert_eq!(config.theme, GuiTheme::Auto);
    assert!(config.custom_accent.is_none());
    assert!(config.topic_tabs.is_none());
    assert_eq!(config.transparency, 0);
    assert!(follows_system(config.theme));
}

#[test]
fn window_transparency_is_bounded_and_convertible() {
    assert_eq!(MAX_WINDOW_TRANSPARENCY, 40);
    assert_eq!(clamp_window_transparency(0), 0);
    assert_eq!(clamp_window_transparency(25), 25);
    assert_eq!(clamp_window_transparency(u8::MAX), 40);
    assert_eq!(window_opacity(0), 1.0);
    assert_eq!(window_opacity(25), 0.75);
    assert_eq!(window_opacity(40), 0.6);
    assert_eq!(window_opacity(u8::MAX), 0.6);
}

#[test]
fn transparency_sliders_and_toml_keep_the_percentage() {
    assert_eq!(transparency_from_slider(0.0), 0);
    assert_eq!(transparency_from_slider(0.5), 20);
    assert_eq!(transparency_from_slider(1.0), 40);
    assert_eq!(transparency_from_slider(-1.0), 0);
    assert_eq!(transparency_from_slider(2.0), 40);
    assert_eq!(transparency_from_slider(f32::NAN), 0);

    let config: GuiConfig = toml::from_str("transparency = 25").unwrap();
    assert_eq!(config.transparency, 25);
    let text = toml::to_string(&config).unwrap();
    let back: GuiConfig = toml::from_str(&text).unwrap();
    assert_eq!(back.transparency, 25);
}

#[test]
fn every_theme_parses_from_its_lowercase_name() {
    for (text, expected) in [
        ("orgn", GuiTheme::Orgn),
        ("light", GuiTheme::Light),
        ("custom", GuiTheme::Custom),
        ("auto", GuiTheme::Auto),
    ] {
        let config: GuiConfig = toml::from_str(&format!("theme = \"{text}\"")).unwrap();
        assert_eq!(config.theme, expected, "theme = {text}");
    }
}

#[test]
fn an_unset_theme_section_is_auto() {
    let config: GuiConfig = toml::from_str("").unwrap();
    assert_eq!(config.theme, GuiTheme::Auto);
}

#[test]
fn the_old_per_appearance_keys_are_gone() {
    // `dark_theme` and `light_theme` named TUI presets. They are not keys any
    // more, so an old config parses with the unknown keys ignored and the
    // theme falls back to `auto`: an intentional reset, not a silent TUI
    // preset carried into the window.
    let config: GuiConfig =
        toml::from_str("dark_theme = \"dracula\"\nlight_theme = \"alucard\"").unwrap();
    assert_eq!(config.theme, GuiTheme::Auto);
    assert!(config.custom_accent.is_none());
}

#[test]
fn a_tui_catalog_name_is_not_a_gui_theme() {
    for name in ["dracula", "alucard", "solarized-light", "gui-dark", "nord"] {
        assert!(
            toml::from_str::<GuiConfig>(&format!("theme = \"{name}\"")).is_err(),
            "{name} must not parse as a GUI theme"
        );
    }
}

#[test]
fn the_theme_round_trips_through_toml() {
    for mode in [
        GuiTheme::Orgn,
        GuiTheme::Light,
        GuiTheme::Custom,
        GuiTheme::Auto,
    ] {
        let config = GuiConfig {
            theme: mode,
            ..GuiConfig::default()
        };
        let text = toml::to_string(&config).unwrap();
        let back: GuiConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.theme, mode);
    }
}

#[test]
fn accents_parse_with_or_without_a_hash() {
    assert_eq!(parse_accent("#d76414"), Some(0xd76414));
    assert_eq!(parse_accent("d76414"), Some(0xd76414));
    assert_eq!(parse_accent("  #D76414  "), Some(0xd76414));
    assert_eq!(format_accent(0xd76414), "#d76414");
}

#[test]
fn invalid_accents_are_rejected() {
    for text in [
        "", "#", "#fff", "d7641", "#d764140", "nothex", "#d7 614", "0xd76414",
    ] {
        assert_eq!(parse_accent(text), None, "{text:?} must not parse");
    }
}

#[test]
fn orgn_and_light_are_fixed_and_ignore_the_system() {
    assert_eq!(base_for(GuiTheme::Orgn, true), ORGN);
    assert_eq!(base_for(GuiTheme::Orgn, false), ORGN);
    assert_eq!(base_for(GuiTheme::Light, true), LIGHT);
    assert_eq!(base_for(GuiTheme::Light, false), LIGHT);
    assert!(!follows_system(GuiTheme::Orgn));
    assert!(!follows_system(GuiTheme::Light));
}

#[test]
fn auto_and_custom_follow_the_system() {
    assert_eq!(base_for(GuiTheme::Auto, true), ORGN);
    assert_eq!(base_for(GuiTheme::Auto, false), LIGHT);
    assert_eq!(base_for(GuiTheme::Custom, true), ORGN);
    assert_eq!(base_for(GuiTheme::Custom, false), LIGHT);
    assert!(follows_system(GuiTheme::Auto));
    assert!(follows_system(GuiTheme::Custom));
}

#[test]
fn an_accent_only_paints_the_custom_theme() {
    let custom = resolve(GuiTheme::Custom, true, Some("#00ff88"), None);
    assert_eq!(custom.accent, 0x00ff88);
    assert_eq!(custom.bg, ORGN.bg, "custom keeps the system base");
    for mode in [GuiTheme::Orgn, GuiTheme::Light, GuiTheme::Auto] {
        let palette = resolve(mode, true, Some("#00ff88"), None);
        assert_eq!(palette.accent, base_for(mode, true).accent);
    }
}

#[test]
fn the_focus_ring_and_selection_follow_the_accent() {
    let painted = resolve(GuiTheme::Custom, true, Some("#00ff88"), None);
    assert_ne!(painted.focus, ORGN.focus);
    assert_ne!(painted.selection, ORGN.selection);
    // The wash is over the background, so it stays on the canvas rather than
    // becoming a solid block of the accent.
    assert!(contrast_ratio(painted.selection, ORGN.bg) < 2.0);
    assert_eq!(
        resolve(GuiTheme::Custom, true, Some("#d76414"), None).accent,
        ORGN.accent
    );
}

#[test]
fn an_unparsable_custom_accent_leaves_the_derived_base_alone() {
    // `custom` derives from the system's own background when no base is
    // picked, so an accent that does not parse leaves exactly that (#2746).
    assert_eq!(
        resolve(GuiTheme::Custom, true, Some("not-a-colour"), None),
        derive(ORGN.bg, None)
    );
    assert_eq!(
        resolve(GuiTheme::Custom, true, None, None),
        derive(ORGN.bg, None)
    );
}

#[test]
fn the_light_palette_keeps_its_text_readable() {
    // 4.5:1 is the WCAG floor for body text; the faint tier is decoration and
    // only needs to clear 3:1.
    for (role, colour) in [
        ("text", LIGHT.text),
        ("text_strong", LIGHT.text_strong),
        ("text_normal", LIGHT.text_normal),
        ("text_muted", LIGHT.text_muted),
        ("text_title", LIGHT.text_title),
        ("text_hint", LIGHT.text_hint),
    ] {
        assert!(
            contrast_ratio(colour, LIGHT.bg) >= 4.5,
            "light {role} is {:.2}:1 on the canvas",
            contrast_ratio(colour, LIGHT.bg)
        );
    }
    assert!(contrast_ratio(LIGHT.text_faint, LIGHT.bg) >= 3.0);
    for (role, colour) in [
        ("green", LIGHT.green),
        ("blue", LIGHT.blue),
        ("amber", LIGHT.amber),
        ("accent", LIGHT.accent),
        ("teal", LIGHT.teal),
        ("red", LIGHT.red),
        ("warn", LIGHT.warn),
    ] {
        assert!(
            contrast_ratio(colour, LIGHT.bg) >= 3.0,
            "light {role} is {:.2}:1 on the canvas",
            contrast_ratio(colour, LIGHT.bg)
        );
    }
}

#[test]
fn every_role_is_readable_on_its_own_panel() {
    // Text sits on panels as often as on the canvas, so certify both.
    for bg in [LIGHT.bg, LIGHT.panel, LIGHT.panel_hi] {
        assert!(contrast_ratio(LIGHT.text, bg) >= 4.5);
        assert!(contrast_ratio(LIGHT.text_muted, bg) >= 4.5);
    }
}
