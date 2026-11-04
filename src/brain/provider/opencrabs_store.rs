//! Where the built-in OpenCrabs provider keeps its install identity and token
//! (#2047, #2198).
//!
//! Two stores, on purpose:
//!
//! - The install ID lives in its own plain-text file, `install_id`, under the
//!   OpenCrabs home. The proxy mints one token per ID and counts one user per
//!   ID, so the ID is the only thing tying a user's traffic together. It used
//!   to live in `keys.toml` alone, which users hand-edit to add provider keys:
//!   one syntax error there made the ID unreadable, the next launch minted a
//!   fresh one, and one real user became two identities.
//! - The token stays in `keys.toml` under `[providers.opencrabs]`, because it
//!   is a secret and that is the file for secrets.
//!
//! The ID file has no parser that can fail: it is one line of text, so a
//! corrupt `keys.toml` cannot destroy the identity. A stored ID with no token
//! means the identity survived but its credential did not, and the caller asks
//! for a reissue rather than starting over.
//!
//! Reads check the ID file first and fall back to the `keys.toml` section for
//! installs created before this split, migrating that ID into the file on the
//! first read. Writes put the ID on disk before the token, so a failed
//! `keys.toml` write can cost the token but never the identity. The section
//! keeps a copy of the ID so an older binary still finds it.

use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

use super::opencrabs_install::is_valid_install_id;
use crate::config::{atomic_write, keys_path, opencrabs_home, write_secret_keys};

/// keys.toml section that holds the token and the legacy copy of the ID.
const SECTION: &str = "providers.opencrabs";

/// Name of the plain-text ID file under the OpenCrabs home.
const ID_FILE: &str = "install_id";

/// An install that has a token the proxy issued for its ID.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredInstall {
    pub install_id: String,
    pub token: String,
}

/// The token is a bearer secret, so `Debug` never prints it.
impl std::fmt::Debug for StoredInstall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredInstall")
            .field("install_id", &self.install_id)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Only the part of keys.toml this module reads. Other sections are ignored.
#[derive(Deserialize, Default)]
struct KeysView {
    #[serde(default)]
    providers: ProvidersView,
}

#[derive(Deserialize, Default)]
struct ProvidersView {
    #[serde(default)]
    opencrabs: Option<OpencrabsView>,
}

#[derive(Deserialize)]
struct OpencrabsView {
    install_id: Option<String>,
    api_key: Option<String>,
}

/// Where the install ID lives: one line of text under the OpenCrabs home.
pub fn install_id_path() -> PathBuf {
    opencrabs_home().join(ID_FILE)
}

/// Read the install ID from its own file.
///
/// `None` when the file is missing, empty, or does not hold a valid ID. An
/// invalid file is left alone rather than deleted: it is evidence, and the
/// next read falls back to `keys.toml` and rewrites it.
pub fn read_id_file() -> Option<String> {
    read_id_file_at(&install_id_path())
}

fn read_id_file_at(path: &Path) -> Option<String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("Could not read the install ID file: {e}");
            return None;
        }
    };
    let id = raw.trim();
    if id.is_empty() {
        tracing::warn!("The install ID file is empty: {}", path.display());
        return None;
    }
    if !is_valid_install_id(id) {
        tracing::warn!(
            "The install ID file does not hold a valid ID: {}",
            path.display()
        );
        return None;
    }
    Some(id.to_string())
}

/// The install ID this machine should present, whether or not it has a token.
///
/// Reads the ID file first. When that is missing, takes the ID from the
/// `keys.toml` section (installs created before the split) and migrates it
/// into the file, so the next hand-edit of `keys.toml` cannot take it away.
pub fn load_install_id() -> Option<String> {
    let legacy = read_keys_section(&keys_path()).and_then(|section| section.install_id);
    resolve_id(&install_id_path(), legacy)
}

/// Store the install ID on its own, before any token exists for it (#2198).
///
/// The acquire loop calls this ahead of its first request, so an ID that the
/// proxy may have minted a token for is already on disk. Losing the token then
/// costs a reissue, not the identity.
pub(crate) fn write_install_id(install_id: &str) -> std::io::Result<()> {
    write_install_id_at(&install_id_path(), install_id)
}

fn write_install_id_at(path: &Path, install_id: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // `atomic_write` publishes the file already at 0600 and refuses a test
    // build writing the live home. The trailing newline keeps it a text file
    // that reads cleanly with `cat`.
    atomic_write(path, &format!("{install_id}\n"))
}

/// Read the stored install. `None` when there is no ID, no token, or
/// `keys.toml` cannot be parsed.
///
/// A missing token is not a lost identity: [`load_install_id`] still answers,
/// and the caller can ask the proxy for a reissue against it.
pub fn load() -> Option<StoredInstall> {
    load_from(&install_id_path(), &keys_path())
}

fn load_from(id_path: &Path, keys: &Path) -> Option<StoredInstall> {
    let section = read_keys_section(keys)?;
    let token = section
        .api_key
        .as_deref()
        .filter(|token| !token.is_empty())?
        .to_string();
    let install_id = resolve_id(id_path, section.install_id)?;
    Some(StoredInstall { install_id, token })
}

/// The ID to use, from the file when possible and from `keys.toml` otherwise.
///
/// A `keys.toml` ID is migrated into the file here, which is what carries an
/// existing install across the split without minting anything.
fn resolve_id(id_path: &Path, legacy: Option<String>) -> Option<String> {
    if let Some(id) = read_id_file_at(id_path) {
        return Some(id);
    }
    let legacy = legacy.filter(|id| is_valid_install_id(id))?;
    match write_install_id_at(id_path, &legacy) {
        Ok(()) => tracing::info!(
            "Migrated the built-in provider install ID into {}",
            id_path.display()
        ),
        Err(e) => tracing::warn!(
            "Could not migrate the install ID into {} (keeping keys.toml as the only copy): {e}",
            id_path.display()
        ),
    }
    Some(legacy)
}

/// The `[providers.opencrabs]` section of keys.toml, if it reads and parses.
///
/// A file that cannot be read or parsed is logged and treated as no section.
/// That is safe now because the identity no longer depends on this file.
fn read_keys_section(path: &Path) -> Option<OpencrabsView> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("Could not read keys.toml for the built-in provider: {e}");
            return None;
        }
    };
    match toml::from_str::<KeysView>(&raw) {
        Ok(view) => view.providers.opencrabs,
        Err(e) => {
            tracing::warn!("keys.toml does not parse, built-in provider token unavailable: {e}");
            None
        }
    }
}

/// Write the install ID and its token.
///
/// The ID goes to its own file first. A failure there warns and continues, so
/// the token still lands; a failure writing `keys.toml` is returned, but by
/// then the identity is already safe on disk.
pub fn save(install: &StoredInstall) -> Result<()> {
    if let Err(e) = write_install_id_at(&install_id_path(), &install.install_id) {
        tracing::warn!(
            "Could not write the install ID file, keys.toml is the only copy of the identity: {e}"
        );
    }
    write_secret_keys(
        SECTION,
        &[
            ("install_id", install.install_id.as_str()),
            ("api_key", install.token.as_str()),
        ],
    )
}

/// Whether a token could be written right now.
///
/// `save` merges into the existing `keys.toml`, so a file that does not parse
/// cannot take a token at all. The acquire loop asks this before it mints a
/// replacement identity: an identity that could never be given a token is a
/// split with no upside, and minting one per launch is the exact bug this
/// module exists to stop (#2198).
pub fn token_store_is_writable() -> bool {
    token_store_is_writable_at(&keys_path())
}

fn token_store_is_writable_at(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        // Same parser `write_secret_keys` uses, so this answer matches what a
        // real write would do instead of approximating it.
        Ok(raw) => raw.parse::<toml_edit::DocumentMut>().is_ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}
