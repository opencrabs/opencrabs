//! A crab's file, as a path on this machine.
//!
//! An operation names a file on the crab's disk (a [`BlobRef`], a plan path,
//! an icon). The window draws, stats and opens files by path, so before it
//! touches one it asks [`Backend::localize`] for the path here. For this
//! process's own crab that is the same path: the disk is shared, nothing is
//! copied. A crab on another machine answers the bytes through `blob.get`,
//! kept in the window's blob cache (`blob_cache_store.rs`).
//!
//! A crab on another machine is not trusted by this one. What its copies may
//! reach here goes through [`Backend::openable`] (the system opener) and
//! [`Backend::localize_allowed`] (a file the window decodes itself): a copy
//! is only handed on when it is in the cache and of a kind in
//! [`OPENS_HERE`](crate::blob_cache_store::OPENS_HERE).
//!
//! [`BlobRef`]: opencrabs::backend::BlobRef

use std::path::{Path, PathBuf};

use crate::backend_glue::Backend;
use crate::blob_cache_store::{NOT_OPENED_HERE, opens_here};

impl Backend {
    /// The path on this machine for `path` on the crab.
    ///
    /// Synchronous and never fetches: the window calls it while it renders.
    /// The local crab answers `path` itself. A crab on another machine
    /// answers its cached copy; with no copy yet, the copy is asked for off
    /// the frame and the answer is where it will land, which does not exist
    /// until then, so the window shows the file as missing and repaints when
    /// it arrives.
    pub fn localize(&self, path: &Path) -> PathBuf {
        self.cached_blob(path)
            .unwrap_or_else(|| self.request_blob(path))
    }

    /// The path the system opener may be handed for the crab's `path`, or
    /// the notice the window shows instead.
    ///
    /// The local crab's file is its own path, as it always was. A crab on
    /// another machine only ever opens its kept copy, and only when that copy
    /// is of a kind in [`OPENS_HERE`](crate::blob_cache_store::OPENS_HERE):
    /// never the crab's path on this disk, never a copy not fetched yet,
    /// never a `.bin`. Synchronous, one `stat`, no network.
    pub fn openable(&self, path: &Path) -> Result<PathBuf, &'static str> {
        match self {
            Backend::Local(_) => Ok(path.to_path_buf()),
            _ => self
                .cached_blob(path)
                .filter(|copy| opens_here(copy))
                .ok_or(NOT_OPENED_HERE),
        }
    }

    /// [`localize`](Self::localize), for a file the window reads and decodes
    /// itself (an icon). The local crab answers `path`; a crab on another
    /// machine answers its copy only when the copy's kind is in
    /// [`OPENS_HERE`](crate::blob_cache_store::OPENS_HERE), and `None`
    /// otherwise, asking for nothing.
    pub fn localize_allowed(&self, path: &Path) -> Option<PathBuf> {
        match self {
            Backend::Local(_) => Some(path.to_path_buf()),
            _ => opens_here(path).then(|| self.localize(path)),
        }
    }

    /// The path whose folder "reveal" shows for the crab's `path`. The local
    /// crab's own path, as it always was; for a crab on another machine its
    /// copy's slot, so the folder shown is always the window's cache, never
    /// a place on this disk the crab named.
    pub fn revealable(&self, path: &Path) -> PathBuf {
        match self {
            Backend::Local(_) => path.to_path_buf(),
            _ => self.localize(path),
        }
    }
}
