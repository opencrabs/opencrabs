//! Clicking a path or url in the window's transcript opens it (#2317).
//!
//! The resolver is shared with the terminal and lives in `ui_core`, so the
//! rules are called for real here: a url, an existing path, a path with a line
//! suffix, a byte offset through multibyte text, and a link label winning over
//! the text drawn around it. The window has no headless harness and the GUI is
//! a separate binary crate, so the wiring inside it (the parser keeping a
//! link's destination, the press recording what it landed on, the release
//! opening only after a copy found nothing) is read from source.

use crate::ui_core::links::{
    ClickTarget, click_names_target, target_at_byte_with, target_at_roots, target_at_with,
    target_for_click, target_for_click_roots, target_for_click_with, token_at_byte,
};
use std::ops::Range;
use std::path::{Path, PathBuf};

const MARKDOWN: &str = include_str!("../gui/markdown.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const MAIN: &str = concat!(
    include_str!("../gui/main.rs"),
    include_str!("../gui/app.rs"),
    include_str!("../gui/dashboard_render.rs"),
    include_str!("../gui/dashboard_state.rs"),
);
const PANES: &str = include_str!("../gui/dashboard_panes.rs");
const SESSIONS: &str = include_str!("../gui/dashboard_sessions.rs");

/// Source text with every whitespace character removed, so a pin on a call
/// shape survives rustfmt wrapping that call across lines. The pin still
/// proves the receiver and the method are one expression; only the line
/// breaks are free.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

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

/// An explicit local destination is a file when it exists relative to the
/// selected cwd. A missing target stays inert instead of falling through to
/// the browser.
#[test]
fn open_target_resolves_an_explicit_local_markdown_link() {
    let box_ = Sandbox::new("markdown-file", &["notes.md"]);
    let text = "read notes now";
    let label = text.find("notes").expect("label");
    let links = vec![(label..label + 5, "notes.md".to_string())];
    assert_eq!(
        target_for_click_with(text, &links, label + 1, &box_.dir, None),
        Some(ClickTarget::File(box_.dir.join("notes.md")))
    );

    let missing = vec![(label..label + 5, "gone.md".to_string())];
    assert_eq!(
        target_for_click_with(text, &missing, label + 1, &box_.dir, None),
        None
    );
}

#[test]
fn open_target_resolves_an_explicit_file_url() {
    let box_ = Sandbox::new("file-url", &["notes.md"]);
    let file = box_.dir.join("notes.md");
    let text = "read notes now";
    let label = text.find("notes").expect("label");
    let links = vec![(label..label + 5, format!("file://{}", file.display()))];
    assert_eq!(
        target_for_click_with(text, &links, label + 1, &box_.dir, None),
        Some(ClickTarget::File(file))
    );
}

/// Explicit links historically accepted every URI scheme. Keep that behavior
/// instead of applying the stricter host-shape check used for bare tokens.
#[test]
fn open_target_keeps_explicit_external_schemes() {
    let text = "read docs now";
    let label = text.find("docs").expect("label");
    for dest in [
        "https://x",
        "mailto:docs@example.com",
        "ftp://example.com/file",
    ] {
        let links = vec![(label..label + 4, dest.to_string())];
        assert_eq!(
            target_for_click_with(text, &links, label + 1, Path::new("/nonexistent"), None),
            Some(ClickTarget::Url(dest.to_string()))
        );
    }
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
        .find("open_clicked_target(app)")
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

/// A link split around a crab picture still opens from either side of it. The
/// transcript dispatches the target to its window instead of choosing an OS
/// opener for a local file itself.
#[test]
fn open_target_survives_a_split_and_routes_through_the_dashboard() {
    let split = body_of(TRANSCRIPT, "fn local_links(");
    assert!(
        split.contains("let from = span.start.max(range.start);")
            && split.contains("let to = span.end.min(range.end);"),
        "a link range is rebased onto the chunk that draws it"
    );
    let open = body_of(
        TRANSCRIPT,
        "pub fn open_clicked_target(&self, app: &mut App) -> bool",
    );
    assert!(
        open.contains("listener(target, app);") && !open.contains("preview::open_command"),
        "the transcript hands the target to its window"
    );
    assert!(
        flat(MAIN).contains("this.copy_drag.set_open_target_listener")
            && flat(PANES).contains("pane_copy.set_open_target_listener"),
        "both the main transcript and every split pane attach the dashboard route"
    );
    let dashboard = body_of(SESSIONS, "pub(crate) fn open_transcript_target(");
    assert!(
        dashboard.contains("self.open_preview(path, cx)")
            && dashboard.contains("preview::url_command(&url)"),
        "files use the overlay while external URLs keep the system handler"
    );
    assert!(
        open.contains("transcript link: open {}"),
        "a hit writes a line naming what opened"
    );
    let miss = body_of(TRANSCRIPT, "fn note_miss(");
    assert!(
        miss.contains("transcript link: nothing to open at"),
        "a miss writes a line too, so a click that found nothing can be told from one that never arrived"
    );
    assert!(
        open.contains("self.note_miss(&pending)"),
        "the click handler routes a miss to the function that writes the line"
    );
}

/// A relative name resolves against the conversation's directory (#2846).
///
/// A window launched from a desktop entry has no shell cwd, so every relative
/// name in the transcript used to resolve against `/tmp` and the click did
/// nothing. The session directory is now the first root.
#[test]
fn a_relative_name_resolves_against_the_session_directory() {
    let box_ = Sandbox::new("roots", &["notes.md"]);
    let line = "the report is in notes.md for you";
    let col = line.find("notes.md").expect("fixture word") + 2;

    let target = target_at_roots(line, col, &[box_.dir.as_path()], None);
    assert_eq!(
        target,
        Some(ClickTarget::File(box_.dir.join("notes.md"))),
        "a relative name must resolve under the session directory"
    );
}

/// The first root wins, so the session directory beats the process cwd
/// (#2846). Without an order the two would race and the window would open a
/// file from a directory the conversation never worked in.
#[test]
fn the_first_root_wins_when_both_hold_the_name() {
    let first = Sandbox::new("root-a", &["report.md"]);
    let second = Sandbox::new("root-b", &["report.md"]);
    let line = "see report.md";

    let target = target_at_roots(line, 5, &[first.dir.as_path(), second.dir.as_path()], None);
    assert_eq!(
        target,
        Some(ClickTarget::File(first.dir.join("report.md"))),
        "the session directory must be tried before the process cwd"
    );
}

/// A name the transcript wraps in prose can hold a space, and a space ends a
/// token, so neither half exists on its own (#2846).
#[test]
fn a_name_with_a_space_opens_from_the_session_directory() {
    let name = "AUDIT-GUI-BEBAN 121126-1837.md";
    let box_ = Sandbox::new("spaced", &[name]);
    let line = format!("wrote {name} just now");

    // Click inside the second word: its own token is `121126-1837.md`, which
    // does not exist, so only the joined span names the real file.
    let col = line.find("121126").expect("second word") + 3;
    assert_eq!(
        target_at_roots(&line, col, &[box_.dir.as_path()], None),
        Some(ClickTarget::File(box_.dir.join(name))),
        "a spaced name must resolve as one span"
    );

    // And from the first word, which is the other half of the same name.
    let col = line.find("AUDIT").expect("first word") + 2;
    assert_eq!(
        target_at_roots(&line, col, &[box_.dir.as_path()], None),
        Some(ClickTarget::File(box_.dir.join(name))),
        "the first half of a spaced name must resolve too"
    );
}

/// Joining adjacent words must not turn prose into a click target: every span
/// is probed against the disk, so a sentence naming nothing stays inert
/// (#2846).
#[test]
fn prose_with_spaces_stays_inert() {
    let box_ = Sandbox::new("prose", &["notes.md"]);
    let line = "the quick brown fox jumps over the lazy dog";

    for col in [0usize, 5, 11, 17] {
        assert_eq!(
            target_at_roots(line, col, &[box_.dir.as_path()], None),
            None,
            "no span of this sentence names a file, so column {col} must be inert"
        );
    }

    // The same line with a real name in it still resolves, so the guard above
    // is not simply refusing everything.
    let with_file = "the quick notes.md over the lazy dog";
    let col = with_file.find("notes.md").expect("fixture word");
    assert_eq!(
        target_at_roots(with_file, col, &[box_.dir.as_path()], None),
        Some(ClickTarget::File(box_.dir.join("notes.md")))
    );
}

/// A markdown destination resolves through the same roots (#2846), so a link
/// to a local file opens from the session directory too.
#[test]
fn a_link_destination_uses_the_same_roots() {
    let box_ = Sandbox::new("link-roots", &["docs/report.md"]);
    let text = "report";
    let links = vec![(0..text.len(), "docs/report.md".to_string())];

    assert_eq!(
        target_for_click_roots(text, &links, 1, &[box_.dir.as_path()], None),
        Some(ClickTarget::File(box_.dir.join("docs/report.md")))
    );
}

/// The window and the terminal both hand the resolver a directory (#2846).
///
/// Neither surface can reach a session from `ui_core`, so the wiring lives in
/// their own sources: the window writes the roots onto the drag state and
/// snapshots them at press, and the terminal passes its working directory.
#[test]
fn both_surfaces_pass_the_session_directory() {
    let transcript = flat(TRANSCRIPT);
    assert!(
        transcript.contains("links::target_for_click_roots"),
        "the window's press must resolve through the roots form"
    );
    assert!(
        transcript.contains("roots:pressed.roots_snapshot()"),
        "the press must snapshot the roots so a release cannot resolve against a moved base"
    );

    let render = flat(MAIN);
    assert!(
        render.contains("copy_drag.set_roots("),
        "the window must hand its session directory to the drag state each frame"
    );

    let terminal = flat(include_str!("../tui/app/clickable.rs"));
    assert!(
        terminal.contains("links::target_at_roots"),
        "the terminal must resolve through the roots form too"
    );
}

/// A click miss logs the token the resolver tried, not the text trailing the
/// pointer (#3085).
///
/// `PendingClick::word` started at the click offset and ran to the next break,
/// so a press in the middle of `brain-audit-2026-11-17.md` logged
/// `26-11-17.md`, a string that never existed as a candidate. The log now asks
/// the resolver's own span walker.
#[test]
fn a_miss_names_the_whole_token_the_pointer_sat_inside() {
    let text = "see brain-audit-2026-11-17.md for the details";
    let middle = text.find("2026").expect("fixture year") + 2;

    assert_eq!(
        token_at_byte(text, middle),
        Some("brain-audit-2026-11-17.md"),
        "the logged name must be the token, not the tail from the pointer"
    );
    assert_eq!(
        token_at_byte(text, text.find("brain").expect("fixture name")),
        Some("brain-audit-2026-11-17.md"),
        "a press on the first character is the same token"
    );
    assert_eq!(
        token_at_byte(text, text.len()),
        None,
        "a press past the end names nothing rather than inventing a token"
    );
    assert_eq!(
        token_at_byte("a row   with  gaps", 7),
        None,
        "a press on whitespace names nothing"
    );
}

/// The byte offset a shaped layout reports can land inside a multibyte
/// character; the log must not slice it apart (#2271).
#[test]
fn the_miss_log_survives_a_multibyte_offset() {
    let text = "the report is café-notes.md today";
    let token = "café-notes.md";
    let start = text.find(token).expect("fixture token");
    let inside_e_acute = start + "caf".len() + 1;

    assert_eq!(
        token_at_byte(text, inside_e_acute),
        Some(token),
        "an offset inside a character rounds down to that character's start"
    );
}

/// The window's miss arm writes the token and the offset, and never calls the
/// old tail-only helper (#3085).
///
/// The source is flattened before the scan, so every pattern below is written
/// without whitespace. The earlier form of this test asserted a spaced string
/// against the flattened text, which no source can ever contain.
#[test]
fn the_miss_line_reports_the_token_and_the_offset() {
    let open = flat(TRANSCRIPT);
    assert!(
        open.contains("transcriptlink:nothingtoopenat{token:?}(offset{})"),
        "the miss line must carry the offset so a log can be matched to a row"
    );
    assert!(
        open.contains("pending.token()"),
        "the miss line must name what the resolver tried"
    );
    assert!(
        !open.contains("pending.word()") && !open.contains("fnword(&self)"),
        "the tail-only helper must be gone, not left beside the fix"
    );
}

/// A click on ordinary prose named no file and no url, so the log must stay
/// silent for it. A token the resolver would have probed still reaches the
/// log, so a real miss is never hidden by the gate (#3085).
#[test]
fn prose_clicks_name_nothing_and_stay_out_of_the_log() {
    for word in ["bisa", "etap", "the", "today", "and"] {
        assert!(
            !click_names_target(word, 1),
            "{word} is prose: the resolver would never probe it, so it is not a miss"
        );
    }
    assert!(
        !click_names_target("a row   with  gaps", 7),
        "a press on whitespace names nothing"
    );
    assert!(
        !click_names_target("", 0),
        "an empty row names nothing and must not panic"
    );
    for named in [
        "src/gui/transcript.rs",
        "README.md",
        "café-notes.md",
        "https://example.com/docs",
        "/usr/local/bin",
        "~/srv/rs/opencrabs",
    ] {
        assert!(
            click_names_target(named, 2),
            "{named} names a target the resolver would probe, so a failed open must log"
        );
    }
}

/// The miss arm asks the gate before it writes anything, and the gate is the
/// resolver's own candidate test rather than a second copy of the rules
/// (#3085).
#[test]
fn the_miss_arm_asks_the_gate_before_it_writes() {
    let open = flat(TRANSCRIPT);
    assert!(
        open.contains("self.note_miss(&pending)"),
        "the miss arm must route through note_miss"
    );
    assert!(
        open.contains("links::click_names_target(&pending.text,pending.offset)"),
        "note_miss must ask the shared candidate test"
    );
}

/// One gesture reaches several elements, so the same token at the same offset
/// twice inside the window is one line, not two (#3085).
#[test]
fn one_gesture_writes_one_miss_line() {
    let open = flat(TRANSCRIPT);
    assert!(
        open.contains("Duration::from_millis(500)"),
        "the repeat window must be short enough to collapse one gesture"
    );
    assert!(
        open.contains("*last_token==token") && open.contains("*last_offset==pending.offset"),
        "a repeat is the same token at the same offset, not just any second miss"
    );
    assert!(
        open.contains("last_miss"),
        "the window needs the last miss it wrote"
    );
}
