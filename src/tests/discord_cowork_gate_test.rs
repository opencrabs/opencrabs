//! The Discord owner's `/cowork` and `/respond_to` pass the channel scope, and
//! the refusal names the check that failed and the key that fixes it.

use crate::channels::discord::commands::{owner_scope_text, refusal_reason};
use crate::config::types::{DiscordChannelConfig, DiscordConfig};

#[test]
fn owner_scope_is_only_the_owners_cowork_and_respond_to() {
    assert!(owner_scope_text("/cowork", true));
    assert!(owner_scope_text("/respond_to all", true));
    assert!(!owner_scope_text("/cowork", false));
    assert!(!owner_scope_text("/respond_to all", false));
    assert!(!owner_scope_text("/coworker", true));
    assert!(!owner_scope_text("please /cowork", true));
    assert!(!owner_scope_text("/models", true));
}

#[test]
fn a_channel_block_admits_its_channel_and_its_threads() {
    let mut cfg = DiscordConfig::default();
    cfg.channels.insert(
        "1554978635209904148".to_string(),
        DiscordChannelConfig {
            respond_to: Some(crate::config::RespondTo::Mention),
            ..Default::default()
        },
    );
    assert!(cfg.has_channel_block("1554978635209904148", None));
    // A thread admitted through its parent's block.
    assert!(cfg.has_channel_block("999", Some("1554978635209904148")));
    // A channel with no block is not admitted by one.
    assert!(!cfg.has_channel_block("42", None));
}

#[test]
fn refusal_names_the_missing_user_setting() {
    let msg = refusal_reason(false, true, true, "42");
    assert!(msg.contains("allowed_users"), "{msg}");
    assert!(msg.contains("/cowork"), "{msg}");
}

#[test]
fn refusal_names_the_channel_and_the_key_to_add_it_to() {
    let msg = refusal_reason(true, false, true, "1554978635209904148");
    assert!(msg.contains("allowed_channels"), "{msg}");
    assert!(msg.contains("1554978635209904148"), "{msg}");
}

#[test]
fn refusal_names_the_dm_only_policy_and_its_fix() {
    let msg = refusal_reason(true, true, false, "42");
    assert!(msg.contains("dm_only"), "{msg}");
    assert!(msg.contains("/respond_to all"), "{msg}");
    assert!(
        msg.contains("channels.discord.channels.42.respond_to"),
        "{msg}"
    );
}
