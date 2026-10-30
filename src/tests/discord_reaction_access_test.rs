//! #2195: the reaction path uses the same identity gate as the message path.
//!
//! Reactions were admitted by `allowed_users` alone, so an owner who was not
//! also listed there, a user admitted through `allowed_roles`, and a channel
//! opened with `/cowork` all had their reactions dropped silently while typed
//! messages in the same place worked.

use crate::channels::discord::access::{DiscordIdentity, identity_admitted};

const OWNER_ID: u64 = 111_111_111_111_111_111;
const STRANGER_ID: u64 = 222_222_222_222_222_222;

fn identity<'a>(
    user_id: u64,
    allowed_users: &'a [String],
    allowed_roles: &'a [String],
    bot_owner: &'a [String],
    guild_roles: &'a [u64],
    open_here: bool,
) -> DiscordIdentity<'a> {
    DiscordIdentity {
        user_id,
        allowed_users,
        allowed_roles,
        bot_owner,
        guild_roles,
        open_here,
    }
}

/// The headline defect: the owner is honoured everywhere except the reaction
/// path, because that path only ever looked at `allowed_users`.
#[test]
fn an_owner_who_is_not_in_the_allowlist_may_react() {
    let allowed = vec![STRANGER_ID.to_string()];
    let owner = vec![OWNER_ID.to_string()];
    let id = identity(OWNER_ID, &allowed, &[], &owner, &[], false);

    // The old gate was this expression, and it is false: the reaction was
    // dropped even though the owner is configured.
    assert!(
        !allowed.contains(&OWNER_ID.to_string()),
        "the fixture must reproduce the old gate denying the owner"
    );
    assert!(
        identity_admitted(&id),
        "the owner must be admitted on the reaction path too"
    );
}

/// A user admitted through `allowed_roles` can talk but could not react.
#[test]
fn a_role_granted_user_may_react() {
    let roles = vec!["900".to_string()];
    let id = identity(STRANGER_ID, &[], &roles, &[], &[900], false);
    assert!(identity_admitted(&id), "a held allowed role must admit");

    // A role nobody holds, or a guild role that is not in the list, still denies.
    let wrong = identity(STRANGER_ID, &[], &roles, &[], &[901], false);
    assert!(
        !identity_admitted(&wrong),
        "an unlisted role must not admit"
    );
}

/// A channel opened with `/cowork` answers messages, so it must accept
/// reactions in it as well.
#[test]
fn a_cowork_opened_channel_may_react() {
    let owner = vec![OWNER_ID.to_string()];
    let id = identity(STRANGER_ID, &[], &[], &owner, &[], true);
    assert!(
        identity_admitted(&id),
        "an open channel must admit a reaction like it admits a message"
    );
}

/// The gate must still deny. Widening it is the fix; removing it is not.
#[test]
fn a_stranger_is_still_denied() {
    let allowed = vec![OWNER_ID.to_string()];
    let owner = vec![OWNER_ID.to_string()];
    let id = identity(STRANGER_ID, &allowed, &[], &owner, &[], false);
    assert!(!identity_admitted(&id), "a stranger must still be denied");

    // Deny-by-default: an unconfigured channel denies everyone, which is what
    // keeps a fresh install from answering reactions.
    let empty: Vec<String> = Vec::new();
    let unconfigured = identity(STRANGER_ID, &empty, &empty, &empty, &[], false);
    assert!(
        !identity_admitted(&unconfigured),
        "an unconfigured channel must deny, matching the message path"
    );
}

/// A DM carries no guild roles, so an `allowed_roles` config alone must not
/// admit anyone there. An empty role list keeps `role_granted` false.
#[test]
fn a_dm_without_guild_roles_is_not_role_granted() {
    let roles = vec!["900".to_string()];
    let dm = identity(STRANGER_ID, &[], &roles, &[], &[], false);
    assert!(
        !identity_admitted(&dm),
        "guild roles do not apply in a DM, so a role config must not admit"
    );
}

/// Both paths must route through the shared gate. A fix that widened only the
/// reaction handler would pass every test above and still leave two gates to
/// drift apart again.
#[test]
fn both_paths_call_the_shared_gate() {
    const REACTIONS: &str = include_str!("../channels/discord/reactions.rs");
    const HANDLER: &str = include_str!("../channels/discord/handler.rs");

    assert!(
        REACTIONS.contains("access::identity_admitted"),
        "the reaction path must use the shared gate"
    );
    assert!(
        !REACTIONS.contains("allowed_users\n        .iter()\n        .any"),
        "the allowlist-only gate is the defect; it must not come back"
    );
    assert!(
        HANDLER.contains("commands::identity_admitted("),
        "the message path must call the same predicate, not an inline copy"
    );
}
