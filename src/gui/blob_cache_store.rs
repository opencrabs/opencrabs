//! The window's copies of a remote crab's files, on this machine's disk.
//!
//! [`Backend::localize`](crate::backend_glue::Backend::localize) answers a
//! path the window can draw, stat and open. For a crab on another machine
//! that is a copy fetched by `blob.get`, kept under
//! `<home>/.opencrabs/cache/crabs/<sha256 of the crab id>/`, one file per
//! crab path, named by the sha256 of that path. The extension rides along, because previews
//! and thumbnails pick their kind by it, but only one of [`OPENS_HERE`]: the
//! crab is not trusted by this machine, and a copy named `x.desktop` or
//! `x.sh` is one click on "open" away from running here. Any other copy is
//! kept as `.bin`, which the window never hands to the system opener.
//!
//! This is the window's cache, not the crab's data: only this module touches
//! it, and the local crab never writes here (its files are already on this
//! disk). Its two disk functions are in the seam's `LOCAL_ONLY`.

use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The extensions a remote crab's copy keeps, lowercased: pictures, clips,
/// sounds, PDFs and plain text, the kinds the system opener shows rather
/// than runs. No `svg` (it carries script), nothing a desktop launches.
pub const OPENS_HERE: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "mp4", "webm", "mov", "mp3", "ogg", "wav", "m4a", "pdf",
    "txt", "md", "json", "csv", "log",
];

/// The extension of a copy whose own is not in [`OPENS_HERE`].
pub const FOREIGN_EXTENSION: &str = "bin";

/// What the window says instead of opening a copy it will not hand over.
pub const NOT_OPENED_HERE: &str = "a remote crab's file: this type is not opened here";

/// Whether `path`'s extension is one of [`OPENS_HERE`], in any case.
pub fn opens_here(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| OPENS_HERE.contains(&ext.to_ascii_lowercase().as_str()))
}

/// One crab's directory of copies.
#[derive(Debug, Clone)]
pub struct BlobStore {
    dir: PathBuf,
}

impl BlobStore {
    /// The copies of the crab whose stable id is `crab_id`, under this
    /// machine's OpenCrabs home.
    pub fn for_crab(crab_id: &str) -> Self {
        Self::at(
            opencrabs::config::opencrabs_home()
                .join("cache")
                .join("crabs")
                .join(dir_name(crab_id)),
        )
    }

    /// The copies kept in `dir`.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Where the copy of the crab's `path` lives, whether or not it is there.
    /// Pure: no disk. Named `<sha256>.<ext>` when the crab's extension is one
    /// of [`OPENS_HERE`] (lowercased), `<sha256>.bin` otherwise.
    pub fn slot(&self, path: &Path) -> PathBuf {
        let digest = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
        let ext = if opens_here(path) {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_else(|| FOREIGN_EXTENSION.to_string())
        } else {
            FOREIGN_EXTENSION.to_string()
        };
        self.dir.join(format!("{digest}.{ext}"))
    }

    /// The copy of `path`, when one was kept and it is in this crab's
    /// directory. A `stat` and two `realpath`s, no network.
    ///
    /// The slot is always built under the directory, but what is on the disk
    /// there is checked, not assumed: the slot and the directory are resolved
    /// (symlinks followed) and the copy is answered only when the resolved
    /// slot's parent is the resolved directory, compared by components. A
    /// slot that resolves anywhere else (a symlink out of the cache) is not a
    /// copy, so nothing hands that file to the system opener or the window.
    pub fn cached(&self, path: &Path) -> Option<PathBuf> {
        let slot = self.slot(path);
        if !slot.is_file() {
            return None;
        }
        let inside = match (slot.canonicalize(), self.dir.canonicalize()) {
            (Ok(copy), Ok(dir)) => copy.parent() == Some(dir.as_path()),
            _ => false,
        };
        inside.then_some(slot)
    }

    /// Keep `bytes` as the copy of `path`. Written beside the slot and
    /// renamed into it, so a frame never reads half a file.
    pub fn keep(&self, path: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let slot = self.slot(path);
        std::fs::create_dir_all(&self.dir)?;
        let partial = slot.with_file_name(format!(
            "{}.part",
            slot.file_name().and_then(|n| n.to_str()).unwrap_or("blob")
        ));
        let mut file = std::fs::File::create(&partial)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&partial, &slot)?;
        Ok(slot)
    }
}

/// A directory name for `crab_id`: always the sha256 of `crab:<id>`, so
/// every id lands in one name space. An id is never used as it is: a plain
/// id of 64 hex digits would otherwise name the directory another id hashed
/// into, and two crabs would share their copies. Nor can an id climb out of
/// the cache.
fn dir_name(crab_id: &str) -> String {
    format!("{:x}", Sha256::digest(format!("crab:{crab_id}").as_bytes()))
}
