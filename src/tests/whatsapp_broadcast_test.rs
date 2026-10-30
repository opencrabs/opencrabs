//! Broadcast safety rails (#1485).

use std::time::Duration;

use crate::channels::whatsapp::broadcast::{
    DEFAULT_DELAY_SECONDS, STATUS_BACKGROUND_ARGB, burst_floor, delay, is_allowed, normalize,
    partition, target_jid,
};

#[test]
fn normalise_strips_formatting_and_the_server() {
    assert_eq!(normalize("+1 (555) 123-4567"), "15551234567");
    assert_eq!(normalize("+15551234567"), "15551234567");
    assert_eq!(normalize("15551234567@s.whatsapp.net"), "15551234567");
}

#[test]
fn an_allowlist_matches_across_formatting() {
    // An allowlist that only matched one spelling would be an allowlist in
    // name only.
    let allow = vec!["+1 555 123 4567".to_string()];
    assert!(is_allowed("15551234567@s.whatsapp.net", &allow));
    assert!(is_allowed("+15551234567", &allow));
}

#[test]
fn an_empty_allowlist_allows_nobody() {
    // The opposite of the usual "empty means everyone", on purpose: an
    // unconfigured install must not be one tool call from messaging strangers.
    assert!(!is_allowed("+15551234567", &[]));
}

#[test]
fn a_number_not_on_the_list_is_refused() {
    let allow = vec!["+15551234567".to_string()];
    assert!(!is_allowed("+15559999999", &allow));
}

#[test]
fn a_target_with_no_digits_is_refused() {
    let allow = vec!["+15551234567".to_string()];
    assert!(!is_allowed("not-a-number", &allow));
    assert!(!is_allowed("", &allow));
}

#[test]
fn the_default_pacing_is_the_conservative_one() {
    assert_eq!(delay(None), Duration::from_secs(DEFAULT_DELAY_SECONDS));
}

#[test]
fn a_configured_delay_is_honoured() {
    assert_eq!(delay(Some(30)), Duration::from_secs(30));
}

#[test]
fn a_zero_delay_does_not_disable_pacing() {
    // Turning the rail off is not a knob this feature offers: a config typo
    // must not become a mass-send.
    assert_eq!(delay(Some(0)), Duration::from_secs(DEFAULT_DELAY_SECONDS));
}

#[test]
fn a_burst_costs_at_least_n_minus_one_delays() {
    let d = Duration::from_secs(5);
    assert_eq!(burst_floor(0, d), Duration::ZERO);
    assert_eq!(burst_floor(1, d), Duration::ZERO);
    assert_eq!(burst_floor(10, d), Duration::from_secs(45));
}

#[test]
fn partition_names_the_refused_numbers() {
    let allow = vec!["+15551234567".to_string()];
    let targets = vec![
        "+15551234567".to_string(),
        "+15559999999".to_string(),
        "15551234567@s.whatsapp.net".to_string(),
    ];
    let (ok, refused) = partition(&targets, &allow);
    assert_eq!(ok.len(), 2);
    assert_eq!(refused, vec![&"+15559999999".to_string()]);
}

#[test]
fn an_allowlist_entry_becomes_a_usable_jid() {
    let jid = target_jid("+1 (555) 123-4567").expect("parses");
    assert_eq!(jid.to_string(), "15551234567@s.whatsapp.net");
}

#[test]
fn a_jid_shaped_entry_round_trips() {
    let jid = target_jid("15551234567@s.whatsapp.net").expect("parses");
    assert_eq!(jid.to_string(), "15551234567@s.whatsapp.net");
}

#[test]
fn an_entry_with_no_digits_yields_no_jid() {
    // A config typo must surface as a refusal naming the entry, not as a
    // silently skipped recipient.
    assert!(target_jid("not-a-number").is_none());
    assert!(target_jid("").is_none());
}

#[test]
fn the_status_background_is_opaque() {
    // A status with a transparent background renders as a black card on some
    // clients. The alpha byte must be full.
    assert_eq!(STATUS_BACKGROUND_ARGB >> 24, 0xFF);
}

#[test]
fn the_shipped_config_default_broadcasts_to_nobody() {
    // The rail that matters most: an install that never touched the config
    // must not be one tool call away from messaging strangers.
    let cfg = crate::config::WaBroadcastConfig::default();
    assert!(cfg.allowed_targets.is_empty());
    assert!(!is_allowed("+15551234567", &cfg.allowed_targets));
    assert_eq!(
        delay(cfg.min_delay_seconds),
        Duration::from_secs(DEFAULT_DELAY_SECONDS),
        "an unset delay must still pace"
    );
}

#[test]
fn a_newsletter_jid_is_gated_by_the_same_allowlist() {
    // #1827: `post_newsletter` reuses this rail. `normalize` keeps the digits
    // and drops the `@newsletter` server, so a channel JID is allowlisted by
    // its own JID and an unlisted channel is refused.
    let allow = vec!["120363123456789012@newsletter".to_string()];
    assert!(is_allowed("120363123456789012@newsletter", &allow));
    assert!(!is_allowed("120363999999999999@newsletter", &allow));
    assert!(!is_allowed("120363123456789012@newsletter", &[]));
}

// ── #2176: an inbound status is not a message ────────────────────────

/// A contact's status arrives on `status@broadcast`, and every
/// `send_message` to that JID is rejected by the server, so the handler has
/// to drop it before it resolves a session. Without the drop, a status ran a
/// full agent turn and bound the session's reply target to a JID that cannot
/// receive anything.
///
/// Parsed from real strings rather than a hand-built struct: the predicate
/// turns on the JID server and user parts, which is exactly what the parse
/// produces in production.
#[test]
fn an_inbound_status_is_not_a_message() {
    use crate::channels::whatsapp::handler::is_inbound_status;
    use wacore_binary::jid::Jid;

    let status: Jid = "status@broadcast".parse().expect("status@broadcast parses");
    assert!(
        is_inbound_status(&status, false),
        "a status post must be dropped before session resolution"
    );

    let dm: Jid = "15551234567@s.whatsapp.net".parse().expect("PN parses");
    assert!(!is_inbound_status(&dm, false), "a DM must still be handled");

    let group: Jid = "120363000000000000@g.us".parse().expect("group parses");
    assert!(
        !is_inbound_status(&group, false),
        "a group must still be handled"
    );

    let broadcast_list: Jid = "1234567890@broadcast".parse().expect("broadcast parses");
    assert!(
        !is_inbound_status(&broadcast_list, false),
        "a broadcast LIST is ordinary group traffic, not a status"
    );

    // Our own echo carries the same chat JID; the guard must not depend on it
    // alone, or an outbound status echo would be dropped as inbound noise.
    assert!(
        !is_inbound_status(&status, true),
        "the predicate must not claim our own echo"
    );
}

/// The predicate above is only half the fix: it has to be CALLED before the
/// session is resolved, and the computed reply target has to be checked too,
/// or a future path can still bind a session to a broadcast JID.
#[test]
fn the_handler_drops_a_status_before_resolving_a_session() {
    const SRC: &str = include_str!("../channels/whatsapp/handler.rs");

    let guard = SRC
        .find("is_inbound_status(&info.source.chat")
        .expect("the inbound status guard is not wired into the handler");
    let target = SRC
        .find("let reply_target")
        .expect("the reply target is gone from the handler");
    assert!(
        guard < target,
        "the status drop must run before the reply target is computed"
    );

    assert!(
        SRC.contains("if reply_target.is_status_broadcast()"),
        "the computed reply target must be checked for a broadcast JID"
    );
}
