//! Clicking a path or url in the window's transcript opens it (#2317).
//!
//! The resolver is shared with the terminal and lives in `ui_core`, so the
//! rules are called for real here: a url, an existing path, a path with a line
//! suffix, a byte offset through multibyte text, and a link label winning over
//! the text drawn around it. The window has no headless harness and the GUI is
//! a separate binary crate, so the wiring inside it (the parser keeping a
//! link's destination, the press recording what it landed on, the release
//! opening only after a copy found nothing) is read from source.

use crate::ui_core::links::{ClickTarget, target_at_byte_with, target_at_with, target_for_click};
use std::ops::Range;
use std::path::{Path, PathBuf};

const MARKDOWN: &str = include_str!("../gui/markdown.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

/// A temp dir unique per test, holding the fixtures it names.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(tag: &str, files: &[&str]) -> Self {
        let dir = std::env::temp_dir().join(format!("oc-click-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        for f in files {
            let p = dir.join(f);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("parent dirs");
            }
            std::fs::write(&p, b"fixture").expect("fixture write");
        }
        Self { dir }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The body of the function whose signature starts with `header`, up to the
/// closing brace that ends it. The brace is matched at the signature's own
/// indentation, so a method reads as one method instead of running on to the
/// end of its `impl`.
fn body_of<'a>(src: &'a str, header: &str) -> &'a str {
    let start = src
        .find(header)
        .unwrap_or_else(|| panic!("{header} must exist"));
    let head = src.get(..start).unwrap_or_default();
    let line_start = head.rfind('\n').map_or(0, |n| n + 1);
    let indent = " ".repeat(start - line_start);
    let close = format!("\n{indent}}}\n");
    let body = src.get(start..).unwrap_or_default();
    let cut = body
        .find(&close)
        .unwrap_or_else(|| panic!("{header} has a closing brace"));
    body.get(..cut).unwrap_or_default()
}

#[test]
fn open_target_resolves_a_url_mid_sentence() {
    let line = "See https://opencrabs.com/docs for more.";
    let col = line.find("opencrabs").expect("needle") + 2;
    assert_eq!(
        target_at_with(line, col, Path::new("/nonexistent"), None),
        Some(ClickTarget::Url("https://opencrabs.com/docs".into()))
    );
}

#[test]
fn open_target_resolves_an_existing_path_and_not_a_missing_one() {
    let box_ = Sandbox::new("path", &["src/gui/main.rs"]);
    let line = "open src/gui/main.rs and then src/gui/gone.rs please";
    let here = line.find("main.rs").expect("needle");
    assert_eq!(
        target_at_with(line, here, &box_.dir, None),
        Some(ClickTarget::File(box_.dir.join("src/gui/main.rs")))
    );
    let gone = line.find("gone.rs").expect("needle");
    assert_eq!(target_at_with(line, gone, &box_.dir, None), None);
}

/// `main.rs:1012` names a file plus a line. The line number is not part of
/// the name, so the file opens instead of the probe asking disk for
/// `main.rs:1012`.
#[test]
fn open_target_from_a_path_with_a_line_suffix() {
    let box_ = Sandbox::new("suffix", &["src/gui/main.rs"]);
    let line = "see src/gui/main.rs:1012:5 for the handler";
    let col = line.find("main.rs").expect("needle");
    assert_eq!(
        target_at_with(line, col, &box_.dir, None),
        Some(ClickTarget::File(box_.dir.join("src/gui/main.rs")))
    );
}

/// A shaped text layout reports a BYTE offset while the token walk counts
/// characters. Multibyte text in front of the path is where the two disagree.
#[test]
fn open_target_resolves_from_a_byte_offset() {
    let box_ = Sandbox::new("byte", &["notes/a.md"]);
    let line = "→ notes/a.md ←";
    let char_col = line.find("notes").expect("needle");
    assert_eq!(
        target_at_byte_with(line, char_col + 3, &box_.dir, None),
        Some(ClickTarget::File(box_.dir.join("notes/a.md")))
    );
    // A byte inside the arrow rounds down to its start, which is a break.
    assert_eq!(target_at_byte_with(line, 1, &box_.dir, None), None);
}

#[test]
fn open_target_is_inert_on_a_prose_word() {
    let line = "for example, e.g. this sentence has no target";
    assert_eq!(
        target_at_with(line, 15, Path::new("/nonexistent"), None),
        None
    );
}

/// A label carries no destination of its own: `[docs](https://x)` draws
/// `docs`, and only the parser knows where it points.
#[test]
fn open_target_prefers_the_label_over_the_text_around_it() {
    let text = "read docs first";
    let label = text.find("docs").expect("label");
    let links: Vec<(Range<usize>, String)> =
        vec![(label..label + 4, "https://opencrabs.com".to_string())];
    assert_eq!(
        target_for_click(text, &links, label + 1),
        Some(ClickTarget::Url("https://opencrabs.com".to_string()))
    );
}

/// Off the label the text is ordinary rendered text, so a link must not make
/// its whole line clickable.
#[test]
fn open_target_is_inert_beside_the_label() {
    let text = "read docs first";
    let label = text.find("docs").expect("label");
    let links: Vec<(Range<usize>, String)> =
        vec![(label..label + 4, "https://opencrabs.com".to_string())];
    assert_eq!(target_for_click(text, &links, 0), None);
}

/// A plain rendered path is what the screenshot showed: inline code, not a
/// link, so it resolves from the token under the pointer.
#[test]
fn open_target_from_a_rendered_path_without_a_link() {
    let box_ = Sandbox::new("plain", &["notes.md"]);
    let file = box_.dir.join("notes.md");
    let text = format!("saved to {} just now", file.display());
    let offset = text.find("notes.md").expect("name in the line");
    assert_eq!(
        target_for_click(&text, &[], offset),
        Some(ClickTarget::File(file))
    );
}

/// The parser used to throw the destination away and keep only the styling,
/// so a click on `[docs](https://x)` had nothing to open.
#[test]
fn open_target_keeps_the_links_destination() {
    assert!(
        MARKDOWN.contains("Tag::Link { dest_url, .. } => {")
            && MARKDOWN.contains("b.inline.link_dest = Some(dest_url.to_string());"),
        "the parser must keep the destination the link points at"
    );
    assert!(
        MARKDOWN.contains("TagEnd::Link => {") && MARKDOWN.contains("b.inline.link_dest = None;"),
        "the destination must be cleared when the link ends, so the next run is not a link"
    );
}

/// A label's own bytes are what the pointer lands on, so the range is taken
/// where the text is written instead of derived afterwards.
#[test]
fn open_target_label_range_is_recorded_where_the_text_is_written() {
    let push = body_of(MARKDOWN, "fn push_inline(");
    assert!(
        push.contains("self.links.push((start..self.buf.len(), dest.clone()));"),
        "the press lands on the label's bytes, so the range is taken at write time"
    );
}

/// A cell is trimmed after the fact, so its link range has to move with the
/// text exactly the way its highlights do.
#[test]
fn open_target_table_cell_moves_its_link_range_with_the_trim() {
    let cell = body_of(MARKDOWN, "fn cell_from(");
    assert!(
        cell.contains("let start = range.start.max(lo);")
            && cell.contains("let end = range.end.min(hi);")
            && cell.contains("moved_links.push((start - lo..end - lo, dest));"),
        "a cell link range moves with the trimmed text"
    );
}

/// A line that loses its links is a line whose labels cannot be clicked.
#[test]
fn open_target_lines_and_cells_carry_their_links() {
    assert_eq!(
        MARKDOWN
            .matches("pub links: Vec<(Range<usize>, String)>,")
            .count(),
        2,
        "MdLine and MdCell both carry link ranges"
    );
}

/// The gesture rule: opening is the `else` of the copy branch. Opening on the
/// press, or before the copy is known, would launch a url the reader was
/// dragging to select.
#[test]
fn open_target_waits_for_the_copy_to_find_nothing() {
    let release = body_of(TRANSCRIPT, "pub fn release(&self, window: &mut Window");
    let copy = release
        .find("finish_copy(app)")
        .expect("the release copies first");
    let open = release
        .find("open_clicked_target()")
        .expect("the release opens a click");
    let guard = release
        .find("} else {")
        .expect("the opener sits on the not-copied branch");
    assert!(
        copy < guard && guard < open,
        "opening must be the else of the copy branch, so a drag that copied never launches"
    );
}

/// A release cannot resolve a token the press never saw.
#[test]
fn open_target_press_records_what_it_landed_on() {
    let segment = body_of(TRANSCRIPT, "fn selectable_text_segment(");
    let press = segment
        .find("on_mouse_down(MouseButton::Left")
        .expect("the segment takes a press");
    let after = segment.get(press..).unwrap_or_default();
    assert!(
        after.contains("pending_click.borrow_mut() = Some(PendingClick"),
        "the press records the candidate inside its own handler"
    );
    assert!(
        segment.contains("let local = layout_offset(&layout, event.position, len);"),
        "the candidate offset is local to the segment that drew the text"
    );
}

/// A press on row padding is not a text click, so it must drop any candidate
/// an earlier press left behind instead of firing it on release.
#[test]
fn open_target_press_on_padding_leaves_no_candidate() {
    let row = body_of(TRANSCRIPT, "fn selectable_row<E: gpui::IntoElement>(");
    assert!(
        row.contains("pending_click.borrow_mut().take()"),
        "the row press clears the candidate"
    );
}

/// A link split around a crab picture still opens from either side of it, and
/// the launch goes through the same commands the attachment preview uses.
#[test]
fn open_target_survives_a_split_and_launches_through_preview() {
    let split = body_of(TRANSCRIPT, "fn local_links(");
    assert!(
        split.contains("let from = span.start.max(range.start);")
            && split.contains("let to = span.end.min(range.end);"),
        "a link range is rebased onto the chunk that draws it"
    );
    let open = body_of(TRANSCRIPT, "pub fn open_clicked_target(&self) -> bool");
    assert!(
        open.contains("crate::preview::launch(&crate::preview::url_command(url))")
            && open.contains("crate::preview::launch(&crate::preview::open_command(path))"),
        "a url and a path go through the platform opener"
    );
    assert!(
        open.contains("transcript link: open {}"),
        "a hit writes a line naming what opened"
    );
    assert!(
        open.contains("transcript link: nothing to open at"),
        "a miss writes a line too, so a click that found nothing can be told from one that never arrived"
    );
}
