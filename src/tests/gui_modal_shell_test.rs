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
    assert!(!modal.contains(".occlude()"));
    assert!(shell.contains(".id(SharedString::from(format!(\"{prefix}-scrim\")))"));
    assert!(shell.contains(".on_click(|_ev, _window, cx| cx.stop_propagation())"));
    assert!(shell.contains(".id(SharedString::from(format!(\"{prefix}-close\")))"));
}
