//! A crab's file, as a path on this machine.
//!
//! An operation names a file on the crab's disk (a [`BlobRef`], a plan path,
//! an icon). The window draws, stats and opens files by path, so before it
//! touches one it asks [`Backend::localize`] for the path here. For this
//! process's own crab that is the same path: the disk is shared, nothing is
//! copied.
//!
//! A crab on another machine answers the bytes through `blob.get`. Its copy
//! lives at [`cache_path`]: under [`CACHE_DIR`], one directory per crab and
//! one file per hash of the crab's path, so a picture drawn every frame is
//! fetched once and two crabs that both have `/home/crab/icon.png` never share
//! a copy. The fetch itself lands with the remote backend.
//!
//! [`BlobRef`]: opencrabs::backend::BlobRef

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::backend_glue::Backend;

/// Where fetched blobs live, under the OpenCrabs home of this machine.
#[allow(dead_code)] // no window caller yet: every backend is local, so nothing is copied
pub const CACHE_DIR: &str = "remote_cache";

impl Backend {
    /// The path on this machine for `path` on the crab.
    ///
    /// Synchronous and never fetches: the window calls it while it renders.
    /// The local crab answers `path` itself.
    pub fn localize(&self, path: &Path) -> PathBuf {
        match self {
            Backend::Local(_) => path.to_path_buf(),
        }
    }
}

/// A crab's name cut to something safe as one directory: `[A-Za-z0-9._-]`
/// kept, anything else turned into `_`, no leading dot, `crab` when nothing is
/// left. A name can never climb out of [`CACHE_DIR`].
#[allow(dead_code)] // no window caller yet: every backend is local, so nothing is copied
fn crab_dir(crab: &str) -> String {
    let kept: String = crab
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = kept.trim_start_matches('.');
    if trimmed.is_empty() {
        "crab".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Where the copy of `path` on `crab` lives under `home`.
///
/// The name is the first 16 hex of the sha256 of the crab's path, plus the
/// path's own extension: the preview and the icon decoder pick a format by
/// extension, so the copy has to keep it. Nothing here touches the disk.
#[allow(dead_code)] // no window caller yet: every backend is local, so nothing is copied
pub fn cache_path(home: &Path, crab: &str, path: &Path) -> PathBuf {
    let digest: String = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()))
        .chars()
        .take(16)
        .collect();
    let name = match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if !ext.is_empty() => format!("{digest}.{ext}"),
        _ => digest,
    };
    home.join(CACHE_DIR).join(crab_dir(crab)).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_crab_answers_its_own_path() {
        let backend = Backend::local(PathBuf::from("/nowhere/opencrabs.db"));
        let path = Path::new("/home/crab/.opencrabs/gui-icons/a b.png");
        assert_eq!(backend.localize(path), path);
    }

    #[test]
    fn a_copy_is_named_by_crab_and_hash_and_keeps_its_extension() {
        let home = Path::new("/home/me/.opencrabs");
        let path = Path::new("/home/crab/.opencrabs/gui-icons/icon.png");
        let copy = cache_path(home, "lenovo", path);
        assert!(copy.starts_with(home.join(CACHE_DIR).join("lenovo")));
        assert_eq!(copy.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(
            copy,
            cache_path(home, "lenovo", path),
            "same path, same copy"
        );
        assert_ne!(
            copy,
            cache_path(home, "lenovo", Path::new("/home/crab/other.png")),
            "another path, another copy"
        );
    }

    #[test]
    fn two_crabs_never_share_a_copy() {
        let home = Path::new("/home/me/.opencrabs");
        let path = Path::new("/home/crab/icon.png");
        assert_ne!(cache_path(home, "a", path), cache_path(home, "b", path));
    }

    #[test]
    fn a_crab_name_cannot_climb_out_of_the_cache() {
        let home = Path::new("/home/me/.opencrabs");
        let path = Path::new("/home/crab/plan");
        for crab in ["..", "../..", "a/../../b", "", "/etc"] {
            let copy = cache_path(home, crab, path);
            assert!(copy.starts_with(home.join(CACHE_DIR)), "{crab}");
            assert_eq!(
                copy.parent().and_then(Path::parent),
                Some(home.join(CACHE_DIR).as_path()),
                "{crab}"
            );
        }
        assert_eq!(
            cache_path(home, "x", path).extension(),
            None,
            "no extension, none invented"
        );
    }
}
