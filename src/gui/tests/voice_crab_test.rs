//! The window's voice speaks to the crab it holds (P4).
//!
//! A take is heard by the crab (`voice.transcribe`), kept on the crab's disk,
//! and the sent row's marker names the path the crab wrote; the window only
//! previews its own copy. The microphone is promised by the crab's
//! `voice.status`, and a spoken answer is the crab's (`voice.speak`).
//!
//! A remote crab is a `RemoteCrab` on a port nothing listens on, so a call
//! that reached it comes back as the transport's refusal, named after the
//! crab and the operation, and a take heard on this machine would not.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opencrabs::backend::BlobRef;
use opencrabs::backend::voice::{Heard, VoiceReady};
use uuid::Uuid;

use crate::backend_glue::{Backend, RemoteCrab};
use crate::blob_cache_store::BlobStore;
use crate::voice_input::{self, Held, Recording};

const EDITOR: &str = include_str!("../editor.rs");
const VOICE_INPUT: &str = include_str!("../voice_input.rs");

fn remote_with_cache(cache: &Path) -> Arc<RemoteCrab> {
    Arc::new(
        RemoteCrab::new(
            "elsewhere",
            "http://127.0.0.1:9",
            None,
            PathBuf::from("unused.db"),
        )
        .with_blob_dir(cache.to_path_buf()),
    )
}

fn remote() -> Backend {
    Backend::Remote(Arc::new(RemoteCrab::new(
        "elsewhere",
        "http://127.0.0.1:9",
        None,
        PathBuf::from("unused.db"),
    )))
}

/// One second of silence, in the two shapes a finished take has.
fn silence() -> Recording {
    let pcm = vec![0i16; voice_input::TARGET_RATE as usize];
    Recording {
        wav: voice_input::wav_bytes_from_pcm(&pcm, voice_input::TARGET_RATE),
        pcm,
    }
}

/// A take a remote crab kept is carried by the crab's path and previewed
/// from the window's copy: the marker never names this machine's home.
#[test]
fn a_remote_take_keeps_the_crabs_path_and_previews_a_copy() {
    let cache = tempfile::tempdir().expect("cache");
    let crab = remote_with_cache(cache.path());
    let on_crab = PathBuf::from("/srv/crab/.opencrabs/voice/voice-1730000000000.ogg");
    BlobStore::at(cache.path().to_path_buf())
        .keep(&on_crab, b"OggS")
        .expect("keep");
    let backend = Backend::Remote(crab.clone());
    let heard = Heard {
        text: "make it shorter".into(),
        language: Some("en".into()),
        clip: Some(BlobRef {
            path: on_crab.to_string_lossy().into_owned(),
            size: Some(4),
        }),
    };

    let spoken = voice_input::landed(&backend, heard, 1.0);

    assert_eq!(spoken.text, "make it shorter");
    let clip = spoken.audio.expect("the kept take");
    assert_eq!(clip.on_crab, on_crab, "the marker names the crab's path");
    assert!(
        !clip
            .on_crab
            .starts_with(opencrabs::config::opencrabs_home()),
        "a take sent to a remote crab never names this machine's home"
    );
    assert!(
        clip.here.starts_with(cache.path()),
        "the preview plays the window's copy: {:?}",
        clip.here
    );
    assert_eq!(
        crab.blob_fetches(),
        0,
        "a cached copy asks the crab nothing"
    );
    assert!(
        voice_input::is_recording(&clip.on_crab),
        "the crab's path is a take for the send path and the row"
    );
}

/// The local crab's take is one path: the one it wrote is the one it plays.
#[test]
fn a_local_take_previews_the_path_it_was_kept_at() {
    let dir = tempfile::tempdir().expect("dir");
    let backend = Backend::local(dir.path().join("opencrabs.db"));
    let kept = dir.path().join("voice-1.ogg");
    let heard = Heard {
        text: "hello".into(),
        language: None,
        clip: Some(BlobRef {
            path: kept.to_string_lossy().into_owned(),
            size: Some(4),
        }),
    };
    let clip = voice_input::landed(&backend, heard, 1.0)
        .audio
        .expect("the kept take");
    assert_eq!(clip.on_crab, kept);
    assert_eq!(clip.here, kept);
}

/// A take for a remote crab is heard by that crab: the refusal comes from
/// its door, named after it, and nothing is transcribed here.
#[test]
fn a_remote_take_is_heard_by_the_crab() {
    let session = Some(Uuid::new_v4());
    let error = match voice_input::transcribe(Some(remote()), silence(), session) {
        Ok(_) => panic!("nothing listens on the crab's port"),
        Err(error) => error,
    };
    assert!(error.contains("elsewhere"), "{error}");
    assert!(error.contains("voice.transcribe"), "{error}");
}

/// The composer and the Seeker send the take to the crab, and the marker on
/// the sent row is the crab's path, never the window's copy.
#[test]
fn the_take_and_its_marker_go_through_the_crab() {
    for gone in [
        "transcribe_with_language",
        "global_pool()",
        "Config::current()",
    ] {
        assert!(
            !VOICE_INPUT.contains(gone),
            "the window hears and keeps nothing itself: {gone}"
        );
    }
    let send = EDITOR
        .split("fn send(")
        .nth(1)
        .and_then(|rest| rest.split("fn payload_with_reply(").next())
        .expect("the send path");
    assert!(
        send.contains("format!(\"<<AUD:{}>>\", draft.on_crab.display())"),
        "the marker carries the path the crab wrote"
    );
}

/// The microphone is promised by the crab the window holds: a remote crab
/// that has not answered promises nothing, an answer for a crab the window
/// left is dropped, and the held crab's answer decides.
#[test]
fn voice_ready_follows_the_crabs_status() {
    let mut held = Held::new(remote(), 3);
    assert!(!held.stt_ready(), "no answer yet, no microphone");
    held.answer(
        2,
        VoiceReady {
            stt_enabled: true,
            tts_enabled: true,
        },
    );
    assert!(!held.stt_ready(), "an answer for the last crab is dropped");
    held.answer(
        3,
        VoiceReady {
            stt_enabled: true,
            tts_enabled: false,
        },
    );
    assert!(held.stt_ready(), "the held crab can hear");
    held.answer(
        3,
        VoiceReady {
            stt_enabled: false,
            tts_enabled: true,
        },
    );
    assert!(!held.stt_ready(), "the held crab cannot hear");
}

/// The local crab answers from its own config on every read, as the
/// composer always did.
#[test]
fn the_local_crab_answers_from_its_config() {
    let dir = tempfile::tempdir().expect("dir");
    let held = Held::new(Backend::local(dir.path().join("opencrabs.db")), 0);
    assert_eq!(
        held.stt_ready(),
        opencrabs::config::Config::current()
            .voice_config()
            .stt_enabled
    );
}

/// A spoken answer is the crab's: a remote crab that cannot be asked leaves
/// no "speaking" card and no clip, and nothing is synthesized here.
#[test]
fn a_spoken_answer_on_a_remote_crab_is_asked_of_the_crab() {
    let (sink, events) = std::sync::mpsc::channel();
    crate::agent_bridge::spawn_speak_reply(remote(), sink, Uuid::new_v4(), "answer".into());
    match events.recv_timeout(Duration::from_secs(60)) {
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
        Ok(_) => panic!("a crab that cannot be asked about its voice gets no card"),
        Err(e) => panic!("the voice reply never finished: {e}"),
    }
}
