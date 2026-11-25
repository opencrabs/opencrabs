//! The window's copies of a remote crab's files, on this machine's disk.
//!
//! [`Backend::localize`](crate::backend_glue::Backend::localize) answers a
//! path the window can draw, stat and open. For a crab on another machine
//! that is a copy fetched by `blob.get`, kept under
//! `<home>/.opencrabs/cache/crabs/<crab id>/`, one file per crab path, named
//! by the sha256 of that path. The extension rides along, because previews
//! and thumbnails pick their kind by it.
//!
//! This is the window's cache, not the crab's data: only this module touches
//! it, and the local crab never writes here (its files are already on this
//! disk). Its two disk functions are in the seam's `LOCAL_ONLY`.

use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

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
    /// Pure: no disk.
    pub fn slot(&self, path: &Path) -> PathBuf {
        let digest = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
        let name = match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()) => {
                format!("{digest}.{ext}")
            }
            _ => digest,
        };
        self.dir.join(name)
    }

    /// The copy of `path`, when one was kept. One `stat`, no network.
    pub fn cached(&self, path: &Path) -> Option<PathBuf> {
        let slot = self.slot(path);
        slot.is_file().then_some(slot)
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

/// A directory name for `crab_id`: its plain characters, or its sha256 when
/// it has others (or none), so an id can never climb out of the cache.
fn dir_name(crab_id: &str) -> String {
    let plain = !crab_id.is_empty()
        && crab_id.len() <= 64
        && crab_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if plain {
        crab_id.to_string()
    } else {
        format!("{:x}", Sha256::digest(crab_id.as_bytes()))
    }
}
