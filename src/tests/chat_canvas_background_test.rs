//! The chat area paints the theme background instead of wiping it (#2163).

#[test]
fn the_chat_area_paints_the_canvas_after_clearing() {
    let src = include_str!("../tui/render/chat.rs");
    let clear = src
        .find("f.render_widget(Clear, area);\n    if let Some(bg) = super::theme::background()")
        .expect("chat clears and then paints the canvas");
    let paragraph = src
        .find("f.render_widget(chat, area);")
        .expect("chat paragraph drawn");
    assert!(
        clear < paragraph,
        "the canvas must be painted before the chat text"
    );
}
