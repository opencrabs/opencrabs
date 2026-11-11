//! The desktop theme modal (#2619): what it offers, how it is reached, who
//! owns the keyboard, and how it leaves. The window has no headless harness, so
//! the wiring is checked against the handler source.

use crate::tui::render::theme_names;
use crate::ui_core::help_keys;

const GUI_MAIN: &str = include_str!("../gui/main.rs");

/// Source with every run of whitespace folded to one space, so an assertion
/// pins a statement rather than the indentation `rustfmt` happened to pick.
fn flat(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The top of the key handler, from its opening to the Seeker card's block:
/// whatever answers here answers before every other reader in the window.
fn on_key_head() -> &'static str {
    GUI_MAIN
        .split("fn on_key(")
        .nth(1)
        .expect("on_key is defined")
        .split("// The Seeker card owns every key")
        .next()
        .expect("the modal gate sits ahead of the Seeker card")
}

/// The modal's own key handler, up to the next method on the window.
fn modal_key() -> &'static str {
    GUI_MAIN
        .split("fn theme_modal_key(")
        .nth(1)
        .expect("theme_modal_key is defined")
        .split("\n    fn ")
        .next()
        .expect("the handler has a body")
}

/// The terminal keeps its own catalog: the window offers four choices, the
/// TUI still lists every preset corpus name. The revamp must not cost the
/// terminal its themes.
#[test]
fn the_terminal_keeps_its_preset_catalog() {
    let names = theme_names();
    assert!(names.contains(&"alucard"), "the light default is listed");
    assert!(names.contains(&"dracula"), "a dark preset is listed");
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "no theme is listed twice");
}

#[test]
fn ctrl_shift_t_is_listed_in_the_help_table_for_the_window() {
    let row = help_keys::gui_rows()
        .find(|r| r.gui == Some("ctrl-shift-t"))
        .expect("the help screen lists ctrl+shift+t for the window");
    assert!(row.description.to_lowercase().contains("theme"));
}

/// The modal is an overlay, not a screen: the window keeps showing whatever it
/// showed, so the choice is made against real content. A screen variant would
/// swap the body out and leave nothing behind the cards.
#[test]
fn the_modal_is_an_overlay_and_not_a_screen() {
    assert!(
        !GUI_MAIN.contains("Screen::Themes"),
        "the themes screen is gone: the modal opens over the current screen"
    );
    assert!(
        GUI_MAIN.contains("themes_open: bool"),
        "the modal keeps its own open flag"
    );
    assert!(
        GUI_MAIN.contains("return vec![OverlayId::Themes];"),
        "the overlay is in the keyboard stack, so it outranks every screen"
    );
}

/// While the modal is open it owns every key, on every screen: the gate sits at
/// the top of the handler, ahead of the Seeker card and the split chords, so
/// nothing behind the scrim answers an arrow or an Enter.
#[test]
fn the_modal_owns_every_key_on_every_screen() {
    let head = flat(on_key_head());
    assert!(
        head.contains("if self.themes_open {"),
        "the gate opens the handler"
    );
    assert!(
        head.contains("self.theme_modal_key(ks, cx);"),
        "every key the modal does not use itself is swallowed"
    );
    assert!(
        head.contains("if ks.modifiers.control && ks.modifiers.shift && ks.key == \"t\" {"),
        "the chord closes the modal it opened, the terminal's own toggle"
    );
}

/// Esc closes the modal and nothing else: the pick was already saved, so there
/// is no draft to restore. It is answered by the modal's gate, which is why a
/// busy turn cannot hold the modal open.
///
/// The font list is the one surface nested inside the modal, so Esc has to
/// close that first and leave the modal standing (#2786). Closing the modal
/// out from under the list would cost the reader the theme they were browsing.
#[test]
fn esc_closes_the_modal_without_restoring_a_draft() {
    let keys = flat(modal_key());
    assert!(
        keys.contains("\"escape\" => {"),
        "the modal answers Escape itself"
    );
    let font_list = keys
        .find("if self.fonts_open {")
        .expect("Escape closes the font list before the modal (#2786)");
    let modal = keys
        .find("self.close_themes(cx)")
        .expect("Esc on the modal closes it: the choice is already in the config");
    assert!(
        font_list < modal,
        "the font list is the innermost surface, so it closes first"
    );
    assert!(
        !GUI_MAIN.contains(".id(\"themes-keep\")"),
        "there is no Keep button: a pick saves at once"
    );
}

/// One key writes the choice, and the four names are the only values it can
/// write. The old `dark_theme`/`light_theme` pair belonged to the deleted
/// sidebar and must not come back.
#[test]
fn a_pick_writes_the_theme_key_and_only_four_names() {
    assert!(
        GUI_MAIN.contains(&["Config::write_key_string(\"gui\", \"theme\", ", "name)"].concat())
    );
    for name in ["\"orgn\"", "\"light\"", "\"custom\"", "\"auto\""] {
        assert!(GUI_MAIN.contains(name), "the modal offers {name}");
    }
    for gone in ["\"dark_theme\"", "\"light_theme\""] {
        assert!(!GUI_MAIN.contains(gone), "the old key {gone} is gone");
    }
}

#[test]
fn the_modal_is_reachable_by_its_chord() {
    assert!(GUI_MAIN.contains("ev.keystroke.modifiers.shift && ev.keystroke.key == \"t\""));
    assert!(GUI_MAIN.contains("self.toggle_themes(cx);"));
}

/// The wheel is the custom accent's control, and it is recorded where it is
/// painted: a press is turned into a hue from that box, never from a guess.
#[test]
fn the_accent_wheel_records_its_own_box() {
    let view = flat(include_str!("../gui/themes_view.rs"));
    let records = "recorded .borrow_mut() .replace([x0, y0, x0 + width, y0 + height])";
    assert!(view.contains(records), "the wheel records its painted box");
    assert!(
        GUI_MAIN.contains("this.themes_press_at(x, y)"),
        "a press is turned into a hue through that box"
    );
    assert!(
        GUI_MAIN.contains("accent::wheel_hit(bounds, x, y)"),
        "the box to hue maths is the tested one, not a copy"
    );
    // The tracks record their boxes the same way the wheel does, so a click on
    // one is a value, not a guess at where it sits.
    assert!(
        view.matches(".borrow_mut() .replace([x0, y0, x0 + width, y0 + height])")
            .count()
            >= 2,
        "both tracks record their painted boxes"
    );
}

/// The arrows walk the four cards and the window repaints in each, so the
/// choice is made by looking. Only Enter saves, because a key that wrote on
/// every press would rewrite config.toml four times for one glance.
#[test]
fn the_arrows_preview_a_theme_and_only_enter_saves_it() {
    let keys = modal_key();
    assert_eq!(
        keys.matches("pick_theme").count(),
        1,
        "only Enter writes the theme"
    );
    assert_eq!(
        keys.matches("preview_card").count(),
        2,
        "both arrow directions repaint without saving"
    );
}

/// Closing puts the saved theme back: an arrow that previewed a card and was
/// never confirmed must not leave its palette on screen.
#[test]
fn closing_returns_to_the_saved_palette() {
    assert!(
        GUI_MAIN.contains("palette::adopt(palette::saved());"),
        "closing re-adopts what the config holds"
    );
}
