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
