//! Regression tests for #2121: a reply to a media message lost the media.
//!
//! A Telegram photo, video, animation, video note or document arrives with
//! `text() == None` and, when captioned, the caption in `caption()`. The reply
//! path used to build its context from text alone, so:
//!
//! 1. an uncaptioned media reply produced `ctx = None`, which is
//!    indistinguishable from "the user did not reply to anything", and
//! 2. a captioned media reply produced context that named the caption and
//!    never the file.
//!
//! Both let the agent answer about an attachment it had never been told
//! existed. These tests pin the fix: the media is named, and when its file is
//! not on disk the context says so instead of implying the agent can see it.

use crate::channels::telegram::handler::{
    ReplyMediaKind, format_reply_context_pruned, resolve_reply_context_pruned,
};

const NOT_STORED: &str = "The replied-to message is a photo and its file was not stored \
                           locally, so you cannot see it. If you need to look at it, ask the \
                           user to send it again.";

#[test]
fn media_kind_labels_are_the_words_the_agent_reads() {
    assert_eq!(ReplyMediaKind::Photo.label(), "photo");
    assert_eq!(ReplyMediaKind::Video.label(), "video");
    assert_eq!(ReplyMediaKind::Animation.label(), "animation");
    assert_eq!(ReplyMediaKind::VideoNote.label(), "video note");
    assert_eq!(ReplyMediaKind::Document.label(), "document");
}

#[test]
fn media_kind_of_detects_each_media_field() {
    // teloxide's `Video`/`Animation`/`VideoNote` deserializers require
    // `mime_type`, so a fixture without it fails to parse and would report
    // "no media" for a reason that has nothing to do with the guard.
    let cases = [
        (
            r#"{"message_id":1,"date":1700000000,"chat":{"id":7,"type":"private","first_name":"A"},
                "photo":[{"file_id":"p","file_unique_id":"pu","width":90,"height":90}]}"#,
            Some(ReplyMediaKind::Photo),
        ),
        (
            r#"{"message_id":2,"date":1700000000,"chat":{"id":7,"type":"private","first_name":"A"},
                "video":{"file_id":"v","file_unique_id":"vu","width":90,"height":90,"duration":3,
                         "mime_type":"video/mp4"}}"#,
            Some(ReplyMediaKind::Video),
        ),
        (
            r#"{"message_id":3,"date":1700000000,"chat":{"id":7,"type":"private","first_name":"A"},
                "animation":{"file_id":"a","file_unique_id":"au","width":90,"height":90,"duration":3,
                             "mime_type":"video/mp4"}}"#,
            Some(ReplyMediaKind::Animation),
        ),
        (
            r#"{"message_id":4,"date":1700000000,"chat":{"id":7,"type":"private","first_name":"A"},
                "video_note":{"file_id":"n","file_unique_id":"nu","length":90,"duration":3,
                              "mime_type":"video/mp4"}}"#,
            Some(ReplyMediaKind::VideoNote),
        ),
        (
            r#"{"message_id":5,"date":1700000000,"chat":{"id":7,"type":"private","first_name":"A"},
                "document":{"file_id":"d","file_unique_id":"du"}}"#,
            Some(ReplyMediaKind::Document),
        ),
        (
            r#"{"message_id":6,"date":1700000000,"chat":{"id":7,"type":"private","first_name":"A"},
                "text":"just words"}"#,
            None,
        ),
    ];

    for (json, expected) in cases {
        let msg: teloxide::types::Message =
            serde_json::from_str(json).expect("fixture must deserialize");
        assert_eq!(
            ReplyMediaKind::of(&msg),
            expected,
            "wrong media kind for {json}"
        );
    }
}

#[test]
fn uncaptioned_media_reply_produces_context_naming_the_file() {
    // Before the fix this returned None and the reply vanished from the prompt.
    let ctx = format_reply_context_pruned("Alice", "", "", false, Some(NOT_STORED))
        .expect("a media reply must produce context");
    assert!(ctx.contains("Replying to Alice"), "{ctx}");
    assert!(ctx.contains("photo"), "{ctx}");
    assert!(
        ctx.contains("not stored locally"),
        "the agent must be told it cannot see the file: {ctx}"
    );
}

#[test]
fn captioned_media_reply_keeps_the_caption_and_names_the_file() {
    let ctx = format_reply_context_pruned("Alice", "look at this", "", false, Some(NOT_STORED))
        .expect("a captioned media reply must produce context");
    assert!(ctx.contains("look at this"), "caption lost: {ctx}");
    assert!(ctx.contains("photo"), "media kind lost: {ctx}");
}

#[test]
fn media_note_does_not_override_an_unrecoverable_note() {
    // Both signals are present here: the text is empty and the sender is
    // known. The media note is the accurate one, so it wins.
    let ctx = resolve_reply_context_pruned("Alice", "", "", true, false, Some(NOT_STORED))
        .expect("must produce context");
    assert!(ctx.contains("not stored locally"), "{ctx}");
    assert!(
        !ctx.contains("could not be retrieved"),
        "generic unrecoverable note must not mask the media note: {ctx}"
    );
}

#[test]
fn non_media_reply_is_unchanged_by_the_new_parameter() {
    // No media note, empty text, nothing recoverable: still None.
    assert_eq!(
        resolve_reply_context_pruned("Alice", "", "", false, false, None),
        None
    );
    // And an ordinary text reply formats exactly as before.
    assert_eq!(
        format_reply_context_pruned("Bob", "hello there", "", false, None).unwrap(),
        "[Replying to Bob: \"hello there\"]"
    );
}

#[test]
fn blank_media_note_is_ignored() {
    // A whitespace-only note must not produce a context that says nothing.
    assert_eq!(
        format_reply_context_pruned("Alice", "", "", false, Some("   ")),
        None
    );
}
