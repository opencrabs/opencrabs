//! Tests for the shared bot-owner resolution helper used by every channel
//! config (Telegram/Discord/Slack/WhatsApp/Trello). Issue #243.

use crate::config::owner::{is_owner, seed_bot_owner};

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn empty_config_denies_access_secure_by_default() {
    let allowed: Vec<String> = Vec::new();
    let bot_owner: Vec<String> = Vec::new();
    // Empty config = unconfigured = deny all (secure by default).
    assert!(!is_owner(&allowed, &bot_owner, "anyone"));
    assert!(!is_owner(&allowed, &bot_owner, ""));
    // Even with an explicit owner list, empty allowed still denies.
    assert!(!is_owner(&allowed, &v(&["explicit"]), "anyone"));
}

#[test]
fn explicit_bot_owner_governs_ownership() {
    let allowed = v(&["a", "b", "c"]);
    let bot_owner = v(&["b", "c"]);
    // A listed (non-first) id is an owner.
    assert!(is_owner(&allowed, &bot_owner, "b"));
    assert!(is_owner(&allowed, &bot_owner, "c"));
    // A non-listed id (even the first allowed entry) is NOT an owner.
    assert!(!is_owner(&allowed, &bot_owner, "a"));
    assert!(!is_owner(&allowed, &bot_owner, "z"));
}

#[test]
fn positional_fallback_uses_first_allowed_when_no_bot_owner() {
    let allowed = v(&["first", "second", "third"]);
    let bot_owner: Vec<String> = Vec::new();
    assert!(is_owner(&allowed, &bot_owner, "first"));
    // A non-first allowed id is NOT the owner under positional fallback.
    assert!(!is_owner(&allowed, &bot_owner, "second"));
    assert!(!is_owner(&allowed, &bot_owner, "third"));
    assert!(!is_owner(&allowed, &bot_owner, "stranger"));
}

#[test]
fn seed_bot_owner_seeds_from_first_allowed() {
    let allowed = v(&["first", "second"]);
    let bot_owner: Vec<String> = Vec::new();
    assert_eq!(seed_bot_owner(&allowed, &bot_owner), Some(v(&["first"])));
}

#[test]
fn seed_bot_owner_none_when_already_explicit() {
    let allowed = v(&["first", "second"]);
    let bot_owner = v(&["second"]);
    assert_eq!(seed_bot_owner(&allowed, &bot_owner), None);
}

#[test]
fn seed_bot_owner_none_when_nothing_to_seed_from() {
    let allowed: Vec<String> = Vec::new();
    let bot_owner: Vec<String> = Vec::new();
    assert_eq!(seed_bot_owner(&allowed, &bot_owner), None);
}

// ── resolve_owner_id (numeric-id variant used by per-user menu scoping) ────

use crate::config::owner::resolve_owner_id;

#[test]
fn resolve_owner_id_explicit_bot_owner_decides() {
    let allowed = v(&["*", "1"]);
    let bot_owner = v(&["2"]);
    assert_eq!(resolve_owner_id(&allowed, &bot_owner), Some(2));
}

#[test]
fn resolve_owner_id_positional_fallback_skips_the_wildcard() {
    // The regression the Telegram menu path shipped with: `.first().parse()`
    // returned None on `["*", "<id>"]`, which silently disabled every owner
    // menu — the `/` picker was left with nothing but `/start`.
    let allowed = v(&["*", "1608111860"]);
    let bot_owner: Vec<String> = Vec::new();
    assert_eq!(resolve_owner_id(&allowed, &bot_owner), Some(1_608_111_860));
}

#[test]
fn resolve_owner_id_none_without_any_numeric_entry() {
    let allowed = v(&["*"]);
    let bot_owner = v(&["@nobody"]);
    assert_eq!(resolve_owner_id(&allowed, &bot_owner), None);
    assert_eq!(resolve_owner_id(&[], &[]), None);
}

#[test]
fn resolve_owner_id_tolerates_a_leading_plus() {
    // Same normalization `is_owner` applies for international numbers (#277).
    let allowed = v(&["+1608111860"]);
    let bot_owner: Vec<String> = Vec::new();
    assert_eq!(resolve_owner_id(&allowed, &bot_owner), Some(1_608_111_860));
}
