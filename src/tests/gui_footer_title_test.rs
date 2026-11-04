//! The chat screen has no header: the session name leads the footer, as in the
//! TUI footer (#2365). The other screens keep their title above the body. The
//! window has no headless harness, so these checks read source.

const GUI_MAIN: &str = include_str!("../gui/main.rs");

#[test]
fn the_header_is_left_off_the_chat_screen() {
    assert!(
        GUI_MAIN
            .contains("if self.screen != Screen::Chat {\n            right = right.child(chrome);"),
        "the header is attached only to the non-chat screens"
    );
}

#[test]
fn the_footer_identity_starts_with_the_session_name() {
    assert!(GUI_MAIN.contains("self.session_title().unwrap_or(\"chat\"),"));
    assert!(GUI_MAIN.contains("\"{} · {}\","));
}
