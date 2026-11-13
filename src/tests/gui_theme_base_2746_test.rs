//! The theme's base, and the palette derived from it (#2746).
//!
//! The Theme screen used to offer an accent painted on a fixed base: `custom`
//! meant "your accent on whatever base the system asked for", so there was no
//! way to pick a background at all. It now offers the base itself, and every
//! surface, border and text tone is derived from it.
//!
//! That change is what makes the contrast floor testable. A hand-written set
//! of values per theme drifts, and the check has to follow it around; a
//! derivation is one function, so the check is a sweep over every base the
//! picker can produce.

use crate::config::gui_theme::{
    BASES, FAINT_FLOOR, LIGHT, MIN_ACCENT_CONTRAST, ORGN, TEXT_FLOOR, contrast_ratio, derive,
    format_accent, parse_accent, relative_luminance, resolve,
};
use crate::config::types::{GuiConfig, GuiTheme};
use crate::tests::gui_dashboard_source::DASHBOARD;

/// Every base the derivation can be handed, coarsely: six steps per channel.
/// The floor has to hold on all of them, not only on the six the picker shows.
fn cube() -> Vec<u32> {
    let mut bases = Vec::new();
    for r in [0x00u32, 0x33, 0x66, 0x99, 0xcc, 0xff] {
        for g in [0x00u32, 0x33, 0x66, 0x99, 0xcc, 0xff] {
            for b in [0x00u32, 0x33, 0x66, 0x99, 0xcc, 0xff] {
                bases.push((r << 16) | (g << 8) | b);
            }
        }
    }
    bases
}

/// The promise the derivation makes: body text reads on the canvas and on a
/// panel, the faint tier reads on a panel, and the accent stays visible. It
/// holds for any base, which is why a picker can offer one at all.
#[test]
fn the_derivation_clears_the_contrast_floor_on_every_base() {
    for bg in cube() {
        let palette = derive(bg, None);
        for (role, ink) in [
            ("text", palette.text),
            ("text_strong", palette.text_strong),
            ("text_normal", palette.text_normal),
            ("text_muted", palette.text_muted),
            ("text_title", palette.text_title),
            ("text_hint", palette.text_hint),
        ] {
            assert!(
                contrast_ratio(ink, palette.bg) >= TEXT_FLOOR,
                "{role} is {:.2}:1 on the canvas 0x{bg:06x}",
                contrast_ratio(ink, palette.bg)
            );
            assert!(
                contrast_ratio(ink, palette.panel) >= TEXT_FLOOR - 0.05,
                "{role} is {:.2}:1 on the panel over 0x{bg:06x}",
                contrast_ratio(ink, palette.panel)
            );
        }
        assert!(
            contrast_ratio(palette.text_faint, palette.panel) >= FAINT_FLOOR,
            "faint text is {:.2}:1 on the panel over 0x{bg:06x}",
            contrast_ratio(palette.text_faint, palette.panel)
        );
        assert!(
            contrast_ratio(palette.accent, palette.bg) >= MIN_ACCENT_CONTRAST,
            "the accent is {:.2}:1 on 0x{bg:06x}",
            contrast_ratio(palette.accent, palette.bg)
        );
        // A message role is text too, so it is held to the text floor.
        for (role, ink) in [
            ("green", palette.green),
            ("blue", palette.blue),
            ("amber", palette.amber),
            ("teal", palette.teal),
            ("red", palette.red),
        ] {
            assert!(
                contrast_ratio(ink, palette.panel) >= TEXT_FLOOR - 0.05,
                "{role} is {:.2}:1 on the panel over 0x{bg:06x}",
                contrast_ratio(ink, palette.panel)
            );
        }
    }
}

/// The six bases the picker offers are backgrounds only. Everything else steps
/// off them, so a card, a border and the canvas cannot collapse into one
/// another: the panel is lighter than the canvas, and the track is a raised
/// groove on a dark base and a sunken one on a light base.
#[test]
fn every_base_offers_surfaces_that_step_off_the_canvas() {
    for (bg, name) in BASES {
        let palette = derive(bg, None);
        assert_eq!(palette.bg, bg, "{name} paints the base it names");
        assert!(
            relative_luminance(palette.panel) > relative_luminance(palette.bg),
            "{name}: the panel sits above the canvas"
        );
        assert!(
            relative_luminance(palette.panel_hi) > relative_luminance(palette.bg),
            "{name}: the hover surface sits above the canvas"
        );
        let dark = relative_luminance(bg) < 0.179;
        assert_eq!(
            relative_luminance(palette.track) > relative_luminance(palette.bg),
            dark,
            "{name}: the track is raised on a dark base and sunken on a light one"
        );
        // The text ladder is monotone: the strong tier never reads weaker than
        // the body tier, and the faint tier never out-reads it. Distance from
        // the panel is the measure, not brightness, because a light base
        // climbs toward black instead of white.
        let read = |ink: u32| contrast_ratio(ink, palette.panel);
        assert!(
            read(palette.text_strong) >= read(palette.text),
            "{name}: text_strong is the strongest tier"
        );
        assert!(
            read(palette.text) >= read(palette.text_faint),
            "{name}: faint is the weakest tier"
        );
    }
}

/// The pair is (base, accent), and the accent is the independent half: two
/// picks on one canvas keep the canvas and move the accent, the focus ring and
/// the selection wash together.
#[test]
fn the_base_and_the_accent_are_independent_inputs() {
    let base = 0x14181f;
    let warm = derive(base, Some(0xd76414));
    let cool = derive(base, Some(0x2f8fbf));
    assert_eq!(warm.bg, cool.bg, "the canvas is the base, not the accent");
    assert_eq!(warm.panel, cool.panel, "surfaces do not follow the accent");
    assert_eq!(warm.text, cool.text, "text does not follow the accent");
    assert_ne!(warm.accent, cool.accent);
    assert_ne!(warm.focus, cool.focus);
    assert_ne!(warm.selection, cool.selection);
    for palette in [warm, cool] {
        assert!(
            contrast_ratio(palette.selection, palette.bg) < 2.0,
            "the selection is a wash over the canvas, not a block of the accent"
        );
    }
}

/// A saved base wins over the system's appearance: that is the whole point of
/// choosing one. With nothing saved, `custom` starts from the system's own
/// background, so an install that never opens the picker looks unchanged.
#[test]
fn a_chosen_base_overrides_the_system_and_unset_keeps_it() {
    let light = 0xf7f5f2;
    let dark = 0x101010;
    assert_eq!(
        resolve(GuiTheme::Custom, true, None, Some(light)).bg,
        light,
        "a light base stays light on a dark system"
    );
    assert_eq!(
        resolve(GuiTheme::Custom, false, None, Some(dark)).bg,
        dark,
        "a dark base stays dark on a light system"
    );
    assert_eq!(
        resolve(GuiTheme::Custom, true, None, None).bg,
        ORGN.bg,
        "with nothing saved, custom uses the system's dark base"
    );
    assert_eq!(
        resolve(GuiTheme::Custom, false, None, None).bg,
        LIGHT.bg,
        "with nothing saved, custom uses the system's light base"
    );
}

/// The four modes still render, and the two fixed ones are untouched by the
/// derivation: `orgn` and `light` are the palettes the window ships.
#[test]
fn the_four_modes_still_render() {
    for mode in [
        GuiTheme::Orgn,
        GuiTheme::Light,
        GuiTheme::Custom,
        GuiTheme::Auto,
    ] {
        let palette = resolve(mode, true, Some("#d76414"), Some(0x14181f));
        assert_ne!(palette.bg, palette.text, "{mode:?} paints readable text");
        assert!(
            contrast_ratio(palette.text, palette.bg) >= TEXT_FLOOR,
            "{mode:?} text is {:.2}:1",
            contrast_ratio(palette.text, palette.bg)
        );
    }
    assert_eq!(resolve(GuiTheme::Orgn, true, None, None), ORGN);
    assert_eq!(resolve(GuiTheme::Light, true, None, None), LIGHT);
    assert_eq!(resolve(GuiTheme::Auto, true, None, None), ORGN);
    assert_eq!(resolve(GuiTheme::Auto, false, None, None), LIGHT);
}

/// `[gui] base` is a hex like the accent, so the picker and the config speak
/// the same language, and an old config without the key keeps parsing.
#[test]
fn the_base_is_stored_as_a_hex_and_an_old_config_still_parses() {
    let config: GuiConfig = toml::from_str("base = \"#1a2430\"").unwrap();
    assert_eq!(
        config.base.as_deref().and_then(parse_accent),
        Some(0x1a2430)
    );
    let old: GuiConfig = toml::from_str("theme = \"custom\"").unwrap();
    assert!(old.base.is_none(), "a config written before #2746 loads");
    let text = format_accent(0x1a2430);
    assert_eq!(parse_accent(&text), Some(0x1a2430), "the hex round trips");
}

/// The picker is wired, and the card does not ask the system for its own
/// colours any more: the dashboard resolves each card's palette from the base
/// it holds, which is what makes a base change visible in the miniatures.
///
/// A source scan, because the rule is about wiring: the modal needs a window
/// and a click to exercise.
#[test]
fn the_theme_screen_offers_the_base_and_the_card_does_not_ask_the_system() {
    let view = include_str!("../gui/themes_view.rs");
    assert!(
        !view.contains("system_is_dark"),
        "the card reads the palette it is handed, not the system's appearance"
    );
    assert!(
        !view.contains("base_for"),
        "the base comes from the window, not from a mode lookup"
    );
    assert!(view.contains("pub fn base_row("), "the modal offers a base");
    assert!(
        view.contains("pub fn swatch("),
        "a base is picked from a swatch"
    );
    assert!(
        DASHBOARD.contains("themes_view::swatch(bg, name, selected, shown)"),
        "the dashboard mints the swatch rows"
    );
    assert!(
        DASHBOARD.contains("this.pick_base(bg, cx);"),
        "a swatch click saves the base"
    );
    assert!(
        DASHBOARD.contains(&["Config::write_key_string(\"gui\", \"base\", ", "&base)"].concat()),
        "the pick is stored as [gui] base"
    );
    assert!(
        DASHBOARD.contains("gui_theme::resolve(mode, dark, card_accent.as_deref(), base)"),
        "each card is resolved from the base in force"
    );
}
