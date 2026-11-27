//! The boot path: the ledger the window opens, and the config it
//! starts with, provisioned on a home that has none.
//!
//! Moved out of the crate root as pure code motion: the functions, their
//! comments and their tests are unchanged, only the module boundary is new.

use super::*;

/// What the window has to draw: the loaded view, or the reason it could not.
pub(crate) enum LoadState {
    Loaded(Box<UsageViewModel>),
    Failed(SharedString),
}

/// The ledger path, unchanged: `config.database.path`, with the same
/// default (`~/.opencrabs/opencrabs.db`) when config cannot be read at all.
pub(crate) fn ledger_path() -> std::path::PathBuf {
    Config::load()
        .map(|c| c.database.path)
        .unwrap_or_else(|_| opencrabs::config::opencrabs_home().join("opencrabs.db"))
}

/// The config the window boots with, provisioned on a home that has none
/// (#2375).
///
/// A window shipped without this step: it loaded whatever config happened to
/// exist, so a fresh install opened with no provider at all and the first
/// message died on "No provider configured", with no terminal to run the
/// wizard from. The order mirrors the CLI's own startup, and each line is
/// load-bearing:
///
/// 1. Write the documented `config.toml` and `keys.toml` when absent. The
///    built-in provider gets its section from that file, so provisioning
///    before this would have nowhere to write.
/// 2. Bring an existing `config.toml` up to the current schema. The terminal
///    has always run this at startup and the window never did, so an install
///    that only ever opened the window kept the file it was seeded with: a
///    migration that moves a shipped default (the built-in provider's model
///    id, #3203) reached the terminal and skipped the window.
/// 3. Re-read the config. The file loaded before seeding carries no provider
///    sections, and `provision_builtin_provider` decides from `enabled`, so
///    the old value would have skipped the token request (#2151).
/// 4. Seed the brain templates, offline-safe and idempotent, so a first-run
///    window has a personality instead of an empty prompt (#1382).
/// 5. Ask the proxy for this install's token. The request needs a reactor,
///    and the main thread has the stack for the current-thread runtime
///    `Config::load` must not run on a gpui worker (see `load_view`).
///
/// Every step is idempotent: the first and the last two only fill a blank
/// home, and the migration rewrites only the values it owns, so a configured
/// install boots as before unless a shipped default moved.
pub(crate) fn boot_config() -> Result<Config, String> {
    if opencrabs::config::seed::ensure_config_seeded() {
        opencrabs::config::seed::ensure_keys_seeded();
        logging::info("first run: documented config and keys written");
    }
    Config::migrate_config_files();
    let mut config = Config::load().map_err(|e| format!("config load: {e}"))?;
    opencrabs::config::profile::ensure_brain_seeded();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("first run: no runtime for provisioning: {e}"))?;
    if runtime.block_on(
        opencrabs::brain::provider::opencrabs_acquire::provision_builtin_provider(&mut config),
    ) {
        logging::info("first run: built-in provider token acquired");
    }
    Ok(config)
}
