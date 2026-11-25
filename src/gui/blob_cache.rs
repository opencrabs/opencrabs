//! A crab's file, as a path on this machine.
//!
//! An operation names a file on the crab's disk (a [`BlobRef`], a plan path,
//! an icon). The window draws, stats and opens files by path, so before it
//! touches one it asks [`Backend::localize`] for the path here. For this
//! process's own crab that is the same path: the disk is shared, nothing is
//! copied. A crab on another machine answers the bytes through `blob.get`;
//! the copy it needs lands with the remote backend.
//!
//! [`BlobRef`]: opencrabs::backend::BlobRef

use std::path::{Path, PathBuf};

use crate::backend_glue::Backend;

impl Backend {
    /// The path on this machine for `path` on the crab.
    ///
    /// Synchronous and never fetches: the window calls it while it renders.
    /// The local crab answers `path` itself.
    pub fn localize(&self, path: &Path) -> PathBuf {
        match self {
            Backend::Local(_) => path.to_path_buf(),
            // A crab on another machine has no copy here yet: the path is
            // returned as named, so the window shows the file as missing
            // instead of fetching inside a frame. The on-disk cache is rb/c5b.
            _ => path.to_path_buf(),
        }
    }
}
