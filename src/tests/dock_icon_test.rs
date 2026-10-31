//! Tests for the macOS Dock icon installer (#2280).
//!
//! The FFI half is main-thread AppKit and cannot run under `cargo test`, so
//! these cover the two decisions that actually gate it: whether the icon
//! bytes are a well-formed `.icns`, and whether the environment asked for the
//! Dock tile at all. Both helpers are platform-independent, so this file runs
//! on every CI runner.

use crate::utils::dock_icon::{ENV_ICON_PATH, ENV_OPT_OUT, icns_is_valid, should_install};

/// A minimal well-formed icns header: magic plus a declared length that covers
/// exactly the buffer.
fn icns(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    v.extend_from_slice(b"icns");
    v.extend_from_slice(&(len as u32).to_be_bytes());
    v.resize(len.max(8), 0);
    v
}

#[test]
fn accepts_a_well_formed_icns_header() {
    assert!(icns_is_valid(&icns(8)));
    assert!(icns_is_valid(&icns(1024)));
}

#[test]
fn rejects_the_wrong_magic() {
    let mut bytes = icns(64);
    bytes[0] = b'I';
    assert!(!icns_is_valid(&bytes), "case matters, the magic is `icns`");

    assert!(!icns_is_valid(b"icns"), "a bare magic is not a container");
    assert!(!icns_is_valid(b""), "empty input");
    assert!(!icns_is_valid(b"png\x00\x00\x00\x00"), "another format");
}

#[test]
fn rejects_a_truncated_or_impossible_length() {
    // Header claims 4096 bytes but the buffer stops at 64: a truncated file.
    let mut truncated = icns(4096);
    truncated.truncate(64);
    assert!(!icns_is_valid(&truncated));

    // Header claims 4 bytes, which cannot even hold the 8-byte header.
    let mut tiny = icns(64);
    tiny[4..8].copy_from_slice(&4u32.to_be_bytes());
    assert!(!icns_is_valid(&tiny));

    // A declared length smaller than the header is never valid.
    let mut zero = icns(64);
    zero[4..8].copy_from_slice(&0u32.to_be_bytes());
    assert!(!icns_is_valid(&zero));
}

#[test]
fn the_real_shipped_icon_is_valid() {
    // The bytes baked in on macOS are the same asset the .app bundle ships.
    // Read it from the source tree so the check also runs off macOS.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/icon.icns");
    let bytes = std::fs::read(path).expect("the repo ships src/assets/icon.icns");
    assert!(
        icns_is_valid(&bytes),
        "src/assets/icon.icns failed validation"
    );
    assert!(
        bytes.len() > 100_000,
        "the real icon carries a full size ladder, got {} bytes",
        bytes.len()
    );
}

#[test]
fn opt_out_defaults_to_installing() {
    assert!(should_install(None), "no variable set means install");

    for empty in ["", "   "] {
        assert!(
            should_install(Some(empty)),
            "an empty value is not an opt-out"
        );
    }

    assert!(should_install(Some("0")), "`0` means keep the icon");
}

#[test]
fn opt_out_honours_any_other_value() {
    for value in ["1", "true", "yes", "off", "please-no"] {
        assert!(
            !should_install(Some(value)),
            "{value:?} must disable the Dock icon"
        );
    }
}

#[test]
fn env_var_names_are_stable() {
    // These are user-facing and documented; renaming them silently breaks
    // anyone's opt-out.
    assert_eq!(ENV_OPT_OUT, "OPENCRABS_NO_DOCK_ICON");
    assert_eq!(ENV_ICON_PATH, "OPENCRABS_DOCK_ICON");
}
