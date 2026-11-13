//! The shared modal owns the overlay; its consumers own only their content.

#[test]
fn theme_consumes_the_shared_modal_shell() {
    let theme = include_str!("../gui/themes_view.rs");
    let shell = include_str!("../gui/modal_shell.rs");
    let modal = theme
        .split("pub fn modal(")
        .nth(1)
        .expect("Theme exposes its modal renderer");
    assert!(modal.contains("crate::modal_shell::render("));
    assert!(modal.contains("header_close: true"));
    assert!(!modal.contains(".occlude()"));
    assert!(shell.contains(".id(SharedString::from(format!(\"{prefix}-scrim\")))"));
    assert!(shell.contains(".on_click(|_ev, _window, cx| cx.stop_propagation())"));
    assert!(shell.contains(".id(SharedString::from(format!(\"{prefix}-close\")))"));
    assert!(shell.contains(".rounded(px(CARD_RADIUS))"));
    assert!(shell.contains(".bg(glass_fill(shown.panel))"));
    assert!(shell.contains(".border_b_1()"));
    assert!(shell.contains(".border_t_1()"));
}

#[test]
fn theme_transparency_reaches_config_native_alpha_and_the_whole_root() {
    let types = include_str!("../config/types.rs");
    let main = include_str!("../gui/main.rs");
    let dashboard = include_str!("../gui/dashboard_screens.rs");
    let theme = include_str!("../gui/themes_view.rs");
    assert!(types.contains("pub transparency: u8"));
    assert!(main.contains("window_background: if initial_transparency > 0"));
    assert!(main.contains("window.set_background_appearance(if transparent"));
    assert!(main.contains(".opacity(gui_theme::window_opacity(self.window_transparency))"));
    assert!(dashboard.contains("Config::write_key(\"gui\", \"transparency\""));
    assert!(dashboard.contains("themes_view::transparency_row("));
    assert!(theme.contains("pub fn transparency_row("));
    assert!(theme.contains(".id(\"theme-modal-body\")"));
    assert!(theme.contains(".overflow_y_scroll()"));
}

#[test]
fn theme_previews_do_not_recolor_their_cards() {
    let theme = include_str!("../gui/themes_view.rs");
    let card = theme
        .split("pub fn card(")
        .nth(1)
        .and_then(|tail| tail.split("pub fn swatch(").next())
        .expect("Theme exposes its card renderer");
    assert!(card.contains("let chrome = crate::palette::current();"));
    assert!(card.contains(".rounded(px(concentric_radius(6.0)))"));
    assert!(card.contains(".bg(rgb(chrome.bg))"));
    assert!(card.contains("Size::Sm, chrome.text"));
    assert!(card.contains("miniature(shown, accent)"));
    assert!(!card.contains(".bg(rgb(shown.panel))"));
}

#[test]
fn topic_prompt_offers_one_open_crabs_family_in_a_larger_card() {
    let overlays = include_str!("../gui/overlays.rs");
    assert!(overlays.contains("DEFAULT_TOPIC_ICON: &str = \"topic:crab_general\""));
    assert!(overlays.contains(".w(rems(40.0))"));
    assert!(overlays.contains(".max_h(rems(24.0))"));
    let icons = include_str!("../gui/topic_icons.rs");
    assert!(icons.contains("(\"OpenCrabs\", \"crab_\")"));
    assert!(!icons.contains("(\"Crabs\", \"crab_\")"));
    assert!(!icons.contains("(\"Pilot\", \"pilot_\")"));
    assert!(!icons.contains("pilot_crab_bugs"));
}

#[test]
fn models_use_the_shared_shell_and_one_provider_filter() {
    let view = include_str!("../gui/models_modal.rs");
    let actions = include_str!("../gui/dashboard_models.rs");
    assert!(view.contains("modal_shell::render("));
    assert!(view.contains("header_close: true"));
    assert!(view.contains("let model_rows = self.visible_model_rows();"));
    assert!(view.contains("rows: &model_rows"));
    assert!(actions.contains("rows.retain(|row| row.provider == provider)"));
    assert!(actions.contains("let visible = self.visible_model_rows();"));
}
