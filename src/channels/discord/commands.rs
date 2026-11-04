//! Native Discord application commands for the Discord channel (#1850).
//!
//! `commands.toml` is the single source of truth for user commands on every
//! channel: Telegram renders it as the BotFather menu (see
//! `src/channels/telegram/menu_refresh.rs`), the TUI completes it, and the
//! `slash_command` tool executes it. Discord had no projection at all, so the
//! commands existed and were invisible: there was no `/` autocomplete and no way
//! to invoke them except typing the name into a message.
//!
//! Scope note: this projects `commands.toml` and nothing else. Telegram's menu
//! additionally lists built-ins and skills; Discord's list here does not, because
//! #1850 named the file rather than the whole catalog. Skills still work when
//! typed as text, through the normal message path. Adding them is a loop over
//! `brain::skills::load_all_skills()` in [`sync_commands`], and it would need its
//! own decision about which built-ins belong in a guild menu.
//!
//! This module is the projection. It turns the catalog into CHAT_INPUT builders
//! and pushes them with the *bulk overwrite* route (`GuildId::set_commands`,
//! which is `PUT /applications/{application.id}/guilds/{guild.id}/commands`), so
//! a sync replaces the whole set instead of creating commands one by one. That
//! distinction matters for Discord's budgets: the 200-per-day-per-guild limit
//! (`developers/interactions/application-commands.mdx:208`) counts per-command
//! **creates**, which bulk overwrite does not perform.
//!
//! Everything here is pure except [`sync_commands`], which takes the HTTP handle
//! and a guild list. Splitting it that way is what makes the grammar rules
//! testable without a Discord application, because there is no bot in CI.
//!
//! ## Name and description grammar
//!
//! Discord rejects a command whose name is not `^[\w-]{1,32}$` (lowercase, at
//! most 32 characters) or whose description is longer than 100 characters, and
//! caps a guild at 100 CHAT_INPUT commands with 8000 characters across the whole
//! command tree. The catalog is free-form text, so every one of those is a real
//! failure mode rather than a theoretical one. The rules are applied in order:
//! sanitize, then dedupe by the sanitized name, then cap, then tree size.
//!
//! Dropping is the only sane option where a rule bites. A rejected name would
//! otherwise take the whole sync with it, including every command that was fine,
//! so the bad entry is dropped loudly and the rest of the guild keeps its menu.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::http::Http;
use serenity::model::application::{CommandOptionType, CommandType};
use serenity::model::id::{ChannelId, GuildId};

use crate::brain::{BrainLoader, CommandLoader, UserCommand};

/// Discord's `^[\w-]{1,32}$` name cap.
pub(crate) const NAME_MAX: usize = 32;

/// CHAT_INPUT descriptions cap at 100 characters.
pub(crate) const DESCRIPTION_MAX: usize = 100;

/// CHAT_INPUT commands per guild (`application-commands.mdx:201`).
pub(crate) const GUILD_COMMAND_CAP: usize = 100;

/// Character budget across one guild's command tree (`:481`).
pub(crate) const TREE_CHAR_CAP: usize = 8000;

/// Every user command is registered with exactly this one optional string
/// option. The argument text is not parsed here: the interaction arm rebuilds
/// `"/<name> <args>"` and feeds it to the same turn router a typed message
/// uses, so the `slash_command` tool sees what it would have seen from chat.
/// Parsing per-command in the channel would fork command semantics into two
/// implementations, and only Discord would behave that way.
pub(crate) const ARGS_OPTION: &str = "args";

/// Description shown for [`ARGS_OPTION`] in the client's argument UI.
pub(crate) const ARGS_OPTION_DESCRIPTION: &str = "Text to pass to the command";

/// Why an entry did not make it into the command set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DropReason {
    /// Nothing legal survived sanitizing, so there is no name to register.
    InvalidName,
    /// Another entry already holds this Discord name. The first one in catalog
    /// order wins, and `kept` names it.
    Collision {
        /// Catalog name that already owns the sanitized name.
        kept: String,
    },
    /// Past [`GUILD_COMMAND_CAP`]; catalog order decides who is kept.
    OverCap,
    /// Including this command would push the tree past [`TREE_CHAR_CAP`].
    TreeTooLarge,
}

/// One catalog entry that was left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dropped {
    /// The name as written in the catalog.
    pub(crate) source: String,
    /// Why it did not make it in.
    pub(crate) reason: DropReason,
}

/// The command set a guild should end up with, plus what was excluded.
#[derive(Debug, Clone, Default)]
pub(crate) struct CommandPlan {
    /// Builders in guild order, ready for bulk overwrite.
    pub(crate) commands: Vec<CreateCommand>,
    /// `(registered name, catalog name, description)` parallel with
    /// [`CommandPlan::commands`]. Held because the builders do not expose their
    /// fields, so without this the plan could not be inspected or signed.
    pub(crate) entries: Vec<(String, String, String)>,
    /// Catalog entries left out, in catalog order.
    pub(crate) dropped: Vec<Dropped>,
}

impl CommandPlan {
    /// A signature that changes exactly when the registered set changes, and
    /// not when the catalog is merely reordered. Same shape as
    /// `menu_refresh::skills_signature`: identity over a sorted projection of
    /// the fields that end up on the wire.
    pub(crate) fn signature(&self) -> u64 {
        let mut items: Vec<(String, String)> = self
            .entries
            .iter()
            .map(|(n, _, d)| (n.clone(), d.clone()))
            .collect();
        items.sort();
        let mut hasher = DefaultHasher::new();
        for (name, description) in &items {
            name.hash(&mut hasher);
            description.hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// Turn a catalog name into a Discord-legal command name: drop the leading
/// slash, lowercase it, map anything outside `[a-z0-9_-]` to `-`, trim edge
/// dashes and clamp to [`NAME_MAX`]. Returns `None` when nothing legal is left.
pub(crate) fn sanitize_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('/').to_lowercase();
    let mut out = String::new();
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        return None;
    }
    Some(out.chars().take(NAME_MAX).collect())
}

/// The description to register: trimmed, falling back to the command name when
/// the catalog left it empty (Discord requires a non-empty CHAT_INPUT
/// description), clamped to [`DESCRIPTION_MAX`].
pub(crate) fn description_for(description: &str, name: &str) -> String {
    let trimmed = description.trim();
    let chosen = if trimmed.is_empty() { name } else { trimmed };
    chosen.chars().take(DESCRIPTION_MAX).collect()
}

/// Project the catalog onto Discord's command grammar. Catalog order is
/// preserved and the first entry wins a sanitized-name collision, which is the
/// ordering rule `trim_catalog_to_budget` already applies on Telegram, so both
/// channels drop the same entries for the same reason.
pub(crate) fn plan_commands(catalog: &[UserCommand]) -> CommandPlan {
    let mut plan = CommandPlan::default();
    let mut tree_chars = 0usize;

    for entry in catalog {
        let Some(name) = sanitize_name(&entry.name) else {
            tracing::warn!(
                "discord: dropping command {:?}: no legal Discord name after sanitizing",
                entry.name
            );
            plan.dropped.push(Dropped {
                source: entry.name.clone(),
                reason: DropReason::InvalidName,
            });
            continue;
        };

        if let Some((_, winner, _)) = plan
            .entries
            .iter()
            .find(|(registered, _, _)| registered == &name)
        {
            tracing::warn!(
                "discord: dropping command {:?}: it maps to {:?}, already used by {:?}",
                entry.name,
                name,
                winner
            );
            plan.dropped.push(Dropped {
                source: entry.name.clone(),
                reason: DropReason::Collision {
                    kept: winner.clone(),
                },
            });
            continue;
        }

        if plan.commands.len() >= GUILD_COMMAND_CAP {
            plan.dropped.push(Dropped {
                source: entry.name.clone(),
                reason: DropReason::OverCap,
            });
            continue;
        }

        let description = description_for(&entry.description, &name);
        let cost = name.chars().count()
            + description.chars().count()
            + ARGS_OPTION.len()
            + ARGS_OPTION_DESCRIPTION.len();
        if tree_chars + cost > TREE_CHAR_CAP {
            plan.dropped.push(Dropped {
                source: entry.name.clone(),
                reason: DropReason::TreeTooLarge,
            });
            continue;
        }
        tree_chars += cost;

        let builder = CreateCommand::new(name.clone())
            .description(description.clone())
            .kind(CommandType::ChatInput)
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    ARGS_OPTION,
                    ARGS_OPTION_DESCRIPTION,
                )
                .required(false),
            );

        plan.entries.push((name, entry.name.clone(), description));
        plan.commands.push(builder);
    }

    plan
}

/// Comparison key for one sync: the command-set signature mixed with the guild
/// membership. Both matter and neither is enough alone.
///
/// The catalog part is what keeps a flapping gateway quiet: the retry loop
/// rebuilds the client every few seconds, and a reconnect that re-PUT the whole
/// tree to every guild on each pass spends the 5-per-second per-route bucket
/// for an identical result. The membership part is what keeps a guild the bot
/// joined mid-session from being missed until a config write, because joining
/// moves the key even when `commands.toml` has not been touched.
///
/// Guild ids are sorted first, so the key depends on the set and not on the
/// order the gateway happened to report them in.
pub(crate) fn sync_key(plan_sig: u64, guilds: &[GuildId]) -> u64 {
    let mut ids: Vec<u64> = guilds.iter().map(|guild| guild.get()).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut hasher = DefaultHasher::new();
    plan_sig.hash(&mut hasher);
    ids.hash(&mut hasher);
    hasher.finish()
}

/// Load `commands.toml` and register it on every guild we share, but only when
/// the comparison key moved. Returns the key that is live now, for the caller to
/// store and hand back on the next call, or `None` when nothing could be
/// registered, which tells the caller not to remember this attempt as done.
///
/// An empty catalog syncs an empty command list rather than being skipped: the
/// file is the source of truth on every other surface, so a command the user
/// deleted must not keep living in Discord's menu pointing at a prompt that no
/// longer exists. That case is logged, because wiping a guild's menu is visible
/// enough to be worth a line.
///
/// Skipping an unchanged re-read is the point of the key: the config watcher
/// fires on every write, and a sync that re-sends an identical set costs an API
/// round-trip per guild for nothing. The 5-per-second per-route bucket
/// (`application-commands.mdx:206`) is what a chatty watcher would hit, not the
/// 200-per-day create budget, which bulk overwrite does not touch.
/// Built-ins offered in the Discord `/` menu beside `commands.toml` (#2013).
/// Each is answered by the interaction path, never routed to the model, so an
/// entry here has to be handled there too.
pub(crate) fn with_menu_builtins(mut catalog: Vec<UserCommand>) -> Vec<UserCommand> {
    const MENU_BUILTINS: &[(&str, &str)] = &[
        ("/respond_to", "Show this bot's respond mode (owner only)"),
        ("/cowork", "Open this channel to its members (owner only)"),
    ];
    for (name, description) in MENU_BUILTINS {
        if !catalog.iter().any(|c| c.name == *name) {
            catalog.push(UserCommand {
                name: (*name).to_string(),
                description: (*description).to_string(),
                action: "system".to_string(),
                prompt: String::new(),
            });
        }
    }
    catalog
}

pub(crate) async fn sync_commands(
    http: &Arc<Http>,
    guilds: &[GuildId],
    last_key: Option<u64>,
) -> Option<u64> {
    let catalog =
        with_menu_builtins(CommandLoader::from_brain_path(&BrainLoader::resolve_path()).load());

    if catalog.is_empty() {
        tracing::info!(
            "discord: command catalog is empty, syncing an empty command list to {} guild(s)",
            guilds.len()
        );
    }

    let plan = plan_commands(&catalog);
    let key = sync_key(plan.signature(), guilds);
    if Some(key) == last_key {
        tracing::debug!(
            "discord: application commands unchanged ({} registered), skipping sync",
            plan.commands.len()
        );
        return Some(key);
    }

    if guilds.is_empty() {
        tracing::warn!(
            "discord: no guild to register {} application command(s) in",
            plan.commands.len()
        );
        return Some(key);
    }

    let mut any_succeeded = false;
    for guild in guilds {
        match guild.set_commands(http, plan.commands.clone()).await {
            Ok(registered) => {
                any_succeeded = true;
                tracing::info!(
                    "discord: synced {} application command(s) to guild {guild}",
                    registered.len()
                );
            }
            Err(why) => {
                // A bot invited before this feature existed has the `bot` scope
                // but not `applications.commands`, and every call here answers
                // 403. Naming the fix in the log is the only way an operator
                // reading one line knows to re-invite instead of restart.
                tracing::error!(
                    "discord: application command sync failed for guild {guild}: {why}. If \
                     this is a permissions error, the bot was invited without the \
                     `applications.commands` OAuth2 scope: re-invite it with \
                     `bot applications.commands`."
                );
            }
        }
    }

    // A sync where every guild answered 403 must not remember itself as done.
    // The usual cause is the missing `applications.commands` scope, and the
    // operator fixes that by re-inviting, often without restarting the process
    // and without touching `commands.toml`. Storing the key on failure would
    // mean the newly granted scope changes nothing until something else moves
    // it, so the menu stays empty after a fix that looks like it did not work.
    // Leaving it unset makes the next reconnect try again, and the error line
    // keeps naming the fix until it lands.
    if !any_succeeded {
        tracing::warn!(
            "discord: application command sync failed for all {} guild(s); leaving the \
             comparison key unset so the next reconnect retries",
            guilds.len()
        );
        return None;
    }

    Some(key)
}

/// The OC-02 deny-by-default verdict, the rule `handle_message` applies to
/// typed text and this channel's only access control. An empty allowlist with
/// no roles and no owner is UNCONFIGURED, and unconfigured denies everybody:
/// the old "empty means everyone" reading made a half-configured Discord bot
/// public. Otherwise the owner, an allowlisted id, or a holder of an allowed
/// role is admitted, and so is any member of a channel that is `open` there
/// (#2014). `open_here` never overrides `unconfigured`.
///
/// Extracted pure so the branch can be pinned without a gateway.
/// Whether a message or interaction is the owner's `/cowork` or `/respond_to`.
/// Those pass the channel scope and the `dm_only` policy: they are how the owner
/// opens or fixes a channel, so the gate must not block them (#2060 cowork gate).
pub(crate) fn owner_scope_text(text: &str, is_owner: bool) -> bool {
    is_owner
        && (crate::channels::respond_to_scope::is_respond_to_command(text)
            || super::cowork::is_cowork_command(text))
}

/// The reply to an interaction the gate refused. It names the check that
/// failed and the config key or owner command that fixes it.
pub(crate) fn refusal_reason(
    admitted: bool,
    channel_ok: bool,
    policy_ok: bool,
    channel_id: &str,
) -> String {
    if !admitted {
        "You are not allowed to use this bot here. Your user id is not in \
         channels.discord.allowed_users or allowed_roles, and this channel is not \
         open. Ask the bot owner to add you, or to run /cowork in this channel."
            .to_string()
    } else if !channel_ok {
        format!(
            "This channel is not in channels.discord.allowed_channels. The bot owner \
             can run /cowork here to enable it, or add a \
             [channels.discord.channels.{channel_id}] block to config.toml."
        )
    } else if !policy_ok {
        format!(
            "This channel is set to dm_only, so the bot does not answer commands here. \
             The bot owner can run /respond_to all here, or set \
             channels.discord.channels.{channel_id}.respond_to to \"all\"."
        )
    } else {
        "This bot is not enabled for you or this channel.".to_string()
    }
}

pub(crate) fn identity_admitted(
    unconfigured: bool,
    is_owner: bool,
    in_allowlist: bool,
    role_granted: bool,
    open_here: bool,
) -> bool {
    !unconfigured && (is_owner || in_allowlist || role_granted || open_here)
}

/// Persist a channel's respond mode under `[channels.discord.channels.<id>]`
/// (#2014). The config watcher picks the change up the way it picks up any
/// config write.
pub(crate) fn write_channel_respond_to(channel_id: &str, mode: &str) -> Result<(), String> {
    crate::config::Config::write_key(
        &format!("channels.discord.channels.{channel_id}"),
        "respond_to",
        mode,
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// The parent channel of a thread or forum post, or `None` for a top-level
/// channel, a DM, or a lookup that failed (#2014). A failed lookup is logged;
/// callers fall back to the thread's own id alone.
pub(crate) async fn parent_channel_id(http: &Http, channel: ChannelId) -> Option<String> {
    match channel.to_channel(http).await {
        Ok(serenity::model::channel::Channel::Guild(gc)) => {
            gc.parent_id.map(|p| p.get().to_string())
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("Discord: could not resolve parent of channel {channel}: {e}");
            None
        }
    }
}

/// Whether any of a member's role ids is on the allowlist. Ids cross a JSON
/// boundary as strings, so the comparison is stringly, exactly as the message
/// path does it. A DM has no member object and no roles, and callers must pass
/// an empty list for one: treating a DM as role-granted would let a DM walk
/// past a guild-only allowlist.
pub(crate) fn holds_allowed_role(allowed_roles: &[String], role_ids: &[u64]) -> bool {
    role_ids
        .iter()
        .any(|r| allowed_roles.iter().any(|ar| ar == &r.to_string()))
}
