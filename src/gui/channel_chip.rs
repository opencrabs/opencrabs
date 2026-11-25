//! The channel chip: which channels the crab's config says are live, who
//! owns them right now, and the one-line status it prints.
//!
//! Every read goes to the crab the window holds, through its backend, the
//! same `ChannelStatus` op for this machine's crab and a remote one. Nothing
//! here reads this machine's config or locks on its own, so a window looking
//! at another crab never prints this machine's channels as that crab's.

use super::*;
use crate::backend_glue::Backend;
use opencrabs::backend::status::ChannelStatus;

/// Which messaging channels the crab's config says are live: enabled and
/// carrying the credential that channel needs. Read when Seeker opens, so the
/// hint says "not set up" only when that is true (#2462).
pub(crate) async fn configured_channels(backend: Backend) -> Vec<Channel> {
    let Ok(status) = backend.call(ChannelStatus { configured: true }).await else {
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

/// The names the chip keys on, in the order the channels came.
pub(crate) fn channel_names(channels: &[Channel]) -> Vec<String> {
    channels.iter().map(|c| c.name().to_string()).collect()
}

/// The channels named in `names`, for a caller that holds the chip's names
/// and wants the window's enums back without a read.
pub(crate) fn channels_named(names: &[String]) -> Vec<Channel> {
    names
        .iter()
        .filter_map(|name| Channel::ALL.iter().copied().find(|c| c.name() == name))
        .collect()
}

/// Who owns the crab's channels right now (#2452): a token lock says
/// something claimed them, a live instance lock says that something is
/// still running. Read-only: the GUI never constructs a ChannelManager
/// and never starts one on its own.
pub(crate) async fn channel_ownership(backend: Backend) -> channel_status::Ownership {
    let status = backend
        .call(ChannelStatus { configured: false })
        .await
        .unwrap_or_default();
    channel_status::ownership(status.lock_present, status.instance_live)
}

/// The chip line for the crab's config and lock state: `None` when every
/// configured channel is served or nothing is configured (#2452).
pub(crate) async fn channel_chip_line(backend: Backend) -> Option<SharedString> {
    let names = channel_names(&configured_channels(backend.clone()).await);
    channel_chip_line_from(backend, names).await
}

/// The chip line from a fixed set of channel names: the probe path. Asks the
/// crab only for its locks, never for its config (#2452).
pub(crate) async fn channel_chip_line_from(
    backend: Backend,
    names: Vec<String>,
) -> Option<SharedString> {
    if names.is_empty() {
        return None;
    }
    let ownership = channel_ownership(backend).await;
    let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
    channel_status::status_line(&borrowed, &ownership).map(SharedString::from)
}

impl Dashboard {
    /// Re-read the chip's channel names from the crab the window holds, then
    /// the chip line for them.
    ///
    /// A crab swap calls this: the names the window kept belong to the crab it
    /// left. An answer for a crab the window has since left is dropped, by
    /// the same epoch the handshake uses.
    pub(crate) fn refresh_chip_names(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let epoch = self.instance_epoch;
        cx.spawn(async move |this, cx| {
            let channels = configured_channels(backend).await;
            this.update(cx, |this, cx| {
                if this.instance_epoch != epoch {
                    return;
                }
                this.chip_channel_names = channel_names(&channels);
                if this.finder.is_some() {
                    this.finder_channels = channels;
                }
                this.refresh_channel_chip(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Seeker's live channels: the names the chip already holds at once, so
    /// the hint has an answer on its first frame, then the crab's own reading
    /// when it lands (#2462).
    pub(crate) fn load_finder_channels(&mut self, cx: &mut Context<Self>) {
        self.finder_channels = channels_named(&self.chip_channel_names);
        let backend = self.backend.clone();
        let epoch = self.instance_epoch;
        cx.spawn(async move |this, cx| {
            let channels = configured_channels(backend).await;
            this.update(cx, |this, cx| {
                if this.instance_epoch != epoch || this.finder.is_none() {
                    return;
                }
                if this.finder_channels != channels {
                    this.finder_channels = channels;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}
