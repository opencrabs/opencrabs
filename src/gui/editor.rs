//! The message editor. gpui 0.2.2 ships no text widget, so this implements
//! `EntityInputHandler` on a view and paints through a custom element, the
//! shape the crate's own `examples/input.rs` demonstrates. Adaptations for
//! the chat client: multiline content (one shaped line per `\n` row, the
//! split is a char operation), Enter sends via an `EditorEvent` the window
//! subscribes to, Shift+Enter inserts a newline, and Esc stays inert while
//! an IME composition is live so it can never back the UI out mid-typing.
//! IME correctness comes from the platform contract: every range crossing
//! the boundary goes through the utf16 <-> utf8 converters, and cursor
//! movement respects grapheme boundaries (`unicode-segmentation`, already a
//! direct dependency).

use crate::palette;
use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, Div, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, PathPromptOptions, Pixels, Point, ShapedLine, SharedString, Style,
    TextRun, UTF16Selection, Window, actions, div, fill, img, point, prelude::*, px, relative, rgb,
    rgba, size,
};

use unicode_segmentation::UnicodeSegmentation;

actions!(
    editor,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Send,
        Newline,
    ]
);

/// u32 theme constants to the `Hsla` the text runs take.
fn hsla_from(rgb_u32: u32) -> gpui::Hsla {
    gpui::rgb(rgb_u32).into()
}

/// Image suffixes, the same list the TUI classifies with
/// (`tui/app/state.rs`, whose comment makes `utils::file_extract` the source
/// of truth). A file is the same kind of thing in both clients.
const IMAGE_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".jfif", ".gif", ".webp", ".bmp", ".svg", ".heic", ".heif", ".avif",
    ".ico", ".tiff", ".tif",
];

/// Video suffixes, mirroring the TUI's list so `<<VID:>>` means the same
/// thing in both clients and the agent calls `analyze_video`.
const VIDEO_EXTENSIONS: &[&str] = &[
    ".mp4", ".m4v", ".mov", ".webm", ".mkv", ".avi", ".3gp", ".flv",
];

/// A file waiting to be sent with the message: the GUI's twin of the TUI's
/// `ImageAttachment`. `is_video` picks the marker the send pipeline appends,
/// exactly like `tui/app/input.rs` does, because that marker is what tells
/// the agent which analysis tool to reach for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Display name (file name), what the chip shows.
    pub name: String,
    /// Full path on this machine, what the marker carries.
    pub path: PathBuf,
    /// Video: emit `<<VID:>>` so the agent calls `analyze_video`.
    pub is_video: bool,
}

impl Attachment {
    /// Classify one path. `None` when the path is not a file that exists: an
    /// attachment pointing nowhere would be a chip that lies, and the agent
    /// would burn a turn failing to read it.
    pub fn from_path(path: PathBuf) -> Option<Self> {
        if !path.is_file() {
            return None;
        }
        Some(Self::for_display(path))
    }

    /// Classify a path that came out of a STORED message. Unlike `from_path`
    /// this does not require the file to still be on disk: the message is the
    /// record that the file was sent, and a thumbnail that disappeared
    /// because the user moved it later would rewrite the history.
    pub fn for_display(path: PathBuf) -> Self {
        let lower = path.to_string_lossy().to_lowercase();
        let is_video = VIDEO_EXTENSIONS.iter().any(|ext| lower.ends_with(ext));
        Self {
            name: display_name(&path),
            path,
            is_video,
        }
    }

    /// Images get a real thumbnail; every other kind gets a glyph tile. A
    /// video frame could be extracted, but a broken frame would misrepresent
    /// the file, so video shows its kind instead.
    pub fn is_image(&self) -> bool {
        let lower = self.path.to_string_lossy().to_lowercase();
        IMAGE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
    }

    /// The marker the agent reads. Same shape the TUI writes: `<<IMG:path>>`
    /// for images and every non-video file, `<<VID:path>>` for videos.
    pub fn marker(&self) -> String {
        let prefix = if self.is_video { "VID" } else { "IMG" };
        format!("<<{prefix}:{}>>", self.path.display())
    }
}

/// The text that actually goes to the agent: what was typed plus one marker
/// per attachment, the format `tui/app/input.rs` sends. Attachments without
/// typed text still send, because the markers are the message.
fn message_with_markers(text: &str, attachments: &[Attachment]) -> String {
    let mut payload = text.trim().to_string();
    for attachment in attachments {
        if !payload.is_empty() {
            payload.push(' ');
        }
        payload.push_str(&attachment.marker());
    }
    payload
}

/// The paths carried by `<<IMG:...>>` / `<<VID:...>>` markers in a message.
/// Pure string work so it is testable without a window, and shared with the
/// send path that tracks each attachment against the session.
pub fn marker_paths(text: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<<") {
        let Some(after) = rest.get(start + 2..) else {
            break;
        };
        let Some(end) = after.find(">>") else {
            break;
        };
        let Some(marker) = after.get(..end) else {
            break;
        };
        if let Some((kind, path)) = marker.split_once(':')
            && (kind == "IMG" || kind == "VID")
            && !path.is_empty()
        {
            paths.push(PathBuf::from(path));
        }
        rest = after.get(end + 2..).unwrap_or_default();
    }
    paths
}

/// The message text without its `<<IMG:...>>` / `<<VID:...>>` markers. Those
/// files are drawn as thumbnails, so leaving the raw marker in the bubble
/// would print the same attachment twice, once as pixels and once as text.
pub fn strip_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<<") {
        let Some(after) = rest.get(start + 2..) else {
            break;
        };
        let Some(end) = after.find(">>") else { break };
        let Some(marker) = after.get(..end) else {
            break;
        };
        let is_marker = marker
            .split_once(':')
            .is_some_and(|(kind, path)| (kind == "IMG" || kind == "VID") && !path.is_empty());
        if is_marker {
            if let Some(head) = rest.get(..start) {
                out.push_str(head);
            }
        } else if let Some(keep) = rest.get(..start + 2 + end + 2) {
            out.push_str(keep);
        }
        rest = after.get(end + 2..).unwrap_or_default();
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// The file name of a path, for chips built from stored messages.
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// What the editor tells the window: a completed message ready to send.
pub enum EditorEvent {
    Sent(String),
}

/// The editor view. Ranges internal to this struct are utf8; every value
/// crossing the `EntityInputHandler` boundary is utf16, per the contract.
pub struct Editor {
    pub focus_handle: FocusHandle,
    pub content: String,
    /// Files attached to the next message. They live here, not in the window
    /// state, because the send action is what consumes them: the editor is
    /// the only place that can append the markers atomically with clearing
    /// the text.
    attachments: Vec<Attachment>,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Vec<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    last_line_height: Pixels,
    is_selecting: bool,
}

impl Editor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            attachments: Vec::new(),
            placeholder: "Message OpenCrabs…".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: Vec::new(),
            last_bounds: None,
            last_line_height: px(20.0),
            is_selecting: false,
        }
    }

    // Attachments ----------------------------------------------------------

    /// Attach every path that is a real file, skipping duplicates. Returns how
    /// many landed so the caller can decide whether a repaint is needed.
    pub fn attach_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        let mut added = 0;
        for path in paths {
            let Some(attachment) = Attachment::from_path(path) else {
                continue;
            };
            if self
                .attachments
                .iter()
                .any(|existing| existing.path == attachment.path)
            {
                continue;
            }
            self.attachments.push(attachment);
            added += 1;
        }
        added
    }

    fn take_attachments(&mut self) -> Vec<Attachment> {
        std::mem::take(&mut self.attachments)
    }

    fn remove_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.attachments.len() {
            self.attachments.remove(index);
            cx.notify();
        }
    }

    /// The paperclip: the platform's own file picker, then the same attach
    /// path a Finder drop takes. Cancelled means nothing was attached, so
    /// nothing is announced.
    fn pick_paths(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach files".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            this.update(cx, |editor, cx| {
                if editor.attach_paths(paths) > 0 {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    // Movement and editing -------------------------------------------------

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    /// Vertical movement in a multiline field: same column, row above or
    /// below, clamped to that row's length.
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let (line, col) = self.line_col(self.cursor_offset());
        if line == 0 {
            return;
        }
        let target = self.line_start(line - 1) + col.min(self.line_len(line - 1));
        self.move_to(target, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let (line, col) = self.line_col(self.cursor_offset());
        if line + 1 >= self.line_count() {
            return;
        }
        let target = self.line_start(line + 1) + col.min(self.line_len(line + 1));
        self.move_to(target, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start(self.line_col(self.cursor_offset()).0), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_col(self.cursor_offset()).0;
        self.move_to(self.line_start(line) + self.line_len(line), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx)
    }

    /// Enter sends: non-empty content goes out as an event, the field clears.
    fn send(&mut self, _: &Send, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.content.trim().to_string();
        let attachments = self.take_attachments();
        if text.is_empty() && attachments.is_empty() {
            return;
        }
        let payload = message_with_markers(&text, &attachments);
        self.content.clear();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
        cx.emit(EditorEvent::Sent(payload));
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content
                    .get(self.selected_range.clone())
                    .unwrap_or_default()
                    .to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content
                    .get(self.selected_range.clone())
                    .unwrap_or_default()
                    .to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    // Mouse ----------------------------------------------------------------

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    /// Esc while an IME composition is live: inert. The composition belongs
    /// to the platform input session, and backing the UI out mid-composition
    /// is exactly the phase-0 vanish class of surprise. Consumed with
    /// `stop_propagation` so no ancestor acts on it either.
    fn intercept_raw_key(
        &mut self,
        ev: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ev.keystroke.key == "escape" && self.marked_range.is_some() {
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    // Line math --------------------------------------------------------------

    fn line_count(&self) -> usize {
        line_count(&self.content)
    }

    /// Utf8 offset where line `line` starts (line 0 starts at 0; every later
    /// line starts after its preceding `\n`).
    fn line_start(&self, line: usize) -> usize {
        line_start(&self.content, line)
    }

    fn line_len(&self, line: usize) -> usize {
        line_len(&self.content, line)
    }

    /// (line, utf8 column) for a utf8 offset.
    fn line_col(&self, offset: usize) -> (usize, usize) {
        line_col(&self.content, offset)
    }

    // Selection helpers ------------------------------------------------------

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(bounds) = self.last_bounds.as_ref() else {
            return 0;
        };
        let line_height = self.last_line_height;
        let rel_y = position.y - bounds.top();
        let line = (rel_y / line_height) as usize;
        let line = line.min(self.line_count() - 1);
        let Some(shaped) = self.last_layout.get(line) else {
            return 0;
        };
        self.line_start(line) + shaped.closest_index_for_x(position.x - bounds.left())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    // Boundaries and conversions ---------------------------------------------

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content.get(range).unwrap_or_default().to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content = self
            .content
            .get(..range.start)
            .unwrap_or_default()
            .to_owned()
            + new_text
            + self.content.get(range.end..).unwrap_or_default();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content = self
            .content
            .get(..range.start)
            .unwrap_or_default()
            .to_owned()
            + new_text
            + self.content.get(range.end..).unwrap_or_default();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.notify();
    }

    /// The IME candidate window's anchor: the rect of a utf16 range. Mapped
    /// onto the row the range starts on, which is where candidates belong.
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let (line, col_start) = self.line_col(range.start);
        let col_end = if range.start == range.end {
            col_start
        } else {
            self.line_col(range.end).1
        };
        let shaped = self.last_layout.get(line)?;
        let y = bounds.top() + self.last_line_height * line as f32;
        Some(Bounds::from_corners(
            point(bounds.left() + shaped.x_for_index(col_start), y),
            point(
                bounds.left() + shaped.x_for_index(col_end),
                y + self.last_line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let rel_y = point.y - bounds.top();
        let line = ((rel_y / self.last_line_height) as usize).min(self.line_count() - 1);
        let shaped = self.last_layout.get(line)?;
        let utf8 = shaped.index_for_x(point.x - bounds.left())?;
        Some(self.offset_to_utf16(self.line_start(line) + utf8))
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<EditorEvent> for Editor {}

/// One painted text row: the shaped line plus, optionally, that row's
/// selection rect and cursor quad.
struct RowPaint {
    line: ShapedLine,
    selection: Option<Pixels>,
    selection_end: Option<Pixels>,
    cursor_x: Option<Pixels>,
}

/// The custom element: shapes every row during prepaint, paints rows and
/// quads during paint, and registers the platform input handler so typing,
/// IME and paste all land in the `Editor`.
struct EditorElement {
    input: Entity<Editor>,
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

struct EditorPrepaint {
    rows: Vec<RowPaint>,
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = EditorPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let line_count = self.input.read(cx).content.split('\n').count().max(1);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * line_count as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());

        let empty = content.is_empty();
        let display = if empty {
            input.placeholder.to_string()
        } else {
            content.clone()
        };

        let mut rows = Vec::new();
        let mut line_start = 0usize;
        for text in display.split('\n') {
            let line_end = line_start + text.len();
            let run_color = if empty {
                hsla_from(palette::current().text_faint)
            } else {
                style.color
            };
            let run = TextRun {
                len: text.len(),
                font: style.font(),
                color: run_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(
                SharedString::from(text.to_string()),
                font_size,
                &[run],
                None,
            );
            // Selection and cursor map onto display rows only when the field
            // has real content; the placeholder paints bare.
            let (sel, sel_end, cursor_x) = if empty {
                (None, None, None)
            } else {
                let sel_start_in_line = selected.start.clamp(line_start, line_end) - line_start;
                let sel_end_in_line = selected.end.clamp(line_start, line_end) - line_start;
                let has_selection_here =
                    !selected.is_empty() && selected.start < line_end && selected.end > line_start;
                let cursor_here = cursor >= line_start && cursor <= line_end;
                (
                    has_selection_here.then(|| shaped.x_for_index(sel_start_in_line)),
                    has_selection_here.then(|| shaped.x_for_index(sel_end_in_line)),
                    (cursor_here && selected.is_empty())
                        .then(|| shaped.x_for_index(cursor - line_start)),
                )
            };
            rows.push(RowPaint {
                line: shaped,
                selection: sel,
                selection_end: sel_end,
                cursor_x,
            });
            line_start = line_end + 1; // past the '\n'
        }

        EditorPrepaint { rows }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let line_height = window.line_height();
        let focused = focus_handle.is_focused(window);
        let rows = std::mem::take(&mut prepaint.rows);
        for (row_idx, row) in rows.iter().enumerate() {
            let y = bounds.top() + line_height * row_idx as f32;
            if let (Some(sel), Some(sel_end)) = (row.selection, row.selection_end) {
                window.paint_quad(fill(
                    Bounds::from_corners(
                        point(bounds.left() + sel.min(sel_end), y),
                        point(bounds.left() + sel.max(sel_end), y + line_height),
                    ),
                    rgba(0x3311ff30),
                ));
            }
            row.line
                .paint(
                    bounds.origin + point(px(0.), line_height * row_idx as f32),
                    line_height,
                    window,
                    cx,
                )
                .unwrap();
            if focused && let Some(cursor_x) = row.cursor_x {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_x, y),
                        size(px(2.), line_height),
                    ),
                    gpui::blue(),
                ));
            }
        }
        self.input.update(cx, |input, _cx| {
            input.last_layout = rows.iter().map(|r| r.line.clone()).collect();
            input.last_bounds = Some(bounds);
            input.last_line_height = line_height;
        });
    }
}

impl Render for Editor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Chips above the text, controls below it: the input stays the
        // biggest thing in the box and the attachment row never pushes the
        // caret around.
        let mut body = div().flex().flex_col().gap_2();
        if !self.attachments.is_empty() {
            body = body.child(attachment_strip(&self.attachments, cx));
        }
        body = body.child(EditorElement { input: cx.entity() }).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(paperclip(cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_faint))
                        .child("drop files to attach"),
                ),
        );
        div()
            .flex()
            .flex_col()
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::intercept_raw_key))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(Self::newline))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            // One step above the chat, with a shadow, so the input reads as a
            // surface of its own in both themes.
            .bg(rgb(palette::current().panel))
            .shadow_sm()
            .border_1()
            .border_color(rgb(palette::current().panel_hi))
            .rounded_sm()
            .p_2()
            .line_height(px(22.))
            .text_size(px(14.))
            .text_color(rgb(palette::current().text))
            .min_h(px(44.))
            .child(body)
    }
}

/// The pending attachments as chips. Images show the real file through
/// `img` (gpui loads it from disk, so a broken path shows nothing rather
/// than a fake tile); videos and every other file get a glyph plus the name.
fn attachment_strip(attachments: &[Attachment], cx: &mut Context<Editor>) -> Div {
    let mut strip = div().flex().flex_row().flex_wrap().gap_2();
    for (index, attachment) in attachments.iter().enumerate() {
        let remove = cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
            // The ✕ must not also reach the editor: a click there would move
            // the caret and start a selection while the user deletes a chip.
            cx.stop_propagation();
            this.remove_attachment(index, cx);
        });
        let tile: AnyElement = if attachment.is_image() {
            img(attachment.path.clone())
                .w(px(34.0))
                .h(px(34.0))
                .rounded(px(4.0))
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(34.0))
                .h(px(34.0))
                .rounded(px(4.0))
                .bg(rgb(palette::current().bg))
                .child(if attachment.is_video { "🎬" } else { "📄" })
                .into_any_element()
        };
        strip = strip.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .p_1()
                .rounded(px(6.0))
                .bg(rgb(palette::current().panel_hi))
                .child(tile)
                .child(
                    div()
                        .max_w(px(160.0))
                        .truncate()
                        .text_xs()
                        .text_color(rgb(palette::current().text))
                        .child(SharedString::from(attachment.name.clone())),
                )
                .child(
                    div()
                        .id(ElementId::Name(format!("attachment-remove-{index}").into()))
                        .cursor_pointer()
                        .text_xs()
                        .text_color(rgb(palette::current().text_faint))
                        .hover(|d| d.text_color(rgb(palette::current().accent)))
                        .child("✕")
                        .on_mouse_down(MouseButton::Left, remove),
                ),
        );
    }
    strip
}

/// The paperclip inside the input: opens the platform file picker, then takes
/// the same attach path a Finder drop does.
fn paperclip(cx: &mut Context<Editor>) -> impl IntoElement {
    div()
        .id("editor-attach")
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_xs()
        .text_color(rgb(palette::current().text_faint))
        .hover(|d| d.text_color(rgb(palette::current().accent)))
        .child("📎")
        .child("attach")
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                this.pick_paths(cx);
            }),
        )
}

/// The keybindings the editor needs, registered once on the App. They are
/// global because the editor is the only `on_action` provider for them: an
/// unmatched action with no focused listener is a no-op elsewhere.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, None),
        KeyBinding::new("delete", Delete, None),
        KeyBinding::new("left", Left, None),
        KeyBinding::new("right", Right, None),
        KeyBinding::new("up", Up, None),
        KeyBinding::new("down", Down, None),
        KeyBinding::new("shift-left", SelectLeft, None),
        KeyBinding::new("shift-right", SelectRight, None),
        KeyBinding::new("cmd-a", SelectAll, None),
        KeyBinding::new("cmd-v", Paste, None),
        KeyBinding::new("cmd-c", Copy, None),
        KeyBinding::new("cmd-x", Cut, None),
        KeyBinding::new("home", Home, None),
        KeyBinding::new("end", End, None),
        KeyBinding::new("enter", Send, None),
        KeyBinding::new("shift-enter", Newline, None),
    ]);
}

// Line math, free over `&str` ------------------------------------------------
//
// These carry the whole multiline model (movement, Home/End, mouse mapping,
// IME range math), and they are pure string arithmetic. They live outside the
// struct so unit tests can seat them without a gpui app context, which no
// headless test can build.

fn line_count(content: &str) -> usize {
    content.split('\n').count()
}

/// Utf8 offset where line `line` starts (line 0 starts at 0; every later
/// line starts after its preceding `\n`).
fn line_start(content: &str, line: usize) -> usize {
    content.split('\n').take(line).map(|l| l.len() + 1).sum()
}

fn line_len(content: &str, line: usize) -> usize {
    content.split('\n').nth(line).map(str::len).unwrap_or(0)
}

/// (line, utf8 column) for a utf8 offset. Past-the-end offsets clamp to the
/// last line's end rather than panicking: a stale offset must never take the
/// window down.
fn line_col(content: &str, offset: usize) -> (usize, usize) {
    let mut consumed = 0usize;
    for (line, text) in content.split('\n').enumerate() {
        let line_end = consumed + text.len();
        if offset <= line_end {
            return (line, offset - consumed);
        }
        consumed = line_end + 1;
    }
    let last = line_count(content).saturating_sub(1);
    (last, line_len(content, last))
}

#[cfg(test)]
mod tests {
    use super::{
        Attachment, line_col, line_count, line_len, line_start, marker_paths, message_with_markers,
        strip_markers,
    };
    use std::path::PathBuf;

    /// A throwaway directory with real files in it: the classifier stats the
    /// path, so a made-up path would prove nothing about what a drop attaches.
    fn scratch_dir(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "opencrabs-gui-attach-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Real files only: a missing path must not become a chip that lies.
    #[test]
    fn attachments_classify_real_files_and_skip_duplicates() {
        let dir = scratch_dir("classify");
        let png = dir.join("shot.png");
        let mp4 = dir.join("clip.mp4");
        let pdf = dir.join("spec.pdf");
        std::fs::write(&png, b"png").unwrap();
        std::fs::write(&mp4, b"mp4").unwrap();
        std::fs::write(&pdf, b"pdf").unwrap();

        let image = Attachment::from_path(png.clone()).expect("png attaches");
        assert!(image.is_image() && !image.is_video, "{image:?}");
        assert_eq!(image.name, "shot.png");
        assert_eq!(image.marker(), format!("<<IMG:{}>>", png.display()));

        let video = Attachment::from_path(mp4.clone()).expect("mp4 attaches");
        assert!(video.is_video && !video.is_image(), "{video:?}");
        assert_eq!(video.marker(), format!("<<VID:{}>>", mp4.display()));

        let document = Attachment::from_path(pdf.clone()).expect("pdf attaches");
        assert!(!document.is_image() && !document.is_video, "{document:?}");

        // Anything that is not a file on disk never attaches.
        assert!(Attachment::from_path(dir.join("nope.png")).is_none());
        assert!(
            Attachment::from_path(dir.clone()).is_none(),
            "a directory is not an attachment"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The payload the agent receives carries one marker per attachment, in
    /// the same shape the TUI writes, and the paths parse back out of it.
    #[test]
    fn markers_ride_on_the_payload_and_parse_back() {
        let image = Attachment {
            name: "a b.png".into(),
            path: PathBuf::from("/fixtures/a b.png"),
            is_video: false,
        };
        let video = Attachment {
            name: "c.mov".into(),
            path: PathBuf::from("/fixtures/c.mov"),
            is_video: true,
        };

        let with_text = message_with_markers("  look at this  ", &[image.clone(), video.clone()]);
        assert_eq!(
            with_text,
            "look at this <<IMG:/fixtures/a b.png>> <<VID:/fixtures/c.mov>>"
        );
        assert_eq!(
            marker_paths(&with_text),
            vec![
                PathBuf::from("/fixtures/a b.png"),
                PathBuf::from("/fixtures/c.mov")
            ]
        );

        // Attachments alone send: the markers are the message, and there is
        // no leading space to trip the trim.
        let alone = message_with_markers("   ", std::slice::from_ref(&video));
        assert_eq!(alone, "<<VID:/fixtures/c.mov>>");
        assert_eq!(marker_paths(&alone), vec![PathBuf::from("/fixtures/c.mov")]);

        // Plain prose and unknown markers are not attachments.
        assert!(marker_paths("just words, <<FILE:/tmp/x>> and <<IMG:>>").is_empty());
    }

    #[test]
    fn strip_markers_removes_only_attachments() {
        assert_eq!(
            strip_markers("look <<IMG:/tmp/a.png>> here <<VID:/tmp/b.mov>>"),
            "look  here"
        );
        assert_eq!(strip_markers("<<IMG:/tmp/a.png>>"), "");
        // An unknown marker is ordinary text and stays put.
        assert_eq!(
            strip_markers("keep <<FILE:/tmp/x>> but not <<IMG:/tmp/y.png>>"),
            "keep <<FILE:/tmp/x>> but not"
        );
        assert_eq!(strip_markers("no markers at all"), "no markers at all");
        assert_eq!(strip_markers("<<IMG:>>"), "<<IMG:>>");
    }

    #[test]
    fn for_display_keeps_a_stored_path_that_is_gone() {
        // A message outlives the file it pointed at; the chip must not vanish.
        let gone = Attachment::for_display(PathBuf::from("/fixtures/definitely-not-here-9f2a.png"));
        assert_eq!(gone.name, "definitely-not-here-9f2a.png");
        assert!(gone.is_image());
        assert!(Attachment::from_path(PathBuf::from("/fixtures/nope")).is_none());
    }

    /// Every (content, offset) pair must resolve to a live line whose start +
    /// column stays inside the content. This is the off-by-one generator: any
    /// boundary mistake in the line model shows up here as either a panic or
    /// an offset that escapes the string.
    #[test]
    fn line_math_stays_in_bounds_for_every_offset() {
        let cases = [
            "",
            "\n",
            "a",
            "a\n",
            "a\nb",
            "a\n\nb",
            "\n\na",
            "one\ntwo\nthree",
            "é\n日本語\n🦀", // multibyte on every line: utf8 offsets, not chars
        ];
        for content in cases {
            let total = content.len();
            let count = line_count(content);
            assert!(count >= 1, "{content:?}: at least one line always exists");
            for offset in 0..=total {
                let (line, col) = line_col(content, offset);
                assert!(line < count, "{content:?} offset {offset}: line escapes");
                let start = line_start(content, line);
                let len = line_len(content, line);
                assert!(
                    start + len <= total,
                    "{content:?} offset {offset}: line {line} span escapes"
                );
                // Round trip: the offset lands on the line it maps to, inside
                // that line's span.
                assert!(
                    col <= len,
                    "{content:?} offset {offset}: column {col} past line {line} len {len}"
                );
                let line_range = start..start + len;
                let line_bytes = &content.as_bytes()[line_range];
                assert!(std::str::from_utf8(line_bytes).is_ok());
            }
        }
    }

    #[test]
    fn line_boundaries_pin_the_exact_contracts() {
        // Empty content is one empty line, and offsets 0 and 1 both land on it
        // (1 is past-the-end and must clamp, not panic).
        assert_eq!(line_count(""), 1);
        assert_eq!(line_col("", 0), (0, 0));
        assert_eq!(line_col("", 1), (0, 0));
        // "a\n\nb": a middle empty line, a trailing short line.
        assert_eq!(line_count("a\n\nb"), 3);
        assert_eq!(line_start("a\n\nb", 2), 3);
        assert_eq!(line_len("a\n\nb", 1), 0);
        assert_eq!(line_col("a\n\nb", 3), (2, 0));
        // Trailing newline: an empty last line that Home/End must reach.
        assert_eq!(line_start("ab\n", 1), 3);
        assert_eq!(line_len("ab\n", 1), 0);
        assert_eq!(line_col("ab\n", 3), (1, 0));
        // Multibyte content counts utf8 bytes: é is 2, each CJK char is 3.
        assert_eq!(line_start("é\n日", 1), 3);
        assert_eq!(line_len("é\n日", 1), 3);
    }
}
