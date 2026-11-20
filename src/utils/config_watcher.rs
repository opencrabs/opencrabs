//! Config hot-reload watcher.
//!
//! Watches the `~/.opencrabs/` directory and reacts to changes of
//! `config.toml`, `keys.toml`, and `commands.toml`. On any modification it
//! re-loads the full `Config` and fires all registered callbacks. Watching the
//! directory (rather than the files directly) is deliberate: atomic saves
//! rename a temp file over the target, which a file-level watch would miss.
//!
//! Designed to be extended: register any channel state update or command reload
//! by pushing a `ReloadCallback` via `spawn()`.
//!
//! The reload pass itself lives in [`reload_once`], so a second watcher in the
//! same binary (the window, `gui::config_reload`) reloads exactly the same way
//! instead of growing its own copy of the recovery rules.

use crate::config::{Config, opencrabs_home};
use notify::{RecursiveMode, Watcher};
use std::sync::Arc;
use std::time::Duration;

/// Callback fired on every successful config reload.
pub type ReloadCallback = Arc<dyn Fn(Config) + Send + Sync>;

/// `" (profile: NAME)"` when a named profile is active, else `""` — appended to
/// config-reload alerts so a multi-profile operator knows WHICH profile's config
/// was rejected (#534).
fn profile_suffix() -> String {
    profile_suffix_from(crate::config::profile::active_profile())
}

/// Pure core of [`profile_suffix`], for tests.
pub(crate) fn profile_suffix_from(profile: Option<&str>) -> String {
    match profile {
        Some(name) if !name.is_empty() && name != "default" => format!(" (profile: {name})"),
        _ => String::new(),
    }
}

/// Callback fired with a user-facing message when a hot reload does NOT cleanly
/// apply the on-disk config: either it recovered from last-known-good (the file
/// failed to parse) or the load failed entirely and the running config is kept.
/// Without this the failure was log-only, so an operator editing `config.toml`
/// had no signal that their change was rejected and the process kept serving
/// the old provider set (#534, mirror of upstream #517).
pub type ReloadNotify = Arc<dyn Fn(String) + Send + Sync>;

/// What one reload pass did.
///
/// Every watcher in the binary reads this, so a reload means the same thing
/// wherever it is observed: the last-good snapshot, the voice-flag audit and
/// the `Config::current()` swap all happen in [`reload_once`], never in a
/// caller.
pub enum ReloadOutcome {
    /// The files parsed cleanly and the running config now holds them.
    Applied(Config),
    /// `config.toml` failed to parse, so the process is running last-known-good
    /// instead. The edits on disk are NOT active. `config` is what the process
    /// actually holds now, which is what callbacks must reconcile against.
    Recovered {
        config: Config,
        reason: String,
        transient: bool,
    },
    /// The load failed outright and the running config is untouched.
    Failed(String),
}

impl ReloadOutcome {
    /// The config the process is running after this pass, when there is one.
    pub fn config(&self) -> Option<&Config> {
        match self {
            ReloadOutcome::Applied(config) => Some(config),
            ReloadOutcome::Recovered { config, .. } => Some(config),
            ReloadOutcome::Failed(_) => None,
        }
    }
}

/// Reload once: load the files, keep the last-known-good snapshot honest,
/// audit voice flags and publish the new values to `Config::current()`.
///
/// This is `Config::load_with_status()`, with the two rules that took incidents
/// to learn already applied:
///
/// - a broken `config.toml` must NOT be snapshotted over the good copy, or
///   recovery is poisoned exactly when it is needed;
/// - a reload that turns a voice engine off is logged with the file's mtime,
///   because no per-key writer claimed it (#1399).
pub fn reload_once() -> ReloadOutcome {
    let base = opencrabs_home();
    match Config::load_with_status() {
        Ok((new_config, status)) => {
            if status.recovered {
                // Say WHAT failed, not just that something did (#909).
                let reason = status
                    .recovery_reason
                    .clone()
                    .unwrap_or_else(|| "no reason recorded".to_string());
                // Config writes are not atomic, so a reader that lands
                // between truncate and write sees zero bytes — harmless
                // and self-healing. This used to be detected by matching
                // "line 1, column 1", which serde ALSO reports for a
                // field error against the struct, so `duplicate field`
                // was announced as a write race with the assurance that
                // the file was fine (#1116). The classifier now requires
                // positive evidence of an empty read and treats anything
                // unrecognised as real.
                let path_for_check = base.join("config.toml");
                let file_is_empty_now = std::fs::metadata(&path_for_check)
                    .map(|m| m.len() == 0)
                    .unwrap_or(false);
                let transient = crate::utils::config_reload_reason::is_transient_read_race(
                    &reason,
                    file_is_empty_now,
                );
                tracing::warn!(
                    "ConfigWatcher: load fell back to last-known-good (transient={transient}), snapshot left untouched. Reason: {reason}"
                );
                // The mirror still takes the recovered values: that is the
                // config the process is actually running, and callbacks
                // reconcile against it.
                Config::set_current(new_config.clone());
                return ReloadOutcome::Recovered {
                    config: new_config,
                    reason,
                    transient,
                };
            }
            // config.toml changed AND parsed cleanly — the real
            // "last known good" moment. Snapshot it now (debounced, so
            // once per edit) so recovery always has the latest valid
            // config, instead of a once-per-process snapshot.
            crate::config::save_last_good_config();
            // A voice engine that was on and is now off gets named
            // here with the file's mtime (#1399): the owner watched
            // voice disable itself for an evening and no line in the
            // log said which flag flipped or when the file changed.
            let switched_off = crate::config::voice_flag_flips::voice_flags_switched_off(
                &Config::current(),
                &new_config,
            );
            if !switched_off.is_empty() {
                let path = base.join("config.toml");
                let modified = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map(|t| format!("{:?}", t))
                    .unwrap_or_else(|e| format!("unknown ({e})"));
                tracing::warn!(
                    "ConfigWatcher: voice switched OFF by this reload of {} (mtime {}): {}. \
                     No per-key writer logged it, so the change came from a wholesale \
                     rewrite or another process.",
                    path.display(),
                    modified,
                    switched_off.join(", ")
                );
            }
            // Refresh the in-memory mirror so Config::current() readers
            // see the new values without touching disk.
            Config::set_current(new_config.clone());
            ReloadOutcome::Applied(new_config)
        }
        Err(e) => {
            tracing::warn!(
                "ConfigWatcher: reload failed, keeping current config: {}",
                e
            );
            ReloadOutcome::Failed(e.to_string())
        }
    }
}

/// The operator-facing line for a pass that did not apply cleanly, or None when
/// it did.
///
/// The terminal delivers it as a message on its channel, the window posts it in
/// the conversation: same words either way, only the delivery differs.
pub fn notice(outcome: &ReloadOutcome) -> Option<String> {
    let path = opencrabs_home().join("config.toml");
    match outcome {
        ReloadOutcome::Applied(_) => None,
        ReloadOutcome::Recovered {
            reason, transient, ..
        } => Some(if *transient {
            format!(
                "⚠️ Config reload{} read {} while it was being written \
                 and saw an empty file, so it is running on the previous \
                 config for now. This is a write race, not a problem with \
                 your file — it clears on the next save.",
                profile_suffix(),
                path.display(),
            )
        } else {
            format!(
                "⚠️ Config reload{} failed, so your edits to {} are NOT \
                 active — it is still running on the previous config.\n\n\
                 {}\n\nFix that and save; hot-reload applies automatically.",
                profile_suffix(),
                path.display(),
                reason,
            )
        }),
        ReloadOutcome::Failed(error) => Some(format!(
            "⚠️ config reload failed{} — keeping the running config. Error: {}. \
             Fix {} and save; hot-reload will apply automatically.",
            profile_suffix(),
            error,
            path.display(),
        )),
    }
}

/// Spawn a background task that watches config files and fires callbacks on change.
/// Debounces rapid file-save events (300 ms window) before reloading.
///
/// # Example
/// ```ignore
/// config_watcher::spawn(vec![
///     Arc::new(move |cfg| {
///         let state = telegram_state.clone();
///         tokio::spawn(async move {
///             state.update_allowed_users(cfg.channels.telegram.allowed_users).await;
///         });
///     }),
/// ]);
/// ```
pub fn spawn(
    callbacks: Vec<ReloadCallback>,
    notify: Option<ReloadNotify>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        let base = opencrabs_home();

        let (tx, rx) = std::sync::mpsc::channel();

        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    // The directory watch also sees the SQLite DB and other churn in
                    // ~/.opencrabs/, which would trigger a reload storm. React only
                    // to our three config files.
                    let relevant = event.paths.iter().any(|p| {
                        matches!(
                            p.file_name().and_then(|n| n.to_str()),
                            Some("config.toml" | "keys.toml" | "commands.toml")
                        )
                    });
                    if relevant {
                        let _ = tx.send(event);
                    }
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    tracing::error!("ConfigWatcher: failed to create watcher: {}", e);
                    return;
                }
            };

        // Watch the DIRECTORY, not the individual files. Editors and our own
        // toml_edit writes save atomically (write a temp file, then rename over
        // the target), which changes the file's inode. A file-level watch stays
        // bound to the now-deleted inode and silently misses every later edit,
        // the cause of "saved config but the daemon (or TUI) didn't hot-reload,
        // needed a restart." A directory watch survives renames and reliably
        // catches every save.
        if base.exists()
            && let Err(e) = watcher.watch(&base, RecursiveMode::NonRecursive)
        {
            tracing::error!("ConfigWatcher: cannot watch {:?}: {}", base, e);
            return;
        }

        tracing::info!(
            "ConfigWatcher: watching config.toml, keys.toml and commands.toml in {:?}",
            base
        );

        let debounce = Duration::from_millis(300);

        while rx.recv().is_ok() {
            // Drain further events within the debounce window
            let deadline = std::time::Instant::now() + debounce;
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match rx.recv_timeout(remaining) {
                    Ok(_) => {}
                    Err(_) => break,
                }
            }

            let outcome = reload_once();
            // A pass that did not apply cleanly says so in the same words the
            // window uses; only the delivery differs.
            if let Some(message) = notice(&outcome)
                && let Some(ref notify) = notify
            {
                notify(message);
            }
            let Some(config) = outcome.config() else {
                continue;
            };
            tracing::info!(
                "ConfigWatcher: reloaded, firing {} callback(s)",
                callbacks.len()
            );
            for cb in &callbacks {
                let cb = cb.clone();
                let cfg = config.clone();
                rt.spawn(async move { cb(cfg) });
            }
        }

        tracing::info!("ConfigWatcher: stopped");
    })
}
