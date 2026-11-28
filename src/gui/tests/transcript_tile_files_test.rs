//! A transcript attachment is drawn from the file on this machine (P9).
//!
//! A stored row names its attachments by the crab's paths. The tile keys its
//! click and its kind by that path, and reads the picture, the size and the
//! day it changed from where `Backend::localize` puts the file here, so a crab
//! on another machine never has its path stat'ed or decoded on this disk. The
//! local crab's `localize` is its own path, as it was.
//!
//! What a tile or the preview decodes (a picture, a sound, a clip's frames)
//! comes from `Backend::localize_allowed` and is judged by the copy's own
//! extension (SEC5): a remote crab's file outside the allowlist is drawn as
//! its kind glyph, never decoded here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::backend_glue::Backend;
use crate::backend_remote::RemoteCrab;
use crate::blob_cache_store::BlobStore;
use crate::preview::{Preview, PreviewKind, VideoFacts};
use crate::transcript::{plays_as_audio, tile_picture};

const TRANSCRIPT: &str = include_str!("../transcript.rs");
const DASHBOARD_RENDER: &str = include_str!("../dashboard_render.rs");
const PANE_VIEW: &str = include_str!("../pane_view.rs");
const PREVIEW: &str = include_str!("../preview.rs");

/// The body of `fn <name>(` up to the next top-level `fn`.
fn body<'a>(src: &'a str, name: &str) -> &'a str {
    src.split(&format!("fn {name}("))
        .nth(1)
        .and_then(|rest| rest.split("\nfn ").next())
        .unwrap_or_else(|| panic!("fn {name} exists"))
}

#[test]
fn the_tile_reads_the_localized_file_and_keys_the_crab_path() {
    let cell = body(TRANSCRIPT, "attachment_cell");
    assert!(cell.contains("Attachment::for_display(path.to_path_buf())"));
    assert!(cell.contains("crate::thumbs::picture(local,"));
    assert!(cell.contains("FileFacts::probe(local)"));
    assert!(!cell.contains("FileFacts::probe(path)"));
    assert!(!cell.contains("crate::thumbs::picture(path,"));

    let strip = body(TRANSCRIPT, "history_attachments");
    assert!(strip.contains("let local = localize(path);"));
    assert!(strip.contains("handlers.get(&key)"));
    assert!(strip.contains("&localize(path),"));
}

#[test]
fn the_view_localizes_through_the_crab_it_was_handed() {
    let view = body(TRANSCRIPT, "view");
    assert!(view.contains("Some(backend) => backend.localize(path),"));
    assert_eq!(
        view.matches("&attachment_handlers").count(),
        1,
        "the click map goes into the tiles once; every row takes the tiles"
    );
    assert!(view.contains("clicks: &attachment_handlers,"));
    for (who, src) in [("window", DASHBOARD_RENDER), ("pane", PANE_VIEW)] {
        assert!(
            src.contains("backend: Some(self.backend.clone()),"),
            "the {who} hands the transcript its own crab"
        );
    }
}

/// A remote crab whose copies of `names` (under `/home/crab`) are already in
/// `cache`. Nothing listens on the discard port, so a fetch could only fail;
/// the tests assert none starts.
fn remote_with_kept(cache: &Path, names: &[&str]) -> (Backend, Arc<RemoteCrab>) {
    let crab = Arc::new(
        RemoteCrab::new(
            "tiles",
            "http://127.0.0.1:9",
            None,
            PathBuf::from("unused.db"),
        )
        .with_blob_dir(cache.to_path_buf()),
    );
    let store = BlobStore::at(cache.to_path_buf());
    for name in names {
        store
            .keep(&Path::new("/home/crab").join(name), b"not what it says")
            .expect("keep");
    }
    (Backend::Remote(crab.clone()), crab)
}

#[test]
fn a_remote_file_outside_the_allowlist_never_reaches_the_picture_nor_the_player() {
    let cache = tempfile::tempdir().expect("cache");
    let names = ["x.svg", "x.desktop", "x.bin", "x.bmp", "x.flac", "x.mkv"];
    let (backend, crab) = remote_with_kept(cache.path(), &names);

    for name in names {
        let on_crab = Path::new("/home/crab").join(name);
        let copy = backend.localize_allowed(&on_crab);
        assert_eq!(copy, None, "{name}");
        assert_eq!(tile_picture(copy.as_deref()), None, "{name}");
        assert!(!plays_as_audio(copy.as_deref()), "{name}");
    }

    // The overlay: a picture or a clip whose copy is not allowed is a file.
    for name in ["x.bmp", "x.mkv"] {
        let open = Preview::of(Path::new("/home/crab").join(name));
        assert_ne!(open.kind, PreviewKind::File, "{name} opens as media");
        let drawn = open.localized_allowed(&backend);
        assert_eq!(drawn.kind, PreviewKind::File, "{name}");
        assert_eq!(
            drawn.path.extension().and_then(|e| e.to_str()),
            Some("bin"),
            "{name}: the kept copy, never the crab's path"
        );
    }

    // The clip's facts: the kept `.bin` is a file on this disk, and still
    // never stat'ed nor handed to ffmpeg.
    let home = tempfile::tempdir().expect("home");
    let facts = VideoFacts::probe(&backend, home.path(), Path::new("/home/crab/x.mkv"));
    assert_eq!(facts.bytes, None);
    assert_eq!(facts.poster, None);

    assert_eq!(crab.blob_fetches(), 0, "nothing refused is fetched");
}

#[test]
fn a_remote_allowed_copy_reaches_the_picture_the_player_and_the_overlay() {
    let cache = tempfile::tempdir().expect("cache");
    let (backend, crab) = remote_with_kept(cache.path(), &["a.PNG", "a.mp3", "a.mp4"]);
    let store = BlobStore::at(cache.path().to_path_buf());

    let png = Path::new("/home/crab/a.PNG");
    let copy = backend.localize_allowed(png);
    assert_eq!(copy, Some(store.slot(png)));
    assert_eq!(tile_picture(copy.as_deref()), copy.as_deref());

    let mp3 = backend.localize_allowed(Path::new("/home/crab/a.mp3"));
    assert!(plays_as_audio(mp3.as_deref()));

    let drawn = Preview::of(png.to_path_buf()).localized_allowed(&backend);
    assert_eq!(drawn.kind, PreviewKind::Image);
    assert_eq!(drawn.path, store.slot(png));

    let mp4 = Path::new("/home/crab/a.mp4");
    let drawn = Preview::of(mp4.to_path_buf()).localized_allowed(&backend);
    assert_eq!(drawn.kind, PreviewKind::Video);
    assert_eq!(drawn.path, store.slot(mp4));
    let home = tempfile::tempdir().expect("home");
    let facts = VideoFacts::probe(&backend, home.path(), mp4);
    assert_eq!(facts.bytes, Some(b"not what it says".len() as u64));

    assert_eq!(crab.blob_fetches(), 0, "kept copies ask the crab nothing");
}

#[test]
fn the_local_crab_s_tiles_and_preview_are_as_they_were() {
    let dir = tempfile::tempdir().expect("dir");
    let backend = Backend::local(dir.path().join("opencrabs.db"));
    for name in ["x.svg", "x.png", "x.bmp"] {
        let file = dir.path().join(name);
        let copy = backend.localize_allowed(&file);
        assert_eq!(
            tile_picture(copy.as_deref()),
            Some(file.as_path()),
            "{name}"
        );
    }
    for name in ["x.flac", "x.mp3"] {
        let copy = backend.localize_allowed(&dir.path().join(name));
        assert!(plays_as_audio(copy.as_deref()), "{name}");
    }
    for name in ["x.png", "x.bmp", "x.mkv", "x.mp4", "x.pdf"] {
        let open = Preview::of(dir.path().join(name));
        assert_eq!(open.localized_allowed(&backend), open, "{name}");
    }
    let clip = dir.path().join("x.mkv");
    std::fs::write(&clip, b"clip").expect("clip");
    let home = tempfile::tempdir().expect("home");
    let facts = VideoFacts::probe(&backend, home.path(), &clip);
    assert_eq!(facts.bytes, Some(4));
}

/// The wiring the tests above cannot reach without a window: the view hands
/// the tiles `localize_allowed`, the strip and the cell decode only through
/// it, and the overlay draws a picture or a clip only from `localized_allowed`.
#[test]
fn the_tile_and_the_overlay_decode_only_the_allowed_copy() {
    let view = body(TRANSCRIPT, "view");
    assert!(view.contains("Some(backend) => backend.localize_allowed(path),"));
    assert!(view.contains("allowed: &allowed,"));

    let strip = body(TRANSCRIPT, "history_attachments");
    assert!(strip.contains("plays_as_audio(allowed(path.as_path())"));
    assert!(strip.contains("let copy = allowed(path);"));
    assert_eq!(strip.matches("copy.as_deref(), side)").count(), 2);

    let cell = body(TRANSCRIPT, "attachment_cell");
    assert!(cell.contains("if let Some(local) = tile_picture(copy)"));
    assert_eq!(cell.matches("crate::thumbs::picture(").count(), 1);

    let shell = body(PREVIEW, "body");
    assert!(shell.contains("match preview.localized_allowed(backend).kind"));
    for decoder in ["image_body", "video_body"] {
        let decoder_body = body(PREVIEW, decoder);
        assert!(
            decoder_body.contains("let preview = &preview.localized_allowed(backend);"),
            "{decoder} draws the copy the allowlist answers"
        );
        assert!(!decoder_body.contains(".localized(backend)"), "{decoder}");
    }
}
