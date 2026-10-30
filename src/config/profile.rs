//! Profile management — multi-instance isolated OpenCrabs environments.
//!
//! Each profile gets its own `config.toml`, `keys.toml`, `opencrabs.db`,
//! `memory/`, brain files, and `layout.json`. The "default" profile maps
//! to `~/.opencrabs/` for backward compatibility; named profiles live
//! under `~/.opencrabs/profiles/<name>/`.
//!
//! Selection priority (first wins):
//! 1. `set_active_profile()` (called from CLI `-p` flag)
//! 2. `OPENCRABS_PROFILE` environment variable
//! 3. Falls back to "default"
//!
//! ## TUI footer display
//!
//! The status bar in `src/tui/render/input.rs::render_status_bar` shows
//! a `profile: <name>` chip ONLY when `active_profile()` returns
//! `Some(name)` (issue #167). When it returns `None` (no `-p`, no env)
//! the chip is omitted entirely — there is no real profile by that name
//! on disk, so the footer would otherwise have to invent a `default`
//! label that doesn't exist anywhere. Named profiles always show up;
//! the base directory stays unannotated.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Global active profile name. Set once at startup before anything calls `opencrabs_home()`.
static ACTIVE_PROFILE: OnceLock<Option<String>> = OnceLock::new();

tokio::task_local! {
    static PROFILE_HOME_OVERRIDE: PathBuf;
    // The profile NAME for the current task, set alongside the home override.
    // Lets `current_profile_name()` attribute data (e.g. a cron job's
    // profile_name) to the profile actually running, not the process global.
    static PROFILE_NAME_OVERRIDE: String;
}

/// Resolve the home directory for an explicit profile name, WITHOUT reading the
/// process-global active profile.
///
/// - `None` / `"default"` → `~/.opencrabs/`
/// - `"ops"` → `~/.opencrabs/profiles/ops/`
pub fn home_for_profile(name: Option<&str>) -> PathBuf {
    let base = base_opencrabs_dir();
    match name {
        None | Some("default") => base,
        Some(n) => base.join("profiles").join(n),
    }
}

/// Run an async future with the profile home pointed at `profile`'s directory.
///
/// The override is a `tokio::task_local!`, so it lives for the entire duration
/// of `fut` including across every `.await`. It is scoped to this one tokio task
/// only, it never leaks to sibling tasks or other jobs on the scheduler. Every
/// call to `opencrabs_home()` / `resolve_profile_home()` inside `fut` (including
/// deep inside tools like memory writes, config reads, brain file ops) resolves
/// to the correct profile home.
pub async fn with_profile_home_async<F, T>(profile: Option<&str>, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let home = home_for_profile(profile);
    let name = profile.unwrap_or("default").to_string();
    PROFILE_NAME_OVERRIDE
        .scope(name, PROFILE_HOME_OVERRIDE.scope(home, fut))
        .await
}

/// The profile name for the CURRENT task. Returns the task-local name set by
/// `with_profile_home_async`/`with_profile_home` when present, otherwise the
/// process-global active profile (or "default"). Use this when stamping data
/// (like a cron job's `profile_name`) so it attributes to the profile actually
/// executing, not the process default.
pub fn current_profile_name() -> String {
    if let Ok(name) = PROFILE_NAME_OVERRIDE.try_with(|n| n.clone()) {
        return name;
    }
    active_profile().unwrap_or("default").to_string()
}

/// Run a sync closure with the profile home pointed at `profile`'s directory.
///
/// Convenience wrapper for synchronous loads (Config::load(), BrainLoader, etc.)
/// that don't have an async context. Internally blocks on the task-local scope.
pub fn with_profile_home<T>(profile: Option<&str>, f: impl FnOnce() -> T) -> T {
    let home = home_for_profile(profile);
    let name = profile.unwrap_or("default").to_string();
    // Use a minimal tokio runtime to host the task-local scope for sync callers.
    // This is only used for initial config/brain materialization before the
    // async agent runs.
    PROFILE_NAME_OVERRIDE.sync_scope(name, || PROFILE_HOME_OVERRIDE.sync_scope(home, f))
}

/// Run a sync closure with `opencrabs_home()` pointed at an explicit directory.
///
/// Unlike `with_profile_home`, the directory is arbitrary rather than derived
/// from a profile name, and unlike setting `$HOME` the override is task-local:
/// it is invisible to every other thread. That distinction is what makes it
/// safe under a parallel test run — pointing `$HOME` at a tempdir redirects
/// config reads for the WHOLE process, so an unrelated test loading config at
/// that moment reads, repairs, and rewrites the tempdir's config.toml out from
/// under whoever set it (#912).
pub fn with_home_override<T>(home: PathBuf, f: impl FnOnce() -> T) -> T {
    PROFILE_HOME_OVERRIDE.sync_scope(home, f)
}

/// Run a future with `opencrabs_home()` pointed at an explicit directory.
///
/// Async counterpart of `with_home_override`: the override is a task-local,
/// so it survives every `.await` inside `fut` and never leaks to sibling
/// tasks. Use this (not the sync wrapper) inside `#[tokio::test]` bodies —
/// `sync_scope` cannot host a `.await`.
pub async fn with_home_override_async<T, F>(home: PathBuf, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    PROFILE_HOME_OVERRIDE.scope(home, fut).await
}

/// Set the active profile. Must be called before any `opencrabs_home()` call.
/// Returns `Err` if called more than once (OnceLock semantics).
pub fn set_active_profile(name: Option<String>) -> Result<()> {
    ACTIVE_PROFILE
        .set(name)
        .map_err(|_| anyhow::anyhow!("active profile already set"))
}

/// Get the active profile name, or `None` for default.
pub fn active_profile() -> Option<&'static str> {
    ACTIVE_PROFILE.get().and_then(|opt| opt.as_deref())
}

/// Whether the active profile lock has been written at all (#983).
/// `active_profile()` cannot distinguish "default profile" from "not yet
/// initialized": main() writes the lock before logging, and cli::run()
/// must skip its own set instead of logging a spurious warning.
pub fn is_profile_set() -> bool {
    ACTIVE_PROFILE.get().is_some()
}

/// Resolve the home directory for the active profile.
///
/// - `None` / `"default"` → `~/.opencrabs/`
/// - `"hermes"` → `~/.opencrabs/profiles/hermes/`
///
/// A task-local override wins when set (cron job running under a foreign
/// profile, #182). It persists across every `.await` inside the task, so
/// all tool calls during agent execution see the right home.
pub fn resolve_profile_home() -> PathBuf {
    // Task-local override: set by with_profile_home_async() for the entire
    // lifetime of a cron job's tokio task. Every opencrabs_home() call inside
    // tools (memory writes, config reads, file ops) resolves here first.
    if let Ok(home) = PROFILE_HOME_OVERRIDE.try_with(|h| h.clone()) {
        return home;
    }

    let base = base_opencrabs_dir();

    let profile_name = active_profile().map(String::from).or_else(|| {
        std::env::var("OPENCRABS_PROFILE")
            .ok()
            .filter(|s| !s.is_empty())
    });

    match profile_name.as_deref() {
        None | Some("default") => base,
        Some(name) => base.join("profiles").join(name),
    }
}

/// The raw `~/.opencrabs/` directory (profile-agnostic).
pub fn base_opencrabs_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".opencrabs")
}

// ─── Profile Registry ────────────────────────────────────────────────

/// Metadata for a single profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    pub last_used: Option<String>,
}

/// Registry of all profiles, stored at `~/.opencrabs/profiles.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileRegistry {
    #[serde(default)]
    pub profiles: HashMap<String, ProfileEntry>,
}

impl ProfileRegistry {
    fn path() -> PathBuf {
        base_opencrabs_dir().join("profiles.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        // Atomic write: write to temp file then rename to prevent concurrent
        // readers from seeing a partially-written file.
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, &contents).with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))
    }

    /// Atomically load, modify, and save the registry under a file lock.
    /// Prevents concurrent load+save races (e.g. two `create_profile` calls).
    pub fn modify<F>(f: F) -> Result<Self>
    where
        F: FnOnce(&mut Self),
    {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Advisory lock file — prevents concurrent modify() calls
        let lock_path = path.with_extension("toml.lock");
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("failed to open lock {}", lock_path.display()))?;

        // Platform-specific exclusive lock
        #[cfg(unix)]
        {
            use crate::config::flock::{FlockOutcome, exclusive};
            use std::os::unix::io::AsRawFd;
            match exclusive(lock_file.as_raw_fd(), false) {
                FlockOutcome::Acquired => {}
                // A blocking request waits rather than reporting contention,
                // so this arm only fires if that ever changes.
                FlockOutcome::Held => {
                    bail!(
                        "failed to lock {}: held by another process",
                        lock_path.display()
                    )
                }
                FlockOutcome::Failed(e) => {
                    bail!("failed to lock {}: {}", lock_path.display(), e)
                }
            }
        }
        #[cfg(windows)]
        {
            // The comment that used to sit here claimed "opening with write +
            // no sharing provides exclusion". std::fs opens with
            // FILE_SHARE_ALL, so that was never true and this block was a
            // no-op. LockFileEx is the real analogue of the flock arm above;
            // the OS releases the range when `lock_file` drops below.
            use crate::config::winlock::LockOutcome;
            use std::os::windows::io::AsRawHandle;
            match crate::config::winlock::exclusive(lock_file.as_raw_handle(), false) {
                LockOutcome::Acquired => {}
                // A blocking request waits rather than reporting contention,
                // so this arm only fires if that ever changes.
                LockOutcome::Held => {
                    bail!(
                        "failed to lock {}: held by another process",
                        lock_path.display()
                    )
                }
                LockOutcome::Failed(e) => {
                    bail!("failed to lock {}: {}", lock_path.display(), e)
                }
            }
        }

        // Load current state under lock
        let mut registry = Self::load()?;
        f(&mut registry);
        registry.save()?;

        // Lock released when lock_file drops
        // Explicitly flush to keep the borrow checker happy
        let _ = lock_file;

        Ok(registry)
    }

    pub fn register(&mut self, name: &str, description: Option<&str>) {
        self.profiles.insert(
            name.to_string(),
            ProfileEntry {
                name: name.to_string(),
                description: description.map(String::from),
                created_at: Utc::now().to_rfc3339(),
                last_used: None,
            },
        );
    }

    pub fn touch(&mut self, name: &str) {
        if let Some(entry) = self.profiles.get_mut(name) {
            entry.last_used = Some(Utc::now().to_rfc3339());
        }
    }
}

// ─── Profile CRUD ────────────────────────────────────────────────────

/// Create a new named profile with its directory structure AND the
/// default brain-file templates seeded into the profile's root.
///
/// Without seeding, a freshly created profile starts with an empty
/// brain dir. `RsiSync::sync_templates` skips files that don't exist
/// locally (by design — it never CREATES brain files, only updates
/// existing ones), so the empty state was sticky: no SOUL.md, no
/// TOOLS.md, no SECURITY.md, etc. until the user manually copied them
/// or re-ran the full onboarding wizard with the profile active.
/// Issue noticed during the audit of #120's `ops` profile breakdown.
///
/// starts with a working brain identical to a fresh default install.
pub fn create_profile(name: &str, description: Option<&str>) -> Result<PathBuf> {
    validate_profile_name(name)?;

    let profile_dir = base_opencrabs_dir().join("profiles").join(name);
    if profile_dir.exists() {
        bail!(
            "profile '{}' already exists at {}",
            name,
            profile_dir.display()
        );
    }

    // Create directory structure
    fs::create_dir_all(&profile_dir)?;
    fs::create_dir_all(profile_dir.join("memory"))?;
    fs::create_dir_all(profile_dir.join("logs"))?;

    // Seed the default brain-file templates so the profile starts with
    // a working brain. Errors here are logged but non-fatal — the
    // profile still gets registered; the user can re-seed by running
    // the onboarding wizard with the profile active.
    seed_brain_templates(&profile_dir);

    // Register under file lock to prevent concurrent write races
    let name_owned = name.to_string();
    let desc_owned = description.map(|s| s.to_string());
    ProfileRegistry::modify(|reg| {
        reg.register(&name_owned, desc_owned.as_deref());
    })?;

    tracing::info!("Created profile '{}' at {}", name, profile_dir.display());
    Ok(profile_dir)
}

/// Write the default brain-file templates into `profile_dir`. The exact
/// same list the onboarding wizard's `seed_templates` path writes, so
/// CLI `profile create` and TUI onboarding produce identical brain
/// starting states. Files already present in `profile_dir` are NOT
/// overwritten (defensive — this function is also safe to call from
/// the template-sync recovery path).
pub(crate) fn seed_brain_templates(profile_dir: &Path) {
    // Inline the canonical template set rather than reaching into the
    // TUI module — `config` must not depend on `tui`. The templates
    // are baked into the binary at compile time via `include_str!`,
    // so this list and the TUI's `TEMPLATE_FILES` stay in lockstep
    // because both reference the same files under
    // `src/docs/reference/templates/`.
    const TEMPLATES: &[(&str, &str)] = &[
        (
            "SOUL.md",
            include_str!("../docs/reference/templates/SOUL.md"),
        ),
        (
            "USER.md",
            include_str!("../docs/reference/templates/USER.md"),
        ),
        (
            "AGENTS.md",
            include_str!("../docs/reference/templates/AGENTS.md"),
        ),
        (
            "TOOLS.md",
            include_str!("../docs/reference/templates/TOOLS.md"),
        ),
        (
            "MEMORY.md",
            include_str!("../docs/reference/templates/MEMORY.md"),
        ),
        (
            "CODE.md",
            include_str!("../docs/reference/templates/CODE.md"),
        ),
        (
            "SECURITY.md",
            include_str!("../docs/reference/templates/SECURITY.md"),
        ),
        (
            "BOOT.md",
            include_str!("../docs/reference/templates/BOOT.md"),
        ),
        (
            "HEARTBEAT.md",
            include_str!("../docs/reference/templates/HEARTBEAT.md"),
        ),
    ];

    for (filename, content) in TEMPLATES {
        let target = profile_dir.join(filename);
        if target.exists() {
            continue;
        }
        if let Err(e) = fs::write(&target, content) {
            tracing::warn!(
                "create_profile: failed to seed {} in {}: {e}",
                filename,
                profile_dir.display(),
            );
        }
    }

    // Seed the brain-verify belief base (#881). Without it the Orient gate is
    // inert — or hard-fails on the autonomous self_improve path. Idempotent:
    // never overwrites a user's customized rules. Seeded here so profile create
    // AND the periodic rsi_sync both ensure existing homes carry it.
    let safety_dir = profile_dir.join("safety");
    let verify_path = safety_dir.join("brain_verify.toml");
    if !verify_path.exists() {
        if let Err(e) = fs::create_dir_all(&safety_dir) {
            tracing::warn!(
                "seed_brain_templates: failed to create safety dir {}: {e}",
                safety_dir.display()
            );
        } else if let Err(e) = fs::write(
            &verify_path,
            include_str!("../docs/reference/templates/brain_verify.toml"),
        ) {
            tracing::warn!(
                "seed_brain_templates: failed to seed {}: {e}",
                verify_path.display()
            );
        }
    }

    // Seed the Ralph verification gate (#1902, shipping what #872 claimed):
    // without a file at safety/ralph_loop.toml the gate silently resolved to
    // Disabled, so every completion outside a project with its own copy
    // passed unverified with only a DEBUG line as signal. Idempotent the
    // same way: a user's customized file is never touched.
    let ralph_path = safety_dir.join("ralph_loop.toml");
    if !ralph_path.exists() {
        if let Err(e) = fs::create_dir_all(&safety_dir) {
            tracing::warn!(
                "seed_brain_templates: failed to create safety dir {}: {e}",
                safety_dir.display()
            );
        } else if let Err(e) = fs::write(
            &ralph_path,
            include_str!("../docs/reference/templates/ralph_loop.toml"),
        ) {
            tracing::warn!(
                "seed_brain_templates: failed to seed {}: {e}",
                ralph_path.display()
            );
        }
    }
}

/// Ensure the active profile's home carries the full set of brain-file
/// templates. Called at every CLI entrypoint (#1382) so a brain exists no
/// matter how the user arrived: wizard aborted midway, daemon-only install,
/// docker, channel-first setup, or `opencrabs init` (which writes config
/// only). Templates are compiled into the binary (`include_str!`), so this
/// needs no network and works on first open, offline.
///
/// Safe on every boot: `seed_brain_templates` is idempotent and
/// never-overwrite, so user-customized and AI-generated brain content
/// survive untouched. The #926 guarantee still holds — the wizard's
/// finalize step overwrites static templates with AI-generated content
/// when the user completes BrainSetup.
pub fn ensure_brain_seeded() {
    let home = crate::config::opencrabs_home();
    seed_brain_templates(&home);
}

/// List all profiles (always includes "default").
pub fn list_profiles() -> Result<Vec<ProfileEntry>> {
    let registry = ProfileRegistry::load()?;

    let mut profiles = vec![ProfileEntry {
        name: "default".to_string(),
        description: Some("Default profile (~/.opencrabs/)".to_string()),
        created_at: String::new(),
        last_used: None,
    }];

    let mut named: Vec<_> = registry.profiles.values().cloned().collect();
    named.sort_by(|a, b| a.name.cmp(&b.name));
    profiles.extend(named);

    Ok(profiles)
}

/// Delete a named profile and its directory.
pub fn delete_profile(name: &str) -> Result<()> {
    if name == "default" {
        bail!("cannot delete the default profile");
    }

    let profile_dir = base_opencrabs_dir().join("profiles").join(name);
    if !profile_dir.exists() {
        bail!("profile '{}' does not exist", name);
    }

    fs::remove_dir_all(&profile_dir).with_context(|| {
        format!(
            "failed to delete profile directory: {}",
            profile_dir.display()
        )
    })?;

    let name_owned = name.to_string();
    ProfileRegistry::modify(|reg| {
        reg.profiles.remove(&name_owned);
    })?;

    tracing::info!("Deleted profile '{}'", name);
    Ok(())
}

/// Export a profile as a tar.gz archive.
pub fn export_profile(name: &str, output: &Path) -> Result<()> {
    let profile_dir = if name == "default" {
        base_opencrabs_dir()
    } else {
        let dir = base_opencrabs_dir().join("profiles").join(name);
        if !dir.exists() {
            bail!("profile '{}' does not exist", name);
        }
        dir
    };

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::Builder;

    let file = fs::File::create(output)
        .with_context(|| format!("failed to create {}", output.display()))?;
    let enc = GzEncoder::new(file, Compression::default());
    let mut tar = Builder::new(enc);

    // Add profile directory contents
    tar.append_dir_all(name, &profile_dir)
        .with_context(|| "failed to add profile to archive")?;

    tar.finish()?;
    tracing::info!("Exported profile '{}' to {}", name, output.display());
    Ok(())
}

/// Import a profile from a tar.gz archive.
pub fn import_profile(archive: &Path) -> Result<String> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    if !archive.exists() {
        bail!("archive not found: {}", archive.display());
    }

    let file = fs::File::open(archive)?;
    let dec = GzDecoder::new(file);
    let mut ar = Archive::new(dec);

    // Peek at the first entry to get the profile name
    let profile_name = {
        let file = fs::File::open(archive)?;
        let dec = GzDecoder::new(file);
        let mut ar = Archive::new(dec);
        let first = ar.entries()?.next();
        match first {
            Some(Ok(entry)) => {
                let path = entry.path()?;
                path.components()
                    .next()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .unwrap_or_default()
            }
            _ => bail!("archive is empty"),
        }
    };

    if profile_name.is_empty() {
        bail!("could not determine profile name from archive");
    }

    let target = base_opencrabs_dir().join("profiles");
    fs::create_dir_all(&target)?;

    ar.unpack(&target)
        .with_context(|| "failed to extract archive")?;

    // Register the imported profile under file lock
    let pname = profile_name.clone();
    ProfileRegistry::modify(|reg| {
        if !reg.profiles.contains_key(&pname) {
            reg.register(&pname, Some("Imported profile"));
        }
    })?;

    tracing::info!(
        "Imported profile '{}' from {}",
        profile_name,
        archive.display()
    );
    Ok(profile_name)
}

// ─── Profile Migration ───────────────────────────────────────────────

/// Migrate config and brain files from one profile to another.
/// Copies `*.md`, `*.toml`, and `memory/` directory.
/// Does NOT copy database, sessions, logs, locks, or layout.
pub fn migrate_profile(from: &str, to: &str, force: bool) -> Result<Vec<String>> {
    let base = base_opencrabs_dir();

    let src_dir = if from == "default" {
        base.clone()
    } else {
        let dir = base.join("profiles").join(from);
        if !dir.exists() {
            bail!("source profile '{}' does not exist", from);
        }
        dir
    };

    let dst_dir = if to == "default" {
        base.clone()
    } else {
        let dir = base.join("profiles").join(to);
        if !dir.exists() {
            bail!(
                "destination profile '{}' does not exist. Create it first with: opencrabs profile create {}",
                to,
                to
            );
        }
        dir
    };

    if src_dir == dst_dir {
        bail!("source and destination profiles are the same");
    }

    let mut migrated = Vec::new();

    // Copy top-level *.md and *.toml files (config, keys, brain files)
    // Skip: profiles.toml (registry), layout.json, locks, DB
    let skip_files = ["profiles.toml", "layout.json"];

    for entry in fs::read_dir(&src_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_file() {
            let dominated = name_str.ends_with(".md") || name_str.ends_with(".toml");
            if !dominated || skip_files.contains(&name_str.as_ref()) {
                continue;
            }

            let dst_path = dst_dir.join(&name);
            if dst_path.exists() && !force {
                tracing::warn!(
                    "Skipping '{}' — already exists in '{}' (use --force to overwrite)",
                    name_str,
                    to
                );
                continue;
            }

            fs::copy(&path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    path.display(),
                    dst_path.display()
                )
            })?;
            migrated.push(name_str.to_string());
        }
    }

    // Copy memory/ directory
    let src_memory = src_dir.join("memory");
    if src_memory.exists() && src_memory.is_dir() {
        let dst_memory = dst_dir.join("memory");
        fs::create_dir_all(&dst_memory)?;

        for entry in fs::read_dir(&src_memory)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                let dst_path = dst_memory.join(&name);

                if dst_path.exists() && !force {
                    tracing::warn!(
                        "Skipping memory/'{}' — already exists (use --force to overwrite)",
                        name_str
                    );
                    continue;
                }

                fs::copy(&path, &dst_path)?;
                migrated.push(format!("memory/{}", name_str));
            }
        }
    }

    tracing::info!(
        "Migrated {} files from profile '{}' to '{}'",
        migrated.len(),
        from,
        to
    );
    Ok(migrated)
}

// ─── Token Lock ──────────────────────────────────────────────────────

/// Parse the owner PID from the second field of a lock file.
///
/// The field is `pid:creation-time` wherever the platform can read a creation
/// time (Windows, Linux, macOS) and a bare `pid` where it cannot (see
/// [`owner_stamp`]), so the PID is the *leading* `:`-separated half. Reading
/// the whole field as a number is not a style question: a live owner's stamp
/// then fails to parse, the caller reads that as corruption, and it overwrites
/// a lock that was protecting a running instance -- exactly the double-start
/// this file exists to prevent.
///
/// Returns `None` when the field names no live owner: a missing/zero PID, or a
/// corrupted value (e.g. an external in-place edit concatenated entries, so the
/// field is `"101528ops:103104"`). Callers treat `None` as a stale lock to take
/// over, rather than coercing it to PID 0 (issue #192).
pub(crate) fn parse_lock_owner_pid(field: &str) -> Option<u32> {
    let field = field.trim();
    let head = match field.split_once(':') {
        Some((head, _)) => head,
        None => field,
    };
    head.trim().parse::<u32>().ok().filter(|&p| p != 0)
}

/// Split a stamp's PID field into `(pid, creation_ticks)`, tolerating both the
/// bare `pid` shape and the `pid:ticks` shape.
///
/// The ticks half is the owner's creation time, written by [`owner_stamp`] and
/// checked by [`stamp_owner_is_live`]; [`crate::config::winlock::terminate`]
/// also uses it to prove ownership by exact creation time instead of comparing
/// a lock file's mtime (a coarser clock) against a process creation time.
/// `None` means the stamp has no creation field -- an older stamp, or a writer
/// on a platform that cannot read one -- and the preempt path then refuses the
/// kill rather than trusting a bare PID.
pub(crate) fn split_pid_ticks(field: &str) -> Option<(u32, Option<u64>)> {
    let (pid_field, ticks_field) = match field.trim().split_once(':') {
        Some((p, t)) => (p, Some(t)),
        None => (field, None),
    };
    let pid = parse_lock_owner_pid(pid_field)?;
    let ticks = ticks_field.and_then(|t| t.trim().parse::<u64>().ok());
    Some((pid, ticks))
}

/// Body of a lock stamp: this process's PID, plus its creation time when the
/// platform can read one. See [`split_pid_ticks`] for what the extra field
/// buys and [`stamp_owner_is_live`] for what the reader does with it. A
/// platform that cannot read a creation time writes the bare PID, and the
/// reader then leans on the boot-epoch check alone.
pub(crate) fn owner_stamp() -> String {
    let pid = std::process::id();
    match own_creation_stamp() {
        Some(ticks) => format!("{pid}:{ticks}"),
        None => pid.to_string(),
    }
}

/// This process's own creation time, in the unit this platform's stamps
/// record. `None` means the platform cannot be asked, and [`owner_stamp`]
/// then writes a bare PID.
#[cfg(windows)]
fn own_creation_stamp() -> Option<u64> {
    crate::config::winlock::own_creation_ticks()
}

#[cfg(target_os = "linux")]
fn own_creation_stamp() -> Option<u64> {
    proc_stat_starttime(std::process::id())
}

#[cfg(target_os = "macos")]
fn own_creation_stamp() -> Option<u64> {
    macos_process_start_epoch(std::process::id())
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn own_creation_stamp() -> Option<u64> {
    None
}

/// The kernel's own reading for `pid`, in the unit [`own_creation_stamp`]
/// writes on this platform. `None` means this platform cannot be asked, which
/// leaves [`stamp_owner_is_live`] with its boot-epoch check alone.
#[cfg(windows)]
pub(crate) fn process_creation_stamp(pid: u32) -> Option<u64> {
    crate::config::winlock::creation_ticks_of(pid)
}

#[cfg(target_os = "linux")]
pub(crate) fn process_creation_stamp(pid: u32) -> Option<u64> {
    proc_stat_starttime(pid)
}

#[cfg(target_os = "macos")]
pub(crate) fn process_creation_stamp(pid: u32) -> Option<u64> {
    macos_process_start_epoch(pid)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(crate) fn process_creation_stamp(_pid: u32) -> Option<u64> {
    None
}

/// Field 22 of `/proc/<pid>/stat`: starttime, in clock ticks since boot.
///
/// Field 2 is the command, parenthesised, and may itself contain spaces and
/// parentheses, so the fields after it line up only once everything up to the
/// LAST `)` is skipped. Starttime is then the 20th field of the remainder.
#[cfg(target_os = "linux")]
fn proc_stat_starttime(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    rest.split_whitespace().nth(19)?.parse::<u64>().ok()
}

/// A process's start time in epoch seconds, through libproc's `PROC_PIDTBSDINFO`
/// flavor.
///
/// `/proc` does not exist on macOS. #2084 was filed against a Linux host while
/// this project's dev host and CI runner are both macOS, so a Linux-only reader
/// would leave the bug in place on the very machines that run our gates.
/// libproc answers for any PID with no privileges needed, and its epoch-seconds
/// clock is the same one `kern.boottime` reports below.
#[cfg(target_os = "macos")]
fn macos_process_start_epoch(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { core::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::addr_of_mut!(info).cast(),
            size,
        )
    };
    // A short read means the flavor answered nothing (usually: no such PID),
    // which is a missing reading, not a creation time of zero.
    (got == size).then_some(info.pbi_start_tvsec)
}

/// This machine's boot time as a Unix timestamp.
///
/// Linux reads `btime` from `/proc/stat`; macOS asks `kern.boottime`. Both feed
/// one check: a lock file written before the boot epoch cannot name a process
/// that is alive now, whatever its PID resolves to.
#[cfg(target_os = "linux")]
pub(crate) fn boot_epoch() -> Option<i64> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    stat.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse::<i64>().ok())
}

#[cfg(target_os = "macos")]
pub(crate) fn boot_epoch() -> Option<i64> {
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    let mut tv: libc::timeval = unsafe { core::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::timeval>() as libc::size_t;
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::addr_of_mut!(tv).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then_some(tv.tv_sec as i64)
}

/// Windows proves ownership by creation time alone and reads no boot epoch, so
/// there is no second reading to take.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn boot_epoch() -> Option<i64> {
    None
}

/// Is the process named by a lock stamp still the one that wrote it?
///
/// A PID alone is not proof of ownership. A lock file outlives the machine it
/// was written on, and the kernel hands a freed PID to whatever starts next, so
/// after a reboot `is_pid_alive` can report a total stranger as the owner. On
/// Unix that stranger is usually another user's process, where `kill(pid, 0)`
/// answers EPERM, which `is_pid_alive` deliberately reads as alive. The stamp
/// then keeps looking held for as long as the unrelated process lives, and the
/// cost is silent: every channel behind the lock is disabled while the daemon
/// still looks healthy, the TUI's preempt path can SIGTERM the stranger, and
/// `instance_running` can report a daemon that is not there (#2084).
///
/// Two checks recover the truth:
/// 1. Exact creation time. A stamp carrying one (`pid:ticks`) must match the
///    live process's own reading. A mismatch is a recycled PID, not an owner.
/// 2. Boot epoch. A stamp written before the machine booted cannot name any
///    process alive now, whatever its PID resolves to. This is the check that
///    covers bare-PID stamps written by builds that predate the field above.
///
/// Residual, stated rather than hidden: a legacy bare-PID stamp written AFTER
/// the current boot whose PID was recycled within that same boot is still read
/// as live. With no recorded creation time and no pre-boot mtime there is
/// nothing left to disprove it against. The window closes on its own the first
/// time a build carrying the creation-time field writes the lock.
///
/// When neither check can be taken (no `/proc` and no libproc answer, an
/// unreadable lock file) the answer is `true`: refusing to start is worse than
/// a duplicate start.
pub(crate) fn stamp_owner_is_live(pid_field: &str, lock_file: &Path) -> bool {
    let Some((pid, ticks)) = split_pid_ticks(pid_field) else {
        return false;
    };
    if !is_pid_alive(pid) {
        return false;
    }
    if let Some(recorded) = ticks
        && let Some(live) = process_creation_stamp(pid)
        && live != recorded
    {
        return false;
    }
    if let Some(boot) = boot_epoch()
        && let Ok(written) = fs::metadata(lock_file).and_then(|m| m.modified())
    {
        let written = written
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(i64::MAX);
        if written < boot {
            return false;
        }
    }
    true
}

/// Check and acquire a token lock for a channel credential.
/// Returns `Err` if another profile holds the lock.
pub fn acquire_token_lock(channel: &str, token_hash: &str) -> Result<()> {
    let lock_dir = base_opencrabs_dir().join("locks");
    fs::create_dir_all(&lock_dir)?;

    let lock_file = lock_dir.join(format!("{}_{}.lock", channel, token_hash));
    let current_profile = active_profile().unwrap_or("default");
    let pid = std::process::id();

    if lock_file.exists() {
        let contents = fs::read_to_string(&lock_file).unwrap_or_default();
        let parts: Vec<&str> = contents.splitn(2, ':').collect();
        if parts.len() == 2 {
            let locked_profile = parts[0];
            // A lock whose PID doesn't parse to a real process names no live
            // owner. This happens when the file is corrupted — e.g. an external
            // in-place edit concatenated several entries, so `parts[1]` is
            // something like "101528ops:103104family" — or when it's literally
            // 0. Treat that as a stale lock and take it over, rather than
            // coercing it to PID 0 and depending on is_pid_alive(0) (issue #192).
            match parse_lock_owner_pid(parts[1]) {
                None => {
                    tracing::warn!(
                        "lock file {} has an invalid owner PID ({:?}) — treating as stale and taking over",
                        lock_file.display(),
                        parts[1]
                    );
                    // fall through to overwrite
                }
                Some(locked_pid) => {
                    // Liveness comes from the stamp, not the bare PID: see
                    // [`stamp_owner_is_live`] for why a recycled PID must not
                    // count as a live owner.
                    let owner_is_live = stamp_owner_is_live(parts[1], &lock_file);
                    if locked_profile == current_profile {
                        // Same profile — only one instance per profile allowed.
                        if owner_is_live && locked_pid != pid {
                            bail!(
                                "profile '{}' already running (PID {}). Only one instance per profile allowed.",
                                current_profile,
                                locked_pid
                            );
                        }
                        // Stale lock from same profile — overwrite
                    } else if owner_is_live {
                        // Different profile holds it with a live process.
                        bail!(
                            "channel '{}' token is locked by profile '{}' (PID {}). \
                             Two profiles cannot share the same bot credential.",
                            channel,
                            locked_profile,
                            locked_pid
                        );
                        // else: stale lock from a dead process — overwrite
                    }
                }
            }
        }
    }

    // `owner_stamp` rather than the bare PID: the reader needs the owner's
    // creation time to tell it apart from a recycled PID on every platform
    // that can record one. See [`stamp_owner_is_live`].
    fs::write(&lock_file, format!("{}:{}", current_profile, owner_stamp()))?;
    Ok(())
}

/// Release a token lock.
pub fn release_token_lock(channel: &str, token_hash: &str) {
    let lock_file = base_opencrabs_dir()
        .join("locks")
        .join(format!("{}_{}.lock", channel, token_hash));
    let _ = fs::remove_file(lock_file);
}

/// Release all locks held by this process.
pub fn release_all_locks() {
    let lock_dir = base_opencrabs_dir().join("locks");
    let current_profile = active_profile().unwrap_or("default");
    // Built from `owner_stamp` so the comparison matches what was written,
    // creation-time field and all.
    let expected = format!("{}:{}", current_profile, owner_stamp());

    if let Ok(entries) = fs::read_dir(&lock_dir) {
        for entry in entries.flatten() {
            if let Ok(contents) = fs::read_to_string(entry.path())
                && contents.trim() == expected
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Hash a token for lock file naming (no raw secrets on disk).
pub fn hash_token(token: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    token.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// RAII guard proving this process owns cron scheduling for one profile (#444).
///
/// While the guard is alive, no other process can spawn a scheduler for the
/// same profile — the multi-profile `daemon`, a `-p <profile>` daemon, and the
/// TUI all contend on one lock file, so a profile's `cron_jobs` table is polled
/// by exactly one scheduler machine-wide. Without it, two daemons that both
/// cover the same profile double-fired every due job.
///
/// The lock is an advisory `flock` on an open file. The kernel releases it when
/// the file descriptor closes — on an explicit drop OR on process death — so a
/// crashed daemon never wedges a profile's scheduling.
pub struct SchedulerLock {
    // Holding the File keeps the flock; nothing reads this field, it exists to
    // tie the lock's lifetime to the guard's.
    _file: fs::File,
}

/// Try to take the cron scheduler lock for `profile`. `Some` means this process
/// now owns scheduling for the profile; `None` means another live process holds
/// it and the caller must NOT spawn a scheduler (it would double-fire jobs).
///
/// Lives in the GLOBAL locks dir under a profile-keyed name so the path is the
/// same regardless of any task-local profile-home scope the caller runs inside.
pub fn acquire_scheduler_lock(profile: &str) -> Option<SchedulerLock> {
    acquire_scheduler_lock_in(
        &base_opencrabs_dir().join("locks").join("scheduler"),
        profile,
    )
}

/// Dir-injectable core of [`acquire_scheduler_lock`] so tests can point at a
/// TempDir instead of the real `~/.opencrabs/locks/` (running it against the
/// real dir is fine here — no SIGTERM like preemption — but a TempDir keeps
/// tests hermetic and parallel-safe).
pub(crate) fn acquire_scheduler_lock_in(lock_dir: &Path, profile: &str) -> Option<SchedulerLock> {
    if let Err(e) = fs::create_dir_all(lock_dir) {
        tracing::warn!(
            "scheduler lock: cannot create {}: {e} — not spawning scheduler",
            lock_dir.display()
        );
        return None;
    }
    let path = lock_dir.join(format!("{profile}.lock"));
    let file = match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                "scheduler lock: cannot open {}: {e} — not spawning scheduler",
                path.display()
            );
            return None;
        }
    };

    #[cfg(unix)]
    {
        use crate::config::flock::{FlockOutcome, exclusive};
        use std::os::unix::io::AsRawFd;
        // Contention means another live process already owns this profile's
        // scheduler. A genuine failure also declines, since spawning on an
        // uncertain lock risks the double-fire this guard exists to prevent,
        // but it says so rather than passing for an ordinary handover.
        match exclusive(file.as_raw_fd(), true) {
            FlockOutcome::Acquired => {}
            FlockOutcome::Held => return None,
            FlockOutcome::Failed(e) => {
                tracing::warn!(
                    "scheduler lock: cannot lock {}: {e} - not spawning scheduler",
                    path.display()
                );
                return None;
            }
        }
    }

    #[cfg(windows)]
    {
        use crate::config::winlock::LockOutcome;
        use std::os::windows::io::AsRawHandle;
        // Same policy as the flock arm: contention declines silently (a live
        // process owns this profile's scheduler), a genuine failure declines
        // loudly — spawning on an uncertain lock is the double-fire this
        // guard exists to prevent.
        match crate::config::winlock::exclusive(file.as_raw_handle(), true) {
            LockOutcome::Acquired => {}
            LockOutcome::Held => return None,
            LockOutcome::Failed(e) => {
                tracing::warn!(
                    "scheduler lock: cannot lock {}: {e} - not spawning scheduler",
                    path.display()
                );
                return None;
            }
        }
    }

    // Stamp the owner PID for observability (`cat` the lock file to see who
    // holds it). The flock, not this write, is the real guard, so a torn write
    // is harmless.
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = &file;
        let _ = f.set_len(0);
        let _ = f.seek(SeekFrom::Start(0));
        if let Err(e) = write!(f, "{}", owner_stamp()) {
            tracing::debug!("scheduler lock: could not stamp PID into {path:?}: {e}");
        }
    }

    Some(SchedulerLock { _file: file })
}

/// Exclusive ownership of a profile for the lifetime of this process (#1072).
///
/// Held from the very top of boot, before the database, the provider, the tool
/// registry or the memory reindex. A second instance of the same profile used
/// to run all of that in parallel with the first: two full reindexes over one
/// memory.db, two provider factories, two sets of channel connect attempts. The
/// scheduler and token locks caught it later and only partially, by which point
/// the duplicate work was already done and the process stayed up as a zombie
/// serving nothing.
///
/// Same mechanics as [`SchedulerLock`]: an advisory `flock` released by the
/// kernel when the fd closes, so a crashed instance never wedges a profile.
pub struct InstanceLock {
    // Holding the File keeps the flock; nothing reads this field.
    _file: fs::File,
}

/// Outcome of trying to claim a profile.
pub enum InstanceGuard {
    /// This process now owns the profile. Hold the lock for its lifetime.
    Acquired(InstanceLock),
    /// Another live process owns it. `pid` is the stamp when it was readable
    /// and the process is still alive; the flock is the guard, not the stamp,
    /// so `None` still means held.
    Held { pid: Option<u32> },
    /// The lock file could not be created or opened. Boot proceeds unguarded.
    ///
    /// Deliberately not a refusal: an unwritable lock dir is an environment
    /// problem, and turning it into "the machine will not start" is a worse
    /// failure than the duplicate work this guard prevents.
    Unavailable,
}

/// Try to take the single-instance lock for `profile`.
pub fn acquire_instance_lock(profile: &str) -> InstanceGuard {
    acquire_instance_lock_in(
        &base_opencrabs_dir().join("locks").join("instance"),
        profile,
    )
}

/// Read-only probe: does `profile` have a live daemon/TUI instance?
///
/// Reads the instance lock stamp (`locks/instance/{profile}.lock`) and
/// checks the PID's liveness. No lock acquisition, no writes, no SIGTERM
/// — never the preempt path. Used by the multi-profile daemon (#194) to
/// decide whether to adopt another profile's cron scheduler.
pub fn instance_running(profile: &str) -> bool {
    instance_running_in(
        &base_opencrabs_dir().join("locks").join("instance"),
        profile,
    )
}

pub(crate) fn instance_running_in(lock_dir: &Path, profile: &str) -> bool {
    let path = lock_dir.join(format!("{profile}.lock"));
    match fs::read_to_string(&path) {
        // The instance stamp is a bare `pid[:ticks]` with no profile prefix,
        // so the whole file is the PID field. `stamp_owner_is_live` rather
        // than `is_pid_alive`: a recycled PID would otherwise report a daemon
        // that is not running, and the multi-profile scheduler would never
        // adopt this profile's cron jobs (#2084).
        Ok(contents) => stamp_owner_is_live(contents.trim(), &path),
        Err(_) => false, // missing lock file => no live instance => adoptable
    }
}

/// Dir-injectable core of [`acquire_instance_lock`], so tests point at a
/// TempDir rather than the real `~/.opencrabs/locks/`.
///
/// Lives in its own `instance/` subdirectory rather than alongside the channel
/// token locks: `foreign_lock_owners` scans that directory for `*.lock` files
/// and SIGTERMs whatever it finds, so a lock file dropped there would make the
/// TUI kill the very process that is starting up.
pub(crate) fn acquire_instance_lock_in(lock_dir: &Path, profile: &str) -> InstanceGuard {
    if let Err(e) = fs::create_dir_all(lock_dir) {
        tracing::warn!(
            "instance lock: cannot create {}: {e} — starting without the guard",
            lock_dir.display()
        );
        return InstanceGuard::Unavailable;
    }
    let path = lock_dir.join(format!("{profile}.lock"));
    let file = match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                "instance lock: cannot open {}: {e} — starting without the guard",
                path.display()
            );
            return InstanceGuard::Unavailable;
        }
    };

    #[cfg(unix)]
    {
        use crate::config::flock::{FlockOutcome, exclusive};
        use std::os::unix::io::AsRawFd;
        match exclusive(file.as_raw_fd(), true) {
            FlockOutcome::Acquired => {}
            FlockOutcome::Failed(e) => {
                // We do not know whether anyone holds it, so do not claim an
                // instance is running: report the same "no guard" state the
                // open-failure path reports, with the reason.
                tracing::warn!(
                    "instance lock: cannot lock {}: {e} - starting without the guard",
                    path.display()
                );
                return InstanceGuard::Unavailable;
            }
            FlockOutcome::Held => {
                let pid = fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| split_pid_ticks(&s).map(|(pid, _)| pid))
                    // A stamp naming a dead process means the file is stale while
                    // something else holds the flock, or the stamp was never
                    // written. Reporting it would send the user chasing a PID that
                    // does not exist.
                    .filter(|p| is_pid_alive(*p));
                return InstanceGuard::Held { pid };
            }
        }
    }

    #[cfg(windows)]
    {
        use crate::config::winlock::LockOutcome;
        use std::os::windows::io::AsRawHandle;
        // Non-blocking: a live holder means a second instance of this
        // profile, and #1072's contract is that it must not boot.
        match crate::config::winlock::exclusive(file.as_raw_handle(), true) {
            LockOutcome::Acquired => {}
            LockOutcome::Held => {
                let pid = fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| split_pid_ticks(&s).map(|(pid, _)| pid))
                    // Same stale-stamp discipline as the Unix arm: never
                    // name a PID that is not alive.
                    .filter(|p| is_pid_alive(*p));
                return InstanceGuard::Held { pid };
            }
            LockOutcome::Failed(e) => {
                tracing::warn!(
                    "instance lock: cannot lock {}: {e} - starting without the guard",
                    path.display()
                );
                return InstanceGuard::Unavailable;
            }
        }
    }

    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = &file;
        let _ = f.set_len(0);
        let _ = f.seek(SeekFrom::Start(0));
        if let Err(e) = write!(f, "{}", owner_stamp()) {
            tracing::debug!("instance lock: could not stamp PID into {path:?}: {e}");
        }
    }

    InstanceGuard::Acquired(InstanceLock { _file: file })
}

/// A live, OTHER instance of the active profile that was holding channel
/// token locks when the interactive TUI started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreemptedInstance {
    /// PID of the background instance (e.g. an `opencrabs daemon`).
    pub pid: u32,
    /// Channels whose token lock it was holding (e.g. `["telegram"]`).
    pub channels: Vec<String>,
    /// Whether it had released its locks (process gone) by the time we
    /// stopped waiting. `false` means it may still contend for the
    /// credential — the caller should say so in the warning.
    pub stopped: bool,
}

/// One foreign lock owner found by [`foreign_lock_owners`]: the channels it
/// held, and -- on stamps that carry one -- the creation time it recorded,
/// which [`crate::config::winlock::terminate`] needs to tell the real owner
/// from a process that merely inherited its PID. `pub(crate)`, fields
/// included, because the sibling test module asserts on what a scan found.
#[derive(Default)]
pub(crate) struct ForeignOwner {
    pub(crate) creation_ticks: Option<u64>,
    pub(crate) channels: Vec<String>,
}

/// Fold two creation-time claims for one PID down to the newest. This is the
/// fallback for when the kernel cannot be asked for the live process's own
/// reading: a process that dies holding a PID leaves its stamp behind, and on
/// a clock that only moves forward whoever inherits that PID is created
/// strictly later, so the greatest claim is the live owner's. `None` -- a
/// legacy stamp, written before the creation-time field existed -- sorts below
/// every real creation time, so it never wins and never erases a value that
/// was already recorded.
fn newest_ticks(current: Option<u64>, candidate: Option<u64>) -> Option<u64> {
    current.max(candidate)
}

/// Resolve every creation-time claim recorded for one live PID down to the one
/// that can belong to the process holding it now.
///
/// `live` is the kernel's own reading for that PID, when it could be taken.
/// With it, only an EXACT match proves ownership: a claim that disagrees
/// belongs to a process that died holding the PID. Comparing against the live
/// process instead of between two claims is what makes this immune to a wall
/// clock that stepped backward between the two births -- taking the greatest
/// claim would then hand `terminate` the corpse's creation time, its birth
/// proof would refuse the real owner, and the handover would never complete.
/// Without a reading (a platform that cannot be asked, or a refused handle)
/// the greatest claim wins, which is order-independent.
pub(crate) fn resolve_creation_ticks(live: Option<u64>, claims: &[Option<u64>]) -> Option<u64> {
    match live {
        Some(live) => claims.contains(&Some(live)).then_some(live),
        None => claims.iter().copied().fold(None, newest_ticks),
    }
}

/// Map every *live, foreign* lock owner for the active profile to the
/// channels it holds, reading lock files from `lock_dir`. "Foreign" = a PID
/// other than this process. Pure file inspection, no side effects. The dir is
/// a parameter so tests can point it at a TempDir and never read the real
/// workspace.
pub(crate) fn foreign_lock_owners(
    lock_dir: &Path,
) -> std::collections::BTreeMap<u32, ForeignOwner> {
    let current_profile = active_profile().unwrap_or("default");
    let self_pid = std::process::id();
    let mut owners: std::collections::BTreeMap<u32, ForeignOwner> = Default::default();
    // Every creation-time claim read for each PID, kept until the whole
    // directory has been scanned: resolution needs them all at once, see
    // [`resolve_creation_ticks`].
    let mut claims: std::collections::BTreeMap<u32, Vec<Option<u64>>> = Default::default();

    let entries = match fs::read_dir(lock_dir) {
        Ok(e) => e,
        Err(_) => return owners,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let fname = match path.file_name().and_then(|n| n.to_str()) {
            Some(f) if f.ends_with(".lock") => f.to_string(),
            _ => continue,
        };
        let contents = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let (profile, pid_field) = match contents.trim().split_once(':') {
            Some(p) => p,
            None => continue,
        };
        if profile != current_profile {
            continue;
        }
        let (pid, ticks) = match split_pid_ticks(pid_field) {
            Some(pair) => pair,
            None => continue,
        };
        // The preempt path SIGTERMs whatever this scan returns, so a bare
        // `is_pid_alive` is not enough here: a stamp naming a recycled PID
        // would hand an unrelated process to `kill`. Same stamp discipline as
        // the token lock itself.
        if pid == self_pid || !stamp_owner_is_live(pid_field, &path) {
            continue;
        }
        // Lock file is `<channel>_<hash>.lock` — channel is everything
        // before the final '_'.
        let channel = fname
            .strip_suffix(".lock")
            .and_then(|s| s.rsplit_once('_'))
            .map(|(c, _)| c.to_string())
            .unwrap_or(fname);
        let owner = owners.entry(pid).or_default();
        owner.channels.push(channel);
        // Several locks can name one PID. When that PID was reused, one stamp
        // belongs to a process that died holding it and another to the live
        // owner, and they disagree about the creation time. Hold every claim
        // until the directory is fully scanned, then resolve once: the answer
        // needs the live process's own reading, not the order `read_dir`
        // happened to return the files in.
        claims.entry(pid).or_default().push(ticks);
    }
    for (pid, owner) in owners.iter_mut() {
        if let Some(claims) = claims.get(pid) {
            owner.creation_ticks = resolve_creation_ticks(process_creation_stamp(*pid), claims);
        }
    }
    owners
}

/// TUI priority: shut down any OTHER live instance of the active profile
/// that currently holds channel token locks, so the interactive session
/// can take over the channels.
///
/// When a user opens the TUI while a background `opencrabs daemon` (or
/// systemd service) for the same profile is already running, both try to
/// own the same bot credentials. Only one process can hold a Telegram
/// `getUpdates` long-poll, so they fight (HTTP 409) and the channel keeps
/// dropping. The interactive session is the one the user is looking at,
/// so it wins.
///
/// We ask the other instance to stop two ways, then wait briefly for it
/// to release its locks:
///   * stop its systemd unit (`systemctl [--user] stop opencrabs*.service`)
///     so a `Restart=always` policy doesn't immediately resurrect it;
///   * SIGTERM the PID directly, covering a bare `opencrabs daemon`
///     launched in a terminal with no systemd unit behind it.
///
/// Per the chosen behavior we do NOT restart the daemon afterwards — it
/// stays down until the user starts it again. Best-effort and quick
/// (~3s cap); returns the instances we preempted so the caller can warn
/// the user. Blocks while waiting, so callers on an async runtime should
/// invoke it via `spawn_blocking`.
pub fn preempt_other_profile_instances() -> Vec<PreemptedInstance> {
    // The ONLY caller that may touch the real lock dir and real services.
    preempt_instances_in(&base_opencrabs_dir().join("locks"), true)
}

/// Core of [`preempt_other_profile_instances`], parameterized by the lock
/// directory and whether to stop systemd services. This separation exists for
/// SAFETY: this function sends real SIGTERM/SIGKILL to whatever live PIDs it
/// finds in `lock_dir`, so it must NEVER run against the real
/// `~/.opencrabs/locks/` from a test (doing so kills the user's running
/// instances). Tests call this with a TempDir and `stop_services = false`.
/// Parse `launchctl list` output into the opencrabs-managed labels.
///
/// `launchctl list` prints `PID\tStatus\tLabel` per line (tab-separated),
/// with a literal header line and `-` in the PID column for services that
/// are loaded but not currently running. We only care about labels under
/// our prefix, since bootouting anything else would be hostile.
// Only the macOS-gated launchd path calls this outside tests, so on other
// unix targets the lib build sees it as dead code. Tests exercise it everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_launchctl_labels(output: &str) -> Vec<String> {
    const PREFIX: &str = "com.opencrabs.";
    let mut labels = Vec::new();
    for line in output.lines() {
        let Some(label) = line.rsplit('\t').next() else {
            continue;
        };
        if label.starts_with(PREFIX) && !labels.iter().any(|l: &String| l == label) {
            labels.push(label.to_string());
        }
    }
    labels
}

pub(crate) fn preempt_instances_in(lock_dir: &Path, stop_services: bool) -> Vec<PreemptedInstance> {
    // The only mutation of `owners` lives in the Windows handover block
    // below; on every other target the `mut` genuinely is unused, so the
    // lint is silenced exactly where it is wrong, never across all targets.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut owners = foreign_lock_owners(lock_dir);

    // Windows handover gap (tracked as #6): a daemon with NO enabled
    // channels holds only the instance lock under locks/instance/, which
    // the token scan above never sees. The TUI then gets `Held`, preemption
    // finds nobody, and the handover dies in its retry loop. Consult the
    // instance stamp as a fallback owner source; the kill is still gated
    // by winlock::terminate's image AND creation-time proofs, so a stale
    // instance stamp gets refused exactly like a stale token stamp.
    #[cfg(windows)]
    {
        if owners.is_empty() {
            let profile = active_profile().unwrap_or("default");
            let ipath = lock_dir.join("instance").join(format!("{profile}.lock"));
            if let Ok(contents) = fs::read_to_string(&ipath) {
                if let Some((pid, ticks)) = split_pid_ticks(&contents) {
                    if pid != std::process::id() && is_pid_alive(pid) {
                        match crate::config::winlock::terminate(pid, ticks) {
                            Ok(()) => {
                                owners.insert(
                                    pid,
                                    ForeignOwner {
                                        creation_ticks: ticks,
                                        channels: vec!["instance".to_string()],
                                    },
                                );
                            }
                            Err(e) => {
                                tracing::debug!(
                                    pid,
                                    error = %e,
                                    "preempt: instance-lock owner rejected by fail-closed verify"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    if owners.is_empty() {
        return Vec::new();
    }

    // Stop launchd-managed agents first on macOS, same reason as systemd
    // below: both LaunchAgent templates we ship set KeepAlive=true, so a
    // bare SIGTERM gets the instance resurrected immediately. `bootout`
    // removes the service from the gui domain, which stops it AND cancels
    // the keep-alive. `launchctl list` covers every profile's label
    // (`com.opencrabs.daemon`, `com.opencrabs.daemon.<profile>`,
    // `com.opencrabs.agent`) without hardcoding them. Best-effort, matching
    // the systemd block: harmless (non-zero exit) when launchd has no such
    // service. Skipped under tests (`stop_services = false`).
    #[cfg(target_os = "macos")]
    if stop_services {
        match std::process::Command::new("launchctl").arg("list").output() {
            Ok(out) => {
                let labels = parse_launchctl_labels(&String::from_utf8_lossy(&out.stdout));
                let uid = unsafe { libc::getuid() };
                for label in labels {
                    tracing::info!(label = %label, "preempt: bootouting launchd agent");
                    let res = std::process::Command::new("launchctl")
                        .arg("bootout")
                        .arg(format!("gui/{uid}/{label}"))
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                    if let Err(e) = res {
                        tracing::debug!(label = %label, error = %e, "preempt: launchctl bootout spawn failed");
                    }
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "preempt: launchctl list spawn failed");
            }
        }
    }

    // Stop a systemd-managed daemon first, so its unit's Restart policy
    // doesn't bring it straight back after the SIGTERM. The glob covers
    // every profile's unit; harmless (just non-zero exit) when there's no
    // systemd or no matching unit. Both scopes, since the daemon may be a
    // system OR a user service. Skipped under tests (`stop_services = false`)
    // so a test can never stop a real service on a CI host.
    if stop_services {
        for user in [false, true] {
            let mut cmd = std::process::Command::new("systemctl");
            if user {
                cmd.arg("--user");
            }
            cmd.arg("stop")
                .arg("opencrabs*.service")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if let Err(e) = cmd.status() {
                tracing::debug!(user, error = %e, "preempt: systemctl stop spawn failed (likely no systemd)");
            }
        }
    }

    // SIGTERM each foreign PID directly — covers a bare `opencrabs daemon`
    // running in a terminal with no unit behind it.
    #[cfg(unix)]
    for &pid in owners.keys() {
        let ret = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            tracing::debug!(pid, error = %err, "preempt: SIGTERM to background instance failed");
        }
    }

    // Windows has no signal channel to a headless console process, so there
    // is no polite rung to try first: this is TerminateProcess straight
    // away, which is exactly why the ownership evidence (an alive PID stamped
    // into a lock file whose range we could not take) must be positive
    // before we get here. See winlock::terminate's contract. The ~3s wait
    // loop below gives the same exit window SIGTERM would, and the
    // stopped-flag computation is already platform-agnostic.
    #[cfg(windows)]
    {
        for (pid, owner) in owners.iter() {
            // Feed terminate the creation time the owner recorded in its own
            // stamp; a legacy stamp without one passes None, which terminate
            // rejects rather than trusting a bare PID (fail-closed).
            if let Err(e) = crate::config::winlock::terminate(*pid, owner.creation_ticks) {
                tracing::debug!(pid, error = %e, "preempt: TerminateProcess refused or failed");
            }
        }
    }

    let mut results: Vec<PreemptedInstance> = owners
        .into_iter()
        .map(|(pid, owner)| {
            let mut channels = owner.channels;
            channels.sort();
            channels.dedup();
            PreemptedInstance {
                pid,
                channels,
                stopped: false,
            }
        })
        .collect();

    // Wait up to ~3s for the instances to exit and release their locks.
    for _ in 0..30 {
        if results.iter().all(|r| !is_pid_alive(r.pid)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Escalate to SIGKILL for anything that ignored SIGTERM. The TUI must
    // win the credential outright: if a stubborn daemon stays alive, its
    // same-profile lock would still block `acquire_token_lock` and the TUI
    // would silently get no Telegram — the exact "I had to reconnect"
    // symptom we're fixing. A cross-user daemon (root vs user) may still
    // resist (EPERM); that we can only warn about.
    #[cfg(unix)]
    {
        let stragglers: Vec<u32> = results
            .iter()
            .filter(|r| is_pid_alive(r.pid))
            .map(|r| r.pid)
            .collect();
        if !stragglers.is_empty() {
            for pid in &stragglers {
                let ret = unsafe { libc::kill(*pid as i32, libc::SIGKILL) };
                if ret != 0 {
                    let err = std::io::Error::last_os_error();
                    tracing::warn!(pid = *pid, error = %err, "preempt: SIGKILL to stubborn instance failed");
                }
            }
            for _ in 0..10 {
                if stragglers.iter().all(|p| !is_pid_alive(*p)) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    for r in &mut results {
        r.stopped = !is_pid_alive(r.pid);
        if r.stopped {
            tracing::info!(
                pid = r.pid,
                channels = ?r.channels,
                "TUI priority: stopped background instance that held channel locks"
            );
        } else {
            tracing::warn!(
                pid = r.pid,
                channels = ?r.channels,
                "TUI priority: background instance still alive after stop request — its channel locks may still contend"
            );
        }
    }
    results
}

// ─── Helpers ─────────────────────────────────────────────────────────

pub fn validate_profile_name(name: &str) -> Result<()> {
    if name == "default" {
        bail!("'default' is reserved — the default profile is ~/.opencrabs/");
    }
    if name.is_empty() || name.len() > 64 {
        bail!("profile name must be 1-64 characters");
    }
    if let Some((pos, c)) = name
        .chars()
        .enumerate()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '-' || *c == '_'))
    {
        bail!(
            "profile name can only contain alphanumeric, hyphens, and underscores \
             (rejected {:?} U+{:04X} at position {})",
            c,
            c as u32,
            pos
        );
    }
    Ok(())
}

pub(crate) fn is_pid_alive(pid: u32) -> bool {
    // PID 0 is never a real process that could own a lock. Critically, on Unix
    // `kill(0, 0)` does NOT probe "process 0" — it signals the CALLING process's
    // entire process group and always succeeds, so without this guard a lock
    // file whose PID parsed to 0 (corruption) would look alive forever and wedge
    // the channel that owns that credential (issue #192). Guard it on every
    // platform.
    // Likewise, any PID exceeding i32::MAX would wrap to a negative pid_t when cast
    // to i32. On Unix, negative PIDs signal process groups or all processes (e.g.
    // kill(-1, 0) signals every process the caller has permission to signal and
    // returns 0), leading to false positives on values like u32::MAX.
    let Ok(signed_pid) = i32::try_from(pid) else {
        return false;
    };
    if signed_pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if we can signal the process.
        // If it returns -1, check errno: ESRCH means the process doesn't exist,
        // EPERM means it exists but we lack permission (still alive).
        let ret = unsafe { libc::kill(signed_pid, 0) };
        if ret == 0 {
            return true;
        }
        // EPERM = process exists but owned by another user (e.g. PID 1 = launchd)
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> isize;
            fn CloseHandle(hObject: isize) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle == 0 {
            false
        } else {
            unsafe { CloseHandle(handle) };
            true
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}
