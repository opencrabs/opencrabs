//! No channel may download inbound media by hand again (#2261).
//!
//! `reqwest::get` returns `Ok` for any HTTP status, so a call site that reads
//! the body straight away hands a 403 page or an expired `file_path` body to
//! the vision or document pipeline as if it were the user's file. It also has
//! no timeout and no retry. `utils::download::fetch_bytes` fixes all three, and
//! this guard is what keeps the fix from decaying back into eleven inline
//! copies the next time someone adds a media type.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn channels_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files
}

#[test]
fn no_channel_downloads_with_a_bare_reqwest_get() {
    let offenders: Vec<String> = channels_files()
        .into_iter()
        .filter(|path| {
            // Outbound speech synthesis fetches a generated file and already
            // checks the status before a byte is read: it is the shape the
            // inbound paths were fixed to copy, not a defect.
            !path.ends_with("voice/local_tts.rs")
        })
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path).ok()?;
            source
                .contains("reqwest::get(")
                .then(|| path.display().to_string())
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "these inbound paths bypass utils::download::fetch_bytes (status check, \
         timeout, retry) and will read an error body as file content: {offenders:?}"
    );
}

#[test]
fn every_telegram_media_kind_goes_through_the_shared_downloader() {
    let inbound = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/telegram/inbound_media.rs"),
    )
    .expect("inbound_media.rs must exist");
    let sites = inbound
        .matches("crate::utils::download::fetch_bytes")
        .count();
    assert_eq!(
        sites, 6,
        "voice, photo, video, animation, video note and document each need the \
         shared downloader; found {sites}"
    );
}
