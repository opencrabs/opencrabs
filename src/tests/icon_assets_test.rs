//! The macOS app icon ships inside a bundle as an `.icns` container. That
//! container is a flat list of typed members, one per pixel size. If the ladder
//! stops short, Finder and the Dock upscale a small member and the icon looks
//! soft on every Retina display.
//!
//! These tests read the shipped asset and assert the ladder is complete, so a
//! future asset swap cannot silently regress it.

use std::path::Path;

fn icns() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/icon.icns");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every member is an 8-byte header (4-char type, big-endian length) followed
/// by the payload. The container itself opens with the same shape: `icns` plus
/// the total byte length.
fn members(bytes: &[u8]) -> Vec<(String, usize)> {
    assert!(bytes.len() > 8, "icns container is truncated");
    assert_eq!(&bytes[0..4], b"icns", "missing icns magic");

    let total = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    assert_eq!(
        total,
        bytes.len(),
        "declared container length {total} does not match file size {}",
        bytes.len()
    );

    let mut out = Vec::new();
    let mut off = 8;
    while off + 8 <= bytes.len() {
        let kind = String::from_utf8_lossy(&bytes[off..off + 4]).to_string();
        let len = u32::from_be_bytes([
            bytes[off + 4],
            bytes[off + 5],
            bytes[off + 6],
            bytes[off + 7],
        ]) as usize;
        assert!(len >= 8, "member {kind} declares length {len}");
        assert!(off + len <= bytes.len(), "member {kind} runs past the file");
        out.push((kind, len));
        off += len;
    }
    assert_eq!(off, bytes.len(), "trailing bytes after the last member");
    out
}

fn kinds() -> Vec<String> {
    members(&icns()).into_iter().map(|(kind, _)| kind).collect()
}

/// `iconutil` emits the ten members a full ladder needs. Without `ic10` the
/// largest member is 512 and a Retina Dock upscales it.
#[test]
fn the_icon_ladder_covers_every_size() {
    let kinds = kinds();
    for (kind, size) in [
        ("ic04", 16),
        ("ic05", 32),
        ("ic07", 128),
        ("ic08", 256),
        ("ic09", 512),
        ("ic10", 1024),
        ("ic11", 32),
        ("ic12", 64),
        ("ic13", 256),
        ("ic14", 512),
    ] {
        assert!(
            kinds.iter().any(|k| k == kind),
            "missing {kind} ({size}px); ladder is {kinds:?}"
        );
    }
}

/// The 1024 member is the one that matters on a Retina display, so it gets its
/// own assertion and its own failure message.
#[test]
fn the_largest_member_is_1024() {
    assert!(
        kinds().iter().any(|k| k == "ic10"),
        "no 1024x1024 member; the largest size is 512 and every Retina display upscales it"
    );
}

#[test]
fn the_container_length_matches_the_file() {
    let bytes = icns();
    let total = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    assert_eq!(
        total,
        bytes.len(),
        "declared container length disagrees with the file on disk"
    );
}

/// The payload of one member, without its 8-byte header. Large members (`ic09`
/// and up) are PNG streams, which is what makes the tile shape checkable.
fn member_payload(kind: &[u8]) -> Vec<u8> {
    let bytes = icns();
    let mut off = 8;
    while off + 8 <= bytes.len() {
        let len = u32::from_be_bytes([
            bytes[off + 4],
            bytes[off + 5],
            bytes[off + 6],
            bytes[off + 7],
        ]) as usize;
        if &bytes[off..off + 4] == kind {
            return bytes[off + 8..off + len].to_vec();
        }
        off += len;
    }
    panic!("no {} member", String::from_utf8_lossy(kind));
}

/// macOS draws an app icon as a rounded tile inset on a transparent canvas.
/// Full-bleed artwork with square corners is what Finder shows for a bare
/// executable, so shipping one reproduces the bug the bundle exists to fix.
/// Decode the largest member and check the shape itself.
#[test]
fn the_largest_member_is_a_rounded_tile() {
    let png = member_payload(b"ic10");
    let img = image::load_from_memory(&png)
        .expect("ic10 is not a decodable PNG")
        .to_rgba8();
    assert_eq!(img.dimensions(), (1024, 1024), "ic10 is not 1024x1024");

    for (x, y) in [(0u32, 0u32), (1023, 0), (0, 1023), (1023, 1023)] {
        assert_eq!(
            img.get_pixel(x, y)[3],
            0,
            "corner ({x},{y}) is opaque; the icon is full-bleed, not a rounded tile"
        );
    }
    assert_eq!(
        img.get_pixel(512, 512)[3],
        255,
        "the centre of the tile is transparent"
    );

    // Apple's 1024 grid insets the tile 100px per side, so the covered area is
    // about 0.65 of the canvas before the corners come off. A full-bleed icon
    // lands at 1.0 and a token 1px rounding stays near it.
    let opaque = img.pixels().filter(|p| p[3] > 0).count();
    let ratio = opaque as f64 / (1024.0 * 1024.0);
    assert!(
        (0.50..0.72).contains(&ratio),
        "opaque coverage is {ratio:.3}; the tile is not inset the way a macOS icon is"
    );
}
