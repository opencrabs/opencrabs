//! One identity gate for the Discord reaction path (#2195).
//!
//! The reaction handler admitted a user by `allowed_users` alone, while the
//! message path admitted by owner, allowlist, allowed roles, and channels
//! opened through `/cowork`. A channel could therefore answer typed messages
//! and silently drop every reaction, and an owner who was not also listed in
//! `allowed_users` lost their reactions entirely.
//!
//! The decision is a pure function of config and identity so it can be tested
//! without a gateway, and it delegates to the same
//! [`crate::channels::discord::commands::identity_admitted`] the message path
//! uses, so the two cannot drift apart again.

/// Everything the gate needs, resolved by the caller from the reaction or the
/// message. Kept as plain data so the decision has no I/O in it.
pub(crate) struct DiscordIdentity<'a> {
    pub user_id: u64,
    pub allowed_users: &'a [String],
    pub allowed_roles: &'a [String],
    pub bot_owner: &'a [String],
    /// Guild roles held by the user. Empty for a DM: guild roles do not apply
    /// there, and an empty list keeps `role_granted` false by construction.
    pub guild_roles: &'a [u64],
    /// The channel (or its parent) was opened with `/cowork`.
    pub open_here: bool,
}

/// Whether this identity may drive the bot in this channel.
pub(crate) fn identity_admitted(id: &DiscordIdentity<'_>) -> bool {
    let user_id = id.user_id.to_string();
    let is_owner = crate::config::owner::is_owner(id.allowed_users, id.bot_owner, &user_id);
    let in_allowlist = id
        .allowed_users
        .iter()
        .any(|s| s.parse::<u64>().ok() == Some(id.user_id));
    let role_granted = !id.guild_roles.is_empty()
        && !id.allowed_roles.is_empty()
        && id.guild_roles.iter().any(|held| {
            id.allowed_roles
                .iter()
                .any(|allowed| allowed == &held.to_string())
        });
    let unconfigured =
        id.allowed_users.is_empty() && id.allowed_roles.is_empty() && id.bot_owner.is_empty();
    super::commands::identity_admitted(
        unconfigured,
        is_owner,
        in_allowlist,
        role_granted,
        id.open_here,
    )
}
