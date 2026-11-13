//! Filesystem IO for config and keys: profile home, daily backups,
//! last-known-good snapshots, keys.toml read/write, and key merging.
//!
//! Split out of `types.rs` to keep config *data definitions* separate from
//! the *file IO* that loads and persists them.

use super::*;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Canonical base directory for the active profile.
///
/// - Default profile: `~/.opencrabs/`
/// - Named profile: `~/.opencrabs/profiles/<name>/`
///
/// Selection priority: `set_active_profile()` > `OPENCRABS_PROFILE` env > default.
pub fn opencrabs_home() -> PathBuf {
    let p = crate::config::profile::resolve_profile_home();
    if !p.exists()
        && let Err(e) = std::fs::create_dir_all(&p)
    {
        tracing::error!("Failed to create opencrabs home directory {p:?}: {e}");
    }
    p
}

/// A save writes many keys in a row. One snapshot per burst is what protects
/// the pre-save state, so a rotation younger than this window counts as
/// already covering the current burst.
const BACKUP_BURST_SECS: u64 = 5;

/// Daily backup of a config file. One snapshot per day, keeps `max_days` files.
///
/// Names backups `file.YYYY-MM-DD.bak`. Skips when the current content already
/// matches the NEWEST existing backup, not merely today's snapshot: a save
/// burst used to write one rotation per key even when nothing had changed
/// since the last one (#2265). If content differs, rotates the current file to
/// a timestamped sibling `file.YYYY-MM-DDTHH-MM-SS.bak` first, so a same-day
/// write can never destroy the day's only pre-write copy (2026-09-07 keys
/// wipe: the dedup skip left zero safety net for post-snapshot states). Prunes
/// backups beyond `max_days` files, oldest MTIME first: sorting by name kept
/// `file.bak` forever while deleting the current day's snapshot (#2265).
/// Silently ignores errors, because a backup failure must never block a config
/// write.
pub fn daily_backup(path: &Path, max_days: usize) {
    if !path.exists() {
        return;
    }
    let parent = match path.parent() {
        Some(p) => p,
        None => return,
    };
    let stem = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_backup = parent.join(format!("{stem}.{today}.bak"));

    // Compare against the newest backup of any age: a write that changes
    // nothing since the last snapshot has nothing to protect. On read error,
    // do nothing (never destroy on uncertainty).
    if let Some(newest) = newest_backup(parent, &stem) {
        let identical = match (fs::read(path), fs::read(&newest)) {
            (Ok(cur), Ok(bak)) => cur == bak,
            _ => true,
        };
        if identical {
            return;
        }
        // One save writes many keys back to back. A rotation taken seconds ago
        // is that save's pre-write state, already covering the rest of the
        // burst: rotating again per key produced five copies of nearly the
        // same file in two seconds and starved the retention budget (#2265).
        let fresh = fs::metadata(&newest)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|when| std::time::SystemTime::now().duration_since(when).ok())
            .is_some_and(|age| age < std::time::Duration::from_secs(BACKUP_BURST_SECS));
        if fresh {
            return;
        }
    }

    // Today's backup exists: rotate the current file to a timestamped sibling
    // first — the day's first snapshot stays intact AND every later pre-write
    // state gets its own copy.
    if today_backup.exists() {
        let stamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S");
        let mut rotated = parent.join(format!("{stem}.{stamp}.bak"));
        let mut n = 1u32;
        while rotated.exists() {
            rotated = parent.join(format!("{stem}.{stamp}-{n}.bak"));
            n += 1;
        }
        if let Err(e) = fs::copy(path, &rotated) {
            tracing::warn!("Failed to rotate {} before write: {e}", path.display());
            return;
        }
        tracing::debug!("Rotated same-day backup: {}", rotated.display());
    } else {
        // Create today's first snapshot
        if let Err(e) = fs::copy(path, &today_backup) {
            tracing::warn!("Failed to back up {} before write: {e}", path.display());
            return;
        }
        tracing::debug!("Daily backup: {}", today_backup.display());
    }

    prune_backups(parent, &stem, max_days);
}

/// Newest backup for `stem` in `parent`, by modification time.
fn newest_backup(parent: &Path, stem: &str) -> Option<PathBuf> {
    let prefix = format!("{stem}.");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(parent).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) || !name.ends_with(".bak") || name == stem {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// Keep the `max_days` newest backups by mtime, delete the rest.
///
/// Sorting by name was the bug (#2265): `config.toml.bak` sorts after every
/// `config.toml.YYYY-MM-DD.bak`, so the reverse walk treated the undated file
/// as the newest and deleted the current day's snapshot. mtime is the only
/// ordering that matches what the files actually are; the name breaks ties so
/// coarse-timestamp filesystems still prune deterministically.
fn prune_backups(parent: &Path, stem: &str, max_days: usize) {
    let prefix = format!("{stem}.");
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let mut backups: Vec<(std::time::SystemTime, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&prefix) || !name.ends_with(".bak") || name == stem {
                return None;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            Some((mtime, name))
        })
        .collect();
    backups.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, old) in backups.iter().skip(max_days) {
        let _ = fs::remove_file(parent.join(old));
        tracing::debug!("Pruned old backup: {old}");
    }
}

/// Snapshot current config + keys as "last known good".
///
/// Called after a successful provider response proves the config works.
/// On config parse failure, `Config::load()` falls back to these files.
/// Silently ignores errors — must never block normal operation.
pub fn save_last_good_config() {
    let home = opencrabs_home();

    let config_path = home.join("config.toml");
    let keys_path_src = home.join("keys.toml");
    let config_good = home.join("config.last_good.toml");
    let keys_good = home.join("keys.last_good.toml");

    if config_path.exists() {
        // NEVER snapshot a config that doesn't parse. A raw copy of a broken
        // config.toml poisons the last-good snapshot and defeats recovery
        // entirely — the whole point of the snapshot is to be loadable.
        if let Err(e) = Config::load_from_path(&config_path) {
            tracing::warn!("Refusing last-good snapshot: config.toml does not parse: {e}");
            return;
        }
        if let Err(e) = fs::copy(&config_path, &config_good) {
            tracing::debug!("Failed to save last-good config: {e}");
        }
    }
    if keys_path_src.exists() {
        // NEVER snapshot keys that don't parse (#712). A raw copy of a broken
        // keys.toml poisons keys.last_good.toml, so recovery fails exactly when
        // it's needed — every API key and the bot token become unloadable. This
        // mirrors the config.toml guard above, which keys.toml was missing.
        match fs::read_to_string(&keys_path_src) {
            Ok(content) => {
                if let Err(e) = crate::config::guard::parses(
                    crate::config::guard::ProtectedFile::Keys,
                    &content,
                ) {
                    tracing::warn!("Refusing last-good snapshot: keys.toml does not parse: {e}");
                } else if let Err(e) = fs::copy(&keys_path_src, &keys_good) {
                    tracing::debug!("Failed to save last-good keys: {e}");
                }
            }
            Err(e) => tracing::debug!("Failed to read keys.toml for last-good snapshot: {e}"),
        }
    }
}

/// Try loading config from last-known-good snapshot.
///
/// Returns None if no snapshot exists or if it also fails to parse.
pub fn load_last_good_config() -> Option<Config> {
    let home = opencrabs_home();
    let config_good = home.join("config.last_good.toml");

    if !config_good.exists() {
        return None;
    }

    tracing::warn!("Attempting recovery from last-known-good config");

    // Log the age of the snapshot so the operator knows how stale it is.
    if let Ok(meta) = fs::metadata(&config_good)
        && let Ok(modified) = meta.modified()
    {
        let age = std::time::SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();
        let saved_at = chrono::DateTime::<chrono::Local>::from(modified)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let hours = age.as_secs() / 3600;
        let age_str = if hours >= 24 {
            format!("{}d {}h", hours / 24, hours % 24)
        } else if hours >= 1 {
            format!("{}h {}m", hours, (age.as_secs() % 3600) / 60)
        } else {
            format!("{}m", age.as_secs() / 60)
        };
        tracing::warn!(
            "last_known_good snapshot is {} old (saved at {}) — \
             config.toml changes since then are silently ignored",
            age_str,
            saved_at
        );
    }

    // Load base config from the good snapshot
    let mut config = match Config::load_from_path(&config_good) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Last-good config also failed: {e}");
            return None;
        }
    };

    // Try loading keys from good snapshot
    let keys_good = home.join("keys.last_good.toml");
    if keys_good.exists()
        && let Ok(content) = fs::read_to_string(&keys_good)
        && let Ok(keys) = toml::from_str::<KeysFile>(&content)
    {
        config.providers = merge_provider_keys(config.providers, keys.providers);
        config.channels = merge_channel_keys(config.channels, keys.channels);
    }

    tracing::warn!("Recovered config from last-known-good snapshot");
    Some(config)
}

/// Get path to keys.toml - separate file for sensitive API keys
pub fn keys_path() -> PathBuf {
    opencrabs_home().join("keys.toml")
}

/// Read the RAW set of custom provider names from config.toml — no merge,
/// no keys.toml fallback. Used by `cleanup_keys_custom_providers` to break
/// the circular dependency where `Config::load()` (the loader) re-creates
/// missing config entries from keys.toml itself, which then made the
/// "orphan in keys.toml" check pass and skip removal.
///
/// Returns `None` when config.toml cannot be read or parsed. `None` means
/// "provider presence UNKNOWN" and callers MUST NOT remove anything — an
/// unreadable config is not an empty config. (#1458: a duplicate-key parse
/// error made the old empty-set fallback classify every keys.toml custom
/// provider as a ghost and wiped all 21 of them at startup. The empty set
/// is now reserved for a readable config with genuinely no custom table.)
pub(crate) fn raw_config_custom_provider_names() -> Option<std::collections::HashSet<String>> {
    use toml_edit::DocumentMut;
    let path = Config::system_config_path().unwrap_or_else(|| opencrabs_home().join("config.toml"));
    let content = std::fs::read_to_string(&path).ok()?;
    let doc = content.parse::<DocumentMut>().ok()?;
    Some(
        doc.as_table()
            .get("providers")
            .and_then(|t| t.as_table())
            .and_then(|t| t.get("custom"))
            .and_then(|t| t.as_table())
            .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
            .unwrap_or_default(),
    )
}

/// Save API keys to keys.toml using merge (preserves existing keys).
/// Only writes non-empty api_key values; never deletes other providers' keys.
pub fn save_keys(keys: &ProviderConfigs) -> Result<()> {
    // Merge each provider key individually via write_secret_key (read-modify-write)
    let providers: &[(&str, Option<&ProviderConfig>)] = &[
        ("providers.anthropic", keys.anthropic.as_ref()),
        ("providers.openai", keys.openai.as_ref()),
        ("providers.openrouter", keys.openrouter.as_ref()),
        ("providers.minimax", keys.minimax.as_ref()),
        ("providers.gemini", keys.gemini.as_ref()),
    ];

    for (section, provider) in providers {
        if let Some(p) = provider
            && let Some(key) = &p.api_key
            && !key.is_empty()
        {
            write_secret_key(section, "api_key", key)?;
        }
    }

    // Handle custom providers (flat "default" and named)
    if let Some(customs) = &keys.custom {
        for (name, p) in customs {
            if let Some(key) = &p.api_key
                && !key.is_empty()
            {
                let section = if name == "default" {
                    "providers.custom".to_string()
                } else {
                    format!("providers.custom.{}", name)
                };
                write_secret_key(&section, "api_key", key)?;
            }
        }
    }

    tracing::info!("Saved API keys to: {:?}", keys_path());
    Ok(())
}

/// Write a single key-value pair into keys.toml at the given dotted section path.
///
/// Equivalent to `Config::write_key` but targets `~/.opencrabs/keys.toml`.
/// Use for persisting secrets (tokens, API keys) that must not go into config.toml.
///
/// Normalize a TOML section key: lowercase, replace dots/underscores/spaces
/// with hyphens, strip non-alphanumeric chars (except hyphen).
/// e.g. "Qwen_2.5_4B" → "qwen-2-5-4b", "My Provider" → "my-provider"
pub fn normalize_toml_key(key: &str) -> String {
    key.trim()
        .to_lowercase()
        .replace(['.', '_', ' '], "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// # Example
/// ```no_run
/// # fn main() -> anyhow::Result<()> {
/// use opencrabs::config::write_secret_key;
/// write_secret_key("channels.telegram", "token", "123456:ABC...")?;
/// // results in keys.toml: [channels.telegram] token = "123456:ABC..."
/// # Ok(())
/// # }
/// ```
pub fn write_secret_key(section: &str, key: &str, value: &str) -> Result<()> {
    write_secret_keys(section, &[(key, value)])
}

/// Write several keys of one keys.toml section in a single atomic write, so a
/// caller that stores an ID and its token together cannot leave one without
/// the other. Values go through the same gate as [`write_secret_key`]; if no
/// value survives it, the file is not touched.
pub fn write_secret_keys(section: &str, pairs: &[(&str, &str)]) -> Result<()> {
    use toml_edit::DocumentMut;

    // The last gate before disk. Callers are supposed to resolve the "a key is
    // already stored" marker themselves, and one of them did not, which is how
    // a real key landed in keys.toml with the marker glued to its front (#1075).
    // Refusing the bare marker keeps a stored key from being overwritten by a
    // placeholder; stripping a prefix keeps the key the user actually typed.
    let writes: Vec<(&str, &str)> = pairs
        .iter()
        .filter_map(|(key, value)| {
            // Sanitize: strip carriage returns, take only first token (reject pasted URLs/junk after key)
            let value = value.split(['\r', '\n']).next().unwrap_or("").trim();
            // Nothing to write: empty, or "leave what is on disk"
            crate::config::stored_key::real_key(value).map(|value| (*key, value))
        })
        .collect();
    if writes.is_empty() {
        return Ok(());
    }

    // Hold lock for entire read-modify-write to prevent races
    let _guard = CONFIG_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let path = keys_path();

    let mut doc: DocumentMut = if path.exists() {
        fs::read_to_string(&path)?.parse()?
    } else {
        DocumentMut::new()
    };

    // Normalize custom provider names (e.g. "Qwen_2.5_4B" → "qwen-2-5-4b")
    let parts: Vec<String> = section
        .split('.')
        .enumerate()
        .map(|(i, p)| {
            if i >= 2 && section.starts_with("providers.custom") {
                normalize_toml_key(p)
            } else {
                p.to_string()
            }
        })
        .collect();

    // Navigate/create nested tables
    let mut current = doc.as_table_mut();
    for part in &parts {
        if current.get(part.as_str()).is_none() {
            current.insert(part, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        current = current
            .get_mut(part.as_str())
            .context("section not found after insert")?
            .as_table_mut()
            .with_context(|| format!("'{}' is not a table", part))?;
    }
    for (key, value) in &writes {
        current.insert(key, toml_edit::value(*value));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    daily_backup(&path, 7);
    atomic_write(&path, &doc.to_string())?;
    for (key, _) in &writes {
        tracing::info!("Wrote secret key [{section}].{key}");
    }
    Ok(())
}

/// Keys file structure (keys.toml) - contains sensitive keys and tokens
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeysFile {
    #[serde(default)]
    pub providers: ProviderConfigs,
    #[serde(default)]
    pub channels: ChannelsConfig,
    #[serde(default)]
    pub a2a: Option<KeysA2a>,
    #[serde(default)]
    pub image: Option<ImageKeys>,
}

/// Image keys section in keys.toml
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageKeys {
    pub api_key: Option<String>,
}

/// A2A keys section in keys.toml
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeysA2a {
    pub api_key: Option<String>,
}

/// Load API keys from keys.toml
/// This file should be chmod 600 for security
pub(crate) fn load_keys_from_file() -> Result<KeysFile> {
    let keys_path = keys_path();
    if !keys_path.exists() {
        return Ok(KeysFile::default());
    }

    tracing::trace!("Loading keys from: {:?}", keys_path);
    let content = std::fs::read_to_string(&keys_path)?;
    let keys: KeysFile = toml::from_str(&content)?;
    Ok(keys)
}

/// Merge API keys from keys.toml into existing provider configs
/// Keys from keys.toml override values in config.toml
pub(crate) fn merge_provider_keys(
    mut base: ProviderConfigs,
    keys: ProviderConfigs,
) -> ProviderConfigs {
    // Never merge the sentinel placeholder `/models` uses internally, and strip
    // it when a typed key was left glued to it (#1075). Sanitising rather than
    // rejecting matters: a keys.toml already holding `__EXISTING_KEY__sk-...`
    // from an older build heals on the next load instead of needing a hand edit.
    let real_key = |k: String| -> Option<String> {
        crate::config::stored_key::real_key(&k).map(str::to_string)
    };

    // Merge each provider's api_key if present in keys
    if let Some(k) = keys.anthropic
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.anthropic.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    if let Some(k) = keys.openai
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.openai.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    if let Some(k) = keys.openrouter
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.openrouter.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    tracing::trace!(
        "merge_provider_keys: minimax keys present={}, base present={}",
        keys.minimax.is_some(),
        base.minimax.is_some()
    );
    if let Some(k) = keys.minimax
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.minimax.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    // Moonshot AI (Kimi) built-in provider — merge its keys.toml secret like any
    // other provider (#610). Without this the key wrote to keys.toml but never
    // merged back, so the factory reported "API key missing" right after setup.
    if let Some(k) = keys.moonshot
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.moonshot.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    // Xiaomi: merge the user's key from keys.toml like any other provider.
    if let Some(k) = keys.xiaomi
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.xiaomi.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    // Built-in OpenCrabs: the install token from keys.toml, like any key.
    if let Some(k) = keys.opencrabs
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.opencrabs.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    if let Some(k) = keys.gemini
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.gemini.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    if let Some(k) = keys.github
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.github.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    // Merge zai (a legacy keys.toml `zhipu` section loads via the serde alias)
    if let Some(k) = keys.zai
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.zai.get_or_insert_with(ProviderConfig::default);
        entry.api_key = Some(key);
    }
    // Merge qwen (DashScope API key). Auto-enable + create the entry if
    // keys.toml has a key but config.toml doesn't — the user authenticated
    // through onboarding and wants Qwen on.
    if let Some(k) = keys.qwen
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.qwen.get_or_insert_with(|| ProviderConfig {
            enabled: true,
            ..Default::default()
        });
        entry.api_key = Some(key);
        if entry.default_model.is_none() && k.default_model.is_some() {
            entry.default_model = k.default_model;
        }
        if entry.base_url.is_none() && k.base_url.is_some() {
            entry.base_url = k.base_url;
        }
    }
    // Merge ollama (Ollama Cloud API key). `ProviderConfigs` has had an
    // `ollama` field for a while, so a key under `[providers.ollama]` in
    // keys.toml deserialised cleanly — and was then dropped here for want
    // of an arm (#1066). The factory reads `api_key` with
    // `unwrap_or_default()`, so the dropped key became an empty bearer
    // token and api.ollama.com answered 401. Local Ollama needs no key and
    // is unaffected; auto-enable on creation matches qwen/opencode, and an
    // existing `[providers.ollama]` entry keeps whatever `enabled` it set.
    if let Some(k) = keys.ollama
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.ollama.get_or_insert_with(|| ProviderConfig {
            enabled: true,
            ..Default::default()
        });
        entry.api_key = Some(key);
        if entry.default_model.is_none() && k.default_model.is_some() {
            entry.default_model = k.default_model;
        }
        if entry.base_url.is_none() && k.base_url.is_some() {
            entry.base_url = k.base_url;
        }
    }
    // Merge opencode (Go/Zen plan API key). Same auto-enable logic as
    // qwen — `/models` writes the key under `[providers.opencode]` in
    // keys.toml, and without this merge the runtime config never sees
    // it (factory.rs reports "API key missing" and the picker's
    // selection silently fails to take effect).
    if let Some(k) = keys.opencode
        && let Some(key) = k.api_key.and_then(real_key)
    {
        let entry = base.opencode.get_or_insert_with(|| ProviderConfig {
            enabled: true,
            ..Default::default()
        });
        entry.api_key = Some(key);
        if entry.default_model.is_none() && k.default_model.is_some() {
            entry.default_model = k.default_model;
        }
        if entry.base_url.is_none() && k.base_url.is_some() {
            entry.base_url = k.base_url;
        }
    }
    // Merge custom provider keys. Both config.toml and keys.toml go through
    // deserialize_custom_providers which normalizes keys via normalize_toml_key,
    // so names should match exactly (e.g. "customprovider-qwen").
    if let Some(custom_keys) = keys.custom {
        let base_customs = base.custom.get_or_insert_with(BTreeMap::default);
        for (name, key_cfg) in custom_keys {
            if let Some(key) = key_cfg.api_key.and_then(real_key) {
                use std::collections::btree_map::Entry;
                match base_customs.entry(name.clone()) {
                    Entry::Occupied(mut occupied) => {
                        tracing::trace!(
                            "merge_provider_keys: merging api_key for custom '{}'",
                            name
                        );
                        occupied.get_mut().api_key = Some(key);
                    }
                    Entry::Vacant(vacant) => {
                        // Key exists in keys.toml but not in config.toml.
                        // Create a minimal entry so the provider can be constructed.
                        tracing::trace!(
                            "merge_provider_keys: custom '{}' has key in keys.toml but no config.toml entry — creating minimal entry",
                            name
                        );
                        vacant.insert(ProviderConfig {
                            api_key: Some(key),
                            base_url: key_cfg.base_url,
                            default_model: key_cfg.default_model,
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }
    // Also handle STT/TTS keys. Each section carries two keyed providers, and
    // until #1066 only one of each had an arm: a key under
    // `[providers.stt.openai_compatible]` or `[providers.tts.openai_compatible]`
    // parsed cleanly and was then discarded here. `/onboard` writes keys to
    // exactly those paths, so two of its four voice-key writes produced a key
    // that could never reach the runtime.
    //
    // The sentinel guard now covers groq/openai too; it was missing, which let
    // the `__EXISTING_KEY__` sentinel `/models` uses internally merge as if it
    // were a credential.
    if let Some(stt) = keys.stt {
        let groq_key = stt.groq.and_then(|g| g.api_key).and_then(real_key);
        let compat_key = stt
            .openai_compatible
            .and_then(|c| c.api_key)
            .and_then(real_key);
        // Materialise `base.stt` only once a real key has arrived, so an empty
        // `[providers.stt]` in keys.toml cannot conjure a phantom section.
        if groq_key.is_some() || compat_key.is_some() {
            let base_stt = base.stt.get_or_insert_with(SttProviders::default);
            if let Some(key) = groq_key {
                base_stt
                    .groq
                    .get_or_insert_with(ProviderConfig::default)
                    .api_key = Some(key);
            }
            if let Some(key) = compat_key {
                base_stt
                    .openai_compatible
                    .get_or_insert_with(OpenaiCompatibleSttConfig::default)
                    .api_key = Some(key);
            }
        }
    }
    if let Some(tts) = keys.tts {
        let openai_key = tts.openai.and_then(|o| o.api_key).and_then(real_key);
        let compat_key = tts
            .openai_compatible
            .and_then(|c| c.api_key)
            .and_then(real_key);
        if openai_key.is_some() || compat_key.is_some() {
            let base_tts = base.tts.get_or_insert_with(TtsProviders::default);
            if let Some(key) = openai_key {
                base_tts
                    .openai
                    .get_or_insert_with(ProviderConfig::default)
                    .api_key = Some(key);
            }
            if let Some(key) = compat_key {
                base_tts
                    .openai_compatible
                    .get_or_insert_with(OpenaiCompatibleTtsConfig::default)
                    .api_key = Some(key);
            }
        }
    }
    if let Some(ws) = keys.web_search {
        let base_ws = base
            .web_search
            .get_or_insert_with(WebSearchProviders::default);
        if let Some(exa) = ws.exa
            && let Some(key) = exa.api_key
            && !key.is_empty()
        {
            let entry = base_ws.exa.get_or_insert_with(ProviderConfig::default);
            entry.api_key = Some(key);
        }
        if let Some(brave) = ws.brave
            && let Some(key) = brave.api_key
            && !key.is_empty()
        {
            let entry = base_ws.brave.get_or_insert_with(ProviderConfig::default);
            entry.api_key = Some(key);
        }
        if let Some(serper) = ws.serper
            && let Some(key) = serper.api_key
            && !key.is_empty()
        {
            let entry = base_ws.serper.get_or_insert_with(ProviderConfig::default);
            entry.api_key = Some(key);
        }
    }
    // Merge image provider keys (e.g. [providers.image.gemini])
    if let Some(img) = keys.image {
        let base_img = base.image.get_or_insert_with(ImageProviders::default);
        if let Some(gemini) = img.gemini
            && let Some(key) = gemini.api_key
            && !key.is_empty()
        {
            let entry = base_img.gemini.get_or_insert_with(ProviderConfig::default);
            entry.api_key = Some(key);
        }
    }
    // Summarise custom-provider key merge at INFO so "auth errors on
    // startup" always have a ground-truth log to correlate with: how
    // many customs exist, which have real keys, which don't.
    if let Some(ref customs) = base.custom {
        let total = customs.len();
        let with_key = customs
            .values()
            .filter(|c| {
                c.api_key
                    .as_ref()
                    .is_some_and(|k| crate::config::stored_key::is_real_key(k))
            })
            .count();
        let missing: Vec<&str> = customs
            .iter()
            .filter(|(_, c)| {
                !c.api_key
                    .as_ref()
                    .is_some_and(|k| crate::config::stored_key::is_real_key(k))
            })
            .map(|(n, _)| n.as_str())
            .collect();
        tracing::trace!(
            "merge_provider_keys: custom providers loaded = {} ({} with real api_key); \
             providers missing a real key: {:?}",
            total,
            with_key,
            missing,
        );
    }
    base
}

/// Merge channel tokens from keys.toml into existing channels config
/// Tokens from keys.toml override values in config.toml
pub(crate) fn merge_channel_keys(mut base: ChannelsConfig, keys: ChannelsConfig) -> ChannelsConfig {
    // Telegram
    if let Some(ref token) = keys.telegram.token
        && !token.is_empty()
    {
        base.telegram.token = Some(token.clone());
    }

    // Telegram userbot (MTProto secrets from keys.toml override config.toml)
    if let Some(ref ub) = keys.telegram.userbot.api_hash
        && !ub.is_empty()
    {
        base.telegram.userbot.api_hash =
            crate::config::stored_key::real_key(ub).map(str::to_string);
    }
    // keys.toml.example ships `api_id = 0` as a placeholder; a zero is not
    // a credential and must not shadow a real api_id set in config.toml.
    if let Some(api_id) = keys.telegram.userbot.api_id
        && api_id != 0
    {
        base.telegram.userbot.api_id = Some(api_id);
    }
    if keys
        .telegram
        .userbot
        .phone
        .as_ref()
        .is_some_and(|p| !p.is_empty())
    {
        base.telegram.userbot.phone = keys.telegram.userbot.phone.clone();
    }

    // Discord
    if let Some(ref token) = keys.discord.token
        && !token.is_empty()
    {
        base.discord.token = Some(token.clone());
    }

    // Slack
    if let Some(ref token) = keys.slack.token
        && !token.is_empty()
    {
        base.slack.token = Some(token.clone());
    }
    if let Some(ref app_token) = keys.slack.app_token
        && !app_token.is_empty()
    {
        base.slack.app_token = Some(app_token.clone());
    }

    // WhatsApp uses QR-code pairing stored in session.db — no token to merge.

    // Trello (app_token = API Key, token = API Token)
    if let Some(ref app_token) = keys.trello.app_token
        && !app_token.is_empty()
    {
        base.trello.app_token = Some(app_token.clone());
    }
    if let Some(ref token) = keys.trello.token
        && !token.is_empty()
    {
        base.trello.token = Some(token.clone());
    }

    base
}

/// Write `contents` to `path` atomically: a unique temp file in the same
/// directory, then a rename (#911).
///
/// `fs::write` truncates at open and then writes, so between those two steps
/// the file is ZERO BYTES on disk. The config watcher watches config.toml,
/// keys.toml and commands.toml, and any event on any of them triggers a full
/// `Config::load()`. A load landing in that window read nothing and reported a
/// perfectly valid config.toml as broken — "TOML parse error at line 1,
/// column 1" is the signature of an empty read, not of a syntax error.
///
/// Rename is atomic on the same filesystem, so a reader sees either the old
/// file or the new one and never a partial state. Same pattern the session
/// context store uses.
pub fn atomic_write(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    // Every config.toml / keys.toml writer funnels through here, so this is
    // the one place a test build can be stopped from rewriting the user's
    // live config (#1399). No-op outside test builds.
    crate::config::live_home_guard::refuse_live_home_write(path)?;

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let tmp = parent.join(format!(
        "{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    fs::write(&tmp, contents)?;

    // Owner-only before the file is ever visible at its final name (OC-05).
    // config.toml and especially keys.toml hold provider keys and channel
    // tokens; every writer funnels through here, and they were landing at the
    // process umask (0644), world-readable on a default home. Set 0600 on the
    // temp file so the rename publishes an already-private file rather than a
    // readable one that a later chmod would race.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)) {
            // Do not publish a secret file we could not lock down.
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    }

    if let Err(e) = fs::rename(&tmp, path) {
        // Never strand the temp file beside the config it failed to become.
        if let Err(cleanup) = fs::remove_file(&tmp) {
            tracing::warn!(
                "atomic_write: rename failed and the temp file could not be removed \
                 either ({cleanup}); leaving {}",
                tmp.display()
            );
        }
        return Err(e);
    }
    Ok(())
}
