//! Which bubbles a rich final is allowed to delete (#2370).
//!
//! The reported turn left an orphan bubble beside a rich answer, and the
//! branch that sends the fresh rich message had already cleared the response
//! placeholder, so the placeholder alone was never the whole set. These tests
//! pin the two halves of the contract: the set covers everything the turn is
//! showing, and it never covers a bubble that holds the only chat copy of a
//! file or a voice note (#1939).

use crate::channels::telegram::delivery::DeliveredFile;
use crate::channels::telegram::flow::StreamingState;
use teloxide::types::MessageId;

fn mid(value: i32) -> MessageId {
    MessageId(value)
}

fn delivered(message_id: i32) -> DeliveredFile {
    DeliveredFile {
        path: std::path::PathBuf::from(format!("/tmp/file-{message_id}.pdf")),
        message_id,
    }
}

/// A turn mid-flight: placeholder, progress block, two narration bubbles and
/// one media bubble, plus the two id classes that must survive.
fn loaded_turn() -> StreamingState {
    let mut s = StreamingState::new_turn(true, false);
    s.msg_id = Some(mid(10));
    s.open_group_msg_id = Some(mid(11));
    s.intermediate_bubbles = vec![(mid(12), "first".to_string()), (mid(13), "second".to_string())];
    s.media_intermediates = vec![
        (mid(14), "report".to_string()),
        (mid(16), "screenshot".to_string()),
    ];
    // 14 is the bubble the report landed in, so deleting it would take the
    // only chat copy of the file with it.
    s.delivered_files = vec![delivered(14)];
    s.voice_msg_ids = vec![mid(15)];
    s
}

#[test]
fn covers_every_bubble_the_turn_is_showing() {
    let ids: Vec<i32> = loaded_turn()
        .live_bubble_ids()
        .into_iter()
        .map(|id| id.0)
        .collect();
    assert_eq!(
        ids,
        vec![10, 11, 12, 13, 16],
        "14 and 15 are the only chat copy of the report and the voice note"
    );
}

#[test]
fn dedupes_an_id_recorded_in_two_fields() {
    let mut s = StreamingState::new_turn(true, false);
    s.msg_id = Some(mid(10));
    s.intermediate_bubbles = vec![(mid(10), "same bubble".to_string())];
    s.open_group_msg_id = Some(mid(11));
    s.intermediate_bubbles
        .push((mid(11), "same block".to_string()));
    let ids: Vec<i32> = s.live_bubble_ids().into_iter().map(|id| id.0).collect();
    assert_eq!(ids, vec![10, 11], "an id must be deleted once, not twice");
}

#[test]
fn never_returns_a_delivered_file_bubble() {
    let mut s = StreamingState::new_turn(true, false);
    s.msg_id = Some(mid(10));
    s.delivered_files = vec![delivered(10)];
    assert!(
        s.live_bubble_ids().is_empty(),
        "the bubble holding a delivered file is the only chat copy of it"
    );
}

#[test]
fn never_returns_a_voice_note_bubble() {
    let mut s = StreamingState::new_turn(true, false);
    s.msg_id = Some(mid(10));
    s.intermediate_bubbles = vec![(mid(11), "narration".to_string())];
    s.voice_msg_ids = vec![mid(10)];
    let ids: Vec<i32> = s.live_bubble_ids().into_iter().map(|id| id.0).collect();
    assert_eq!(
        ids,
        vec![11],
        "a voice note cannot be re-downloaded, so its bubble stays"
    );
}

#[test]
fn protects_a_media_intermediate_that_carries_documents() {
    // `intermediates.rs` records every file the media bubble shipped in
    // `delivered_files` with that bubble's id, which is what keeps a document
    // bubble out of the delete list (#1939). One entry with the same id is
    // enough; the assertion is about the id, not the count.
    let mut s = StreamingState::new_turn(true, false);
    s.media_intermediates = vec![(mid(20), "two reports".to_string())];
    s.delivered_files = vec![delivered(20), delivered(20)];
    assert!(s.live_bubble_ids().is_empty());
}

#[test]
fn empty_turn_has_nothing_to_delete() {
    let s = StreamingState::new_turn(false, true);
    assert!(s.live_bubble_ids().is_empty());
}