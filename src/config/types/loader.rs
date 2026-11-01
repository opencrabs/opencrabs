//! Config loading, defaults/merge, auto-repair, and provider resolution.
//!
//! Split out of `types.rs` to separate config *behaviour* (loading, merging,
//! recovery) from the config *data definitions* that remain there.

use super::*;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// What one `Config::load()` had to do to produce a usable config.
///
/// Returned by value from [`Config::load_with_status`] so a caller learns the
/// outcome of ITS OWN load. The flags this replaced were process-wide and
/// consumed on read, so two threads loading config at once raced for a single
/// signal and one of them lost it (#912).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigLoadStatus {
    /// A broken config file was mechanically repaired in place and re-loaded.
    pub autofixed: bool,
    /// The load fell back to the last-known-good snapshot.
    pub recovered: bool,
    /// Why it fell back, so the user is told WHAT failed rather than only that
    /// something did (#909). Set only alongside `recovered`.
    pub recovery_reason: Option<String>,
}

impl ConfigLoadStatus {
    /// Nothing went wrong: the config on disk parsed as written.
    fn clean() -> Self {
        Self::default()
    }
}

impl Config {
    /// Build a runtime `VoiceConfig` from `providers.stt` / `providers.tts`.
    pub fn voice_config(&self) -> VoiceConfig {
        let stt = self.providers.stt.as_ref();
        let tts = self.providers.tts.as_ref();

        // The session's active provider can carry the audio models itself
        // (`stt_model` / `tts_model`), exactly as it carries `vision_model`
        // and `generation_model`. When it does, transcription and speech work
        // with no `[providers.stt.*]` block written by hand, which is what
        // makes voice available out of the box on an install whose provider
        // ships the models.
        let provider_stt = crate::brain::provider::factory::active_provider_stt(self);
        let provider_tts = crate::brain::provider::factory::active_provider_tts(self);

        // STT: detect all modes
        let groq_enabled = stt.and_then(|s| s.groq.as_ref()).is_some_and(|g| g.enabled);
        let local_stt_enabled = stt
            .and_then(|s| s.local.as_ref())
            .is_some_and(|l| l.enabled);
        let openai_compatible_stt_enabled = stt
            .and_then(|s| s.openai_compatible.as_ref())
            .is_some_and(|c| c.enabled);
        let voicebox_stt_enabled = stt
            .and_then(|s| s.voicebox.as_ref())
            .is_some_and(|v| v.enabled);

        let stt_enabled = groq_enabled
            || local_stt_enabled
            || openai_compatible_stt_enabled
            || voicebox_stt_enabled
            || provider_stt.is_some();
        let stt_mode = if local_stt_enabled {
            SttMode::Local
        } else {
            SttMode::Api
        };
        let local_stt_model = stt
            .and_then(|s| s.local.as_ref())
            .map(|l| l.model.clone())
            .unwrap_or_else(default_local_stt_model);
        // A disabled engine contributes NOTHING to the runtime view (#1399).
        // The dispatcher picks providers by the presence of these fields, so
        // a disabled openai_compatible block that still carries its
        // base_url used to be dispatched first and fail on every voice note
        // while the enabled provider waited behind it.
        let stt_compat = stt
            .and_then(|s| s.openai_compatible.as_ref())
            .filter(|c| c.enabled);
        // The active provider's own audio route wins over the standalone
        // `[providers.stt.openai_compatible]` block, the same precedence
        // `generation_model` has over the global `image.generation.model`.
        // The #1399 rule is untouched: a DISABLED block still contributes
        // nothing, and this is an additional source, not a re-enable.
        let stt_base_url = provider_stt
            .as_ref()
            .map(|(_, url, _)| url.clone())
            .or_else(|| stt_compat.and_then(|c| c.base_url.clone()));
        let stt_model = provider_stt
            .as_ref()
            .map(|(_, _, m)| m.clone())
            .or_else(|| stt_compat.and_then(|c| c.model.clone()))
            .or_else(|| Some("whisper-large-v3-turbo".to_string()));
        let stt_api_key = provider_stt
            .as_ref()
            .map(|(key, _, _)| key.clone())
            .filter(|k| !k.is_empty())
            .or_else(|| stt_compat.and_then(|c| c.api_key.clone()))
            .or_else(|| {
                stt.and_then(|s| s.groq.as_ref())
                    .filter(|g| g.enabled)
                    .and_then(|g| g.api_key.clone())
            });

        // TTS: detect all modes
        let openai_tts_enabled = tts
            .and_then(|t| t.openai.as_ref())
            .is_some_and(|o| o.enabled);
        let local_tts_enabled = tts
            .and_then(|t| t.local.as_ref())
            .is_some_and(|l| l.enabled);
        let openai_compatible_tts_enabled = tts
            .and_then(|t| t.openai_compatible.as_ref())
            .is_some_and(|c| c.enabled);
        let voicebox_tts_enabled = tts
            .and_then(|t| t.voicebox.as_ref())
            .is_some_and(|v| v.enabled);

        let tts_enabled = openai_tts_enabled
            || local_tts_enabled
            || openai_compatible_tts_enabled
            || voicebox_tts_enabled
            || provider_tts.is_some();
        let tts_mode = if local_tts_enabled {
            TtsMode::Local
        } else {
            TtsMode::Api
        };
        let tts_voice = tts
            .and_then(|t| t.openai.as_ref())
            .and_then(|o| o.voice.clone())
            .or_else(|| {
                tts.and_then(|t| t.openai_compatible.as_ref())
                    .and_then(|c| c.voice.clone())
            })
            .unwrap_or_else(default_tts_voice);
        // The active provider's `tts_model` is the primary synthesis route,
        // mirroring the STT precedence above.
        let tts_model = provider_tts
            .as_ref()
            .map(|(_, _, m)| m.clone())
            .or_else(|| {
                tts.and_then(|t| t.openai.as_ref())
                    .and_then(|o| o.model.clone().or_else(|| o.default_model.clone()))
            })
            .or_else(|| {
                tts.and_then(|t| t.openai_compatible.as_ref())
                    .and_then(|c| c.model.clone())
            })
            .unwrap_or_else(default_tts_model);
        // Same rule as STT (#1399): only an ENABLED openai_compatible block
        // feeds tts_base_url / tts_api_key, and only an enabled openai block
        // lends its key. The OpenAI kind never reads tts_base_url, so no
        // synthetic api.openai.com URL is planted here any more; it only
        // made the dispatcher label real OpenAI calls as openai_compatible.
        let tts_compat = tts
            .and_then(|t| t.openai_compatible.as_ref())
            .filter(|c| c.enabled);
        let tts_base_url = provider_tts
            .as_ref()
            .map(|(_, url, _)| url.clone())
            .or_else(|| tts_compat.and_then(|c| c.base_url.clone()));
        let tts_api_key = provider_tts
            .as_ref()
            .map(|(key, _, _)| key.clone())
            .filter(|k| !k.is_empty())
            .or_else(|| tts_compat.and_then(|c| c.api_key.clone()))
            .or_else(|| {
                tts.and_then(|t| t.openai.as_ref())
                    .filter(|o| o.enabled)
                    .and_then(|o| o.api_key.clone())
            });
        let local_tts_voice = tts
            .and_then(|t| t.local.as_ref())
            .map(|l| l.voice.clone())
            .unwrap_or_else(default_local_tts_voice);

        // Voicebox config
        let voicebox_stt_base_url = stt
            .and_then(|s| s.voicebox.as_ref())
            .map(|v| v.base_url.clone())
            .unwrap_or_else(default_voicebox_url);
        let voicebox_tts_base_url = tts
            .and_then(|t| t.voicebox.as_ref())
            .map(|v| v.base_url.clone())
            .unwrap_or_else(default_voicebox_url);
        let voicebox_tts_profile_id = tts
            .and_then(|t| t.voicebox.as_ref())
            .map(|v| v.profile_id.clone())
            .unwrap_or_default();
        let voicebox_tts_engine = tts
            .and_then(|t| t.voicebox.as_ref())
            .map(|v| v.engine.clone())
            .unwrap_or_default();

        // The provider blocks the Groq and OpenAI kinds read their key from
        // are present only while enabled (#1399), so a switched-off engine
        // with a stored key is not a candidate.
        let stt_provider = stt.and_then(|s| s.groq.clone()).filter(|g| g.enabled);
        let tts_provider = tts.and_then(|t| t.openai.clone()).filter(|o| o.enabled);

        // STT fallback chain: empty by default (dispatcher uses its built-
        // in priority). User configures via [providers.stt].fallback_chain
        // in config.toml, e.g. fallback_chain = ["voicebox", "groq", "local"].
        let stt_fallback_chain = stt
            .and_then(|s| s.fallback_chain.clone())
            .unwrap_or_default();

        // TTS fallback chain: same shape, [providers.tts].fallback_chain.
        let tts_fallback_chain = tts
            .and_then(|t| t.fallback_chain.clone())
            .unwrap_or_default();

        VoiceConfig {
            stt_enabled,
            stt_mode,
            local_stt_model,
            stt_base_url,
            stt_model,
            stt_api_key,
            tts_enabled,
            tts_mode,
            tts_voice,
            tts_model,
            tts_base_url,
            tts_api_key,
            local_tts_voice,
            stt_provider,
            tts_provider,
            voicebox_stt_enabled,
            voicebox_stt_base_url,
            voicebox_tts_enabled,
            voicebox_tts_base_url,
            voicebox_tts_profile_id,
            voicebox_tts_engine,
            stt_fallback_chain,
            tts_fallback_chain,
        }
    }

    /// Load configuration from default locations
    ///
    /// Priority (lowest to highest):
    /// 1. Default values
    /// 2. System config: ~/.opencrabs/config.toml
    /// 3. Local config: ./opencrabs.toml
    /// 4. Environment variables
    pub fn load() -> Result<Self> {
        Self::load_with_status().map(|(config, _)| config)
    }

    /// `load()`, plus what it had to do to succeed.
    ///
    /// Use this over `load()` + a global flag whenever the outcome of a
    /// particular load matters: the status is returned to the caller that
    /// asked for it and cannot be observed, cleared, or stolen by another
    /// thread loading config at the same time (#912).
    pub fn load_with_status() -> Result<(Self, ConfigLoadStatus)> {
        let loaded = match Self::load_inner() {
            Ok(config) => Ok((config, ConfigLoadStatus::clean())),
            Err(e) => {
                tracing::error!("Config load failed: {e:#} — attempting auto-repair");
                // First: try to mechanically fix a syntactically-broken config
                // file (e.g. an unterminated array left by a hand edit), save
                // the fix back to disk, and re-load. This heals the typo on
                // disk instead of silently running on a stale copy.
                if let Some(fixed) = Self::try_autofix_configs() {
                    Ok((
                        fixed,
                        ConfigLoadStatus {
                            autofixed: true,
                            ..ConfigLoadStatus::clean()
                        },
                    ))
                } else {
                    // If we can't safely auto-fix it, fall back to last-known-good
                    // so a typo never changes behaviour (e.g. flipping auto-always
                    // approvals into prompts).
                    tracing::error!("Auto-repair did not resolve it — trying last-known-good");
                    match load_last_good_config() {
                        // Keep the reason: the warning used to name a file and
                        // give no cause, so a transient truncated read looked
                        // identical to a genuine syntax error in the user's
                        // edit (#909).
                        Some(good) => Ok((
                            good,
                            ConfigLoadStatus {
                                recovered: true,
                                recovery_reason: Some(format!("{e:#}")),
                                ..ConfigLoadStatus::clean()
                            },
                        )),
                        None => Err(e),
                    }
                }
            }
        };

        // Record the first outcome for the startup notification. Ignored on
        // every later load — `FIRST_LOAD_STATUS` describes the config the
        // process started on, which is what the user is being told about.
        if let Ok((_, ref status)) = loaded {
            crate::config::types::FIRST_LOAD_STATUS.get_or_init(|| status.clone());
        }

        loaded
    }

    /// Outcome of the first `Config::load()` in this process, for the startup
    /// notification. Non-destructive: reading it never clears it.
    pub fn first_load_status() -> ConfigLoadStatus {
        crate::config::types::FIRST_LOAD_STATUS
            .get()
            .cloned()
            .unwrap_or_default()
    }

    /// On a parse failure, mechanically repair the broken config file(s) in
    /// place (see [`crate::config::repair`]) and re-load. Returns the loaded
    /// Config only if a repair was written AND loading then succeeds.
    fn try_autofix_configs() -> Option<Self> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(p) = Self::system_config_path() {
            candidates.push(p);
        }
        candidates.push(Self::local_config_path());

        // Hold the file lock only for the read/repair/write; it is released
        // before re-entering load_inner (which re-locks via migrate_if_needed)
        // to avoid a self-deadlock.
        let repaired_any = {
            let _guard = CONFIG_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            crate::config::repair::autofix_config_files(&candidates)
        };
        if !repaired_any {
            return None;
        }
        match Self::load_inner() {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                tracing::error!("Auto-fix wrote repairs but config still fails to load: {e}");
                None
            }
        }
    }

    /// Bring config.toml up to date with the current schema, rewriting it in
    /// place if anything changed.
    ///
    /// Call this once at process start, before the first `load()`. It used to
    /// run inside `load_inner`, which made every config read a potential write
    /// to disk — including reads from background threads and from code that
    /// only wanted to look at a value (#912).
    pub fn migrate_config_files() {
        if let Some(ref path) = Self::system_config_path() {
            Self::migrate_if_needed(path);
        }
    }

    /// Inner load implementation — separated so `load()` can wrap with recovery.
    fn load_inner() -> Result<Self> {
        tracing::trace!("Loading configuration...");

        // Start with defaults
        let mut config = Self::default();

        // 1. Try to load system config
        if let Some(system_config_path) = Self::system_config_path()
            && system_config_path.exists()
        {
            tracing::trace!("Loading system config from: {:?}", system_config_path);
            config = Self::merge_from_file(config, &system_config_path)?;
        }

        // 2. Try to load local config
        let local_config_path = Self::local_config_path();
        if local_config_path.exists() {
            tracing::trace!("Loading local config from: {:?}", local_config_path);
            config = Self::merge_from_file(config, &local_config_path)?;
        }

        // Migration used to run here, so every `load()` REWROTE config.toml —
        // a read that mutates the file it read. Startup calls
        // `migrate_config_files()` instead (#912): loading config is now a pure
        // read, and the migration happens once, where it can be reasoned about.

        // 3. Load API keys from keys.toml (overrides config.toml keys)
        //    On parse failure, try keys.last_good.toml before giving up.
        match load_keys_from_file() {
            Err(e) => {
                tracing::error!("Failed to load keys.toml: {:#}", e);

                // Try recovering from last-good keys snapshot
                let keys_good = opencrabs_home().join("keys.last_good.toml");
                if keys_good.exists() {
                    match fs::read_to_string(&keys_good)
                        .context("reading keys.last_good.toml")
                        .and_then(|content| {
                            toml::from_str::<KeysFile>(&content)
                                .context("parsing keys.last_good.toml")
                        }) {
                        Ok(keys) => {
                            tracing::warn!(
                                "Recovered API keys from keys.last_good.toml — \
                                 fix or delete keys.toml to clear this warning"
                            );
                            config.providers =
                                merge_provider_keys(config.providers, keys.providers);
                            config.channels = merge_channel_keys(config.channels, keys.channels);
                            if let Some(a2a_keys) = keys.a2a
                                && let Some(key) = a2a_keys.api_key
                                && !key.is_empty()
                            {
                                config.a2a.api_key = Some(key);
                            }
                        }
                        Err(e2) => {
                            tracing::error!(
                                "keys.last_good.toml also failed: {:#} — no API keys loaded",
                                e2
                            );
                        }
                    }
                } else {
                    tracing::error!("No keys.last_good.toml backup — no API keys loaded");
                }
            }
            Ok(keys) => {
                config.providers = merge_provider_keys(config.providers, keys.providers);
                config.channels = merge_channel_keys(config.channels, keys.channels);
                // Merge A2A API key from keys.toml
                if let Some(a2a_keys) = keys.a2a
                    && let Some(key) = a2a_keys.api_key
                    && !key.is_empty()
                {
                    config.a2a.api_key = Some(key);
                }
                // Merge image API key into config.image (generation + vision)
                // New path: [providers.image.gemini] (already merged above)
                // Legacy fallback: flat [image] section in keys.toml
                let image_key = config
                    .providers
                    .image
                    .as_ref()
                    .and_then(|img| img.gemini.as_ref())
                    .and_then(|g| g.api_key.as_ref())
                    .filter(|k| !k.is_empty())
                    .cloned()
                    .or_else(|| {
                        keys.image
                            .and_then(|img| img.api_key)
                            .filter(|k| !k.is_empty())
                    });
                if let Some(key) = image_key {
                    config.image.generation.api_key = Some(key.clone());
                    config.image.vision.api_key = Some(key);
                }
            }
        }

        // 4. Apply environment variable overrides
        config = Self::apply_env_overrides(config)?;

        // Expand tilde in database path (TOML doesn't expand ~)
        config.database.path = expand_tilde(&config.database.path);

        // Warn about unknown top-level keys in config.toml
        if let Some(path) = Self::system_config_path()
            && path.exists()
        {
            Self::warn_unknown_keys(&path);
        }

        tracing::trace!("Configuration loaded successfully");
        Ok(config)
    }

    /// Check for unknown top-level keys and log warnings.
    /// Only collects warnings once — subsequent calls are no-ops.
    fn warn_unknown_keys(path: &Path) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static CHECKED: AtomicBool = AtomicBool::new(false);
        if CHECKED.swap(true, Ordering::Relaxed) {
            return;
        }

        // #83: the same serde_ignored pass the write guard uses — the
        // compiled `Config` struct is the registry, so this warning can
        // never drift (its predecessor, the hand-maintained
        // `KNOWN_TOP_LEVEL_KEYS` list, was missing `doctor`, so a live
        // [doctor] section warned as a possible typo).
        let Ok(raw) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(unknown) = crate::config::sections::unknown_config_paths(&raw) else {
            return;
        };
        if !unknown.is_empty() {
            tracing::warn!(
                "Unknown keys in config.toml (possible typos or stale entries): {}",
                unknown.join(", ")
            );
            CONFIG_TYPO_WARNINGS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(unknown);
        }
    }

    /// Returns any config typo warnings collected during load (drains the list).
    pub fn take_typo_warnings() -> Vec<String> {
        CONFIG_TYPO_WARNINGS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }

    /// Load configuration from a specific file path
    ///
    /// Priority (lowest to highest):
    /// 1. Default values
    /// 2. Custom config file (specified path)
    /// 3. Environment variables
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        tracing::debug!("Loading configuration from custom path: {:?}", path);

        // Start with defaults
        let mut config = Self::default();

        // Load from custom path
        if path.exists() {
            config = Self::merge_from_file(config, path)?;
        } else {
            anyhow::bail!("Config file not found: {:?}", path);
        }

        // Apply environment variable overrides
        config = Self::apply_env_overrides(config)?;

        // Expand tilde in database path (TOML doesn't expand ~)
        config.database.path = expand_tilde(&config.database.path);

        tracing::debug!("Configuration loaded successfully from custom path");
        Ok(config)
    }

    /// Migrate old config keys in-place.
    ///
    /// Currently handles: `channels.trello.allowed_channels` → `board_ids`.
    /// Called once after loading so old configs are silently upgraded on first run.
    pub(crate) fn migrate_if_needed(path: &Path) {
        // Hold lock to prevent races between main thread and config watcher
        let _guard = CONFIG_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Converge the legacy section name onto the current one, on disk.
        // The read-time fold keeps both spellings working, but leaves the file
        // as written, so an old config keeps its old name forever and the two
        // names persist. Renaming it once means nobody carries the legacy
        // spelling afterwards (#1116).
        //
        // Its own read/write via toml_edit rather than joining the
        // `toml::Value` pass below, because that round-trip discards comments
        // and these files are mostly comments.
        match crate::config::alias_merge::migrate_file(path) {
            Ok(renamed) if !renamed.is_empty() => {
                tracing::info!(
                    "Config migration: renamed section(s) {} to their current names in {}",
                    renamed.join(", "),
                    path.display()
                );
            }
            Ok(_) => {}
            Err(e) => {
                // Not fatal: the read-time fold still makes the file load.
                tracing::warn!(
                    "Config migration: could not rename legacy section(s) in {}: {e}",
                    path.display()
                );
            }
        }

        let Ok(content) = fs::read_to_string(path) else {
            return;
        };

        let mut doc: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut changed = false;

        // ── Migration 1: channels.trello.allowed_channels → board_ids ──
        if let Some(trello) = doc
            .get_mut("channels")
            .and_then(|c| c.get_mut("trello"))
            .and_then(|t| t.as_table_mut())
            && let Some(val) = trello.remove("allowed_channels")
            && !trello.contains_key("board_ids")
        {
            trello.insert("board_ids".to_string(), val);
            changed = true;
        }

        // ── Migration 2: [voice] → providers.stt.* / providers.tts.* ──
        let mut voice_migrated = false;
        if let Some(voice) = doc.get("voice").and_then(|v| v.as_table()).cloned() {
            voice_migrated = true;
            let root = doc.as_table_mut().unwrap();

            // Ensure providers.stt and providers.tts tables exist
            if !root.contains_key("providers") {
                root.insert(
                    "providers".to_string(),
                    toml::Value::Table(toml::map::Map::new()),
                );
            }
            let providers = root.get_mut("providers").unwrap().as_table_mut().unwrap();
            if !providers.contains_key("stt") {
                providers.insert("stt".to_string(), toml::Value::Table(toml::map::Map::new()));
            }
            if !providers.contains_key("tts") {
                providers.insert("tts".to_string(), toml::Value::Table(toml::map::Map::new()));
            }
            // STT: stt_enabled + stt_mode → providers.stt.groq / providers.stt.local
            let stt_enabled = voice
                .get("stt_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let stt_mode = voice
                .get("stt_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("api")
                .to_string();
            // A legacy [voice] that says enabled MEANS enabled (#1399). The
            // old `entry("enabled").or_insert(true)` was a no-op whenever the
            // provider table already carried `enabled = false`, so the
            // migration could never re-enable what the user had on.
            if stt_enabled {
                let stt = providers.get_mut("stt").unwrap().as_table_mut().unwrap();
                let engine = if stt_mode == "local" { "local" } else { "groq" };
                if !stt.contains_key(engine) {
                    stt.insert(
                        engine.to_string(),
                        toml::Value::Table(toml::map::Map::new()),
                    );
                }
                let table = stt.get_mut(engine).unwrap().as_table_mut().unwrap();
                table.insert("enabled".to_string(), toml::Value::Boolean(true));
                if engine == "local"
                    && let Some(model) = voice.get("local_stt_model")
                {
                    table.entry("model").or_insert(model.clone());
                }
            }

            // TTS: tts_enabled + tts_mode → providers.tts.openai / providers.tts.local
            let tts_enabled = voice
                .get("tts_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let tts_mode = voice
                .get("tts_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("api")
                .to_string();
            if tts_enabled {
                let tts = providers.get_mut("tts").unwrap().as_table_mut().unwrap();
                let engine = if tts_mode == "local" {
                    "local"
                } else {
                    "openai"
                };
                if !tts.contains_key(engine) {
                    tts.insert(
                        engine.to_string(),
                        toml::Value::Table(toml::map::Map::new()),
                    );
                }
                let table = tts.get_mut(engine).unwrap().as_table_mut().unwrap();
                table.insert("enabled".to_string(), toml::Value::Boolean(true));
                if engine == "local" {
                    if let Some(voice_name) = voice.get("local_tts_voice") {
                        table.entry("voice").or_insert(voice_name.clone());
                    }
                } else {
                    if let Some(v) = voice.get("tts_voice") {
                        table.entry("voice").or_insert(v.clone());
                    }
                    if let Some(m) = voice.get("tts_model") {
                        table.entry("model").or_insert(m.clone());
                    }
                }
            }

            // Remove the old [voice] section
            root.remove("voice");
            changed = true;
        }

        // ── Migration 4: seed channel bot_owner from the first allow-list entry ──
        for (channel, list_key) in Self::OWNER_SEED_CHANNELS {
            if Self::owner_seed_needed(&doc, channel, list_key) {
                changed = true;
            }
        }

        if !changed {
            // Migration 3: inject commented subagent defaults into [agent] section
            // Uses text-level injection (comments can't survive toml::Value round-trip)
            let content = fs::read_to_string(path).unwrap_or_default();
            let has_subagent =
                content.contains("subagent_provider") || content.contains("subagent_model");
            if !has_subagent && let Ok(injected) = inject_subagent_defaults(&content) {
                match crate::config::types::io::atomic_write(path, &injected) {
                    Ok(()) => {
                        tracing::info!("Config migrated: injected subagent defaults into [agent]")
                    }
                    Err(e) => tracing::warn!(
                        "Config migration: failed to write injected defaults to {}: {e}",
                        path.display()
                    ),
                }
            }
            // Migration 5: inject commented plan-mode routing defaults (#793).
            // Re-read: the subagent injection above may have just rewritten the
            // file, and writing a stale copy would drop it.
            let content = fs::read_to_string(path).unwrap_or_default();
            if !content.contains("plan_provider")
                && !content.contains("execute_provider")
                && let Ok(injected) = inject_plan_mode_defaults(&content)
                && let Err(e) = crate::config::types::io::atomic_write(path, &injected)
            {
                tracing::warn!("Config migration: failed to inject plan-mode defaults: {e}");
            }
            // No structural migration needed — do NOT re-serialize with
            // toml::to_string_pretty as that destroys comments and ordering.
            return;
        }

        // Voice/trello migration occurred — need to write structural changes.
        // Use toml_edit to preserve formatting of untouched sections.
        let Ok(mut edit_doc) = content.parse::<toml_edit::DocumentMut>() else {
            tracing::warn!(
                "Config migration: failed to parse config.toml for format-preserving write"
            );
            return;
        };

        // Apply the same structural changes via toml_edit
        // Migration 1: trello rename
        if let Some(trello) = edit_doc
            .get_mut("channels")
            .and_then(|c| c.as_table_mut())
            .and_then(|c| c.get_mut("trello"))
            .and_then(|t| t.as_table_mut())
            && let Some(val) = trello.remove("allowed_channels")
            && trello.get("board_ids").is_none()
        {
            trello.insert("board_ids", val);
        }
        // Migration 2: remove [voice] section and carry the provider
        // enablement it produced into this document (#1399); the removal
        // alone left every migrated flag on the value document only.
        edit_doc.as_table_mut().remove("voice");
        if voice_migrated {
            super::voice_port::port_voice_providers(&doc, &mut edit_doc);
        }

        // Migration 4: seed bot_owner per channel where missing.
        for (channel, list_key) in Self::OWNER_SEED_CHANNELS {
            Self::seed_bot_owner_edit(&mut edit_doc, channel, list_key);
        }

        Self::backup_config(path, 7);
        if crate::config::types::io::atomic_write(path, &edit_doc.to_string()).is_ok() {
            tracing::info!(
                "Config migrated: rewrote {} (structural changes written)",
                path.display()
            );
        }

        // Migration 3: inject subagent defaults after structural migration
        let updated_content = fs::read_to_string(path).unwrap_or_default();
        let has_subagent = updated_content.contains("subagent_provider")
            || updated_content.contains("subagent_model");
        if !has_subagent
            && let Ok(injected) = inject_subagent_defaults(&updated_content)
            && let Err(e) = crate::config::types::io::atomic_write(path, &injected)
        {
            tracing::warn!("Config migration: failed to inject subagent defaults: {e}");
        }

        // Migration 5: inject plan-mode routing defaults after structural
        // migration. Re-read for the same reason as above.
        let updated_content = fs::read_to_string(path).unwrap_or_default();
        if !updated_content.contains("plan_provider")
            && !updated_content.contains("execute_provider")
            && let Ok(injected) = inject_plan_mode_defaults(&updated_content)
            && let Err(e) = crate::config::types::io::atomic_write(path, &injected)
        {
            tracing::warn!("Config migration: failed to inject plan-mode defaults: {e}");
        }
    }

    /// Channels whose `bot_owner` is seeded from the first allow-list entry on
    /// migration, paired with that channel's allow-list key (WhatsApp keys off
    /// `allowed_phones`, the rest off `allowed_users`).
    const OWNER_SEED_CHANNELS: &'static [(&'static str, &'static str)] = &[
        ("telegram", "allowed_users"),
        ("discord", "allowed_users"),
        ("slack", "allowed_users"),
        ("trello", "allowed_users"),
        ("whatsapp", "allowed_phones"),
    ];

    /// True when `channel` has a non-empty allow list but no `bot_owner` yet, so
    /// the owner must be seeded. Owner identity used to be the implicit first
    /// allow-list entry; this makes it explicit and stable (#243).
    fn owner_seed_needed(doc: &toml::Value, channel: &str, list_key: &str) -> bool {
        let Some(t) = doc
            .get("channels")
            .and_then(|c| c.get(channel))
            .and_then(|t| t.as_table())
        else {
            return false;
        };
        let has_owner = t
            .get("bot_owner")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        let has_first = t
            .get(list_key)
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        !has_owner && has_first
    }

    /// Insert `bot_owner = [<first allow-list entry>]` for `channel` when it is
    /// missing, preserving the entry's TOML type (string id or numeric id).
    /// Documentation values from `config.toml.example`, which must never become
    /// an owner (#1440).
    ///
    /// The template ships an example id in each channel's allow-list to show
    /// the shape of the field. Seeded verbatim into a real config, the
    /// migration below used to promote the first one to `bot_owner`, and an
    /// explicit `bot_owner` IS the owner set, so the positional fallback that
    /// makes the first real user the owner could never fire. The operator was
    /// silently not the owner of their own bot, and anyone holding the account
    /// that happens to match the placeholder was.
    ///
    /// Compared as text so one list covers the numeric ids and the string ones.
    /// `template_placeholders_are_all_listed` pins this against the template
    /// itself, so an id added there cannot drift past this list unnoticed.
    pub(crate) const OWNER_SEED_PLACEHOLDERS: &[&str] = &[
        "+15551234567",
        "123456789012345",
        "123456789",
        "U12345678",
        // The per-group allow-list example. Commented in the template
        // today, listed anyway so uncommenting it cannot make a
        // placeholder an owner.
        "222333444",
        "555666777",
    ];

    fn seed_bot_owner_edit(doc: &mut toml_edit::DocumentMut, channel: &str, list_key: &str) {
        let Some(t) = doc
            .get_mut("channels")
            .and_then(|c| c.as_table_mut())
            .and_then(|c| c.get_mut(channel))
            .and_then(|t| t.as_table_mut())
        else {
            return;
        };
        // A channel nobody has turned on has no operator to pin yet (#1440).
        // Seeding one now only records whatever the template happened to carry.
        if !t.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
            return;
        }
        let has_owner = t
            .get("bot_owner")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        if has_owner {
            return;
        }
        let Some(first) = t
            .get(list_key)
            .and_then(|v| v.as_array())
            .and_then(|a| a.get(0))
        else {
            return;
        };
        let mut owner = toml_edit::Array::new();
        let shown: String;
        if let Some(s) = first.as_str() {
            shown = s.to_string();
            owner.push(s);
        } else if let Some(i) = first.as_integer() {
            shown = i.to_string();
            owner.push(i);
        } else {
            return;
        }
        if Self::OWNER_SEED_PLACEHOLDERS.contains(&shown.as_str()) {
            tracing::warn!(
                "channels.{channel}: not seeding bot_owner from {shown}, that is a \
                 documentation placeholder from config.toml.example. Set \
                 channels.{channel}.bot_owner yourself, or clear {list_key} and let \
                 the first user to talk become the owner."
            );
            return;
        }
        // Silent before (#1440): an owner appeared on disk with nothing saying
        // where it came from, so the operator had no way to tell a seeded value
        // from one they set.
        tracing::info!(
            "channels.{channel}: seeded bot_owner = [{shown}] from the first \
             {list_key} entry (no bot_owner was set)"
        );
        t.insert("bot_owner", toml_edit::value(owner));
    }

    /// Get the system config path: ~/.opencrabs/config.toml
    pub fn system_config_path() -> Option<PathBuf> {
        Some(opencrabs_home().join("config.toml"))
    }

    /// Get the local config path: ./opencrabs.toml
    pub(crate) fn local_config_path() -> PathBuf {
        PathBuf::from("./opencrabs.toml")
    }

    /// Load and merge configuration from a TOML file
    fn merge_from_file(base: Self, path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {:?}", path))?;

        // Fold a legacy section name into its canonical one before serde sees
        // the document. A file carrying both `[gateway]` and `[a2a]` is one
        // feature written twice, and serde reads an alias as a second spelling
        // of the same field rather than a second field — so it failed with
        // `duplicate field` pointing at line 1, and the load path then treated
        // that like a syntax error and fell back to a stale snapshot (#1116).
        let mut doc: toml::Value = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config file: {:?}", path))?;
        let folded = crate::config::alias_merge::fold_legacy_sections(&mut doc);
        if !folded.is_empty() {
            tracing::info!(
                "Config: folded legacy section(s) {} into their current names — \
                 both spellings configure one feature",
                folded.join(", ")
            );
        }
        let file_config: Self = doc
            .try_into()
            .with_context(|| format!("Failed to parse config file: {:?}", path))?;

        Ok(Self::merge(base, file_config))
    }

    /// Merge two configs (file_config overwrites base where specified)
    fn merge(_base: Self, overlay: Self) -> Self {
        // For now, we'll do a simple overlay merge where overlay completely replaces base
        // In the future, we could make this more sophisticated with field-level merging
        Self {
            provider_registry: overlay.provider_registry,
            database: overlay.database,
            logging: overlay.logging,
            debug: overlay.debug,
            gui: overlay.gui,
            decisions: overlay.decisions,
            providers: overlay.providers,
            channels: overlay.channels,
            agent: overlay.agent,
            daemon: overlay.daemon,
            a2a: overlay.a2a,
            image: overlay.image,
            cron: overlay.cron,
            doctor: overlay.doctor,
            memory: overlay.memory,
            brain: overlay.brain,
            browser: overlay.browser,
            tui: overlay.tui,
            features: overlay.features,
        }
    }

    /// Apply environment variable overrides
    fn apply_env_overrides(mut config: Self) -> Result<Self> {
        // Database path
        if let Ok(db_path) = std::env::var("OPENCRABS_DB_PATH") {
            config.database.path = PathBuf::from(db_path);
        }

        // Log level
        if let Ok(log_level) = std::env::var("OPENCRABS_LOG_LEVEL") {
            config.logging.level = log_level;
        }

        // Log file
        if let Ok(log_file) = std::env::var("OPENCRABS_LOG_FILE") {
            config.logging.file = Some(PathBuf::from(log_file));
        }

        // Debug options
        if let Ok(debug_lsp) = std::env::var("OPENCRABS_DEBUG_LSP") {
            config.debug.debug_lsp = debug_lsp.parse().unwrap_or(false);
        }

        if let Ok(profiling) = std::env::var("OPENCRABS_PROFILING") {
            config.debug.profiling = profiling.parse().unwrap_or(false);
        }

        // provider registry options
        if let Ok(enabled) = std::env::var("OPENCRABS_PROVIDER_REGISTRY_ENABLED") {
            // Unparseable means off, not on (#972): an opt-in integration must
            // not switch itself on because someone typo'd the env var.
            config.provider_registry.enabled = enabled.parse().unwrap_or(false);
        }

        if let Ok(base_url) = std::env::var("OPENCRABS_PROVIDER_REGISTRY_URL") {
            config.provider_registry.base_url = base_url;
        }

        if let Ok(auto_update) = std::env::var("OPENCRABS_PROVIDER_REGISTRY_AUTO_UPDATE") {
            config.provider_registry.auto_update = auto_update.parse().unwrap_or(true);
        }

        Ok(config)
    }

    /// Reload configuration from disk (re-runs `Config::load()`).
    pub fn reload() -> Result<Self> {
        tracing::info!("Reloading configuration from disk");
        Self::load()
    }

    /// Write a key-value pair into the system config.toml using TOML merge.
    ///
    /// `section` is a dotted path like "agent" or "providers.tts.local".
    /// `key` is the field name inside that section.
    /// `value` is the TOML-serialisable value.
    ///
    /// JSON arrays and objects convert faithfully — objects become inline
    /// tables, arrays recurse (so arrays-of-tables like `memory.extra_paths`
    /// survive instead of being silently dropped, #87). Values TOML cannot
    /// represent (`null`, mixed-type arrays) hard-error BEFORE the file is
    /// touched. On success the returned `String` is the TOML rendering of the
    /// value actually written, so callers can echo reality, not the raw input.
    pub fn write_key(section: &str, key: &str, value: &str) -> Result<String> {
        // Sanitize: trim whitespace/newlines that may leak from TUI input
        let value = value.trim();

        // Parse the value — try JSON array/object, integer, float, bool, then
        // fall back to string.
        let parsed: toml_edit::Item = if value.starts_with('[') || value.starts_with('{') {
            match serde_json::from_str::<serde_json::Value>(value) {
                Ok(json) => toml_edit::Item::Value(json_to_toml_value(&json)?),
                // Bracketed but not valid JSON — a plain string that happens
                // to start with '['/'{' stays a string (e.g. "[in progress]").
                Err(_) => toml_edit::value(value),
            }
        } else if let Ok(v) = value.parse::<i64>() {
            toml_edit::value(v)
        } else if let Ok(v) = value.parse::<f64>() {
            toml_edit::value(v)
        } else if let Ok(v) = value.parse::<bool>() {
            toml_edit::value(v)
        } else {
            toml_edit::value(value)
        };

        let written = parsed.to_string();
        Self::write_item(section, key, parsed)?;
        Ok(written)
    }

    /// Write a config key as a TOML string, whatever the text looks like.
    ///
    /// [`Self::write_key`] infers the type, which is right for settings the
    /// user types but wrong for captured free text: a group named `2026` would
    /// land as an integer and no longer deserialize into a `String` field.
    pub fn write_key_string(section: &str, key: &str, value: &str) -> Result<()> {
        Self::write_item(section, key, toml_edit::value(value.trim()))
    }

    /// Shared read-modify-write behind [`Self::write_key`] and
    /// [`Self::write_key_string`]: navigate to the section, insert the
    /// already-typed item, and re-validate before touching the file.
    fn write_item(section: &str, key: &str, parsed: toml_edit::Item) -> Result<()> {
        use toml_edit::DocumentMut;

        // Hold lock for entire read-modify-write to prevent races between
        // concurrent write_key calls (e.g. fallback provider switching fires
        // multiple writes in rapid succession).
        let _guard = CONFIG_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let path =
            Self::system_config_path().unwrap_or_else(|| opencrabs_home().join("config.toml"));

        // A config.toml that does not exist yet is seeded from the shipped
        // example rather than conjured empty (#1437). The example is where
        // every provider section a user might want to edit comes from, and
        // seeding only ever fires while the file is absent, so writing one
        // bare key into an empty document used to lock it out for good.
        crate::config::seed::ensure_config_seeded();

        // Converge legacy spellings before the write: a file still carrying
        // [providers.zhipu] plus this write's [providers.zai] is one field
        // written twice as far as serde is concerned (the alias is another
        // spelling, not a second field), so the guard below would deny the
        // write with `duplicate field` until the file was hand-edited.
        // Renaming first converges the file with comments kept, same as the
        // load path already does.
        if let Err(e) = crate::config::alias_merge::migrate_file(&path) {
            tracing::warn!(
                "alias_merge: pre-write migration failed for {}: {e}",
                path.display()
            );
        }

        // Format-preserving parse — only the targeted key is modified
        let mut doc: DocumentMut = if path.exists() {
            fs::read_to_string(&path)?.parse()?
        } else {
            DocumentMut::new()
        };

        // Navigate/create the section table (supports dotted paths like "channels.slack")
        // Normalize custom provider names (e.g. "Qwen_2.5_4B" → "qwen-2-5-4b")
        let parts: Vec<String> = section
            .split('.')
            .enumerate()
            .map(|(i, p)| {
                // providers.custom.<name> — normalize the <name> part
                if i >= 2 && section.starts_with("providers.custom") {
                    normalize_toml_key(p)
                } else {
                    p.to_string()
                }
            })
            .collect();

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

        current.insert(key, parsed);

        // Write back
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Defense in depth (#714): never write a config.toml that won't load.
        let serialized = doc.to_string();
        if let Err(e) =
            crate::config::guard::parses(crate::config::guard::ProtectedFile::Config, &serialized)
        {
            tracing::error!(
                target: "config_guard",
                "Denied config.toml write [{section}].{key}: would break parsing: {e}"
            );
            anyhow::bail!(
                "config write denied: setting [{section}].{key} would make config.toml invalid \
                 ({e}). The file was NOT changed."
            );
        }

        // Schema guard (#83): deserialize the candidate through `Config`
        // with serde_ignored and refuse iff the struct would ignore a key at
        // the target path. The compiled struct is the registry — a section
        // the struct knows ([memory], [daemon], ...) writes without ceremony;
        // anything the struct would discard on load is an orphan by
        // construction and is refused (the #1199 rule, now struct-derived so
        // no hand-maintained section list can drift). The parse guard above
        // runs first, so a type error keeps surfacing with the #714 message.
        let doc_path_owned = parts.join(".");
        let doc_path = if doc_path_owned.is_empty() {
            section
        } else {
            doc_path_owned.as_str()
        };
        if let Err(e) = crate::config::sections::write_guard(doc_path, key, &serialized) {
            tracing::error!(
                target: "config_guard",
                "Denied config.toml write [{section}].{key}: {e}"
            );
            anyhow::bail!("config write denied: {e}");
        }

        // Back up before overwriting
        Self::backup_config(&path, 7);

        crate::config::types::io::atomic_write(&path, &serialized)?;
        tracing::info!("Wrote config key [{section}].{key}");
        Ok(())
    }

    /// Write a key-value pair into the system keys.toml using TOML merge.
    /// Same as write_key but targets keys.toml instead of config.toml.
    pub fn write_keys_key(section: &str, key: &str, value: &str) -> Result<()> {
        use toml_edit::DocumentMut;

        let value = value.trim();
        let _guard = CONFIG_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let path = opencrabs_home().join("keys.toml");

        // Seeded for the same reason config.toml is: the example carries a
        // slot per provider, and a file created around one written key has
        // none of them (#1437).
        crate::config::seed::ensure_keys_seeded();

        // Same pre-write convergence as write_item: an old keys.toml still
        // carrying [providers.zhipu] plus this write's [providers.zai] fails
        // the guard below as a duplicate field. Rename first, comments kept.
        if let Err(e) = crate::config::alias_merge::migrate_file(&path) {
            tracing::warn!(
                "alias_merge: pre-write keys migration failed for {}: {e}",
                path.display()
            );
        }

        let mut doc: DocumentMut = if path.exists() {
            fs::read_to_string(&path)?.parse()?
        } else {
            DocumentMut::new()
        };

        let parts: Vec<String> = section.split('.').map(|p| p.to_string()).collect();

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

        let parsed: toml_edit::Item = toml_edit::value(value);

        current.insert(key, parsed);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Defense in depth (#714): never write a keys.toml that won't load — a
        // broken secrets file loses every API key and the bot token.
        let serialized = doc.to_string();
        if let Err(e) =
            crate::config::guard::parses(crate::config::guard::ProtectedFile::Keys, &serialized)
        {
            tracing::error!(
                target: "config_guard",
                "Denied keys.toml write [{section}].{key}: would break parsing: {e}"
            );
            anyhow::bail!(
                "keys write denied: setting [{section}].{key} would make keys.toml invalid ({e}). \
                 The file was NOT changed."
            );
        }

        Self::backup_config(&path, 7);

        crate::config::types::io::atomic_write(&path, &serialized)?;
        tracing::info!("Wrote keys.toml key [{section}].{key}");
        Ok(())
    }

    /// Remove a dotted section from keys.toml. Mirror of `remove_section`
    /// but targets the secrets file. Used by the custom-provider rename
    /// path so the old `[providers.custom.<old>]` entry doesn't survive
    /// in keys.toml and get re-materialised on next load via
    /// `merge_provider_keys`'s "create minimal entry from keys.toml"
    /// fallback. Returns Ok(()) when the file or section doesn't exist
    /// — same shape as `remove_section` so callers can fire-and-forget
    /// with a single `.is_err()` check for the actual write failure.
    pub fn remove_secret_section(section: &str) -> Result<()> {
        use toml_edit::DocumentMut;

        let _guard = CONFIG_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let path = opencrabs_home().join("keys.toml");
        if !path.exists() {
            return Ok(());
        }

        let mut doc: DocumentMut = fs::read_to_string(&path)?.parse()?;

        let parts: Vec<&str> = section.split('.').collect();
        if parts.is_empty() {
            return Ok(());
        }

        let parent_parts = &parts[..parts.len() - 1];
        let leaf = parts[parts.len() - 1];

        let mut current = doc.as_table_mut();
        for part in parent_parts {
            match current.get_mut(part) {
                Some(v) if v.is_table() => {
                    current = v.as_table_mut().unwrap();
                }
                _ => return Ok(()),
            }
        }

        if current.remove(leaf).is_some() {
            tracing::info!("Removed keys.toml section [{section}]");
            Self::backup_config(&path, 7);
            crate::config::types::io::atomic_write(&path, &doc.to_string())?;
        }
        Ok(())
    }

    /// Remove a dotted section from config.toml.
    /// e.g. `remove_section("providers.custom.default")` removes `[providers.custom.default]`.
    pub fn remove_section(section: &str) -> Result<()> {
        use toml_edit::DocumentMut;

        let _guard = CONFIG_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let path =
            Self::system_config_path().unwrap_or_else(|| opencrabs_home().join("config.toml"));
        if !path.exists() {
            return Ok(());
        }

        let mut doc: DocumentMut = fs::read_to_string(&path)?.parse()?;

        let parts: Vec<&str> = section.split('.').collect();
        if parts.is_empty() {
            return Ok(());
        }

        // Navigate to the parent table and remove the last key
        let parent_parts = &parts[..parts.len() - 1];
        let leaf = parts[parts.len() - 1];

        let mut current = doc.as_table_mut();
        for part in parent_parts {
            match current.get_mut(part) {
                Some(v) if v.is_table() => {
                    current = v.as_table_mut().unwrap();
                }
                _ => return Ok(()), // parent doesn't exist, nothing to remove
            }
        }

        current.remove(leaf);
        tracing::info!("Removed config section [{section}]");

        Self::backup_config(&path, 7);
        crate::config::types::io::atomic_write(&path, &doc.to_string())?;
        Ok(())
    }

    /// Move a config section to a new key, keeping its contents.
    ///
    /// Returns `true` if the section existed and was moved. A destination that
    /// already exists is left alone and `false` is returned: the newer entry is
    /// the live one, and overwriting it with the stale copy would undo whatever
    /// has been configured since.
    ///
    /// Written for a Telegram group that migrates to a supergroup and takes a
    /// new chat id (#946) — the settings are still the user's, only the key
    /// they hang from changed.
    pub fn rename_section(from: &str, to: &str) -> Result<bool> {
        use toml_edit::DocumentMut;

        let _guard = CONFIG_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let path =
            Self::system_config_path().unwrap_or_else(|| opencrabs_home().join("config.toml"));
        if !path.exists() {
            return Ok(false);
        }
        let mut doc: DocumentMut = fs::read_to_string(&path)?.parse()?;

        let from_parts: Vec<&str> = from.split('.').collect();
        let to_parts: Vec<&str> = to.split('.').collect();
        let (Some(from_leaf), Some(to_leaf)) = (from_parts.last(), to_parts.last()) else {
            return Ok(false);
        };

        // Both keys are resolved under their own parents, so this also handles a
        // move between different parent tables.
        let Some(parent) = Self::descend_mut(doc.as_table_mut(), &from_parts) else {
            return Ok(false);
        };
        let Some(item) = parent.get(from_leaf).cloned() else {
            return Ok(false);
        };

        let Some(dest) = Self::descend_mut(doc.as_table_mut(), &to_parts) else {
            return Ok(false);
        };
        if dest.contains_key(to_leaf) {
            tracing::info!("Not moving [{from}] to [{to}]: destination already exists");
            return Ok(false);
        }
        dest.insert(to_leaf, item);

        if let Some(parent) = Self::descend_mut(doc.as_table_mut(), &from_parts) {
            parent.remove(from_leaf);
        }
        tracing::info!("Moved config section [{from}] to [{to}]");

        Self::backup_config(&path, 7);
        crate::config::types::io::atomic_write(&path, &doc.to_string())?;
        Ok(true)
    }

    /// Walk to the parent table of a dotted section path. `None` when any
    /// component is missing or is not a table.
    fn descend_mut<'a>(
        root: &'a mut toml_edit::Table,
        parts: &[&str],
    ) -> Option<&'a mut toml_edit::Table> {
        let mut current = root;
        for part in &parts[..parts.len().saturating_sub(1)] {
            current = current.get_mut(part)?.as_table_mut()?;
        }
        Some(current)
    }

    /// Clean up custom provider entries that have no base_url and no default_model.
    /// These are ghost entries created when disabling all providers on save.
    ///
    /// Calls `cleanup_keys_custom_providers` at the end so a keys.toml entry
    /// left behind by a rename / delete (whose corresponding config entry no
    /// longer exists) gets pruned in the same pass.
    pub fn cleanup_empty_custom_providers() {
        // Clean config.toml
        if let Ok(config) = Self::load()
            && let Some(customs) = &config.providers.custom
        {
            for (name, cfg) in customs {
                let is_empty_name = name.is_empty();
                let has_url = cfg.base_url.as_ref().is_some_and(|u| !u.is_empty());
                let has_model = cfg.default_model.as_ref().is_some_and(|m| !m.is_empty());
                if is_empty_name || (!has_url && !has_model) {
                    let section = format!("providers.custom.{}", name);
                    if let Err(e) = Self::remove_section(&section) {
                        tracing::warn!("Failed to remove empty custom provider '{}': {}", name, e);
                    }
                }
            }
        }

        // Clean keys.toml — remove empty-key entries and entries with no
        // matching config (ghost keys left by normalization mismatches).
        Self::cleanup_keys_custom_providers();
    }

    /// Remove ghost custom provider entries from keys.toml.
    ///
    /// Uses `raw_config_custom_provider_names()` (NOT `Self::load()`) to
    /// determine "what's in config.toml" — going through the loader's
    /// merge step would feed keys.toml back into itself and turn the
    /// orphan check into a no-op. See the inline note below.
    pub(crate) fn cleanup_keys_custom_providers() {
        use toml_edit::DocumentMut;

        let keys_file = keys_path();
        if !keys_file.exists() {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&keys_file) else {
            return;
        };
        let Ok(mut doc) = content.parse::<DocumentMut>() else {
            return;
        };

        // Navigate to providers.custom table
        let custom_table = match doc
            .as_table_mut()
            .get_mut("providers")
            .and_then(|t| t.as_table_mut())
            .and_then(|t| t.get_mut("custom"))
            .and_then(|t| t.as_table_mut())
        {
            Some(t) => t,
            None => return,
        };

        // Collect keys to remove: empty names or entries with no config
        // counterpart. CRITICAL: read config.toml RAW here — going
        // through `Self::load()` would invoke `merge_provider_keys`,
        // which RE-CREATES entries in config.providers.custom from
        // keys.toml itself (see line ~1878 "creating minimal entry").
        // That feedback loop made cleanup a no-op for orphans: a key
        // in keys.toml without a config entry was always rescued by
        // the merge, then "found" in config_names, then skipped. The
        // 2026-06-05 modelscope-qwen rename surfaced this — renaming
        // removed the config section but the old keys.toml section
        // survived, and the next /models open re-materialised the
        // ghost via merge_provider_keys.
        // If config.toml cannot be read or parsed, provider presence is
        // UNKNOWN. Removing keys based on unknown state is exactly how
        // #1458 happened: a duplicate-key parse error made every keys.toml
        // entry look like a ghost and the cleanup wiped all 21 custom
        // providers at startup. Fail open — keep everything.
        let Some(config_names) = raw_config_custom_provider_names() else {
            tracing::warn!(
                "config.toml unreadable or unparseable — skipping ghost-key cleanup; \
                 keys.toml left untouched"
            );
            return;
        };

        let remove: Vec<String> = custom_table
            .iter()
            .map(|(k, _)| k.to_string())
            .filter(|k| k.is_empty() || !config_names.contains(k))
            .collect();

        if remove.is_empty() {
            return;
        }

        for key in &remove {
            custom_table.remove(key);
            tracing::info!("Removed ghost key from keys.toml: providers.custom.{}", key);
        }

        daily_backup(&keys_file, 7);
        if let Err(e) = crate::config::types::io::atomic_write(&keys_file, &doc.to_string()) {
            tracing::warn!("Failed to clean ghost keys from keys.toml: {e}");
        }
    }

    /// Write a string array to a dotted config section.
    /// e.g. `write_array("channels.slack", "allowed_users", &["U123"])` →
    /// `[channels.slack] allowed_users = ["U123"]`
    pub fn write_array(section: &str, key: &str, values: &[String]) -> Result<()> {
        use toml_edit::DocumentMut;

        let path =
            Self::system_config_path().unwrap_or_else(|| opencrabs_home().join("config.toml"));

        let mut doc: DocumentMut = if path.exists() {
            fs::read_to_string(&path)?.parse()?
        } else {
            DocumentMut::new()
        };

        // Navigate/create nested section
        let parts: Vec<&str> = section.split('.').collect();
        let mut current = doc.as_table_mut();

        for part in &parts {
            if current.get(part).is_none() {
                current.insert(part, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            current = current
                .get_mut(part)
                .context("section not found after insert")?
                .as_table_mut()
                .with_context(|| format!("'{}' is not a table", part))?;
        }

        let mut arr = toml_edit::Array::new();
        for v in values {
            arr.push(v.as_str());
        }
        current.insert(key, toml_edit::value(arr));

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Defense in depth (#714): never write a config.toml that won't load.
        let serialized = doc.to_string();
        if let Err(e) =
            crate::config::guard::parses(crate::config::guard::ProtectedFile::Config, &serialized)
        {
            tracing::error!(
                target: "config_guard",
                "Denied config.toml array write [{section}].{key}: would break parsing: {e}"
            );
            anyhow::bail!(
                "config write denied: setting [{section}].{key} would make config.toml invalid \
                 ({e}). The file was NOT changed."
            );
        }
        Self::backup_config(&path, 7);
        crate::config::types::io::atomic_write(&path, &serialized)?;
        tracing::info!(
            "Wrote config array [{section}].{key} ({} items)",
            values.len()
        );
        Ok(())
    }

    /// Validate configuration
    /// Check if any provider has an API key configured (from config).
    pub fn has_any_api_key(&self) -> bool {
        let has_anthropic = self
            .providers
            .anthropic
            .as_ref()
            .is_some_and(|p| p.api_key.is_some());
        let has_openai = self
            .providers
            .openai
            .as_ref()
            .is_some_and(|p| p.api_key.is_some());
        let has_gemini = self
            .providers
            .gemini
            .as_ref()
            .is_some_and(|p| p.api_key.is_some());

        has_anthropic || has_openai || has_gemini
    }

    pub fn validate(&self) -> Result<()> {
        tracing::debug!("Validating configuration...");

        // Validate database path parent directory exists
        if let Some(parent) = self.database.path.parent()
            && !parent.exists()
        {
            tracing::warn!(
                "Database parent directory does not exist, will be created: {:?}",
                parent
            );
        }

        // Validate log level
        let valid_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_levels.contains(&self.logging.level.as_str()) {
            anyhow::bail!(
                "Invalid log level: {}. Must be one of: {:?}",
                self.logging.level,
                valid_levels
            );
        }

        // Validate provider registry URL if enabled
        if self.provider_registry.enabled && self.provider_registry.base_url.is_empty() {
            anyhow::bail!("provider registry is enabled but base_url is empty");
        }

        // #1648: [decisions] tiers are opt-in contracts; a half-written one
        // must stop the load rather than run a ring on undefined semantics.
        for (tier_id, tier) in &self.decisions.tiers {
            if tier.policy_version.trim().is_empty() {
                anyhow::bail!(
                    "decisions.tiers.{tier_id}: policy_version is required \
                     (bump it whenever the tier's decision semantics change)"
                );
            }
            if tier.similarity {
                anyhow::bail!(
                    "decisions.tiers.{tier_id}: similarity reuse is not implemented (#1648 L2); \
                     set similarity = false"
                );
            }
            if let Some(ttl) = tier.ttl_hours
                && ttl <= 0
            {
                anyhow::bail!(
                    "decisions.tiers.{tier_id}: ttl_hours must be positive when set (got {ttl})"
                );
            }
        }

        tracing::debug!("Configuration validation passed");
        Ok(())
    }

    /// Daily backup before writing. Delegates to the standalone function.
    fn backup_config(path: &Path, max_days: usize) {
        daily_backup(path, max_days);
    }

    /// Save configuration to a file
    pub fn save(&self, path: &Path) -> Result<()> {
        let _guard = CONFIG_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let toml_string =
            toml::to_string_pretty(self).context("Failed to serialize config to TOML")?;

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {:?}", parent))?;
        }

        // Back up before overwriting
        Self::backup_config(path, 7);

        crate::config::types::io::atomic_write(path, &toml_string)
            .with_context(|| format!("Failed to write config file: {:?}", path))?;

        tracing::info!("Configuration saved to: {:?}", path);
        Ok(())
    }
}

/// Inject commented-out subagent provider/model defaults into the [agent] section.
///
/// Used by migration so users can discover the feature by uncommenting lines in config.toml.
/// Pure text-level operation — preserves all existing formatting and comments.
fn inject_subagent_defaults(content: &str) -> Result<String> {
    let comment_block = "\n# Sub-agent routing — override the parent session's provider for\n# spawned agents, team members, and background workers.\n# subagent_provider = \"anthropic\"    # e.g. openrouter, minimax, or a [providers.custom.<name>] name such as ollama\n# subagent_model = \"claude-sonnet-4-6\"  # only used when subagent_provider is set\n";
    inject_into_agent_section(content, comment_block)
}

/// Commented plan-mode routing defaults, so the keys are discoverable in an
/// existing config rather than only in the README (#793).
///
/// All four stay commented: uncommenting is opt-in, and an untouched config
/// must keep routing every turn to the session's own provider.
fn inject_plan_mode_defaults(content: &str) -> Result<String> {
    let comment_block = "\n# Plan-mode routing: run /plan and /execute on their own provider+model.\n# Drafting applies between /plan and approval; executing applies from\n# approval until the plan is 100% complete, then the session returns to\n# its own pair. Unset (the default) changes nothing.\n# plan_provider = \"anthropic\"          # omit to keep the session's provider\n# plan_model = \"claude-opus-4-6\"       # alone, swaps the model only\n# execute_provider = \"openrouter\"\n# execute_model = \"qwen3.8-max\"\n";
    inject_into_agent_section(content, comment_block)
}

/// Append a comment block to the end of the `[agent]` section.
///
/// Text-level insertion because comments cannot survive a `toml::Value`
/// round-trip, which is why this is not done with the structural editor.
fn inject_into_agent_section(content: &str, comment_block: &str) -> Result<String> {
    // Find [agent] section boundary
    let marker = "[agent]";
    let agent_pos = content
        .find(marker)
        .ok_or_else(|| anyhow::anyhow!("No [agent] section found"))?;

    let section_start = agent_pos + marker.len();

    // Find end of [agent] section: next section header or EOF
    let rest = &content[section_start..];
    let next_section = rest.find("\n[");
    let section_end = if let Some(pos) = next_section {
        section_start + pos
    } else {
        content.len()
    };

    // Insert the comment block at the end of the section
    let mut out = String::with_capacity(content.len() + comment_block.len());
    out.push_str(&content[..section_end]);
    let before = &content[..section_end];
    let ends_with_blank = before.ends_with("\n\n");
    if !ends_with_blank {
        out.push('\n');
    }
    out.push_str(comment_block);
    out.push_str(&content[section_end..]);
    Ok(out)
}

/// Recursively convert a JSON value into a `toml_edit::Value`.
///
/// Faithful for every JSON shape TOML can represent: strings, integers,
/// floats, booleans, arrays (recursively) and objects → inline tables. Two
/// shapes hard-error BEFORE any file access because TOML cannot represent
/// them without silent loss (#87):
/// - `null` — TOML has no null;
/// - mixed-type arrays — TOML v1.0 arrays must be homogeneous; integer and
///   float count as one family (`[0.1, 1, 2]` is legal TOML), but e.g. a
///   string next to a table would have to be dropped or stringified.
fn json_to_toml_value(json: &serde_json::Value) -> Result<toml_edit::Value> {
    use serde_json::Value as J;
    let v = match json {
        J::Null => {
            anyhow::bail!("null is not representable in TOML — the value was NOT written");
        }
        J::String(s) => toml_edit::Value::from(s.clone()),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml_edit::Value::from(i)
            } else if let Some(f) = n.as_f64() {
                toml_edit::Value::from(f)
            } else {
                anyhow::bail!(
                    "number `{n}` is not representable as a TOML integer or float — \
                     the value was NOT written"
                );
            }
        }
        J::Bool(b) => toml_edit::Value::from(*b),
        J::Array(items) => {
            let mut arr = toml_edit::Array::new();
            let mut family: Option<&'static str> = None;
            for item in items {
                let value = json_to_toml_value(item)?;
                let fam = toml_family(&value);
                if let Some(prev) = family {
                    if prev != fam {
                        anyhow::bail!(
                            "array mixes types `{prev}` and `{fam}` — TOML arrays must be \
                             homogeneous (#87). The value was NOT written."
                        );
                    }
                } else {
                    family = Some(fam);
                }
                arr.push(value);
            }
            toml_edit::Value::from(arr)
        }
        J::Object(map) => {
            let mut tbl = toml_edit::InlineTable::new();
            for (k, v) in map {
                tbl.insert(k.clone(), json_to_toml_value(v)?);
            }
            toml_edit::Value::from(tbl)
        }
    };
    Ok(v)
}

/// The TOML type family of a value, used for the array-homogeneity check.
/// Integer and float share one family (`[0.1, 1, 2]` is legal TOML #87).
fn toml_family(v: &toml_edit::Value) -> &'static str {
    match v {
        toml_edit::Value::Integer(_) | toml_edit::Value::Float(_) => "number",
        toml_edit::Value::String(_) => "string",
        toml_edit::Value::Boolean(_) => "boolean",
        toml_edit::Value::Array(_) => "array",
        toml_edit::Value::InlineTable(_) => "table",
        toml_edit::Value::Datetime(_) => "datetime",
    }
}

/// Resolve provider display name and model from config.
///
/// Walks `ProviderConfigs::provider_registry()` (the single source of
/// truth) in priority order and returns the display name + active model
/// for the first matching provider. Falls through to the first active
/// custom provider, otherwise `("Not configured", "N/A")`.
///
/// Replaces the prior 130-line if-else ladder that hardcoded a subset of
/// providers and silently omitted `opencode`, `ollama`, `bedrock`, and
/// `vertex` from the TUI display for months (#141). Adding a new
/// provider now only requires one new line in `provider_registry()`.
#[allow(clippy::items_after_test_module)]
pub fn resolve_provider_from_config(config: &Config) -> (&str, &str) {
    for (_id, display, requires_api_key, cfg) in config.providers.provider_registry() {
        if let Some(c) = cfg
            && c.enabled
            && (!requires_api_key || c.api_key.is_some())
        {
            let model = c.default_model.as_deref().unwrap_or("(default)");
            return (display, model);
        }
    }
    if let Some((name, cfg)) = config.providers.active_custom() {
        let model = cfg.default_model.as_deref().unwrap_or("default");
        return (name, model);
    }
    ("Not configured", "N/A")
}
