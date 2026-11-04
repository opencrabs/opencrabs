//! `/cowork` on Discord (#2015): the owner opens a channel or thread to its
//! members, the way `/cowork` opens a Telegram group.
//!
//! The write is `[channels.discord.channels.<id>]` with `open = true` and the
//! channel's `name`. Members are not registered anywhere: `open` already
//! admits them (#2014), so there is no per-member list to keep in step.

use serenity::http::Http;
use serenity::model::channel::Channel;
use serenity::model::id::ChannelId;

use crate::config::Config;

const RESTRICTED: &str = "🔒 `/cowork` is restricted to the bot owner.";
const NOT_IN_SERVER: &str =
    "ℹ️ `/cowork` opens a server channel. Run it in the channel you want open.";

/// Whether `text` is a `/cowork` command, with or without trailing text.
pub(crate) fn is_cowork_command(text: &str) -> bool {
    text.trim()
        .strip_prefix("/cowork")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

/// Persist the channel's open entry. Goes through the guarded `write_key`
/// path, which Telegram's `/cowork` also uses.
///
/// The block is the channel's scope: `open` admits members, `respond_to = "all"`
/// answers them, and `name` labels it. `[channels.discord.channels.<id>]` is
/// what the gate checks, so the channel is admitted without touching the
/// global `allowed_channels` list.
pub(crate) fn write_channel_open(channel_id: &str, name: Option<&str>) -> Result<(), String> {
    let section = format!("channels.discord.channels.{channel_id}");
    Config::write_key(&section, "open", "true").map_err(|e| e.to_string())?;
    Config::write_key(&section, "respond_to", "all").map_err(|e| e.to_string())?;
    if let Some(name) = name {
        Config::write_key_string(&section, "name", name).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The reply for `/cowork` typed in a Discord channel or thread. `None` when
/// `text` is not the command. Nothing is written unless the owner asked and
/// the channel is a server channel.
pub(crate) fn cowork_discord_channel(
    text: &str,
    is_owner: bool,
    in_server: bool,
    channel_id: &str,
    channel_name: Option<&str>,
    write: impl FnOnce(&str, Option<&str>) -> Result<(), String>,
) -> Option<String> {
    if !is_cowork_command(text) {
        return None;
    }
    if !is_owner {
        return Some(RESTRICTED.to_string());
    }
    if !in_server {
        return Some(NOT_IN_SERVER.to_string());
    }
    match write(channel_id, channel_name) {
        Ok(()) => Some(format!(
            "✅ This channel is open: any member can talk to me here, threads and forum posts included. Recorded as `[channels.discord.channels.{channel_id}]` (open = true). Members are not registered: access comes from `open`."
        )),
        Err(e) => Some(format!("❌ Could not open this channel: {e}")),
    }
}

/// The name of a server channel or thread, for the config's display field.
/// `None` when the lookup fails; the open flag is still written in that case.
pub(crate) async fn channel_name(http: &Http, channel: ChannelId) -> Option<String> {
    match channel.to_channel(http).await {
        Ok(Channel::Guild(gc)) => Some(gc.name),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("Discord: could not read the name of channel {channel}: {e}");
            None
        }
    }
}
