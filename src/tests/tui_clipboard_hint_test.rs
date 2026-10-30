//! #1816: the clipboard hint fires only for an image-only clipboard, the
//! exact shape of a screenshot copied to the clipboard. Finder and browser
//! copies carry a text flavor next to the image, so they stay quiet.

use crate::tui::app::clipboard_route::{
    clipboard_info_fingerprint, clipboard_info_has_image, clipboard_info_has_text,
    kinds_from_clipboard_info, kinds_from_mime_list, kinds_from_windows_probe,
};

/// Real `clipboard info` output for a macOS screenshot copied to the
/// clipboard: nine image classes, zero text classes.
const SCREENSHOT_INFO: &str = "«class PNGf», 22396, «class AVIF», 3219, «class 8BPS», 103000, \
GIF picture, 8071, «class jp2 », 16380, JPEG picture, 12842, TIFF picture, 1497170, \
«class BMP », 1493722, «class TPIC», 34885";

#[test]
fn a_screenshot_clipboard_is_image_only() {
    assert!(kinds_from_clipboard_info(SCREENSHOT_INFO).is_image_only());
}

#[test]
fn an_image_with_a_text_flavor_is_not_image_only() {
    // Finder file copy: file URL and text next to the icon image.
    let info = "«class PNGf», 22396, string, 42, «class utf8», 42, «class furl», 120";
    assert!(clipboard_info_has_image(info));
    assert!(clipboard_info_has_text(info));
    assert!(!kinds_from_clipboard_info(info).is_image_only());
}

#[test]
fn text_only_clipboards_never_hint() {
    assert!(clipboard_info_has_text(
        "string, 5, «class utf8», 5, «class ut16», 5"
    ));
    assert!(!clipboard_info_has_image("string, 5, «class utf8», 5"));
    assert!(!kinds_from_clipboard_info("string, 5, «class utf8», 5").is_image_only());
}

#[test]
fn an_empty_clipboard_never_hints() {
    assert!(!kinds_from_clipboard_info("").is_image_only());
    assert!(!kinds_from_clipboard_info("   \n  ").is_image_only());
}

#[test]
fn the_fingerprint_ignores_whitespace_but_not_content() {
    let a = clipboard_info_fingerprint("«class PNGf», 22396, TIFF picture, 1497170");
    let b = clipboard_info_fingerprint("«class PNGf»,  22396,  TIFF  picture,  1497170");
    let c = clipboard_info_fingerprint("«class PNGf», 99, TIFF picture, 1497170");
    assert_eq!(a, b);
    assert_ne!(a, c);
}

/// #2174: on Linux the hint comes from a MIME-type list. `wl-paste
/// --list-types` for a screenshot lists image types and no text type, which
/// is the same shape the macOS probe reports.
#[test]
fn mime_list_with_only_images_is_image_only() {
    let list = "image/png\nimage/bmp\nimage/tiff\n";
    let kinds = kinds_from_mime_list(list);
    assert!(kinds.image);
    assert!(!kinds.text);
    assert!(kinds.is_image_only());
}

/// A browser or file-manager copy carries a text flavor next to the image, so
/// the hint must stay quiet there.
#[test]
fn mime_list_with_text_and_image_is_not_image_only() {
    let kinds = kinds_from_mime_list("text/html\ntext/plain\nimage/png\n");
    assert!(kinds.image);
    assert!(kinds.text);
    assert!(!kinds.is_image_only());
}

/// X11 spells plain text `UTF8_STRING`, not `text/plain`, and that still
/// counts as text.
#[test]
fn mime_list_understands_x11_text_targets() {
    assert!(kinds_from_mime_list("UTF8_STRING\nSTRING\n").text);
    assert!(kinds_from_mime_list("text/plain;charset=utf-8\n").text);
}

/// An empty probe means nothing is classified, so no hint fires.
#[test]
fn empty_probe_is_not_image_only() {
    let kinds = kinds_from_mime_list("");
    assert!(!kinds.image);
    assert!(!kinds.text);
    assert!(!kinds.is_image_only());
}

/// The Windows probe prints `image=True text=False`; parsing is pure so it can
/// be tested on macOS.
#[test]
fn windows_probe_parses_its_flags() {
    let shot = kinds_from_windows_probe("image=True text=False\r\n");
    assert!(shot.is_image_only());

    let text_only = kinds_from_windows_probe("image=False text=True");
    assert!(!text_only.image);
    assert!(text_only.text);
    assert!(!text_only.is_image_only());

    let both = kinds_from_windows_probe("image=True text=True");
    assert!(!both.is_image_only());
}

/// The macOS string parser feeds the same decision the Linux and Windows
/// parsers do, so every platform drives the hint through one predicate.
#[test]
fn macos_info_and_kinds_agree() {
    let kinds = kinds_from_clipboard_info(SCREENSHOT_INFO);
    assert!(kinds.image && !kinds.text && kinds.is_image_only());
}
