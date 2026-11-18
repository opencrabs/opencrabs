//! The window's config hot reload.
//!
//! The terminal has reloaded `config.toml` on save for months
//! (`utils::config_watcher`), and the window already starts that same watcher
//! when it boots its channel layer (`services::surface`). What the window never
//! had is a way to hear about it: its view state (the settings snapshot, the
//! channel names, the palette and font caches) was read once at boot, so a
//! hand edit, or the agent writing config on its own, stayed invisible until a
//! restart.
//!
//! This module is the missing half. It runs its own watcher thread, because the
//! surface can refuse to boot (another instance holding the profile) and the
//! window still has to reflect config, and it hands each pass to the UI as a
//! message the poll tick drains. The pass itself is
//! `config_watcher::reload_once`, so the load, the last-good snapshot, the
//! recovery rule and the `Config::current()` swap are the terminal's own code:
//! the load is `Config::load_with_status()` under the hood, not a second
//! interpretation of it.
//!
//! Nothing here writes config. A watcher that writes what it reads is how the
//! terminal grew a reload loop once already, and the rule is the same inside a
//! window.

use notify::Watcher;
use opencrabs::config::{Config, opencrabs_home};
use opencrabs::utils::config_watcher::{self, ReloadOutcome};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use crate::logging;

/// The stack the watcher thread runs on.
///
/// Loading config off the UI thread is what killed the window on 2026-11-03:
/// `Config::load` builds the whole config by value, and running it on a GPUI
/// background worker overflowed that worker's stack and died with SIGILL five
/// times (#2452). Every config or database path in `agent_bridge` therefore
/// runs on a 4 MB thread, and this watcher follows the same rule rather than
/// loading config on whatever stack the window happens to give it.
const STACK_BYTES: usize = 4 * 1024 * 1024;

/// How long one save's burst of filesystem events is folded into one reload,
/// matching the terminal's watcher so both halves of a process react together.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// The three files a reload reacts to.
///
/// The directory watch also sees the ledger, the logs and the last-good
/// snapshots. Reacting to those would make the reload trigger itself.
pub(crate) fn is_config_file(name: &str) -> bool {
    matches!(name, "config.toml" | "keys.toml" | "commands.toml")
}

/// What one reload means for the window.
pub enum Reload {
    /// The files parsed and the in-memory mirror already holds them.
    Applied(Config),
    /// The edits did not apply. `config` is what the process is running now:
    /// the recovered last-known-good on a parse failure, nothing when the load
    /// failed outright. `notice` is the line the reader should see.
    Rejected {
        config: Option<Config>,
        notice: String,
    },
}

/// Turn one reload pass into what the window needs.
///
/// A pass that did not apply cleanly still carries the config the process is
/// running, because that is what the window must show; the reason travels with
/// it as the line to post.
fn classify(outcome: ReloadOutcome) -> Reload {
    let notice = config_watcher::notice(&outcome);
    match outcome {
        ReloadOutcome::Applied(config) => Reload::Applied(config),
        ReloadOutcome::Recovered { config, .. } => Reload::Rejected {
            config: Some(config),
            notice: notice.unwrap_or_default(),
        },
        ReloadOutcome::Failed(_) => Reload::Rejected {
            config: None,
            notice: notice.unwrap_or_default(),
        },
    }
}

/// Start the watcher thread and hand back the channel the poll tick drains.
///
/// The thread stays up for the life of the process and stops on the next config
/// change after the window's receiver is dropped, so a closed window leaves no
/// watcher behind for long.
pub fn spawn() -> Receiver<Reload> {
    let (tx, rx) = channel();
    let started = std::thread::Builder::new()
        .name("gui-config-reload".into())
        .stack_size(STACK_BYTES)
        .spawn(move || watch(tx));
    if let Err(e) = started {
        // A failed thread leaves the receiver disconnected, and the poll tick
        // drops it on the first read: the window keeps working without live
        // reload rather than pretending it has one.
        logging::error(&format!("config reload: thread not started: {e}"));
    }
    rx
}

fn watch(tx: Sender<Reload>) {
    let base = opencrabs_home();
    let (event_tx, event_rx) = std::sync::mpsc::channel();

    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && event.paths.iter().any(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(is_config_file)
                })
            {
                let _ = event_tx.send(());
            }
        }) {
            Ok(watcher) => watcher,
            Err(e) => {
                logging::error(&format!("config reload: no watcher: {e}"));
                return;
            }
        };

    // The directory, not the files: an atomic save renames a temp file over the
    // target, and a file-level watch stays bound to the old inode and silently
    // misses every later edit. Same reason as the terminal's watcher.
    if base.exists()
        && let Err(e) = watcher.watch(&base, notify::RecursiveMode::NonRecursive)
    {
        logging::error(&format!(
            "config reload: cannot watch {}: {e}",
            base.display()
        ));
        return;
    }
    logging::info(&format!(
        "config reload: watching config.toml, keys.toml and commands.toml in {}",
        base.display()
    ));

    while event_rx.recv().is_ok() {
        // Fold the burst of events one save produces into a single pass.
        let deadline = std::time::Instant::now() + DEBOUNCE;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match event_rx.recv_timeout(remaining) {
                Ok(()) => {}
                Err(_) => break,
            }
        }

        let event = classify(config_watcher::reload_once());
        let applied = matches!(&event, Reload::Applied(_));
        if tx.send(event).is_err() {
            break;
        }
        logging::info(if applied {
            "config reload: applied, window state refreshing"
        } else {
            "config reload: did not apply, saying so in the conversation"
        });
    }

    logging::info("config reload: stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a save of one of the three config files starts a reload. The
    /// directory watch also sees the ledger, the logs and the last-good
    /// snapshots, and reloading on those would make every pass trigger the next
    /// one.
    #[test]
    fn only_the_three_config_files_start_a_reload() {
        for watched in ["config.toml", "keys.toml", "commands.toml"] {
            assert!(is_config_file(watched), "{watched} must reload");
        }
        for ignored in [
            "opencrabs.db",
            "opencrabs.db-wal",
            "config.last_good.toml",
            "keys.last_good.toml",
            "opencrabs-gui.log",
            "rail_seen.json",
            "config.toml.bak",
        ] {
            assert!(!is_config_file(ignored), "{ignored} must not reload");
        }
    }

    /// A load that failed outright carries nothing to show and a line to say.
    /// The window must not read it as an applied pass and quietly drop the
    /// reason, which is the whole point of carrying it across the channel.
    #[test]
    fn a_failed_load_carries_no_config_and_the_reason() {
        match classify(ReloadOutcome::Failed("duplicate field `agent`".into())) {
            Reload::Rejected { config, notice } => {
                assert!(config.is_none(), "a failed load has nothing new to show");
                assert!(
                    notice.contains("duplicate field `agent`"),
                    "the reason has to reach the window: {notice}"
                );
                assert!(
                    notice.contains("keeping the running config"),
                    "the line says the old config is still in force: {notice}"
                );
            }
            Reload::Applied(_) => panic!("a failed load must never read as applied"),
        }
    }

    /// A recovered pass is still a rejected one, and it carries the config the
    /// process is running so the window can show it. A write race and a real
    /// field error read differently, which is the whole reason the classifier
    /// exists (#1116), so both wordings have to survive the trip.
    #[test]
    fn a_recovered_pass_carries_the_running_config_and_says_why() {
        let config = Config::default();
        let raced = classify(ReloadOutcome::Recovered {
            config: config.clone(),
            reason: "EOF while parsing a value at line 1 column 0".into(),
            transient: true,
        });
        match raced {
            Reload::Rejected {
                config: carried,
                notice,
            } => {
                assert!(carried.is_some(), "the running config has to be shown");
                assert!(
                    notice.contains("write race"),
                    "an empty read is named as a race, not as a bad file: {notice}"
                );
            }
            Reload::Applied(_) => panic!("a recovered pass is never applied"),
        }

        let broken = classify(ReloadOutcome::Recovered {
            config,
            reason: "duplicate field `agent`".into(),
            transient: false,
        });
        match broken {
            Reload::Rejected {
                config: carried,
                notice,
            } => {
                assert!(carried.is_some());
                assert!(
                    notice.contains("duplicate field `agent`") && notice.contains("NOT"),
                    "a real parse error says the edits are not active: {notice}"
                );
            }
            Reload::Applied(_) => panic!("a recovered pass is never applied"),
        }
    }
}
