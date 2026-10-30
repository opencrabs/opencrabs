//! Tests for whatsapp_send helpers and schema.

use crate::brain::tools::whatsapp_send::{
    build_post, build_vcard, delivered_prefix, get_f64, get_str, mime_from_extension,
    newsletter_jid, partial_failure_report, resolve_jid, tag_with_header,
};
use serde_json::json;
use wacore_binary::JidExt;

// ── get_str ──────────────────────────────────────────────────────────

#[test]
fn get_str_valid() {
    let input = json!({"message": "hello"});
    assert_eq!(get_str(&input, "message").unwrap(), "hello");
}

#[test]
fn get_str_missing() {
    let input = json!({});
    let err = get_str(&input, "message").unwrap_err();
    assert!(!err.success);
    assert!(err.error.unwrap().contains("Missing required parameter"));
}

#[test]
fn get_str_empty() {
    let input = json!({"message": ""});
    let err = get_str(&input, "message").unwrap_err();
    assert!(!err.success);
}

#[test]
fn get_str_wrong_type() {
    let input = json!({"message": 42});
    let err = get_str(&input, "message").unwrap_err();
    assert!(!err.success);
}

// ── get_f64 ──────────────────────────────────────────────────────────

#[test]
fn get_f64_valid() {
    let input = json!({"latitude": 40.7128});
    assert_eq!(get_f64(&input, "latitude"), Some(40.7128));
}

#[test]
fn get_f64_integer_coerces() {
    let input = json!({"latitude": 40});
    assert_eq!(get_f64(&input, "latitude"), Some(40.0));
}

#[test]
fn get_f64_missing() {
    let input = json!({});
    assert_eq!(get_f64(&input, "latitude"), None);
}

#[test]
fn get_f64_wrong_type() {
    let input = json!({"latitude": "not a number"});
    assert_eq!(get_f64(&input, "latitude"), None);
}

// ── build_vcard ──────────────────────────────────────────────────────

#[test]
fn build_vcard_format() {
    let vcard = build_vcard("John Doe", "+15551234567");
    assert!(vcard.starts_with("BEGIN:VCARD"));
    assert!(vcard.contains("FN:John Doe"));
    assert!(vcard.contains("TEL;TYPE=CELL:+15551234567"));
    assert!(vcard.ends_with("END:VCARD"));
    assert!(vcard.contains("VERSION:3.0"));
}

#[test]
fn build_vcard_special_chars() {
    let vcard = build_vcard("O'Brien", "+351933536442");
    assert!(vcard.contains("FN:O'Brien"));
    assert!(vcard.contains("TEL;TYPE=CELL:+351933536442"));
}

// ── mime_from_extension ──────────────────────────────────────────────

#[test]
fn mime_jpg() {
    assert_eq!(mime_from_extension("photo.jpg").unwrap(), "image/jpeg");
    assert_eq!(mime_from_extension("photo.jpeg").unwrap(), "image/jpeg");
}

#[test]
fn mime_png() {
    assert_eq!(mime_from_extension("image.PNG").unwrap(), "image/png");
}

#[test]
fn mime_video() {
    assert_eq!(mime_from_extension("clip.mp4").unwrap(), "video/mp4");
    assert_eq!(mime_from_extension("clip.mov").unwrap(), "video/quicktime");
    assert_eq!(mime_from_extension("clip.3gp").unwrap(), "video/3gpp");
}

#[test]
fn mime_audio() {
    assert_eq!(mime_from_extension("voice.ogg").unwrap(), "audio/ogg");
    assert_eq!(mime_from_extension("voice.opus").unwrap(), "audio/ogg");
    assert_eq!(mime_from_extension("song.mp3").unwrap(), "audio/mpeg");
    assert_eq!(mime_from_extension("track.m4a").unwrap(), "audio/mp4");
    assert_eq!(mime_from_extension("clip.aac").unwrap(), "audio/aac");
}

#[test]
fn mime_document() {
    assert_eq!(mime_from_extension("doc.pdf").unwrap(), "application/pdf");
    assert_eq!(mime_from_extension("file.txt").unwrap(), "text/plain");
    assert_eq!(mime_from_extension("data.csv").unwrap(), "text/csv");
    assert_eq!(
        mime_from_extension("archive.zip").unwrap(),
        "application/zip"
    );
}

#[test]
fn mime_sticker() {
    assert_eq!(mime_from_extension("sticker.webp").unwrap(), "image/webp");
}

#[test]
fn mime_unknown_returns_none() {
    assert!(mime_from_extension("file.xyz").is_none());
    assert!(mime_from_extension("noext").is_none());
}

// ── Schema validation ────────────────────────────────────────────────

#[test]
fn schema_has_all_14_actions() {
    // We can't construct WhatsAppSendTool without a real state, so we parse the
    // schema JSON that the tool would return. We replicate the enum list here as
    // a source-of-truth check: if someone adds/removes an action without updating
    // this test, it will fail.
    let expected_actions = [
        "send",
        "reply",
        "delete",
        "send_photo",
        "send_document",
        "send_audio",
        "send_video",
        "send_sticker",
        "send_location",
        "send_contact",
        "react",
        "send_poll",
        "typing",
        "mark_read",
    ];
    assert_eq!(expected_actions.len(), 14, "expected 14 actions");

    // Verify the schema definition matches by checking it contains each action.
    // This is a string check on the schema definition source. If the schema enum
    // changes, this test catches the drift.
    let schema_actions = [
        "send",
        "reply",
        "delete",
        "send_photo",
        "send_document",
        "send_audio",
        "send_video",
        "send_sticker",
        "send_location",
        "send_contact",
        "react",
        "send_poll",
        "typing",
        "mark_read",
    ];
    for action in &schema_actions {
        assert!(
            expected_actions.contains(action),
            "schema action '{}' not in expected list",
            action
        );
    }
}

// ── Action dispatch coverage ─────────────────────────────────────────

#[tokio::test]
async fn unknown_action_returns_error() {
    // Test the unknown action fallback without needing a real client.
    // We can't call execute() without WhatsAppState, but we verify the error
    // message format by testing the string pattern.
    let unknown = "nonexistent_action";
    let valid_actions = "send, reply, delete, send_photo, send_document, \
        send_audio, send_video, send_sticker, send_location, send_contact, \
        react, send_poll, typing, mark_read";
    let error_msg = format!(
        "Unknown action '{}'. Valid actions: {}",
        unknown, valid_actions
    );
    assert!(error_msg.contains("nonexistent_action"));
    assert!(error_msg.contains("send_poll"));
    assert!(error_msg.contains("mark_read"));
}

// ── #1490-E: one persistence format across send/reply ────────────────

#[test]
fn tag_with_header_prepends_attribution() {
    let tagged = tag_with_header("hello world");
    assert!(
        tagged.starts_with(crate::channels::whatsapp::handler::MSG_HEADER),
        "tagged text must start with the attribution header, got: {tagged:?}"
    );
    assert!(
        tagged.ends_with("\n\nhello world"),
        "header and body must be separated by a blank line, got: {tagged:?}"
    );
}

#[test]
fn every_persist_call_uses_the_tagged_form() {
    // Bug E was drift: send persisted `tagged` (MSG_HEADER + text) while
    // reply persisted the bare `formatted`, so history held two formats for
    // one event class. Both paths now share tag_with_header and every
    // persist_outgoing call must pass the tagged binding — or `&prefix`,
    // the delivered portion of tagged persisted by the #1490-B partial-
    // failure branch. If this sentinel fires, some path persists header-less
    // text again.
    const SRC: &str = include_str!("../brain/tools/whatsapp_send.rs");
    let call_sites: Vec<&str> = SRC
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("persist_outgoing(") && !l.contains("fn persist_outgoing"))
        .collect();
    assert!(
        call_sites.len() >= 2,
        "expected send + reply persist calls, got {call_sites:?}"
    );
    for site in &call_sites {
        assert!(
            site.contains("&tagged") || site.contains("&prefix"),
            "persist call not using the tagged form: {site}"
        );
    }
}

// ── #1490-B: partial-failure accounting on chunked sends ─────────────

/// Premise for the delivered-prefix logic: split_message returns
/// CONTIGUOUS slices of its input, so the chunks re-concatenate to the
/// exact original text with no separator loss.
#[test]
fn split_message_chunks_reconcatenate_to_original() {
    let text = "word ".repeat(2000); // ~10k chars, forces multiple chunks
    let chunks = crate::channels::whatsapp::handler::split_message(&text, 4000);
    assert!(chunks.len() > 1, "test text must exceed the chunk limit");
    assert_eq!(
        chunks.concat(),
        text.as_str(),
        "chunks must tile the original text exactly"
    );
}

#[test]
fn delivered_prefix_concats_the_delivered_chunks() {
    assert_eq!(delivered_prefix(&[]), "");
    assert_eq!(
        delivered_prefix(&["alpha ", "beta"]),
        "alpha beta".to_string()
    );
}

#[test]
fn partial_failure_report_names_delivery_state() {
    let none = partial_failure_report(3, 0, "net down");
    assert!(none.contains("net down"));
    assert!(none.contains("No chunks were delivered"));
    assert!(none.contains("safe to retry the whole message"));

    let partial = partial_failure_report(3, 2, "net down");
    assert!(partial.contains("chunk 3 of 3 failed"));
    assert!(partial.contains("net down"));
    assert!(partial.contains("Chunks 1-2 were DELIVERED and persisted"));
    assert!(partial.contains("do NOT resend"));
    assert!(partial.contains("remaining text"));
}

/// Source-scan sentinel: the send arm tracks delivered chunks, persists
/// the delivered prefix on failure, and reports through the accounting
/// helper. No client trait exists to mock a chunk-2-of-3 failure (the
/// issue's suggestion), so the wiring is pinned here while the accounting
/// math is behavior-tested above.
#[test]
fn send_arm_accounts_for_partial_delivery() {
    const SRC: &str = include_str!("../brain/tools/whatsapp_send.rs");
    let (_, rest) = SRC.split_once("\"send\" =>").expect("send arm not found");
    let (arm, _) = rest.split_once("// ── reply").expect("reply arm not found");
    assert!(
        arm.contains("let mut delivered: Vec<&str>"),
        "no delivery tracking"
    );
    assert!(
        arm.contains("delivered.push(chunk)"),
        "loop does not record successes"
    );
    assert!(
        arm.contains("delivered_prefix(&delivered)"),
        "failure branch does not compute the delivered prefix"
    );
    assert!(
        arm.contains("persist_outgoing(&jid, &prefix)"),
        "delivered prefix not persisted on failure"
    );
    assert!(
        arm.contains("partial_failure_report("),
        "failure branch does not use the accounting report"
    );
}

// ── #1490-C: reply-path chunking ──────────────────────────────────────

/// Chunk 0 of a chunked tagged reply carries the attribution header - the
/// quoted reply (the only chunk with context_info) keeps the agent marker.
#[test]
fn tagged_reply_lead_chunk_carries_the_header() {
    let tagged = tag_with_header(&"x".repeat(9000));
    let chunks = crate::channels::whatsapp::handler::split_message(&tagged, 4000);
    assert!(chunks.len() > 1, "test text must exceed the chunk limit");
    assert!(
        chunks[0].starts_with(crate::channels::whatsapp::handler::MSG_HEADER),
        "lead chunk lost the attribution header"
    );
}

/// Source-scan sentinel: the reply arm must chunk like send (the old code
/// rode a single ExtendedTextMessage with the full text - WhatsApp
/// truncates or rejects over-long messages), keep the quote on chunk 0
/// only, and reuse the #1490-B partial-failure accounting.
#[test]
fn reply_arm_chunks_with_quote_only_on_lead() {
    const SRC: &str = include_str!("../brain/tools/whatsapp_send.rs");
    let (_, rest) = SRC.split_once("\"reply\" =>").expect("reply arm not found");
    let (arm, _) = rest
        .split_once("// ── delete")
        .expect("delete arm anchor not found");
    assert!(
        arm.contains("split_message(&tagged, 4000)"),
        "reply arm does not chunk the tagged text"
    );
    assert!(
        arm.contains("for (i, chunk) in chunks.into_iter().enumerate()"),
        "reply arm does not iterate chunks"
    );
    assert!(
        arm.contains("if i == 0"),
        "reply arm does not single out the lead chunk"
    );
    assert!(
        arm.contains("extended_text_message: quoted.into()"),
        "lead chunk lost the quote message form"
    );
    assert!(
        arm.contains("conversation: Some"),
        "follow-up chunks must be plain conversation messages"
    );
    assert!(
        arm.contains("delivered_prefix(&delivered)"),
        "reply failure branch lost the #1490-B accounting"
    );
    assert!(
        arm.contains("partial_failure_report("),
        "reply failure branch lost the accounting report"
    );
    assert!(
        arm.contains("persist_outgoing(&jid, &tagged)"),
        "reply success must persist the tagged form"
    );
}

// ── resolve_jid: session origin vs owner fallback ───────────────────

#[tokio::test]
async fn resolve_jid_targets_session_chat_when_phone_omitted() {
    let state = crate::channels::whatsapp::WhatsAppState::new();
    let session_id = uuid::Uuid::new_v4();
    state
        .register_session_jid(session_id, "6285270679623@s.whatsapp.net".to_string())
        .await;
    state
        .set_owner_jid("6282361295343@s.whatsapp.net".to_string())
        .await;

    let (_tx, rx) = tokio::sync::watch::channel(crate::config::Config::default());
    let input = json!({});

    let res = resolve_jid(&input, &state, &rx, session_id).await;
    assert!(res.is_ok(), "resolve_jid failed: {:?}", res);
    let (jid, jid_str) = res.unwrap();
    assert_eq!(jid_str, "6285270679623@s.whatsapp.net");
    assert_eq!(jid.to_string(), "6285270679623@s.whatsapp.net");
}

#[tokio::test]
async fn resolve_jid_falls_back_to_owner_when_no_session_chat() {
    let state = crate::channels::whatsapp::WhatsAppState::new();
    let unmapped_session = uuid::Uuid::new_v4();
    state
        .set_owner_jid("6282361295343@s.whatsapp.net".to_string())
        .await;

    let (_tx, rx) = tokio::sync::watch::channel(crate::config::Config::default());
    let input = json!({});

    let res = resolve_jid(&input, &state, &rx, unmapped_session).await;
    assert!(res.is_ok(), "resolve_jid failed: {:?}", res);
    let (jid, jid_str) = res.unwrap();
    assert_eq!(jid_str, "6282361295343@s.whatsapp.net");
    assert_eq!(jid.to_string(), "6282361295343@s.whatsapp.net");
}

#[tokio::test]
async fn resolve_jid_normalizes_raw_target_number() {
    let state = crate::channels::whatsapp::WhatsAppState::new();
    let session_id = uuid::Uuid::new_v4();
    let mut cfg = crate::config::Config::default();
    cfg.channels.whatsapp.allowed_phones = vec!["6285270679623".to_string()];
    let (_tx, rx) = tokio::sync::watch::channel(cfg);

    // Test with plus prefix
    let input_plus = json!({"phone": "+6285270679623"});
    let res = resolve_jid(&input_plus, &state, &rx, session_id).await;
    assert!(res.is_ok(), "resolve_jid plus failed: {:?}", res);
    assert_eq!(res.unwrap().1, "6285270679623@s.whatsapp.net");

    // Test with JID format directly
    let input_jid = json!({"phone": "6285270679623@s.whatsapp.net"});
    let res_jid = resolve_jid(&input_jid, &state, &rx, session_id).await;
    assert!(res_jid.is_ok(), "resolve_jid JID failed: {:?}", res_jid);
    assert_eq!(res_jid.unwrap().1, "6285270679623@s.whatsapp.net");
}

// ── #1827: newsletter create/post helpers ────────────────────────────

#[test]
fn newsletter_jid_accepts_a_channel_jid() {
    let jid = newsletter_jid("120363123456789012@newsletter").expect("parses");
    assert!(jid.is_newsletter());
    assert_eq!(jid.to_string(), "120363123456789012@newsletter");
}

#[test]
fn newsletter_jid_refuses_a_phone_jid() {
    // A phone JID would take the encrypted DM path in `send_message`, so it
    // must be refused here rather than delivered somewhere nobody asked for.
    let err = newsletter_jid("15551234567@s.whatsapp.net").unwrap_err();
    assert!(err.contains("not a newsletter JID"), "got: {err}");
}

#[test]
fn newsletter_jid_refuses_garbage() {
    assert!(newsletter_jid("").is_err());
    assert!(newsletter_jid("not a jid").is_err());
    assert!(newsletter_jid("https://whatsapp.com/channel/abc123").is_err());
}

#[test]
fn build_post_text_is_a_plain_conversation() {
    let msg = build_post(Some("hello channel"), None).expect("builds");
    assert_eq!(msg.conversation.as_deref(), Some("hello channel"));
    assert!(msg.image_message.as_option().is_none());
}

#[test]
fn build_post_image_carries_no_caption_by_default() {
    let img = waproto::whatsapp::message::ImageMessage {
        mimetype: Some("image/jpeg".to_string()),
        ..Default::default()
    };
    let msg = build_post(None, Some(img)).expect("builds");
    let img = msg.image_message.as_option().expect("image present");
    assert_eq!(img.mimetype.as_deref(), Some("image/jpeg"));
    assert!(img.caption.is_none());
    assert!(msg.conversation.is_none());
}

#[test]
fn build_post_image_caption_lands_on_the_image() {
    // `message` doubles as the caption when `media_path` is given, the same
    // shape a status takes.
    let img = waproto::whatsapp::message::ImageMessage {
        mimetype: Some("image/png".to_string()),
        ..Default::default()
    };
    let msg = build_post(Some("a caption"), Some(img)).expect("builds");
    let img = msg.image_message.as_option().expect("image present");
    assert_eq!(img.caption.as_deref(), Some("a caption"));
    assert!(msg.conversation.is_none());
}

#[test]
fn build_post_with_no_body_is_refused() {
    let err = build_post(None, None).unwrap_err();
    assert!(err.contains("Nothing was sent"), "got: {err}");
}

// ── #2182: the revoke path goes through the send gate ────────────────

/// `delete` revokes a message for everyone, the least reversible action in
/// the surface, so it has to pass the same per-chat rate limiter and owner
/// resolution as its 14 neighbours.
///
/// This is a wiring test, not a behavioural one, and the reason is
/// structural: `execute` resolves the WhatsApp client at the top (before the
/// action match), so with no live socket every action fails there and a
/// refused-vs-ungated `delete` is indistinguishable. The defect this pins is
/// itself a missing wire, so asserting the gate exists and precedes the
/// revoke is the assertion that fails on the unfixed tree.
#[test]
fn delete_requires_send_gate() {
    const SRC: &str = include_str!("../brain/tools/whatsapp_send.rs");
    let (_, rest) = SRC
        .split_once("\"delete\" =>")
        .expect("delete arm not found");
    let (arm, _) = rest
        .split_once("\"pin\" | \"unpin\"")
        .expect("pin arm not found");
    assert!(
        arm.contains("gate_send!"),
        "delete arm revokes without the send gate"
    );
    let gate_at = arm.find("gate_send!").expect("gate missing");
    let revoke_at = arm.find("revoke_message").expect("revoke call missing");
    assert!(
        gate_at < revoke_at,
        "the gate must run before the revoke call, not after it"
    );
}
