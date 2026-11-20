//! What counts as a voice note in a stored row (#3111).
//!
//! The row draws the clip first and folds the words away, so the rule that
//! decides "this message is a take the microphone heard" is load-bearing: get it
//! wrong and a message the reader typed loses its text behind a fold, or a
//! picked audio file draws its player above words that are the actual message.
//!
//! `voice_note` is a plain function over the body string, so these cases run it
//! rather than restate it. `is_recording` reads no filesystem, only the path's
//! parent and its name prefix, so a constructed path is enough and nothing here
//! touches the profile on disk.

use crate::transcript::voice_note;
use std::path::PathBuf;

/// The folder the capture path writes takes into, spelled the way the row
/// spells it: the profile's `voice/` directory.
fn voice_folder() -> PathBuf {
    opencrabs::config::opencrabs_home().join(crate::voice_input::VOICE_DIR)
}

/// A body that carries one recording.
#[test]
fn a_recording_returns_its_clip() {
    let clip = voice_folder().join("voice-1730000000000.ogg");
    let body = format!("make this shorter please <<AUD:{}>>", clip.display());
    assert_eq!(voice_note(&body), Some(clip));
}

/// A file the reader picked with the paperclip is not a voice note. Its words
/// are the message, so they stay in front and its player sits under them.
#[test]
fn a_picked_audio_file_is_not_a_voice_note() {
    let picked = voice_folder().join("interview.ogg");
    let body = format!("listen to this <<AUD:{}>>", picked.display());
    assert!(
        voice_note(&body).is_none(),
        "a clip the user attached is a file, not a take the microphone heard"
    );
    let elsewhere = PathBuf::from("/tmp/voice-1.ogg");
    let body = format!("and this one <<AUD:{}>>", elsewhere.display());
    assert!(
        voice_note(&body).is_none(),
        "the recording prefix outside the voice folder is still not a recording"
    );
}

/// A body with no marker at all is every ordinary message.
#[test]
fn a_body_with_no_marker_is_not_a_voice_note() {
    assert!(voice_note("").is_none());
    assert!(voice_note("just words").is_none());
    assert!(
        voice_note("a stray << that never closes").is_none(),
        "an unterminated marker is text, not a clip"
    );
}

/// An image is not audio, and a marker of another kind never makes a voice note.
#[test]
fn another_marker_kind_is_not_a_voice_note() {
    assert!(voice_note("look <<IMG:/tmp/shot.png>>").is_none());
    let clip = voice_folder().join("voice-2.ogg");
    assert!(
        voice_note(&format!("clip <<VID:{}>>", clip.display())).is_none(),
        "the kind decides, not the path"
    );
}

/// The marker is not always the first one in the body. A recording sent beside
/// an image still has to be found, or that row draws its words in front and its
/// player under them like any other message with a file.
#[test]
fn a_recording_behind_another_marker_is_still_found() {
    let clip = voice_folder().join("voice-3.ogg");
    let body = format!("here <<IMG:/tmp/shot.png>> and <<AUD:{}>>", clip.display());
    assert_eq!(voice_note(&body), Some(clip));
}
