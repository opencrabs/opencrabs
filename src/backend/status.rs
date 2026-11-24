//! Channel status and git branch.
//!
//! Operations land here as their GUI doors move behind the contract.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::ui_core::palette::Channel;

use super::{BackendError, Ctx, Op, Scope};

/// Which messaging channels `config` says are live: enabled and carrying the
/// credential that channel needs (#2462). The rule the channel chip and the
/// finder share, here so the crab answers it about its own config.
pub fn configured_channels_from(config: &Config) -> Vec<Channel> {
    let channels = &config.channels;
    let filled = |value: &Option<String>| value.as_deref().is_some_and(|text| !text.is_empty());
    let mut live = Vec::new();
    if channels.telegram.enabled && filled(&channels.telegram.token) {
        live.push(Channel::Telegram);
    }
    if channels.discord.enabled && filled(&channels.discord.token) {
        live.push(Channel::Discord);
    }
    if channels.slack.enabled && filled(&channels.slack.token) && filled(&channels.slack.app_token)
    {
        live.push(Channel::Slack);
    }
    // WhatsApp pairs by QR: being enabled is the whole credential.
    if channels.whatsapp.enabled {
        live.push(Channel::WhatsApp);
    }
    if channels.trello.enabled
        && filled(&channels.trello.app_token)
        && filled(&channels.trello.token)
    {
        live.push(Channel::Trello);
    }
    live
}

/// The crab's channels: which ones its config makes live, and the two lock
/// probes that say who serves them (#2452, #2462). Read-only: nothing here
/// constructs a ChannelManager or claims a lock.
///
/// `configured` asks for the config read too. The chip's 5-second probe
/// leaves it off, because that probe never loaded the config and must not
/// start now; the finder turns it on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelStatus {
    /// Load the config and list the live channels.
    pub configured: bool,
}

/// What [`ChannelStatus`] answers. Plain names and booleans, so a surface
/// that is not Rust reads it without the window's enums.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelStatusReply {
    /// Live channel names, as [`Channel::name`] prints them, in the fixed
    /// order. Empty when not asked for or when the config does not load.
    pub configured: Vec<String>,
    /// At least one channel token lock file exists.
    pub lock_present: bool,
    /// The instance lock of the active profile names a live process.
    pub instance_live: bool,
}

impl Op for ChannelStatus {
    const NAME: &'static str = "status.channels";
    const SCOPE: Scope = Scope::Read;
    type Reply = ChannelStatusReply;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let configured = if self.configured {
            configured_channel_names()
        } else {
            Vec::new()
        };
        let locks = crate::config::profile::base_opencrabs_dir().join("locks");
        // Token locks are files named `{channel}_{hash}.lock`; the instance,
        // paths and scheduler locks live in their own subdirectories, so the
        // suffix test on files only sees the channel claims.
        let lock_present = std::fs::read_dir(&locks)
            .map(|entries| {
                entries.filter_map(Result::ok).any(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    e.path().is_file() && name.ends_with(".lock") && name.contains('_')
                })
            })
            .unwrap_or(false);
        let profile = crate::config::profile::active_profile().unwrap_or("default");
        let instance_live =
            crate::config::profile::instance_running_in(&locks.join("instance"), profile);
        Ok(ChannelStatusReply {
            configured,
            lock_present,
            instance_live,
        })
    }
}

/// The live channel names from the crab's config, read on a thread of its
/// own with a main-thread sized stack.
///
/// `Config::load` builds the whole config by value, and on a small stack
/// that build overflows (SIGILL, 2026-11-03): the window used to run it on
/// its UI thread only. An op runs on a 4 MB worker lane locally and on a
/// runtime thread remotely, so the load gets its own 8 MB thread and only
/// the names cross back. The profile home is task-local, so it is resolved
/// here and pinned again inside the thread: a crab answering under an
/// override reads that home's config, not the process default. A config
/// that does not load, or a thread that cannot start, answers no channels,
/// as the window did before.
fn configured_channel_names() -> Vec<String> {
    let home = crate::config::profile::resolve_profile_home();
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new()
            .name("status-channels".into())
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, || {
                crate::config::profile::with_home_override(home, || match Config::load() {
                    Ok(config) => configured_channels_from(&config)
                        .iter()
                        .map(|c| c.name().to_string())
                        .collect(),
                    Err(_) => Vec::new(),
                })
            });
        spawned
            .ok()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default()
    })
}

/// The checkout's branch name in `dir` on the crab, or `None` when the
/// directory is not a repo. Runs a `git` subprocess, so the caller keeps the
/// answer beside the directory and asks again only when that changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBranch {
    /// The directory to ask, on the crab.
    pub dir: PathBuf,
}

impl Op for GitBranch {
    const NAME: &'static str = "status.git_branch";
    const SCOPE: Scope = Scope::Read;
    type Reply = Option<String>;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let Ok(out) = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
        else {
            return Ok(None);
        };
        if !out.status.success() {
            return Ok(None);
        }
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(if name.is_empty() { None } else { Some(name) })
    }
}

ops! { ChannelStatus, GitBranch }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, parity};

    #[tokio::test]
    async fn channel_status_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(gateway, ChannelStatus { configured: true });
        assert!(answer["Ok"]["configured"].is_array(), "{answer}");
        let answer = parity!(gateway, ChannelStatus { configured: false });
        assert_eq!(answer["Ok"]["configured"], serde_json::json!([]), "{answer}");
    }

    #[tokio::test]
    async fn git_branch_has_parity() {
        let gateway = Gateway::start().await;
        // The temp home is usually no checkout, but TMPDIR may sit inside
        // one, so the test holds the doors to each other, not to `None`.
        let answer = parity!(
            gateway,
            GitBranch {
                dir: gateway.home().to_path_buf(),
            }
        );
        assert!(
            answer["Ok"].is_null() || answer["Ok"].is_string(),
            "{answer}"
        );
    }

    #[test]
    fn configured_channels_need_their_credential() {
        let mut config = Config::default();
        config.channels.discord.enabled = false;
        config.channels.slack.enabled = false;
        config.channels.whatsapp.enabled = false;
        config.channels.trello.enabled = false;
        config.channels.telegram.enabled = true;
        config.channels.telegram.token = None;
        assert!(configured_channels_from(&config).is_empty());
        config.channels.telegram.token = Some("t".into());
        config.channels.whatsapp.enabled = true;
        assert_eq!(
            configured_channels_from(&config),
            [Channel::Telegram, Channel::WhatsApp]
        );
    }
}
