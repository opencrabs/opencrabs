//! The WhatsApp pairing popup has to reserve every row and column it draws
//! (#2246). The old formula was one row short of its own content on any
//! terminal, so the `Waiting for scan...` footer never rendered, and on a
//! short terminal the code itself was clipped with no message. The size
//! helper is asserted here; the render path only consumes it.

use crate::tui::onboarding_render::whatsapp_qr_popup_size;

#[test]
fn popup_height_covers_all_content_rows_plus_border() {
    // instruction, blank, `qr_h` code rows, blank, footer, then the border.
    assert_eq!(whatsapp_qr_popup_size(41, 41).1, 41 + 6);
    assert_eq!(whatsapp_qr_popup_size(41, 10).1, 10 + 6);
}

#[test]
fn popup_width_covers_the_widest_line_plus_border() {
    // A wide code dominates, its two padding columns included.
    assert_eq!(whatsapp_qr_popup_size(60, 10).0, 60 + 6);
    // A narrow code leaves the instruction line as the widest content.
    let (narrow_w, _) = whatsapp_qr_popup_size(10, 10);
    assert!(
        narrow_w >= 48 + 2,
        "instruction line plus border must fit, got {narrow_w} columns"
    );
}

#[test]
fn popup_size_is_zero_safe() {
    // No code yet: the instruction line still sets the width, and the border
    // plus four chrome rows still set the height.
    assert_eq!(whatsapp_qr_popup_size(0, 0), (50, 6));
}
