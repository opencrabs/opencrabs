//! #3088 regression pins: unknown claude CLI event types are ignored quietly.
//!
//! Newer CLI builds emit event types this build does not know. `CliMessage`
//! had a closed set of variants, so each unknown one failed to parse and
//! logged a WARN per event. An unknown `type` must now land on
//! `CliMessage::Unknown`, while a malformed known type must still fail.

use crate::brain::provider::claude_cli::CliMessage;

/// Shape measured on 2026-11-21 with claude CLI 2.1.329. The CLI emits the
/// frame only for a message that arrives over `--input-format stream-json`
/// with a client-supplied `uuid`; the one-shot `-p "prompt"` text path emits
/// none, so this shape cannot reach a provider that spawns the CLI that way.
/// The WARN itself fired on this host with `tool_progress` (33 times,
/// 2026-11-11/12).
const COMMAND_LIFECYCLE: &str = r#"{"type":"command_lifecycle","command_uuid":"6f1c0e2a-1111-4a2b-9c3d-000000000001","state":"started","uuid":"8d5a55b1-c95b-44af-9846-752decae0d59","session_id":"70eb3d93-f974-4c25-89ae-de37f2b79c59"}"#;

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
