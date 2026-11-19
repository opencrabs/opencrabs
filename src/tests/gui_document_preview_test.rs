use std::fs;

use crate::ui_core::local_document::{DocumentKind, DocumentLoadError, load, load_with_limits};
use crate::ui_core::scroll::{ScrollThumb, vertical_scroll_thumb};

#[test]
fn document_kind_accepts_only_the_first_preview_slice() {
    assert_eq!(
        DocumentKind::of_path(std::path::Path::new("notes.MD")),
        Some(DocumentKind::Markdown)
    );
    assert_eq!(
        DocumentKind::of_path(std::path::Path::new("notes.markdown")),
        Some(DocumentKind::Markdown)
    );
    assert_eq!(
        DocumentKind::of_path(std::path::Path::new("notes.txt")),
        Some(DocumentKind::PlainText)
    );
    assert_eq!(
        DocumentKind::of_path(std::path::Path::new("sheet.pdf")),
        None
    );
    assert_eq!(DocumentKind::of_path(std::path::Path::new("README")), None);
}

#[test]
fn loader_reads_text_without_marking_it_truncated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("note.md");
    let source = "# Heading\n\nA **small** document.\n";
    fs::write(&path, source).expect("write document");

    let loaded = load(&path).expect("valid document loads");
    assert_eq!(loaded.text, source);
    assert!(!loaded.truncated);
}

#[test]
fn loader_reports_missing_paths_and_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.md");
    assert!(matches!(load(&missing), Err(DocumentLoadError::Io(_))));
    assert_eq!(load(dir.path()), Err(DocumentLoadError::NotAFile));
}

#[test]
fn loader_rejects_malformed_utf8() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.txt");
    fs::write(&path, [b'a', 0xff, b'b']).expect("write invalid UTF-8");

    assert_eq!(load(&path), Err(DocumentLoadError::InvalidUtf8));
}

#[test]
fn byte_limit_drops_only_an_incomplete_trailing_codepoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("unicode.txt");
    fs::write(&path, "ééé").expect("write Unicode document");

    let loaded = load_with_limits(&path, 5, 100).expect("valid prefix loads");
    assert_eq!(loaded.text, "éé");
    assert!(loaded.truncated);
}

#[test]
fn character_limit_preserves_whole_codepoints() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("unicode.txt");
    fs::write(&path, "aé🦀b").expect("write Unicode document");

    let loaded = load_with_limits(&path, 100, 3).expect("character cap applies");
    assert_eq!(loaded.text, "aé🦀");
    assert!(loaded.truncated);
}

#[test]
fn document_preview_gui_keeps_io_parsing_scrolling_and_lifetime_wired() {
    let dashboard = include_str!("../gui/dashboard_sessions.rs");
    assert!(dashboard.contains(".background_executor()"));
    assert!(dashboard.contains("preview_document::parse(&load_path, kind)"));
    assert!(dashboard.contains("document.matches(&path, generation)"));
    assert!(dashboard.contains("if visible && current"));
    assert!(dashboard.contains("self.preview_document = None;"));

    let document = include_str!("../gui/preview_document.rs");
    assert!(document.contains("scroll: ScrollHandle::new()"));
    assert!(document.contains(".overflow_y_scroll()"));
    assert!(document.contains(".track_scroll(&state.scroll)"));
    assert!(document.contains("DocumentStatus::Error"));
    assert!(document.contains("Preview truncated."));
    assert!(document.contains("markdown::blocks_flow"));
    assert!(document.contains(".h(px(height))"));
    assert!(document.contains(".on_mouse_down(MouseButton::Middle"));
    assert!(document.contains("preview-document-scrollbar"));
    assert!(document.contains("vertical_scroll_thumb("));

    let transcript = include_str!("../gui/transcript.rs");
    assert!(transcript.contains("md_parsed_blocks(parsed.as_slice(), look, selection)"));
    assert!(transcript.contains("pub(super) fn document_blocks"));
    assert!(transcript.contains("md_parsed_blocks(blocks, MdLook::body(USER_TEXT_SIZE), None)"));

    let preview = include_str!("../gui/preview.rs");
    assert!(preview.contains("shell.child(content).child(actions)"));
    assert!(preview.contains("modal_shell::render("));
    assert!(preview.contains("width_cap = if document_open { 1040.0 }"));
    assert!(preview.contains("PgUp PgDn"));
    assert!(preview.contains("Home End"));

    let keys = include_str!("../gui/dashboard_keys.rs");
    let main = include_str!("../gui/main.rs");
    let middle = include_str!("../gui/middle_scroll.rs");
    let sessions = include_str!("../gui/dashboard_sessions.rs");
    assert!(keys.contains("self.preview_key(ev, window, cx);"));
    assert!(keys.contains("\"pageup\" | \"pagedown\""));
    assert!(keys.contains("\"home\" => Some(0.0)"));
    assert!(keys.contains("\"end\" => Some(-max)"));
    assert!(keys.contains("self.close_preview();"));
    assert!(main.contains("let document_height = (viewport.1 - 300.0).clamp(120.0, 620.0);"));
    assert!(main.contains("this.close_preview();"));
    assert!(sessions.contains("middle_scroll::MiddleScrollTarget::Preview"));
    assert!(middle.contains("MiddleScrollTarget::Preview"));
    assert!(middle.contains("move_scroll(&document.scroll, delta)"));
}

#[test]
fn document_scrollbar_tracks_the_reading_position() {
    assert_eq!(
        vertical_scroll_thumb(400.0, 1600.0, 0.0),
        Some(ScrollThumb {
            top: 0.0,
            height: 80.0
        })
    );
    assert_eq!(
        vertical_scroll_thumb(400.0, 1600.0, -800.0),
        Some(ScrollThumb {
            top: 160.0,
            height: 80.0
        })
    );
    assert_eq!(
        vertical_scroll_thumb(400.0, 1600.0, -1600.0),
        Some(ScrollThumb {
            top: 320.0,
            height: 80.0
        })
    );
}

#[test]
fn document_scrollbar_keeps_a_visible_thumb_and_hides_when_content_fits() {
    assert_eq!(
        vertical_scroll_thumb(400.0, 19_600.0, -19_600.0),
        Some(ScrollThumb {
            top: 376.0,
            height: 24.0
        })
    );
    assert_eq!(vertical_scroll_thumb(400.0, 0.0, 0.0), None);
    assert_eq!(vertical_scroll_thumb(0.0, 100.0, 0.0), None);
    assert_eq!(vertical_scroll_thumb(f32::NAN, 100.0, 0.0), None);
}
