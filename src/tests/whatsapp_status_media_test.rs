//! Tests for the image path of `status_update` (#1485 follow-up).
//!
//! The status arm used to be text-only: `status()` in the pinned lib can post
//! an image (`send_image`), but nothing in OpenCrabs reached it. These tests pin
//! the two halves that can be checked without a live WhatsApp client — the
//! thumbnail builder's behavior, and the wiring in the dispatch arm (via a
//! source scan, the same technique the send/reply accounting tests use).

use crate::brain::tools::whatsapp_send::{STATUS_THUMBNAIL_MAX_EDGE, make_jpeg_thumbnail};

/// Encode an RGB gradient as PNG bytes, so the decoder has real work to do.
fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, 128])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode test PNG");
    out.into_inner()
}

#[test]
fn thumbnail_is_jpeg_within_the_max_edge() {
    let png = png_bytes(640, 480);
    let thumb = make_jpeg_thumbnail(&png).expect("a PNG must produce a thumbnail");

    assert_eq!(thumb[0], 0xFF, "JPEG SOI marker, byte 0");
    assert_eq!(thumb[1], 0xD8, "JPEG SOI marker, byte 1");

    let decoded = image::load_from_memory(&thumb).expect("the thumbnail must decode");
    assert!(
        decoded.width() <= STATUS_THUMBNAIL_MAX_EDGE
            && decoded.height() <= STATUS_THUMBNAIL_MAX_EDGE,
        "thumbnail {}x{} exceeds the {}px cap",
        decoded.width(),
        decoded.height(),
        STATUS_THUMBNAIL_MAX_EDGE
    );
    // A 32px JPEG is a few hundred bytes. If it ever balloons, the thumbnail is
    // carrying the full image and the cap stopped working.
    assert!(thumb.len() < 4096, "thumbnail is {} bytes", thumb.len());
}

#[test]
fn thumbnail_rejects_non_image_bytes() {
    assert!(make_jpeg_thumbnail(b"this is not an image").is_none());
    assert!(make_jpeg_thumbnail(&[]).is_none());
    // A PDF header is a plausible mistake for `media_path` and must not pass.
    assert!(make_jpeg_thumbnail(b"%PDF-1.7\n").is_none());
}

/// Encode an RGBA gradient as PNG bytes, forcing `alpha` on every pixel.
///
/// The status path has to accept images that carry an alpha channel, and at a
/// glance it looks like it cannot: JPEG has no alpha channel, so a reader of
/// `make_jpeg_thumbnail` reasonably concludes that an RGBA PNG makes `write_to`
/// fail and the function return `None` (PR-Agent reported exactly that on
/// #1882). It does not fail. `DynamicImage::write_to` routes through
/// `ImageEncoder::make_compatible_img`, which converts `Rgba8` to `Rgb8` for
/// the JPEG encoder (`image` 0.25.10, `src/codecs/jpeg/encoder.rs`). The tests
/// below pin that, so an `image` bump cannot quietly start refusing alpha PNGs
/// and break image statuses.
fn rgba_png_bytes(width: u32, height: u32, alpha: u8) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 251) as u8, (y % 241) as u8, 128, alpha])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode test PNG");
    out.into_inner()
}

#[test]
fn thumbnail_accepts_alpha_pngs() {
    // Fully transparent is the case a "JPEG has no alpha" reading predicts
    // fails hardest; opaque and semi-transparent pin the rest of the range.
    for (label, alpha) in [
        ("fully transparent", 0u8),
        ("semi transparent", 128u8),
        ("fully opaque", 255u8),
    ] {
        let png = rgba_png_bytes(640, 480, alpha);
        let thumb = match make_jpeg_thumbnail(&png) {
            Some(t) => t,
            None => panic!("an RGBA PNG ({label}) must produce a thumbnail"),
        };

        assert_eq!(thumb[0], 0xFF, "JPEG SOI marker, byte 0 ({label})");
        assert_eq!(thumb[1], 0xD8, "JPEG SOI marker, byte 1 ({label})");

        let decoded = match image::load_from_memory(&thumb) {
            Ok(d) => d,
            Err(e) => panic!("the {label} thumbnail must decode: {e}"),
        };
        assert!(
            decoded.width() <= STATUS_THUMBNAIL_MAX_EDGE
                && decoded.height() <= STATUS_THUMBNAIL_MAX_EDGE,
            "thumbnail {}x{} exceeds the {}px cap ({label})",
            decoded.width(),
            decoded.height(),
            STATUS_THUMBNAIL_MAX_EDGE
        );
        assert!(
            thumb.len() < 4096,
            "thumbnail is {} bytes ({label})",
            thumb.len()
        );
    }
}

// ── Source-scan sentinels: the dispatch wiring ───────────────────────

fn status_arm() -> &'static str {
    const SRC: &str = include_str!("../brain/tools/whatsapp_send.rs");
    let (_, rest) = SRC
        .split_once("\"status_update\" =>")
        .expect("status_update arm not found");
    let (arm, _) = rest
        .split_once("\"list_newsletters\" =>")
        .expect("list_newsletters anchor not found");
    arm
}

#[test]
fn status_arm_prepares_the_payload_before_charging() {
    let arm = status_arm();
    assert!(
        arm.contains("make_jpeg_thumbnail(&bytes)"),
        "status arm does not build the JPEG thumbnail an image status requires"
    );
    assert!(
        arm.contains("wacore::download::MediaType::Image"),
        "status arm does not upload the image"
    );
    // #2177: the payload is built once, before any slot is charged, so a bad
    // image or a failed upload never spends budget.
    assert!(
        arm.contains("StatusPayload::Image {"),
        "status arm does not prepare the image payload it fans out"
    );
}

#[test]
fn status_arm_fans_out_per_recipient() {
    let arm = status_arm();
    assert!(
        arm.contains("fan_out_status("),
        "status arm does not fan out per recipient, so one send still covers the whole list"
    );
    assert!(
        !arm.contains("sleep(pace)"),
        "status arm paces outside a send: the delay must sit between two sends (#2177)"
    );
}

#[test]
fn the_status_sink_keeps_both_send_paths() {
    // #2177 moved the lib calls into the sink with the fan-out, so both paths
    // are asserted against the whole module rather than the dispatch arm.
    const SRC: &str = include_str!("../brain/tools/whatsapp_send.rs");
    assert!(
        SRC.contains(".send_image("),
        "the image status path disappeared; status_update must still do both"
    );
    assert!(
        SRC.contains(".send_text("),
        "the text status path disappeared; status_update must still do both"
    );
    assert!(
        SRC.contains("STATUS_BACKGROUND_ARGB"),
        "text status lost its background colour"
    );
    assert!(
        SRC.contains("FontType::SYSTEM"),
        "text status lost its font"
    );
}

#[test]
fn status_arm_refuses_a_non_image_media_path() {
    let arm = status_arm();
    assert!(
        arm.contains("mime.starts_with(\"image/\")"),
        "a document or audio path would be uploaded as an image status"
    );
    assert!(
        arm.contains("could not be decoded as an image"),
        "undecodable media is not refused"
    );
}

#[test]
fn status_arm_refuses_an_empty_status() {
    let arm = status_arm();
    assert!(
        arm.contains("text.is_none() && media_path.is_none()"),
        "a status with neither text nor media would be posted empty"
    );
}

#[test]
fn schema_advertises_status_media_path() {
    const SRC: &str = include_str!("../brain/tools/whatsapp_send.rs");
    assert!(
        SRC.contains("For status_update: a local image file to post as an image status"),
        "media_path's description does not mention status_update, so an agent cannot discover the image path"
    );
}
