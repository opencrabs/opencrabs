//! The channel chip: which channels the config says are live, who
//! owns them right now, and the one-line status it prints.
//!
//! Moved out of the crate root as pure code motion: the functions, their
//! comments and their tests are unchanged, only the module boundary is new.

use super::*;

/// Which messaging channels the config says are live: enabled and carrying the
/// credential that channel needs. Read once when Seeker opens, so the hint says
/// "not set up" only when that is true (#2462).
pub(crate) fn configured_channels() -> Vec<Channel> {
    match Config::load() {
        Ok(config) => configured_channels_from(&config),
        Err(_) => Vec::new(),
    }
}

/// The same answer for a config that is already loaded, so a hot reload can
/// refresh the cached list without taking a second disk read.
pub(crate) fn configured_channels_from(config: &Config) -> Vec<Channel> {
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

/// Who owns the channels right now (#2452): a token lock on disk says
/// something claimed them, a live instance lock says that something is
/// still running. Read-only: the GUI never constructs a ChannelManager
/// and never starts one on its own.
pub(crate) fn channel_ownership() -> channel_status::Ownership {
    let locks = opencrabs::config::profile::base_opencrabs_dir().join("locks");
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
    let profile = opencrabs::config::profile::active_profile().unwrap_or("default");
    let live = opencrabs::config::profile::instance_running_in(&locks.join("instance"), profile);
    channel_status::ownership(lock_present, live)
}

/// The chip line for the current config and lock state: `None` when every
/// configured channel is served or nothing is configured (#2452).
///
/// UI THREAD ONLY: `Config::load` builds the whole config by value and on
/// a GPUI background worker that build overflows the small stack (SIGILL,
/// 2026-11-03). Background callers use `channel_chip_line_from` with the
/// names read here once and passed in.
pub(crate) fn channel_chip_line() -> Option<SharedString> {
    let names: Vec<String> = configured_channels()
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    channel_chip_line_from(&names)
}

/// The chip line from a fixed set of channel names: the probe path. Never
/// touches Config, so it is safe on a background worker; the lock and
/// instance checks are small directory reads (#2452).
pub(crate) fn channel_chip_line_from(names: &[String]) -> Option<SharedString> {
    if names.is_empty() {
        return None;
    }
    let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
    channel_status::status_line(&borrowed, &channel_ownership()).map(SharedString::from)
}
