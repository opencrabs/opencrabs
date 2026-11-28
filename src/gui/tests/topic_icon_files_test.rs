//! A topic's image icon is read from the file its crab named (P9).
//!
//! `LoadTopicIcons` answers each image icon with a `BlobRef` on the crab's
//! disk. The window keeps that path per topic and localizes it, so a remote
//! crab's picture comes from that crab's copy in the blob cache, never from
//! this window's own icon store under the same name. The local crab's path is
//! its own, as it was.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use opencrabs::backend::BlobRef;
use opencrabs::backend::sessions::TopicIcon;

use crate::backend_glue::Backend;
use crate::backend_remote::RemoteCrab;
use crate::blob_cache_store::BlobStore;
use crate::dashboard_topics::read_icon_images;
use crate::sessions::split_topic_icons;

/// A remote crab whose copies go to `cache`. Nothing listens on the discard
/// port, so a fetch could only fail; the tests assert none starts.
fn crab_with_cache(cache: &Path) -> Arc<RemoteCrab> {
    Arc::new(
        RemoteCrab::new(
            "icons",
            "http://127.0.0.1:9",
            None,
            PathBuf::from("unused.db"),
        )
        .with_blob_dir(cache.to_path_buf()),
    )
}

/// Not a decodable picture: the read keeps bytes, it never decodes them.
const PICTURE: &[u8] = b"\x89PNG crab picture";

#[test]
fn the_crab_file_is_kept_per_topic_and_only_for_an_image() {
    let image_topic = uuid::Uuid::from_u128(1);
    let mark_topic = uuid::Uuid::from_u128(2);
    let on_crab = "/home/crab/.opencrabs/gui-icons/a.png";
    let (stored, files) = split_topic_icons(vec![
        TopicIcon {
            id: image_topic,
            stored: "img:a.png".to_string(),
            blob: Some(BlobRef::at(on_crab)),
        },
        TopicIcon {
            id: mark_topic,
            stored: "topic:development".to_string(),
            blob: None,
        },
    ]);

    assert_eq!(stored.len(), 2, "every topic keeps its stored value");
    assert_eq!(
        stored.get(&image_topic).map(String::as_str),
        Some("img:a.png")
    );
    assert_eq!(
        stored.get(&mark_topic).map(String::as_str),
        Some("topic:development")
    );
    assert_eq!(
        files.get(&image_topic),
        Some(&PathBuf::from(on_crab)),
        "the image topic keeps the file its crab named"
    );
    assert!(
        !files.contains_key(&mark_topic),
        "a mark names no file, so it keeps none"
    );
}

#[test]
fn a_remote_crab_icon_is_read_from_its_copy_by_the_crab_path() {
    let cache = tempfile::tempdir().expect("cache");
    let crab = crab_with_cache(cache.path());
    let on_crab = Path::new("/home/crab/.opencrabs/gui-icons/remote-icon.png");
    assert!(!on_crab.exists(), "the crab's path is not on this machine");
    BlobStore::at(cache.path().to_path_buf())
        .keep(on_crab, PICTURE)
        .expect("keep");
    let backend = Backend::Remote(crab.clone());

    let topic = uuid::Uuid::from_u128(7);
    let icons = HashMap::from([(topic, "img:remote-icon.png".to_string())]);
    let files = HashMap::from([(topic, on_crab.to_path_buf())]);
    let images = read_icon_images(&backend, &icons, &files);

    let image = images
        .get("img:remote-icon.png")
        .expect("the icon was read from the crab's copy");
    assert_eq!(image.bytes, PICTURE);
    assert_eq!(
        crab.blob_fetches(),
        0,
        "a cached icon asks the crab nothing"
    );
}

#[test]
fn a_remote_crab_icon_of_a_kind_not_opened_here_is_never_read() {
    let cache = tempfile::tempdir().expect("cache");
    let crab = crab_with_cache(cache.path());
    let on_crab = Path::new("/home/crab/.opencrabs/gui-icons/remote-icon.svg");
    BlobStore::at(cache.path().to_path_buf())
        .keep(on_crab, b"<svg/>")
        .expect("keep");
    let backend = Backend::Remote(crab.clone());

    let topic = uuid::Uuid::from_u128(8);
    let icons = HashMap::from([(topic, "img:remote-icon.svg".to_string())]);
    let files = HashMap::from([(topic, on_crab.to_path_buf())]);
    let images = read_icon_images(&backend, &icons, &files);

    assert!(
        images.is_empty(),
        "an svg from a remote crab is not decoded here"
    );
    assert_eq!(crab.blob_fetches(), 0, "and it is never asked for");
}

#[test]
fn a_remote_icon_decodes_as_its_file_not_as_its_stored_value() {
    let cache = tempfile::tempdir().expect("cache");
    let crab = crab_with_cache(cache.path());
    let on_crab = Path::new("/home/crab/.opencrabs/gui-icons/claims-svg.png");
    BlobStore::at(cache.path().to_path_buf())
        .keep(on_crab, PICTURE)
        .expect("keep");
    let backend = Backend::Remote(crab.clone());

    let topic = uuid::Uuid::from_u128(10);
    let icons = HashMap::from([(topic, "img:claims-svg.svg".to_string())]);
    let files = HashMap::from([(topic, on_crab.to_path_buf())]);
    let images = read_icon_images(&backend, &icons, &files);

    let image = images
        .get("img:claims-svg.svg")
        .expect("the png file is read");
    assert_eq!(
        image.format,
        gpui::ImageFormat::Png,
        "the decoder is told the checked file's kind, never the stored value's"
    );
}

#[test]
fn a_local_crab_icon_is_read_from_its_own_path() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("local-icon.png");
    std::fs::write(&path, PICTURE).expect("icon file");
    let backend = Backend::local(dir.path().join("opencrabs.db"));

    let topic = uuid::Uuid::from_u128(9);
    let icons = HashMap::from([(topic, "img:local-icon.png".to_string())]);
    let files = HashMap::from([(topic, path)]);
    let images = read_icon_images(&backend, &icons, &files);

    let image = images
        .get("img:local-icon.png")
        .expect("the local crab's icon is its own file");
    assert_eq!(image.bytes, PICTURE);
}

/// Boot and the topic refresh fill the file map from the same read that
/// fills the stored values, and the image refresh reads through it.
#[test]
fn boot_and_refresh_keep_the_crab_file_beside_the_stored_value() {
    let app = include_str!("../app.rs");
    let topics = include_str!("../dashboard_topics.rs");
    assert!(app.contains("sessions::split_topic_icons("));
    assert!(app.contains("topic_icon_files"));
    assert!(topics.contains("sessions::split_topic_icons(icons)"));
    assert!(topics.contains("this.sessions.topic_icon_files = files;"));
    assert!(topics.contains("&self.sessions.topic_icon_files,"));
}
