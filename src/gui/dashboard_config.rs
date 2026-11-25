//! What the window re-derives when `config.toml` changes (#3040).
//!
//! The window read its config once, when it opened, so a save never reached the
//! view: not a hand edit, and not the agent writing config on its own.
//! `config_reload` runs the watcher and hands each pass over here. The pass is
//! the terminal's own `config_watcher::reload_once`, which has already loaded
//! the files, kept the last-good snapshot honest and published the new values
//! to `Config::current()`. So this file refreshes only what the window cached
//! at boot, and everything that reads `Config::current()` per frame is already
//! up to date before it runs.
//!
//! Part of the dashboard split (#2822).

use super::*;

impl Dashboard {
    /// Drain every reload the watcher thread has queued.
    ///
    /// One `try_recv` per poll tick when nothing happened, which is the common
    /// case: a save is the only thing that ever puts a value here.
    pub(crate) fn drain_config_reloads(&mut self, cx: &mut Context<Self>) {
        let mut queued = Vec::new();
        while let Ok(reload) = self.config_reload.try_recv() {
            queued.push(reload);
        }
        for reload in queued {
            self.apply_config_reload(reload, cx);
        }
    }

    /// Fold one reload into the window.
    fn apply_config_reload(&mut self, reload: config_reload::Reload, cx: &mut Context<Self>) {
        let (config, notice) = match reload {
            config_reload::Reload::Applied(config) => (Some(config), None),
            config_reload::Reload::Rejected { config, notice } => (config, Some(notice)),
        };
        // A rejected write still carries the config the process is running
        // (recovered last-known-good), and that is what the window must show.
        if let Some(config) = config {
            self.refresh_from_config(&config, cx);
        }
        if let Some(notice) = notice {
            logging::warn(&format!("config reload: {notice}"));
            // When the surface is serving, its own watcher already says this
            // line in the conversation. Two watchers on one file must not make
            // the reader hear it twice, so the window speaks only for itself,
            // which is the case where no surface booted and nothing else can.
            if !agent_bridge::surface_serving() {
                self.turn.live.notices.push(notice);
            }
            cx.notify();
        }
    }

    /// Rebuild the values the window keeps instead of reading the disk per
    /// frame. Everything else on the window already reads `Config::current()`,
    /// which the reload swapped in place.
    fn refresh_from_config(&mut self, config: &Config, cx: &mut Context<Self>) {
        // The file that reloaded is this machine's. Its channels are the
        // chip's only while the window holds this machine's crab: a crab
        // elsewhere keeps the names its own reply gave.
        let this_machine = self
            .backend
            .lacks(opencrabs::backend::cap::Cap::Setup)
            .is_none();
        let names: Vec<String> = configured_channels_from(config)
            .iter()
            .map(|channel| channel.name().to_string())
            .collect();
        if this_machine && names != self.chip_channel_names {
            logging::info(&format!(
                "config reload: {} configured channel(s)",
                names.len()
            ));
            self.chip_channel_names = names;
            // The chip's line is worked out off the UI thread, so ask it to
            // run now rather than leave the previous answer up for up to five
            // seconds (#2452).
            self.refresh_channel_chip(cx);
        }
        // Seeker reads the live channels when it opens. While it is open it has
        // to be told, or its hint keeps saying "not set up" for a channel that
        // was just configured (#2462).
        if this_machine && self.finder.is_some() {
            self.finder_channels = configured_channels_from(config);
        }
        // A hand edit or an agent write while the modal is open: re-read the
        // rows from the crab the window holds, not from this machine's file.
        // The cursor, the open section, the status line and any credential
        // being typed are left alone, so a reload never moves the reader's
        // place.
        if self.settings_open {
            self.reload_settings(cx);
        }
        cx.notify();
    }
}
