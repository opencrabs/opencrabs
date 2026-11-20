//! #3088 regression pins: unknown claude CLI event types are ignored quietly.
//!
//! CLI 2.1.325 started emitting `{"type":"command_lifecycle",...}` lines.
//! `CliMessage` had a closed set of variants, so each one failed to parse and
//! logged a WARN on every command. An unknown `type` must now land on
//! `CliMessage::Unknown`, while a malformed known type must still fail.

use crate::brain::provider::claude_cli::CliMessage;

/// Real payload shape from a daemon log (2026-11-20, CLI 2.1.325).
const COMMAND_LIFECYCLE: &str = r#"{"type":"command_lifecycle","command_uuid":"b160bddd-689a-47d9-905e-6573cfae6fd9","state":"started","uuid":"a2e66082-7d5b-4fa7-8d54-750d55c7f571","session_id":"ade7dbe2-30d2-4ed6-a5dd-4d44f1ad313f"}"#;

#[test]
fn command_lifecycle_parses_as_unknown() {
    let msg: CliMessage =
        serde_json::from_str(COMMAND_LIFECYCLE).expect("an unknown type must not be a parse error");
    assert!(matches!(msg, CliMessage::Unknown));
}

#[test]
fn any_future_type_parses_as_unknown() {
    let msg: CliMessage = serde_json::from_str(r#"{"type":"some_event_added_later","x":1}"#)
        .expect("forward-compatible");
    assert!(matches!(msg, CliMessage::Unknown));
}

#[test]
fn known_types_still_parse_to_their_variant() {
    let msg: CliMessage = serde_json::from_str(r#"{"type":"rate_limit_event"}"#).unwrap();
    assert!(matches!(msg, CliMessage::RateLimitEvent {}));
}

#[test]
fn a_known_type_with_a_broken_payload_still_fails() {
    // `assistant` requires `message`; this must keep reaching the WARN path.
    assert!(serde_json::from_str::<CliMessage>(r#"{"type":"assistant"}"#).is_err());
}

#[test]
fn a_line_without_a_type_still_fails() {
    assert!(serde_json::from_str::<CliMessage>(r#"{"state":"started"}"#).is_err());
}
