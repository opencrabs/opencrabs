//! Materialise the shipped example files when they are absent.
//!
//! `config.toml.example` carries a section for every provider, disabled, with
//! its options documented inline. That file is how a provider setting gets
//! changed by hand: find the section, edit the value, save. It only serves
//! that purpose if it reaches disk.
//!
//! It used to reach disk from exactly one place, a single onboarding step,
//! guarded on `config.toml` not already existing. Meanwhile two other paths
//! created the file without it: a `write_key` for any section at all built its
//! document from nothing, and a startup auto-generate serialised the in-memory
//! config over the top. Whichever ran first won permanently, because the
//! seeding guard only ever fires once, and a user left holding the loser has a
//! config with no provider sections in it and nothing to edit.
//!
//! Seeding is not a wizard step, it is what the file needs before anyone
//! writes to it, so it lives here and every creator goes through it.

use std::path::{Path, PathBuf};

use crate::config::types::io::atomic_write;
use crate::config::{Config, opencrabs_home};

/// The shipped config, every provider section present and disabled.
pub const CONFIG_EXAMPLE: &str = include_str!("../../config.toml.example");
/// The shipped keys file, every credential slot present and empty.
pub const KEYS_EXAMPLE: &str = include_str!("../../keys.toml.example");

/// Where `write_key` and the loader agree `config.toml` lives.
pub fn config_path() -> PathBuf {
    Config::system_config_path().unwrap_or_else(|| opencrabs_home().join("config.toml"))
}

/// Where `write_keys_key` agrees `keys.toml` lives.
pub fn keys_path() -> PathBuf {
    opencrabs_home().join("keys.toml")
}

/// Write `contents` to `path` when nothing is there yet.
///
/// Returns whether it wrote. An existing file is never touched: seeding
/// supplies a starting point, it does not restore one.
fn seed_if_absent(path: &Path, contents: &str) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Through atomic_write so seeding inherits the guard that stops a test
    // build writing the live config, same as every other config writer.
    atomic_write(path, contents)?;
    Ok(true)
}

/// Ensure `config.toml` exists, seeded from the shipped example.
///
/// Failure is reported and swallowed: a caller is on its way to writing the
/// file itself, and losing the comments is a worse outcome than losing the
/// user's write.
pub fn ensure_config_seeded() -> bool {
    let path = config_path();
    match seed_if_absent(&path, CONFIG_EXAMPLE) {
        Ok(true) => {
            tracing::info!("Seeded config.toml from the shipped example at {:?}", path);
            true
        }
        Ok(false) => false,
        Err(e) => {
            tracing::warn!("Could not seed config.toml at {:?}: {}", path, e);
            false
        }
    }
}

/// Write the shipped example over `path`, whether or not a file is there.
///
/// Not a first-run filler like [`ensure_config_seeded`]: `init` asks for the
/// documented file, and with `--force` it asks for it over an existing one
/// (#2093).
pub fn write_config_example(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(path, CONFIG_EXAMPLE)
}

/// Ensure `keys.toml` exists, seeded from the shipped example.
pub fn ensure_keys_seeded() -> bool {
    let path = keys_path();
    match seed_if_absent(&path, KEYS_EXAMPLE) {
        Ok(true) => {
            tracing::info!("Seeded keys.toml from the shipped example at {:?}", path);
            true
        }
        Ok(false) => false,
        Err(e) => {
            tracing::warn!("Could not seed keys.toml at {:?}: {}", path, e);
            false
        }
    }
}

/// Enable, in the config on disk, every provider the loaded config already
/// holds a key for. Returns the sections written.
///
/// This is what the startup auto-generate wants: a config that reflects the
/// credentials already present, so a user who arrives with keys set is not
/// asked to choose a provider they have configured. It writes key by key into
/// the seeded document rather than serialising the whole struct over it, which
/// is what kept the provider sections and their comments in the file.
pub fn enable_providers_with_keys(config: &Config) -> Vec<String> {
    use crate::utils::providers::{KNOWN_PROVIDERS, config_for};

    let mut written = Vec::new();
    for meta in KNOWN_PROVIDERS {
        // A key, not merely a section: the keyless CLI providers are listed
        // here too, and enabling one the user never asked for is the outcome
        // this path exists to avoid.
        if !config_for(&config.providers, meta.id).is_some_and(|c| c.api_key.is_some()) {
            continue;
        }
        match Config::write_key(meta.config_section, "enabled", "true") {
            Ok(_) => written.push(meta.config_section.to_string()),
            Err(e) => tracing::warn!(
                "Could not enable {} in the seeded config: {}",
                meta.config_section,
                e
            ),
        }
    }
    written
}
