//! #2114: `/whisper` never refreshed a cached WhisperCrabs binary.
//!
//! The old `ensure_whispercrabs` returned on `path.exists()` alone, so a
//! cached binary was used forever. The first release after the one a user
//! installed would have stranded them: no API call, no download, no message.
//! Two more failure modes came with it. A killed download leaves a zero-byte
//! file that `exists()` happily returns and `Command::new` then fails on, and
//! a Windows install could never produce a binary at all because only the
//! tar.gz branch extracted anything.
//!
//! The decision is kept pure here so the four cases need neither network nor
//! filesystem. Anything that does touch disk uses a `TempDir`, never the live
//! home.

use crate::tui::app::dialogs::{
    WHISPERCRABS_BIN, extract_whispercrabs_archive, helper_binary_is_usable, helper_needs_refresh,
    helper_refresh_is_due, install_staged_helpers, migrate_legacy_whispercrabs, read_installed_tag,
    write_installed_tag,
};
use std::path::Path;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

/// Release tags look like `v0.1.23`; the sidecar stores exactly that.
const LATEST: &str = "v0.1.23";

// ---- the pure decision: four cases, no network, no filesystem ----

#[test]
fn no_cached_tag_always_refreshes() {
    // A binary with no sidecar has an unknown release. It cannot be proven
    // current, so it is replaced.
    assert!(helper_needs_refresh(None, LATEST, true));
}

#[test]
fn a_matching_tag_and_a_usable_binary_needs_no_refresh() {
    assert!(!helper_needs_refresh(Some(LATEST), LATEST, true));
}

#[test]
fn a_stale_tag_refreshes() {
    assert!(helper_needs_refresh(Some("v0.1.22"), LATEST, true));
}

#[test]
fn an_unusable_binary_refreshes_even_when_the_tag_matches() {
    // The zero-byte and non-executable cache cases: the tag says current, the
    // file says broken, and the file wins.
    assert!(helper_needs_refresh(Some(LATEST), LATEST, false));
    assert!(helper_needs_refresh(None, LATEST, false));
}

#[test]
fn tag_comparison_ignores_surrounding_whitespace() {
    // The sidecar is written with a trailing newline; a hand-edited file may
    // carry spaces. Neither is a different release.
    assert!(!helper_needs_refresh(Some(" v0.1.23\n"), LATEST, true));
    assert!(!helper_needs_refresh(Some(LATEST), " v0.1.23 ", true));
}

// ---- the re-check TTL ----

#[test]
fn a_fresh_check_is_not_due_and_an_old_one_is() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
    let ttl = Duration::from_secs(24 * 60 * 60);
    assert!(!helper_refresh_is_due(
        Some(now - Duration::from_secs(60)),
        now,
        ttl
    ));
    assert!(helper_refresh_is_due(
        Some(now - Duration::from_secs(25 * 60 * 60)),
        now,
        ttl
    ));
}

#[test]
fn an_unknown_or_future_check_time_is_due() {
    // No sidecar means never checked. A timestamp in the future comes from
    // clock skew or a restored backup, and must not suppress the check.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
    let ttl = Duration::from_secs(24 * 60 * 60);
    assert!(helper_refresh_is_due(None, now, ttl));
    assert!(helper_refresh_is_due(
        Some(now + Duration::from_secs(3600)),
        now,
        ttl
    ));
}

// ---- what counts as a usable binary ----

#[cfg(unix)]
#[test]
fn a_missing_empty_or_non_executable_file_is_not_usable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("whispercrabs");
    assert!(!helper_binary_is_usable(&path), "missing file");

    std::fs::write(&path, b"").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!helper_binary_is_usable(&path), "zero-byte file");

    std::fs::write(&path, b"\x7fELF").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!helper_binary_is_usable(&path), "no exec bit");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(helper_binary_is_usable(&path), "non-empty executable");
}

#[test]
fn a_directory_is_not_a_usable_binary() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("whispercrabs");
    std::fs::create_dir(&path).unwrap();
    assert!(!helper_binary_is_usable(&path));
}

// ---- the tag sidecar ----

#[test]
fn the_installed_tag_round_trips() {
    let dir = TempDir::new().unwrap();
    let tag_path = dir.path().join("whispercrabs.version");
    assert_eq!(read_installed_tag(&tag_path), None, "no sidecar yet");

    write_installed_tag(&tag_path, LATEST).unwrap();
    assert_eq!(read_installed_tag(&tag_path).as_deref(), Some(LATEST));

    // Overwriting is how the re-check TTL is stamped, so it must work twice.
    write_installed_tag(&tag_path, "v0.1.24").unwrap();
    assert_eq!(read_installed_tag(&tag_path).as_deref(), Some("v0.1.24"));
}

#[test]
fn a_blank_sidecar_reads_as_no_tag() {
    let dir = TempDir::new().unwrap();
    let tag_path = dir.path().join("whispercrabs.version");
    std::fs::write(&tag_path, "  \n").unwrap();
    assert_eq!(read_installed_tag(&tag_path), None);
}

// ---- archive extraction: the Windows zip that never ran ----

fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }
    buf.into_inner()
}

#[cfg(unix)]
#[test]
fn a_zip_extracts_the_binary_and_its_helpers() {
    // The old code only had a tar.gz branch, so a Windows download was written
    // to a temp file and then thrown away with no binary produced. The zip
    // ships DLLs beside the exe; moving only the exe leaves it unable to start.
    let dir = TempDir::new().unwrap();
    let staging = dir.path().join("staging");
    let bytes = zip_with(&[
        (WHISPERCRABS_BIN, b"MZ-fake-exe"),
        ("ggml.dll", b"DLL"),
        ("ggml-base.dll", b"DLL"),
    ]);

    extract_whispercrabs_archive(&bytes, &staging).unwrap();

    assert_eq!(
        std::fs::read(staging.join(WHISPERCRABS_BIN)).unwrap(),
        b"MZ-fake-exe"
    );
    assert!(staging.join("ggml.dll").is_file());
    assert!(staging.join("ggml-base.dll").is_file());
}

#[test]
fn a_tar_gz_extracts_too() {
    let dir = TempDir::new().unwrap();
    let staging = dir.path().join("staging");
    let bin = WHISPERCRABS_BIN;
    let bytes = {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut tar = tar::Builder::new(&mut gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(9);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, bin, &b"fake-bina"[..])
                .unwrap();
            tar.finish().unwrap();
        }
        gz.finish().unwrap()
    };

    extract_whispercrabs_archive(&bytes, &staging).unwrap();
    assert!(staging.join(bin).is_file());
}

#[test]
fn an_archive_without_the_binary_is_rejected() {
    // A layout change upstream must fail loudly here, not produce a path that
    // `Command::new` then fails on with no explanation.
    let dir = TempDir::new().unwrap();
    let staging = dir.path().join("staging");
    let bytes = zip_with(&[("README.txt", b"nope")]);
    let err = extract_whispercrabs_archive(&bytes, &staging).unwrap_err();
    assert!(
        err.to_string().contains("release layout changed"),
        "unexpected error: {err}"
    );
}

#[test]
fn bytes_that_are_neither_zip_nor_tar_gz_are_rejected() {
    let dir = TempDir::new().unwrap();
    let staging = dir.path().join("staging");
    let err = extract_whispercrabs_archive(b"<html>rate limited</html>", &staging).unwrap_err();
    assert!(
        err.to_string().contains("neither a zip nor a tar.gz"),
        "unexpected error: {err}"
    );
}

// ---- installing over an existing binary ----

#[cfg(unix)]
#[test]
fn installing_replaces_the_old_binary_and_brings_the_helpers_along() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let bin_dir = dir.path().join("bin");
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&staging).unwrap();

    let name = WHISPERCRABS_BIN;
    let installed = bin_dir.join(name);
    std::fs::write(&installed, b"old-build").unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o644)).unwrap();

    std::fs::write(staging.join(name), b"new-build").unwrap();
    std::fs::write(staging.join("helper.dll"), b"DLL").unwrap();

    install_staged_helpers(&staging, &bin_dir).unwrap();

    assert_eq!(std::fs::read(&installed).unwrap(), b"new-build");
    assert!(bin_dir.join("helper.dll").is_file());
    let mode = std::fs::metadata(&installed).unwrap().permissions().mode();
    assert_ne!(mode & 0o111, 0, "the installed binary must be executable");
}

// ---- the legacy install location ----

#[cfg(unix)]
#[test]
fn a_legacy_binary_is_migrated_once_and_its_tag_is_cleared() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let bin_dir = dir.path().join("bin");
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&staging).unwrap();

    let name = WHISPERCRABS_BIN;
    let legacy = dir.path().join("legacy-whispercrabs");
    let installed = bin_dir.join(name);
    let tag_path = bin_dir.join("whispercrabs.version");

    std::fs::write(&legacy, b"legacy-build").unwrap();
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A sidecar from a previous install must not claim the migrated file is
    // current: its release is unknown.
    write_installed_tag(&tag_path, LATEST).unwrap();

    migrate_legacy_whispercrabs(&legacy, &installed, &tag_path).unwrap();

    assert_eq!(std::fs::read(&installed).unwrap(), b"legacy-build");
    assert_eq!(
        read_installed_tag(&tag_path),
        None,
        "the migrated binary's release is unknown, so the tag is cleared"
    );
    assert!(legacy.exists(), "the user's file is copied, never deleted");
}

#[cfg(unix)]
#[test]
fn a_usable_installed_binary_is_not_overwritten_by_a_legacy_copy() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();

    let name = WHISPERCRABS_BIN;
    let legacy = dir.path().join("legacy-whispercrabs");
    let installed = bin_dir.join(name);
    let tag_path = bin_dir.join("whispercrabs.version");

    std::fs::write(&legacy, b"legacy-build").unwrap();
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&installed, b"current-build").unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).unwrap();
    write_installed_tag(&tag_path, LATEST).unwrap();

    migrate_legacy_whispercrabs(&legacy, &installed, &tag_path).unwrap();

    assert_eq!(std::fs::read(&installed).unwrap(), b"current-build");
    assert_eq!(read_installed_tag(&tag_path).as_deref(), Some(LATEST));
}

#[cfg(unix)]
#[test]
fn an_unusable_legacy_file_is_left_alone() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();

    let name = WHISPERCRABS_BIN;
    let legacy = dir.path().join("legacy-whispercrabs");
    let installed = bin_dir.join(name);
    let tag_path = bin_dir.join("whispercrabs.version");

    // Zero bytes: a killed download in the old location.
    std::fs::write(&legacy, b"").unwrap();
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o755)).unwrap();

    migrate_legacy_whispercrabs(&legacy, &installed, &tag_path).unwrap();

    assert!(
        !installed.exists(),
        "a broken legacy file must not be copied"
    );
}

// ---- the helper dir is not the brain dir ----

#[test]
fn helper_binaries_never_land_in_the_brain_dir() {
    // `~/.opencrabs` is agent state; executables belong on PATH. The old
    // legacy branch returned a path inside the brain dir, which is how the
    // second install location was born.
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home");
    let bin = crate::config::profile::with_home_override(home.clone(), || {
        crate::tui::app::dialogs::helper_bin_dir()
    });
    assert!(
        !bin.starts_with(&home),
        "helper dir {} is inside the brain dir {}",
        bin.display(),
        home.display()
    );
    assert!(Path::new(&bin).is_absolute());
}
