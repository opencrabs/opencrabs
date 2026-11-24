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
    let Ok(status) =
        crate::status::ask_crab(opencrabs::backend::status::ChannelStatus { configured: true })
    else {
        return Vec::new();
    };
    status
        .configured
        .iter()
        .filter_map(|name| Channel::ALL.iter().copied().find(|c| c.name() == name))
        .collect()
}

/// The same answer for a config that is already loaded, so a hot reload can
/// refresh the cached list without taking a second disk read.
pub(crate) fn configured_channels_from(config: &Config) -> Vec<Channel> {
    opencrabs::backend::status::configured_channels_from(config)
}

/// Who owns the channels right now (#2452): a token lock on disk says
/// something claimed them, a live instance lock says that something is
/// still running. Read-only: the GUI never constructs a ChannelManager
/// and never starts one on its own.
pub(crate) fn channel_ownership() -> channel_status::Ownership {
    let status =
        crate::status::ask_crab(opencrabs::backend::status::ChannelStatus { configured: false })
            .unwrap_or_default();
    channel_status::ownership(status.lock_present, status.instance_live)
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
