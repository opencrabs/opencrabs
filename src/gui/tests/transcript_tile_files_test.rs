//! A transcript attachment is drawn from the file on this machine (P9).
//!
//! A stored row names its attachments by the crab's paths. The tile keys its
//! click and its kind by that path, and reads the picture, the size and the
//! day it changed from where `Backend::localize` puts the file here, so a crab
//! on another machine never has its path stat'ed or decoded on this disk. The
//! local crab's `localize` is its own path, as it was.

const TRANSCRIPT: &str = include_str!("../transcript.rs");
const DASHBOARD_RENDER: &str = include_str!("../dashboard_render.rs");
const PANE_VIEW: &str = include_str!("../pane_view.rs");

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
