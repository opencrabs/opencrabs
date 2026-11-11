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
use crate::topic_icons::CRAB_ICONS;
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};
use opencrabs::ui_core::commands::completions;
use opencrabs::ui_core::composer::{self, ComposerDraft, SharedHistory, lock_history};
use opencrabs::ui_core::input_history::{
    input_history_down, input_history_up, record_input_history,
};
use opencrabs::ui_core::palette::{next_word_end, previous_word_start};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardEntry, ClipboardItem, Context, Corners,
    CursorStyle, Div, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, Font, GlobalElementId, ImageFormat, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point,
    RenderImage, ScrollHandle, SharedString, Style, TextAlign, TextRun, UTF16Selection, Window,
    WrappedLine, actions, div, fill, point, prelude::*, px, relative, rgb, rgba, size,
    transparent_black,
};

use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

actions!(
    editor,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        WordLeft,
        WordRight,
        DeleteWordBack,
        DeleteToLineStart,
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
        /// Tab on a half-typed slash command: fill the name, do not run it.
        AcceptCommand,
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

/// One row of the composer's `:query` list (#2769).
///
/// A glyph row inserts its character. A crab row is one of our own pictures and
/// inserts its shortcode instead: the token is what the transcript draws as
/// that picture, so the message carries an emoji as text and never a file
/// (#2771).
#[derive(Clone, Copy)]
pub struct EmojiRow {
    /// The character this row inserts. Empty for a crab, which inserts its
    /// shortcode in colons.
    pub glyph: &'static str,
    /// The shortcode shown and matched on, without its colons.
    pub shortcode: &'static str,
    /// The embedded PNG for a crab row.
    pub crab: Option<&'static [u8]>,
}

/// What accepting a row writes into the draft: the glyph as it is, or, for a
/// crab, its shortcode between colons (#2771).
///
/// The token is the emoji's text form. It is what the transcript reads back and
/// draws as the pack picture, so a crab never needs a file and the message
/// keeps its words around it.
fn emoji_insertion(row: &EmojiRow) -> String {
    match row.crab {
        Some(_) => format!(":{}:", row.shortcode),
        None => row.glyph.to_string(),
    }
}

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
    /// A chip's thumbnail was clicked. The editor does not own a preview
    /// overlay, so it hands the path to whoever does instead of opening one.
    Preview(std::path::PathBuf),
}

/// A queue recall the composer is holding: every message Up took out of one
/// session's queue, oldest first, and the draft the recall covered. Taking them
/// is the point: a message the user pulled out to edit must not still be queued,
/// or the drain would send the original while they edit a copy (#2637).
struct Recall {
    session: Uuid,
    /// Oldest first, the order they were written; `join` shows them the same
    /// way. Empty only between the take and the assignment.
    entries: Vec<crate::agent_bridge::PendingMessage>,
    draft: String,
}

/// The editor view. Ranges internal to this struct are utf8; every value
/// crossing the `EntityInputHandler` boundary is utf16, per the contract.
pub struct Editor {
    pub focus_handle: FocusHandle,
    /// A request to put the caret back in this field once a frame with a
    /// window is drawing it (#2665). The platform's file dialog takes the
    /// keyboard away, and the callback that runs after it closes has no
    /// window to focus through, so the request waits for `render`. The
    /// window also arms it after a press that hands the root the keys, so the
    /// field the user is about to type into reclaims the caret (#2741).
    pub(crate) focus_pending: bool,
    pub content: String,
    /// Submitted messages, shared by the window and every split pane. One list
    /// means a message sent in one composer is recalled in all of them, and
    /// the terminal reads the same list off the same file.
    history: SharedHistory,
    /// Where `history` is persisted, so a restart keeps it.
    history_path: PathBuf,
    /// Position in the shared list and the draft a recall stashed. These stay
    /// per composer: two fields can walk the same list at different places.
    input_history_index: Option<usize>,
    input_history_stash: String,
    /// The queued messages Up pulled out of the queue to edit, newest taken
    /// first, and the draft the walk covered. The last entry is what the field
    /// shows. They are OUT of the queue while they are here, so a recall the
    /// user decides not to send is never sent behind their back (#2637).
    recall: Option<Recall>,
    /// Files attached to the next message. They live here, not in the window
    /// state, because the send action is what consumes them: the editor is
    /// the only place that can append the markers atomically with clearing
    /// the text.
    attachments: Vec<Attachment>,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// The shaped lines the last frame painted, shared with the prepaint
    /// state instead of copied: a deep clone of every glyph is the most
    /// expensive thing a frame could do twice (#2688).
    last_layout: Arc<Vec<WrappedLine>>,
    /// (top, height) of every logical line once wrapped, in field-local
    /// pixels. Mouse mapping needs the row's own origin, not the field's.
    last_tops: Vec<(Pixels, Pixels)>,
    last_bounds: Option<Bounds<Pixels>>,
    last_line_height: Pixels,
    /// Width the field was last laid out at. `request_layout` has to answer a
    /// height before it is told its own bounds, so it measures with the width
    /// the previous frame recorded.
    wrap_width: Pixels,
    is_selecting: bool,
    /// Where the arrows stand in the slash-command list. The list itself is
    /// derived from the field's own text, so only the position is remembered;
    /// `command_popup` clamps it when the list shrank.
    command_index: usize,
    /// Set by Escape. The list stays closed while the draft is unchanged and
    /// the first edit opens it again: without this, dismissing a popup whose
    /// content is still a bare command word would be impossible.
    command_dismissed: bool,
    /// The emoji list under a `:query`: the row the arrows stand on, and whether
    /// Esc closed it for this draft (#2413).
    emoji_index: usize,
    emoji_dismissed: bool,
    /// The line the composer shows for a moment after an action with nothing
    /// to look at, like a copy (#2587). Timed, never sticky.
    notice: Option<Notice>,
    /// Bumped on every new notice so an older timer cannot clear a newer one.
    notice_generation: u64,
    /// Last shaped draft, reused while its key holds still (#2688).
    shaped_draft: Option<ShapedDraft>,
    /// The pack pictures the field has already decoded, by shortcode. A crab
    /// shortcode in the draft is painted as its picture, and decoding a PNG on
    /// every frame would cost more than the paint it feeds (#2794).
    crab_images: HashMap<&'static str, Arc<RenderImage>>,
    /// The box's own scroll position, so a draft taller than
    /// `COMPOSER_MAX_ROWS` moves under the field instead of painting over the
    /// pane below it (#2773).
    scroll: ScrollHandle,
    /// Where the caret wants the box scrolled, decided during prepaint and
    /// applied on the next frame. `None` means "leave the box where the user
    /// put it": this is how a wheel scroll is not yanked back by a caret blink.
    reveal: Option<Pixels>,
    /// (cursor offset, content length) the last reveal was computed for, so a
    /// repaint that moved neither leaves the scroll position alone.
    reveal_key: (usize, usize),
    /// The window point a right press opened the composer's menu at, or `None`
    /// while it is closed (#2749). Window coordinates, never field-local: the
    /// card is placed at the press, and the field's own origin would move it.
    ctx_menu_at: Option<Point<Pixels>>,
}

/// Rows the composer shows before it scrolls instead of growing. Ten rows is
/// about a third of a laptop window: enough to read a paragraph back, never
/// enough to cover the conversation behind it (#2773).
const COMPOSER_MAX_ROWS: usize = 10;

/// One transient line inside the composer (#2587). The terminal keeps the same
/// notice on the input border for two seconds, so the GUI says it where the
/// user is already looking instead of growing the transcript with chrome.
pub struct Notice {
    pub text: SharedString,
    pub shown_at: Instant,
}

/// How long a notice stays up, matching the terminal's `NOTIFICATION_TTL` so
/// the two surfaces keep the same rhythm.
pub const NOTICE_TTL: Duration = Duration::from_secs(2);

/// What the composer says once a dragged range reaches the clipboard (#2587).
/// The terminal prints these same words on its input border.
pub const COPIED_NOTICE: &str = "Copied to clipboard";

/// What the composer says when a stop before the first token handed the
/// message back (#2635). The text is in the field again, so the line does not
/// have to explain where it went.
pub const STOPPED_NOTICE: &str = "Stopped, message back in the composer";

/// True once a notice has outlived the TTL. Pure, so the expiry rule stays a
/// test instead of a wall-clock guess.
pub fn notice_expired(shown_at: Instant, now: Instant, ttl: Duration) -> bool {
    now.saturating_duration_since(shown_at) >= ttl
}

impl Editor {
    /// A composer walking `history`, persisting every new entry to
    /// `history_path`. Every composer takes this path: the window hands its
    /// own handle to each pane, so there is exactly one list on screen.
    pub fn new_with_history(
        history: SharedHistory,
        history_path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            focus_pending: false,
            content: String::new(),
            history,
            history_path,
            input_history_index: None,
            input_history_stash: String::new(),
            recall: None,
            attachments: Vec::new(),
            placeholder: "Message OpenCrabs…".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: Arc::new(Vec::new()),
            last_tops: Vec::new(),
            last_bounds: None,
            last_line_height: px(20.0),
            wrap_width: Pixels::ZERO,
            is_selecting: false,
            command_index: 0,
            command_dismissed: false,
            emoji_index: 0,
            emoji_dismissed: false,
            notice: None,
            notice_generation: 0,
            shaped_draft: None,
            crab_images: HashMap::new(),
            scroll: ScrollHandle::new(),
            reveal: None,
            reveal_key: (0, 0),
            ctx_menu_at: None,
        }
    }

    // Notices --------------------------------------------------------------

    /// Say one thing for a moment inside the composer. A newer notice replaces
    /// the old one and its timer: the generation check is what stops the first
    /// timer from clearing the second notice early.
    fn show_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(Notice {
            text: text.into(),
            shown_at: Instant::now(),
        });
        self.notice_generation = self.notice_generation.wrapping_add(1);
        let generation = self.notice_generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_TTL).await;
            let _ = this.update(cx, |editor, cx| {
                if editor.notice_generation == generation && editor.notice.is_some() {
                    editor.notice = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A stop took the message back: say it where the text just reappeared
    /// (#2635). Same transient line the copy receipt uses.
    pub fn show_returned(&mut self, cx: &mut Context<Self>) {
        self.show_notice(STOPPED_NOTICE, cx);
    }

    /// Put the text an abandoned turn was carrying back in the field, caret
    /// at the end, ready to edit (#2635). The caller only does this when the
    /// draft is empty, so nothing the user typed since is overwritten.
    pub fn restore_draft(&mut self, text: String, cx: &mut Context<Self>) {
        // A recall belongs to the queue, not to this hand-back: whatever it
        // still held goes back to wait there.
        self.give_back_recall(false);
        self.content = text;
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.command_dismissed = false;
        self.emoji_dismissed = false;
        cx.notify();
    }

    /// A drag-copied range reached the clipboard, so say so and let the line
    /// clear itself (#2587). Same words the terminal puts on the input border.
    pub fn show_copied(&mut self, cx: &mut Context<Self>) {
        self.show_notice(COPIED_NOTICE, cx);
    }

    /// The notice still inside its window, if any. Read through this so an
    /// expired notice never paints while its timer is still in flight.
    fn active_notice(&self, now: Instant) -> Option<&str> {
        let notice = self.notice.as_ref()?;
        (!notice_expired(notice.shown_at, now, NOTICE_TTL)).then(|| notice.text.as_ref())
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
                    // The dialog held the keyboard, so attaching is the
                    // moment to hand it back to the field the chips landed
                    // in (#2665).
                    editor.focus_pending = true;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The clipboard's image, when the clipboard holds a picture and no text
    /// (#2479). A screenshot carries no text, so image only is the same rule
    /// the TUI hint follows; an entry with text stays a text paste.
    pub fn clipboard_image(cx: &App) -> Option<(ImageFormat, Vec<u8>)> {
        let item = cx.read_from_clipboard()?;
        if item.text().is_some() {
            return None;
        }
        item.entries().iter().find_map(|entry| {
            let ClipboardEntry::Image(image) = entry else {
                return None;
            };
            Some((image.format, image.bytes.clone()))
        })
    }

    /// The slash-command list to draw above the field, or `None` when the field
    /// is not spelling a command. Carries the selection so the view does not
    /// have to know how the list is computed or where the arrows stand.
    ///
    /// Derived from the content, not cached: the field is the only thing that
    /// edits text, so a cache would need a refresh call at every one of those
    /// sites and one missed site means a stale popup over the wrong draft.
    pub fn command_popup(&self) -> Option<(Vec<&'static str>, usize)> {
        if self.command_dismissed {
            return None;
        }
        let names = completions(&self.content);
        if names.is_empty() {
            return None;
        }
        Some((names.clone(), self.command_index.min(names.len() - 1)))
    }

    /// Escape: close the list and keep the draft. The next edit reopens it,
    /// which is the terminal's behaviour too.
    pub fn dismiss_command_popup(&mut self, cx: &mut Context<Self>) {
        if self.command_popup().is_some() {
            self.command_dismissed = true;
            cx.notify();
        }
    }

    /// The picture for a crab, decoded once and kept (#2794).
    ///
    /// The field paints a crab as a picture rather than its `:name:`, so the
    /// decode has to happen somewhere on the paint path. Caching it here means
    /// the second frame of the same draft, and every frame after it, only
    /// clones an `Arc`.
    fn crab_image(&mut self, name: &'static str) -> Option<Arc<RenderImage>> {
        if let Some(image) = self.crab_images.get(name) {
            return Some(Arc::clone(image));
        }
        let image = crab_picture(crate::topic_icons::crab_icon(name)?)?;
        self.crab_images.insert(name, Arc::clone(&image));
        Some(image)
    }

    /// How many rows the `:query` list shows, the terminal's cap (#2413).
    const EMOJI_ROWS: usize = 8;

    /// The `:query` list for a query, our own pack first (#2769).
    ///
    /// The pack is a handful of pictures that only exist here, matched on the
    /// same shortcode the glyphs use (`:ok:`, `:bolt:`, `:fire:`). Leading with
    /// it is what makes it reachable: the eight-row cap buries anything that
    /// sorts behind the thousands of glyphs.
    fn emoji_rows(query: &str) -> Vec<EmojiRow> {
        let mut rows: Vec<EmojiRow> = CRAB_ICONS
            .iter()
            .copied()
            .filter(|(name, _)| name.contains(query))
            .map(|(name, bytes)| EmojiRow {
                glyph: "",
                shortcode: name,
                crab: Some(bytes),
            })
            .collect();
        // The name that was typed answers first: `:yes:` listed `eyes` above
        // `yes`, because both carry the query and the pack is ordered by file
        // name. The sort is stable, so the rest keep their order.
        rows.sort_by_key(|row| row.shortcode != query);
        let room = Self::EMOJI_ROWS.saturating_sub(rows.len());
        if room > 0 {
            rows.extend(
                emojis::iter()
                    .filter_map(|e| {
                        e.shortcodes()
                            .find(|sc| sc.contains(query))
                            .map(|sc| EmojiRow {
                                glyph: e.as_str(),
                                shortcode: sc,
                                crab: None,
                            })
                    })
                    .take(room),
            );
        }
        rows
    }

    /// The `:query` the caret sits in: the colon's byte offset and the query,
    /// lower cased. None when the text before the caret is not one.
    fn emoji_query(&self) -> Option<(usize, String)> {
        let before = self.content.get(..self.cursor_offset())?;
        let colon = before.rfind(':')?;
        let query = before.get(colon + 1..)?;
        if query.is_empty() || query.contains([' ', ':', '\n']) {
            return None;
        }
        Some((colon, query.to_lowercase()))
    }

    /// The emoji list for the `:query` under the caret, as the terminal shows
    /// it: up to eight rows with their shortcodes, and the highlighted row.
    /// Our own pack is part of the list, not a separate picker (#2769).
    pub fn emoji_popup(&self) -> Option<(Vec<EmojiRow>, usize)> {
        if self.emoji_dismissed {
            return None;
        }
        let (_, query) = self.emoji_query()?;
        let items = Self::emoji_rows(&query);
        if items.is_empty() {
            return None;
        }
        let index = self.emoji_index.min(items.len() - 1);
        Some((items, index))
    }

    /// Whether a list is open under the caret: a command or an emoji (#2413).
    pub fn list_open(&self) -> bool {
        self.command_popup().is_some() || self.emoji_popup().is_some()
    }

    /// Up and down walk the emoji list while it is open.
    fn step_emoji_popup(&mut self, delta: i64, cx: &mut Context<Self>) -> bool {
        let Some((items, index)) = self.emoji_popup() else {
            return false;
        };
        let last = items.len() as i64 - 1;
        let next = (index as i64 + delta).clamp(0, last) as usize;
        if next != self.emoji_index {
            self.emoji_index = next;
            cx.notify();
        }
        true
    }

    /// Replace `:query` with the highlighted row and keep typing after it. A
    /// glyph inserts its character; a crab inserts its shortcode, which the
    /// transcript draws as the pack picture, so the emoji travels as text and
    /// nothing is attached (#2771).
    fn accept_emoji(&mut self, cx: &mut Context<Self>) {
        let (Some((colon, _)), Some((items, index))) = (self.emoji_query(), self.emoji_popup())
        else {
            return;
        };
        let insert = emoji_insertion(&items[index]);
        let end = self.cursor_offset();
        self.content.replace_range(colon..end, insert.as_str());
        let caret = colon + insert.len();
        self.selected_range = caret..caret;
        self.selection_reversed = false;
        self.marked_range = None;
        self.emoji_index = 0;
        cx.notify();
    }

    /// Esc closes the emoji list and keeps the draft. The next edit reopens it.
    pub fn dismiss_emoji_popup(&mut self, cx: &mut Context<Self>) {
        if self.emoji_popup().is_some() {
            self.emoji_dismissed = true;
            cx.notify();
        }
    }

    /// Step the slash-command selection. Answers whether there was a list to
    /// step in, so the arrow handlers know whether to fall through to moving
    /// the caret: with a popup open, Up and Down walk names, not lines.
    fn step_command_popup(&mut self, delta: i64, cx: &mut Context<Self>) -> bool {
        let Some((names, index)) = self.command_popup() else {
            return false;
        };
        let last = names.len() as i64 - 1;
        let next = (index as i64 + delta).clamp(0, last) as usize;
        if next != self.command_index {
            self.command_index = next;
            cx.notify();
        }
        true
    }

    /// Tab on a half-typed command fills the name and stops there: the argument
    /// is the user's to type, and running on Tab would take that choice away.
    fn accept_command(&mut self, _: &AcceptCommand, _: &mut Window, cx: &mut Context<Self>) {
        // Tab with no list open is free: the window uses it to move focus (#2417).
        let Some((names, index)) = self.command_popup() else {
            // An emoji list takes Tab; with no list open the window gets it (#2417).
            if self.emoji_popup().is_some() {
                self.accept_emoji(cx);
            } else {
                cx.propagate();
            }
            return;
        };
        let Some(name) = names.get(index).copied() else {
            return;
        };
        // The trailing space closes the list: the word is finished.
        self.content = format!("{name} ");
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.command_index = 0;
        cx.notify();
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

    /// Every visual row of the field in paint order, as `(byte offset where
    /// the row starts, its length in bytes)`. A row is a wrapped piece of one
    /// logical line; before the field has ever been shaped the layout is
    /// empty and each logical line stands in for its own single row.
    fn visual_rows(&self) -> Vec<(usize, usize)> {
        let mut rows = Vec::new();
        for line in 0..self.line_count() {
            let start = self.line_start(line);
            let len = self.line_len(line);
            match self.last_layout.get(line) {
                Some(shaped) => {
                    for (from, to) in visual_segments(shaped, len) {
                        rows.push((start + from, to - from));
                    }
                }
                None => rows.push((start, len)),
            }
        }
        if rows.is_empty() {
            rows.push((0, 0));
        }
        rows
    }

    /// Everything the recall still holds, for the caller to put back in the
    /// queue it came from.
    ///
    /// The field shows every recalled message joined into one edit (#2696), so
    /// `keep_current` is the send path: the whole joined text is going out, and
    /// all of it leaves with the send. Abandoning returns every original
    /// untouched, oldest first, the order they were written in (#2637).
    pub fn release_recall(
        &mut self,
        keep_current: bool,
    ) -> Option<(Uuid, Vec<crate::agent_bridge::PendingMessage>)> {
        let recall = self.recall.take()?;
        if keep_current {
            return Some((recall.session, Vec::new()));
        }
        Some((recall.session, recall.entries))
    }

    /// Put a recall's messages back in their own queue.
    fn give_back_recall(&mut self, keep_current: bool) {
        if let Some((session, entries)) = self.release_recall(keep_current) {
            for msg in entries {
                crate::agent_bridge::queue_recall_return(session, msg);
            }
        }
    }

    /// The hint an empty field shows. It belongs to the window's state, not to
    /// the field's: what the composer can do depends on whether a turn runs and
    /// what is queued, and the editor sees neither (#2630).
    pub fn set_placeholder(&mut self, hint: SharedString, cx: &mut Context<Self>) {
        if self.placeholder != hint {
            self.placeholder = hint;
            cx.notify();
        }
    }

    /// Vertical movement runs over visual rows, so a wrapped paragraph behaves
    /// like the lines the reader sees. At the top row Up goes to the very
    /// start, and at the bottom row Down goes to the very end.
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        // Up brings the WHOLE queue into the field in one press (#2696): every
        // waiting message joins into one edit, oldest first, so three queued
        // messages are one correction instead of three. The take empties the
        // queue, so what the composer holds cannot also be sent by the drain
        // behind the user's back (#2637).
        //
        // Only into an empty field with no recall already held: there is no
        // walk left to continue, and taking again would replace the originals
        // the recall is holding with whatever arrived since.
        if self.content.is_empty()
            && self.recall.is_none()
            && let Some((session, msgs)) = crate::agent_bridge::queue_recall_take()
        {
            // The display halves join with newlines and a synthetic entry keeps
            // its receipt tag, so a background task's payload never turns into
            // editable text (#2650).
            let joined =
                crate::agent_bridge::PendingMessage::join(&msgs).expect("a non-empty recall joins");
            let recall = self.recall.get_or_insert_with(|| Recall {
                session,
                entries: Vec::new(),
                draft: std::mem::take(&mut self.content),
            });
            recall.entries = msgs;
            self.content = joined.display_text;
            let end = self.content.len();
            self.selected_range = end..end;
            self.selection_reversed = false;
            self.marked_range = None;
            cx.notify();
            return;
        }
        if self.step_command_popup(-1, cx) {
            return;
        }
        if self.step_emoji_popup(-1, cx) {
            return;
        }
        let rows = self.visual_rows();
        let target = vertical_row_target(&rows, self.cursor_offset(), -1);
        if target != self.cursor_offset() {
            self.move_to(target, cx);
            return;
        }
        // The guard is released before anything else touches `self`: holding it
        // across `move_to` would keep an immutable borrow of the editor alive.
        let recalled = {
            let entries = lock_history(&self.history);
            input_history_up(
                entries.as_slice(),
                &mut self.content,
                &mut self.input_history_index,
                &mut self.input_history_stash,
            )
        };
        if recalled {
            let end = self.content.len();
            self.selected_range = end..end;
            self.selection_reversed = false;
            self.marked_range = None;
            cx.notify();
        } else {
            self.move_to(target, cx);
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.step_command_popup(1, cx) {
            return;
        }
        if self.step_emoji_popup(1, cx) {
            return;
        }
        // Inside a recall the field holds several messages joined, so Down
        // walks the caret through them first. Off the last row it is the way
        // out: every original goes back to wait untouched, oldest first, and
        // the draft the recall covered returns (#2696, #2637).
        let rows = self.visual_rows();
        let target = vertical_row_target(&rows, self.cursor_offset(), 1);
        if target != self.cursor_offset() {
            self.move_to(target, cx);
            return;
        }
        if self.recall.is_some() {
            let recall = self.recall.take().expect("a recall is active");
            for msg in recall.entries {
                crate::agent_bridge::queue_recall_return(recall.session, msg);
            }
            self.content = recall.draft;
            let end = self.content.len();
            self.selected_range = end..end;
            self.selection_reversed = false;
            self.marked_range = None;
            cx.notify();
            return;
        }
        let recalled = {
            let entries = lock_history(&self.history);
            input_history_down(
                entries.as_slice(),
                &mut self.content,
                &mut self.input_history_index,
                &mut self.input_history_stash,
            )
        };
        if recalled {
            let end = self.content.len();
            self.selected_range = end..end;
            self.selection_reversed = false;
            self.marked_range = None;
            cx.notify();
        } else {
            self.move_to(target, cx);
        }
    }

    // Word motion walks whole words, not graphemes. Sharing the arrow helpers
    // here would have made alt+left move one character, which is what the
    // composer shipped with before the owner asked for the TUI's word keys.
    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(previous_word_start(&self.content, self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(next_word_end(&self.content, self.cursor_offset()), cx);
    }

    fn delete_word_back(
        &mut self,
        _: &DeleteWordBack,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(previous_word_start(&self.content, self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    /// ctrl+u: delete from the caret back to the start of its line, the TUI's
    /// binding. A selection is deleted as it is.
    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            let caret = self.cursor_offset();
            let before = self.content.get(..caret).unwrap_or("");
            let line_start = before.rfind('\n').map_or(0, |i| i + 1);
            self.select_to(line_start, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
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
        // Enter on an open emoji list takes the highlighted glyph, as the TUI does.
        if self.emoji_popup().is_some() {
            self.accept_emoji(cx);
            return;
        }
        // A command it is still spelling runs the highlighted name, which is
        // what Enter on the terminal's suggestion list does. Attachments are
        // not part of a command, so a draft carrying them goes out as text.
        let picked = if self.attachments.is_empty() {
            self.command_popup()
                .and_then(|(names, index)| names.get(index).copied())
        } else {
            None
        };
        if let Some(name) = picked {
            self.remember(name);
            self.clear_after_send(cx);
            cx.emit(EditorEvent::Sent(name.to_string()));
            return;
        }
        let text = self.content.trim().to_string();
        let attachments = self.take_attachments();
        if text.is_empty() && attachments.is_empty() {
            return;
        }
        let payload = message_with_markers(&text, &attachments);
        self.remember(&text);
        self.clear_after_send(cx);
        cx.emit(EditorEvent::Sent(payload));
    }

    /// Record one sent entry in the shared list and on disk.
    ///
    /// The dedup and the cap live in `ui_core::input_history`, the file write
    /// in `ui_core::composer`, so the terminal and this field cannot record
    /// the same message differently.
    fn remember(&mut self, entry: &str) {
        {
            let mut entries = lock_history(&self.history);
            record_input_history(&mut entries, entry);
        }
        let _ = composer::append_history(&self.history_path, entry);
    }

    /// The state a conversation switch has to carry: the text, the caret, and
    /// any recall in progress. Attachments are deliberately not part of it:
    /// a chip points at a file for the message being written, and carrying it
    /// into another conversation would aim it at the wrong one.
    pub fn draft(&self) -> ComposerDraft {
        ComposerDraft {
            text: self.content.clone(),
            cursor: self.cursor_offset(),
            history_index: self.input_history_index,
            history_stash: self.input_history_stash.clone(),
        }
    }

    /// Take the draft out of the field, leaving it ready for another
    /// conversation. The popup state resets with it: a list opened for this
    /// text makes no sense over different text.
    pub fn take_draft(&mut self, cx: &mut Context<Self>) -> ComposerDraft {
        let draft = self.draft();
        self.content.clear();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.input_history_index = None;
        self.input_history_stash.clear();
        self.command_index = 0;
        self.command_dismissed = false;
        self.emoji_index = 0;
        self.emoji_dismissed = false;
        cx.notify();
        draft
    }

    /// Put a stored draft back, caret at its stored offset. Recalled text is
    /// not re-shown: the walk starts over on the shared list.
    pub fn restore_state(&mut self, draft: ComposerDraft, cx: &mut Context<Self>) {
        let end = draft.cursor.min(draft.text.len());
        self.content = draft.text;
        let end = self.content.floor_char_boundary(end);
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.input_history_index = draft.history_index;
        self.input_history_stash = draft.history_stash;
        cx.notify();
    }

    /// Empty the field and put the command list back to its resting state.
    fn clear_after_send(&mut self, cx: &mut Context<Self>) {
        self.content.clear();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.input_history_index = None;
        self.input_history_stash.clear();
        self.command_index = 0;
        self.command_dismissed = false;
        self.emoji_dismissed = false;
        self.emoji_index = 0;
        cx.notify();
    }

    /// A fresh conversation owns a fresh composer (#2481): no draft carried
    /// from the conversation just left, no attachments waiting to fly into
    /// the wrong one, both popups back to rest. The clipboard paste hint is
    /// derived from the empty field at render time, so it stands down with
    /// the content; nothing stored to reset there.
    pub fn begin_conversation(&mut self, cx: &mut Context<Self>) {
        self.content.clear();
        self.attachments.clear();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.input_history_index = None;
        self.input_history_stash.clear();
        self.command_index = 0;
        self.command_dismissed = false;
        self.emoji_index = 0;
        self.emoji_dismissed = false;
        cx.notify();
    }

    /// Whether there is anything to throw away: text in the field, or a chip
    /// waiting to fly with it. A recall counts as text, because its joined
    /// entries are what the field is showing.
    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty() && self.attachments.is_empty()
    }

    /// A Ctrl+C that throws the draft away (#2308). The text and the chips go,
    /// and the queue takes back whatever an Up walk had pulled out of it: a
    /// draft the user cancelled must never be sent behind their back.
    pub fn discard_draft(&mut self, cx: &mut Context<Self>) {
        self.give_back_recall(false);
        self.begin_conversation(cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        // A screenshot copies no text: the picture lands on a scratch file and
        // attaches through the same `attach_paths` door a Finder drop takes
        // (#2479). An entry with text stays a text paste.
        if let Some(image) = Self::clipboard_image(cx)
            && let Some(path) = write_clipboard_image(&image)
            && self.attach_paths([path]) > 0
        {
            cx.notify();
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    pub fn has_selection(&self) -> bool {
        !self.selected_range.is_empty()
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

    /// A right press opens the composer's own menu at the pointer (#2749).
    ///
    /// It does not move the caret: Cut and Copy act on what is selected, and a
    /// press that collapsed the selection first would leave both with nothing
    /// to act on.
    fn on_right_press(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        cx.stop_propagation();
        self.ctx_menu_at = Some(event.position);
        cx.notify();
    }

    /// Close the composer's context menu, if one is open (#2749). The window
    /// calls this on a left press anywhere, and the card stops its own
    /// presses, so a press that gets here is a press outside it.
    pub fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.ctx_menu_at.take().is_some() {
            cx.notify();
        }
    }

    /// The composer's context menu: the four edits a text field is expected to
    /// offer, drawn at the press that asked for them (#2749).
    ///
    /// Cut and Copy are drawn disabled rather than dropped while nothing is
    /// selected, so the card keeps one shape under the pointer. Paste is the
    /// action's own path, which means a screenshot on the clipboard still
    /// attaches as an image (#2479) instead of pasting nothing.
    fn context_menu(&self, at: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let has_selection = self.has_selection();
        let mut rows: Vec<gpui::Stateful<Div>> = Vec::new();
        if has_selection {
            let cut = cx.listener(|this, _ev: &ClickEvent, window, cx| {
                this.ctx_menu_at = None;
                this.cut(&Cut, window, cx);
            });
            rows.push(crate::menu::menu_item("Cut").on_click(cut));
            let copy = cx.listener(|this, _ev: &ClickEvent, window, cx| {
                this.ctx_menu_at = None;
                this.copy(&Copy, window, cx);
            });
            rows.push(crate::menu::menu_item("Copy").on_click(copy));
        } else {
            rows.push(crate::menu::menu_item_disabled("Cut"));
            rows.push(crate::menu::menu_item_disabled("Copy"));
        }
        let paste = cx.listener(|this, _ev: &ClickEvent, window, cx| {
            this.ctx_menu_at = None;
            this.paste(&Paste, window, cx);
        });
        rows.push(crate::menu::menu_item("Paste").on_click(paste));
        let select_all = cx.listener(|this, _ev: &ClickEvent, window, cx| {
            this.ctx_menu_at = None;
            this.select_all(&SelectAll, window, cx);
        });
        rows.push(crate::menu::menu_item("Select all").on_click(select_all));
        crate::menu::menu_popover("composer-context-menu", rows, at)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        // A left press in the field is a press outside the card, so it goes
        // with it (#2749). The card itself stops its presses before they get
        // here.
        self.ctx_menu_at = None;
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
            return;
        }
        // Esc closes the card before anything above the field acts on the key
        // (#2749).
        if ev.keystroke.key == "escape" && self.ctx_menu_at.take().is_some() {
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
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
        // Once a line wraps, rows no longer share one height, so the click is
        // resolved against the row that actually covers it instead of by
        // dividing by a single line height.
        let mut line = self.last_tops.len().saturating_sub(1);
        let mut row_top = Pixels::ZERO;
        for (index, (top, height)) in self.last_tops.iter().enumerate() {
            if rel_y >= *top && rel_y < *top + *height {
                line = index;
                row_top = *top;
                break;
            }
        }
        line = line.min(self.line_count() - 1);
        let Some(shaped) = self.last_layout.get(line) else {
            return 0;
        };
        let rel = point(
            position.x - bounds.left(),
            (rel_y - row_top).max(Pixels::ZERO),
        );
        self.line_start(line)
            // Both variants carry the index: Err is the boundary of a wrapped row,
            // and dropping it sent a click past a row's end to the line start (#2439).
            + shaped
                .closest_index_for_position(rel, line_height)
                .unwrap_or_else(|index| index)
                .min(self.line_len(line))
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
        // The draft changed, so a dismissed list is offered again.
        self.command_dismissed = false;
        self.emoji_dismissed = false;
        self.emoji_index = 0;
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
        self.command_dismissed = false;
        self.emoji_dismissed = false;
        self.emoji_index = 0;
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
        let line_height = self.last_line_height;
        let shaped = self.last_layout.get(line)?;
        let line_top = self
            .last_tops
            .get(line)
            .map(|(top, _)| *top)
            .unwrap_or_default();
        let start = shaped.position_for_index(col_start, line_height)?;
        let end = shaped.position_for_index(col_end, line_height)?;
        Some(Bounds::from_corners(
            point(bounds.left() + start.x, bounds.top() + line_top + start.y),
            point(
                bounds.left() + end.x,
                bounds.top() + line_top + end.y + line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_mouse_position(point)))
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<EditorEvent> for Editor {}

/// One logical line, already shaped and wrapped to the field width, plus the
/// y offset it paints at and the quads that belong to it.
struct RowPaint {
    /// Which shaped line this row paints. The lines themselves live once, in
    /// the prepaint state, so a frame no longer clones the whole layout per
    /// row and then again per frame (#2688).
    line_ix: usize,
    /// Distance from the top of the field to this line's first visual row.
    top: Pixels,
    /// Total height of this line, every wrapped row counted.
    height: Pixels,
    /// One rect per visual row the selection spans, line-local pixels.
    selection: Vec<(Point<Pixels>, Point<Pixels>)>,
    /// Caret origin within the line, when the caret sits on it.
    cursor: Option<Point<Pixels>>,
    /// The crab shortcodes on this line, as (byte offset in the logical line,
    /// pack name). The field paints the pack picture at each one, over the
    /// transparent run the shaper left for it (#2794).
    crabs: Vec<(usize, &'static str)>,
}

/// The composer's shaped lines plus the inputs they were shaped from (#2688).
/// Shaping is the expensive half of a composer frame; a caret blink, a
/// selection, a notice timer or a streaming answer re-renders the field without
/// changing the draft, size, width, font or color, so those frames reuse this.
///
/// `rows` rides along because the height that shaping implies is the other
/// half of the same question. `request_layout` runs before prepaint and used to
/// answer it by shaping the whole draft a second time, uncached, on every
/// frame: the cache below then saved nothing on the frames it was built for.
struct ShapedDraft {
    key: (String, Pixels, Pixels, Font, [f32; 4]),
    /// One `Arc` for the whole shaping, so a hit is a pointer bump and both
    /// the prepaint state and `last_layout` can hold the same lines. Cloning
    /// the `Vec` copied every glyph of the draft, twice per frame (#2688).
    lines: Option<Arc<Vec<WrappedLine>>>,
    rows: usize,
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
    /// The shaped lines the rows index into, held once instead of cloned per
    /// row and per frame (#2688), and shared with `last_layout` rather than
    /// deep-copied into it (#2688).
    layout: Arc<Vec<WrappedLine>>,
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
        // The height comes from the shaping prepaint already paid for (#2688).
        // `text_system().shape_text` keeps no cache of its own, so asking it
        // here meant every frame shaped the whole draft again, caret blinks and
        // notice timers included, and the prepaint cache saved nothing. The key
        // below is the one prepaint writes, so a frame that changed nothing
        // answers in one comparison. An empty field has no content key (its
        // cache holds the placeholder), and costs nothing to fall back on.
        let (content, wrap_width, cached_rows, key) = {
            let input = self.input.read(cx);
            let wrap_width = input.wrap_width;
            let key = (!input.content.is_empty() && wrap_width > Pixels::ZERO).then(|| {
                let style = window.text_style();
                let font_size = style.font_size.to_pixels(window.rem_size());
                let color = style.color;
                (
                    input.content.clone(),
                    font_size,
                    wrap_width,
                    style.font(),
                    [color.h, color.s, color.l, color.a],
                )
            });
            let cached = key.as_ref().and_then(|key| {
                input
                    .shaped_draft
                    .as_ref()
                    .filter(|cache| &cache.key == key)
                    .map(|cache| cache.rows)
            });
            (input.content.clone(), wrap_width, cached, key)
        };
        let rows = match (cached_rows, key) {
            (Some(rows), _) => rows.max(1),
            (None, None) => wrapped_row_count(&content, wrap_width, window),
            (None, Some(key)) => {
                // A keystroke changes the content, so the cache misses on
                // exactly the frame that has to feel instant. Shaping here just
                // to count rows used to be thrown away and prepaint then shaped
                // the same draft a second time (#2688). Keeping the lines hands
                // prepaint the work it was about to repeat, so that frame pays
                // one shaping instead of two.
                let lines = shape_draft(&content, wrap_width, window).map(Arc::new);
                let rows = shaped_rows(lines.as_deref(), &content);
                // The cache holds the shaping as an `Option` because shaping
                // can fail; the lines move in whole, so the miss path costs one
                // shaping and prepaint finds it waiting.
                if lines.is_some() {
                    self.input.update(cx, |input, _| {
                        input.shaped_draft = Some(ShapedDraft { key, lines, rows });
                    });
                }
                rows.max(1)
            }
        };
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * rows as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let (content, selected, cursor, placeholder, empty, bare, notice) = {
            let input = self.input.read(cx);
            (
                input.content.clone(),
                input.selected_range.clone(),
                input.cursor_offset(),
                input.placeholder.to_string(),
                input.content.is_empty(),
                input.attachments.is_empty(),
                input.active_notice(Instant::now()).map(str::to_string),
            )
        };
        // A copy leaves nothing to look at, so an empty field says it in the
        // slot the placeholder owns (#2587). The words are the terminal's own,
        // and the line clears itself on the same two-second clock.
        //
        // The composer also says when a screenshot is waiting (#2479): an empty
        // field over an image-only clipboard swaps that hint in for the name,
        // the same notice the TUI prints under its input. A picture already in
        // the strip is the answer the hint would give, so it stands down, and a
        // live copy notice outranks it because the copy is the newer event.
        let placeholder = match notice {
            Some(notice) => notice,
            None if empty && bare && Editor::clipboard_image(cx).is_some() => {
                CLIPBOARD_IMAGE_HINT.to_string()
            }
            None => placeholder,
        };
        let style = window.text_style();
        // The hint is a whisper, not a message: painted smaller than the text
        // it stands in for, so a queue's keys never shout over the
        // conversation the way full-size words did (#2630).
        let font_size = if empty {
            style.font_size.to_pixels(window.rem_size()) * HINT_SCALE
        } else {
            style.font_size.to_pixels(window.rem_size())
        };
        let line_height = window.line_height();
        // The field never widens past its own bounds: wrap to exactly the
        // width the parent handed us, which is what makes a resize reflow.
        let wrap_width = bounds.size.width;

        // The draft is cloned once, when it is painted, not twice: the copy
        // the shape key needed is the copy the paint loop already holds
        // (#2688). Only its length is needed after this point.
        let content_len = content.len();
        let display = if empty { placeholder } else { content };
        let run_color = if empty {
            hsla_from(palette::current().text_faint)
        } else {
            style.color
        };
        let font = style.font();
        // One run per stretch of text, with a transparent run over every crab
        // shortcode: the glyphs still take their width, so the caret, the
        // selection and the wrap land where they would with the token shown,
        // and the picture painted on top of that empty box is the emoji the
        // user picked (#2794).
        let runs = draft_runs(&display, &font, run_color);
        // The pack pictures this draft draws, decoded before the frame paints
        // so the paint pass only reads them (#2794).
        if !empty {
            let crabs: Vec<&'static str> = crate::topic_icons::crab_tokens(&display)
                .into_iter()
                .filter_map(|(_, bytes)| crate::topic_icons::crab_name(bytes))
                .collect();
            if !crabs.is_empty() {
                self.input.update(cx, |input, _| {
                    for name in crabs {
                        // Decoded here so the paint pass only reads: a miss
                        // still paints nothing rather than a PNG decode
                        // inside the frame.
                        let _ = input.crab_image(name);
                    }
                });
            }
        }
        // Shaping is the expensive half of a composer frame, and the draft,
        // size, width, font and color are the only inputs it depends on. A
        // caret blink, a selection, a notice timer or a streaming answer
        // re-renders the field without touching any of them, so reuse the last
        // shaping instead of paying for it again (#2688).
        let key = (
            display.clone(),
            font_size,
            wrap_width,
            font,
            [run_color.h, run_color.s, run_color.l, run_color.a],
        );
        let cached = self
            .input
            .read(cx)
            .shaped_draft
            .as_ref()
            .filter(|cache| cache.key == key)
            .map(|cache| cache.lines.clone());
        let shaped = match cached {
            Some(lines) => lines,
            None => {
                let lines = window
                    .text_system()
                    .shape_text(
                        SharedString::from(display.clone()),
                        font_size,
                        &runs,
                        Some(wrap_width),
                        None,
                    )
                    .ok()
                    // shape_text hands back a SmallVec; settle it into one
                    // shared Vec so the cache, the prepaint state and
                    // `last_layout` all point at the same glyphs (#2688).
                    .map(|lines| Arc::new(lines.into_vec()));
                let rows = shaped_rows(lines.as_deref(), &display);
                self.input.update(cx, |input, _| {
                    input.shaped_draft = Some(ShapedDraft {
                        key,
                        lines: lines.clone(),
                        rows,
                    });
                });
                lines
            }
        };

        let mut rows = Vec::new();
        let mut line_start = 0usize;
        let mut top = Pixels::ZERO;
        // A line the shape pass did not return still paints its own height, so
        // the field never collapses; the line itself is borrowed instead of
        // cloned, and each row records only which line it paints (#2688).
        let blank = WrappedLine::default();
        for (index, text) in display.split('\n').enumerate() {
            let line_end = line_start + text.len();
            let line = shaped
                .as_ref()
                .and_then(|lines| lines.get(index))
                .unwrap_or(&blank);
            let height = line_height * (line.wrap_boundaries().len() + 1) as f32;

            // A selection only maps onto a real line when the field has
            // content; the placeholder paints bare. The caret is decided by
            // `caret_in_row`, which paints one on an empty field too.
            let mut selection = Vec::new();
            let mut caret = None;
            if !empty
                && !selected.is_empty()
                && selected.start < line_end
                && selected.end > line_start
            {
                let sel_start = selected.start.clamp(line_start, line_end) - line_start;
                let sel_end = selected.end.clamp(line_start, line_end) - line_start;
                for (seg_start, seg_end) in visual_segments(line, text.len()) {
                    let lo = sel_start.max(seg_start);
                    let hi = sel_end.min(seg_end);
                    if lo >= hi {
                        continue;
                    }
                    if let (Some(from), Some(to)) = (
                        line.position_for_index(lo, line_height),
                        line.position_for_index(hi, line_height),
                    ) {
                        selection.push((from, to));
                    }
                }
            }
            if let Some(offset) = caret_in_row(empty, &selected, cursor, line_start, line_end) {
                caret = line.position_for_index(offset, line_height);
            }

            // The pack pictures on this line, at line-local byte offsets. A
            // crab shortcode is shaped into a transparent run, so the glyphs
            // still hold their width and the picture lands exactly where the
            // token sat (#2794).
            let crabs: Vec<(usize, &'static str)> = if empty {
                Vec::new()
            } else {
                crate::topic_icons::crab_tokens(text)
                    .into_iter()
                    .filter_map(|(range, bytes)| {
                        crate::topic_icons::crab_name(bytes).map(|name| (range.start, name))
                    })
                    .collect()
            };

            rows.push(RowPaint {
                line_ix: index,
                top,
                height,
                selection,
                cursor: caret,
                crabs,
            });
            top += height;
            line_start = line_end + 1; // past the '\n'
        }

        // The shaped lines already hold everything a row paints, so the layout
        // is taken from them once instead of rebuilt from copies (#2688).
        let layout: Arc<Vec<WrappedLine>> = shaped.unwrap_or_else(|| Arc::new(Vec::new()));
        let tops: Vec<(Pixels, Pixels)> = rows.iter().map(|row| (row.top, row.height)).collect();
        // Where the caret sits inside the draft, and where the box is scrolled
        // to right now. A caret that wandered out of a box capped at
        // `COMPOSER_MAX_ROWS` asks for a scroll; a frame that moved neither the
        // caret nor the draft asks for nothing, so a wheel scroll survives the
        // caret's next blink (#2773).
        let caret_top = rows
            .iter()
            .find_map(|row| row.cursor.map(|caret| row.top + caret.y));
        let (current_scroll, key) = {
            let input = self.input.read(cx);
            (-input.scroll.offset().y, (cursor, content_len))
        };
        let reveal = (key != self.input.read(cx).reveal_key).then(|| {
            reveal_scroll(
                top,
                line_height * COMPOSER_MAX_ROWS as f32,
                caret_top,
                line_height,
                current_scroll,
            )
        });
        self.input.update(cx, |input, cx| {
            let resized = input.wrap_width != wrap_width;
            input.wrap_width = wrap_width;
            input.last_layout = Arc::clone(&layout);
            input.last_tops = tops;
            input.last_bounds = Some(bounds);
            input.last_line_height = line_height;
            if let Some(target) = reveal {
                input.reveal = Some(target);
                input.reveal_key = key;
            }
            // Height is measured before bounds are known, so the width that
            // just became real only takes effect on the next frame. Re-render
            // once when it changes; two frames later the width is stable.
            if resized {
                cx.notify();
            }
        });

        EditorPrepaint { rows, layout }
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
        for row in rows.iter() {
            let Some(line) = prepaint.layout.get(row.line_ix) else {
                continue;
            };
            let y = bounds.top() + row.top;
            for (from, to) in &row.selection {
                window.paint_quad(fill(
                    Bounds::from_corners(
                        point(bounds.left() + from.x.min(to.x), y + from.y),
                        point(
                            bounds.left() + from.x.max(to.x),
                            y + from.y.max(to.y) + line_height,
                        ),
                    ),
                    rgba(0x3311ff30),
                ));
            }
            line.paint(
                bounds.origin + point(px(0.), row.top),
                line_height,
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            )
            .unwrap();
            // The pack pictures, drawn over the transparent runs the shaper
            // left where each shortcode sits (#2794). The square is centred in
            // the box the token still holds, so the text after it never moves
            // and the picture never spills onto the next word.
            if !row.crabs.is_empty() {
                let images = &self.input.read(cx).crab_images;
                for (offset, name) in row.crabs.iter() {
                    let Some(image) = images.get(name) else {
                        continue;
                    };
                    let Some(at) = line.position_for_index(*offset, line_height) else {
                        continue;
                    };
                    let token_end = line.position_for_index(*offset + name.len() + 2, line_height);
                    let width = token_end
                        .filter(|end| end.y == at.y)
                        .map(|end| end.x - at.x)
                        .unwrap_or(line_height)
                        .max(line_height);
                    let inset = (width - line_height) * 0.5;
                    let _ = window.paint_image(
                        Bounds::new(
                            point(bounds.left() + at.x + inset, y + at.y),
                            size(line_height, line_height),
                        ),
                        Corners::default(),
                        Arc::clone(image),
                        0,
                        false,
                    );
                }
            }
            if focused && let Some(caret) = row.cursor {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left() + caret.x, y + caret.y),
                        size(px(2.), line_height),
                    ),
                    gpui::blue(),
                ));
            }
        }
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A deferred caret request (#2665): only a frame holding a window can
        // put focus back after the platform's file dialog closed.
        if self.focus_pending {
            self.focus_pending = false;
            window.focus(&self.focus_handle);
        }
        // The box is capped at `COMPOSER_MAX_ROWS`, so a caret below the fold
        // asked prepaint for a scroll. Applying it here, where the handle
        // lives, keeps the height question and the scroll question on the
        // frames that own them (#2773).
        if let Some(target) = self.reveal.take()
            && -self.scroll.offset().y != target
        {
            self.scroll.set_offset(point(Pixels::ZERO, -target));
        }
        // Chips above the text, the paperclip beside it: the input stays the
        // biggest thing in the box and the attachment row never pushes the
        // caret around.
        let mut body = div().flex().flex_col().gap_2();
        if !self.attachments.is_empty() {
            body = body.child(attachment_strip(&self.attachments, cx));
        }
        // With a draft in the field the placeholder is gone, so the copy notice
        // gets its own line under the text instead (#2587). It is chrome, never
        // a transcript line: the box grows by nothing when it expires.
        let notice_row_text = self
            .active_notice(Instant::now())
            .filter(|_| !self.content.is_empty())
            .map(str::to_string);
        // The icon sits inside the input, to the right of the text, as the
        // messaging apps place it; the text and its hint are gone (#2445).
        body = body.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl_1()
                        // A draft taller than the box scrolls inside it. Without
                        // the cap the field grew with every line and painted
                        // over the pane below, the overlap the owner saw
                        // (#2773).
                        .max_h(window.line_height() * COMPOSER_MAX_ROWS as f32)
                        // The scroll methods live on a stateful element, so
                        // the box carries an id before it asks to scroll at
                        // all. A plain div has neither method and the gui
                        // binary stopped compiling without this (#2773).
                        .id("composer-input")
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(EditorElement { input: cx.entity() }),
                )
                .child(paperclip(cx)),
        );
        if let Some(notice) = notice_row_text {
            body = body.child(notice_row(&notice));
        }
        let root = div()
            .flex()
            .flex_col()
            .justify_center()
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
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::delete_word_back))
            .on_action(cx.listener(Self::delete_to_line_start))
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
            .on_action(cx.listener(Self::accept_command))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_press))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            // One step above the chat, with a shadow, so the input reads as a
            // surface of its own in both themes.
            .bg(glass_fill(palette::current().panel))
            .shadow_sm()
            .border_1()
            .border_color(rgb(palette::current().panel_hi))
            .rounded(px(CARD_RADIUS))
            .p_3()
            .line_height(px(22.))
            .text_size(px(14.))
            .text_color(rgb(palette::current().text))
            .min_h(px(44.))
            .child(body);
        // Deferred, so the card paints over the field that opened it and the
        // conversation above it rather than under the next sibling (#2690).
        match self.ctx_menu_at {
            Some(at) => root
                .child(gpui::deferred(self.context_menu(at, cx)).with_priority(2))
                .into_any_element(),
            None => root.into_any_element(),
        }
    }
}

/// One transient line inside the composer, in the terminal's own voice
/// (#2587). Faint and one row tall: it confirms the copy without competing
/// with the draft right above it.
fn notice_row(text: &str) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(rgb(palette::current().text_faint))
        .child(text.to_string())
}

/// The pending attachments as chips. Images show the real file through
/// `img` (gpui loads it from disk, so a broken path shows nothing rather
/// than a fake tile); videos and every other file get a glyph plus the name.
fn attachment_strip(attachments: &[Attachment], cx: &mut Context<Editor>) -> Div {
    // `w_full().min_w_0()` is what keeps the row inside the composer: without
    // it the strip sized itself from the chips and ran past the input until
    // the window clipped it (#2443).
    let mut strip = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .w_full()
        .min_w_0()
        .gap_2();
    for (index, attachment) in attachments.iter().enumerate() {
        let remove = cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
            // The ✕ must not also reach the editor: a click there would move
            // the caret and start a selection while the user deletes a chip.
            cx.stop_propagation();
            this.remove_attachment(index, cx);
        });
        // The thumbnail opens the same overlay a transcript tile opens. It is
        // its own element with its own id and the remove button is a sibling,
        // so the two clicks cannot collide: the ✕ handler never sees this one
        // and this one never sees the ✕. The gesture is the completed CLICK, the
        // same one a transcript tile answers to, so both paths carry one
        // meaning and the overlay is never mounted between a press and its
        // release (#2395).
        let open_preview = {
            let path = attachment.path.clone();
            cx.listener(move |_this, _ev: &ClickEvent, _window, cx| {
                // The chip's own receipt: if the log shows this line and no
                // `preview: open` after it, the emit never reached the
                // dashboard; if it shows neither, the click never arrived
                // (#2395).
                crate::logging::info(&format!("preview: chip {}", path.display()));
                cx.stop_propagation();
                cx.emit(EditorEvent::Preview(path.clone()));
            })
        };
        // The box owns the size and clips the picture, the shape
        // `transcript::attachment_cell` proved: an `img` that sizes itself
        // bleeds out of the composer instead of staying in its tile (#2443).
        let art: AnyElement = if attachment.is_image() {
            div()
                .w(px(34.0))
                .h(px(34.0))
                .flex_shrink_0()
                .overflow_hidden()
                .rounded(px(4.0))
                // A square crop painted with `Fill`: the painted quad is the
                // box, so the chip's radius lands on the picture instead of
                // leaving square pixels under the arc (#2796).
                .child(crate::thumbs::picture(&attachment.path, px(4.0)))
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(34.0))
                .h(px(34.0))
                .flex_shrink_0()
                .rounded(px(4.0))
                .bg(rgb(palette::current().bg))
                .child(if attachment.is_video { "🎬" } else { "📄" })
                .into_any_element()
        };
        let tile = div()
            .id(ElementId::Name(
                format!("attachment-preview-{index}").into(),
            ))
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .cursor_pointer()
            .on_click(open_preview)
            .child(art);
        strip = strip.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .p_1()
                .min_w_0()
                .overflow_hidden()
                .rounded(px(6.0))
                .bg(rgb(palette::current().panel_hi))
                .child(tile)
                .child(
                    div()
                        .max_w(px(160.0))
                        .min_w_0()
                        .flex_shrink()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(palette::current().text))
                        .child(SharedString::from(attachment.name.clone())),
                )
                .child(
                    div()
                        .id(ElementId::Name(format!("attachment-remove-{index}").into()))
                        .flex_shrink_0()
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
        .flex_none()
        .items_center()
        .justify_center()
        .size(gpui::rems(1.8))
        .mr_1()
        .rounded_sm()
        .cursor_pointer()
        .text_size(gpui::rems(1.1))
        .text_color(rgb(palette::current().text_faint))
        .hover(|d| d.text_color(rgb(palette::current().accent)))
        .child("📎")
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
        KeyBinding::new("alt-left", WordLeft, None),
        KeyBinding::new("alt-right", WordRight, None),
        KeyBinding::new("ctrl-left", WordLeft, None),
        KeyBinding::new("ctrl-right", WordRight, None),
        KeyBinding::new("alt-backspace", DeleteWordBack, None),
        KeyBinding::new("ctrl-backspace", DeleteWordBack, None),
        KeyBinding::new("shift-left", SelectLeft, None),
        KeyBinding::new("shift-right", SelectRight, None),
        KeyBinding::new("cmd-a", SelectAll, None),
        KeyBinding::new("cmd-v", Paste, None),
        KeyBinding::new("cmd-c", Copy, None),
        KeyBinding::new("ctrl-c", Copy, None),
        KeyBinding::new("cmd-x", Cut, None),
        KeyBinding::new("home", Home, None),
        KeyBinding::new("end", End, None),
        KeyBinding::new("enter", Send, None),
        KeyBinding::new("shift-enter", Newline, None),
        KeyBinding::new("ctrl-j", Newline, None),
        KeyBinding::new("ctrl-u", DeleteToLineStart, None),
        KeyBinding::new("ctrl-w", DeleteWordBack, None),
        KeyBinding::new("tab", AcceptCommand, None),
    ]);
}

// Line math, free over `&str` ------------------------------------------------
//
// These carry the whole multiline model (movement, Home/End, mouse mapping,
// IME range math), and they are pure string arithmetic. They live outside the
// struct so unit tests can seat them without a gpui app context, which no
// headless test can build.

/// Where the caret sits inside one painted row, as a byte offset from that
/// row's start, or `None` when this row paints no caret.
///
/// An empty field still paints one, at the start of its single row: the
/// placeholder is what the user sees, and without a caret a focused composer
/// gives no sign that typing lands anywhere (#2446). A row that holds a
/// selection paints the selection instead, and a caret that belongs to a
/// different row is that row's business.
fn caret_in_row(
    empty: bool,
    selected: &Range<usize>,
    cursor: usize,
    line_start: usize,
    line_end: usize,
) -> Option<usize> {
    if empty {
        return Some(0);
    }
    if !selected.is_empty() {
        return None;
    }
    (cursor >= line_start && cursor <= line_end).then(|| cursor - line_start)
}

fn line_count(content: &str) -> usize {
    content.split('\n').count()
}

/// Rows the shaped lines cover: one per wrap boundary plus one per logical
/// line, which is the same height `wrapped_row_count` answers. A failed
/// shaping falls back to the logical lines, so the field keeps its own height
/// instead of collapsing to one row.
fn shaped_rows(lines: Option<&Vec<WrappedLine>>, display: &str) -> usize {
    lines
        .map(|lines| {
            lines
                .iter()
                .map(|line| line.wrap_boundaries().len() + 1)
                .sum::<usize>()
        })
        .unwrap_or_else(|| display.split('\n').count())
        .max(1)
}

/// Where the box scrolls so the caret stays visible. `total` is the height of
/// the whole draft, `view` the height of the capped box, `caret_top` the
/// caret's own offset inside the draft. A draft that fits never scrolls at all,
/// which is what keeps a short field pinned to its top edge (#2773).
fn reveal_scroll(
    total: Pixels,
    view: Pixels,
    caret_top: Option<Pixels>,
    line_height: Pixels,
    current: Pixels,
) -> Pixels {
    let max = (total - view).max(Pixels::ZERO);
    if max <= Pixels::ZERO {
        return Pixels::ZERO;
    }
    let mut offset = current.max(Pixels::ZERO).min(max);
    if let Some(caret) = caret_top {
        if caret < offset {
            offset = caret;
        } else if caret + line_height > offset + view {
            offset = caret + line_height - view;
        }
    }
    offset.max(Pixels::ZERO).min(max)
}

/// Visual rows the text occupies at `wrap_width`.
///
/// `request_layout` has to answer a height before it is handed its own
/// bounds, so it measures with the width the previous frame recorded. Zero
/// means nothing has been laid out yet, and the logical line count is the
/// honest fallback; the first prepaint stores the real width and asks for
/// one more frame.
fn wrapped_row_count(content: &str, wrap_width: Pixels, window: &Window) -> usize {
    if wrap_width <= Pixels::ZERO {
        return content.split('\n').count().max(1);
    }
    shaped_rows(shape_draft(content, wrap_width, window).as_ref(), content)
}

/// Decode a pack PNG into the picture gpui paints (#2794).
///
/// gpui takes pictures as BGRA bytes, one `image::Frame` per animation step,
/// and a crab is one step: the decode is the RGBA read, the red/blue swap and
/// nothing else. The field decodes each crab once and keeps the result, so a
/// draft that holds the same picture on several lines pays for it once.
fn crab_picture(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let rgba = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut bgra = rgba.into_raw();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// One run per stretch of `text`, with every crab shortcode drawn in
/// `transparent_black` (#2794).
///
/// The token stays in the string, so the glyphs still hold the width the
/// shortcode took and every caret, selection and wrap offset after it stays
/// honest. The run only makes those glyphs invisible: the field paints the
/// pack picture on top of the empty box the run leaves behind. Cutting the
/// token out instead would shift every offset past it.
fn draft_runs(text: &str, font: &Font, color: gpui::Hsla) -> Vec<TextRun> {
    let run = |len: usize, color| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut cursor = 0;
    for (token, _) in crate::topic_icons::crab_tokens(text) {
        if cursor < token.start {
            runs.push(run(token.start - cursor, color));
        }
        runs.push(run(token.end - token.start, transparent_black()));
        cursor = token.end;
    }
    if cursor < text.len() || runs.is_empty() {
        runs.push(run(text.len() - cursor, color));
    }
    runs
}

/// Shape the draft with the same inputs the field paints with: the style the
/// window hands down, the width it was given, no forced line limit. The caller
/// decides whether the lines are worth keeping. Both the height question and
/// the paint question ask this, so the shape call itself lives in one place
/// (#2688).
fn shape_draft(content: &str, wrap_width: Pixels, window: &Window) -> Option<Vec<WrappedLine>> {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let runs = draft_runs(content, &style.font(), style.color);
    window
        .text_system()
        .shape_text(
            SharedString::from(content.to_string()),
            font_size,
            &runs,
            Some(wrap_width),
            None,
        )
        .ok()
        .map(|lines| lines.into_vec())
}

/// Byte ranges, relative to the logical line, that each visual row covers
/// once the line wraps. A line that fits is one range; a wrapped line is one
/// range per wrap boundary, contiguous and covering the whole line.
fn visual_segments(line: &WrappedLine, line_len: usize) -> Vec<(usize, usize)> {
    let mut starts = vec![0usize];
    for boundary in line.wrap_boundaries() {
        if let Some(run) = line.runs().get(boundary.run_ix)
            && let Some(glyph) = run.glyphs.get(boundary.glyph_ix)
            && glyph.index > *starts.last().unwrap_or(&0)
            && glyph.index < line_len
        {
            starts.push(glyph.index);
        }
    }
    let mut segments = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(line_len);
        segments.push((*start, end));
    }
    segments
}

/// Where the caret lands when it travels `delta` visual rows from `offset`.
///
/// Row ends are exclusive: a caret parked exactly at a wrap point stays on the
/// row it just finished, so pressing Up there climbs from that row instead of
/// skipping it. Off the top the caret goes to the very start of the content;
/// off the bottom, to its very end. The column is carried across, clamped to
/// whatever the destination row can hold.
fn vertical_row_target(rows: &[(usize, usize)], offset: usize, delta: isize) -> usize {
    let Some(last) = rows.last() else {
        return 0;
    };
    let mut current = rows.len() - 1;
    for (index, (start, len)) in rows.iter().enumerate() {
        // Inclusive at the end: a caret parked at a wrap point (or at the end of a
        // line, just before its `\n`) belongs to the row it just finished, so
        // Up and Down climb from there instead of skipping that row.
        if offset >= *start && offset <= start + len {
            current = index;
            break;
        }
    }
    let column = offset.saturating_sub(rows[current].0);
    if delta < 0 {
        if current == 0 {
            return 0;
        }
        let (start, len) = rows[current - 1];
        start + column.min(len)
    } else {
        if current + 1 >= rows.len() {
            return last.0 + last.1;
        }
        let (start, len) = rows[current + 1];
        start + column.min(len)
    }
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

/// What the composer says while an image sits unpasted in the clipboard
/// (#2479). The TUI prints the same notice with its own key; here the field's
/// own binding is `cmd-v`.
const CLIPBOARD_IMAGE_HINT: &str = "\u{1f4cb} Image in clipboard - \u{2318}V to paste";

/// The hint paints at this fraction of the composer's own text size. A full
/// size line of key names read as a second paragraph above the caret (#2630).
const HINT_SCALE: f32 = 0.8;

/// The file extension a clipboard image format maps to. `png` covers the
/// screenshot case; the rest are the formats a browser can hand over.
fn image_format_extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
    }
}

/// Write clipboard image bytes to a scratch file so the attachment pipeline,
/// which is path based, can pick the paste up (#2479). The file outlives the
/// send on purpose: the transcript stores the path, and a file deleted at
/// once would leave the message pointing at nothing.
fn write_clipboard_image(image: &(ImageFormat, Vec<u8>)) -> Option<PathBuf> {
    let (format, bytes) = image;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    let path = std::env::temp_dir().join(format!(
        "opencrabs-paste-{stamp}.{}",
        image_format_extension(*format)
    ));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::{
        Attachment, CLIPBOARD_IMAGE_HINT, COPIED_NOTICE, NOTICE_TTL, caret_in_row,
        image_format_extension, line_col, line_count, line_len, line_start, marker_paths,
        message_with_markers, next_word_end, notice_expired, previous_word_start, strip_markers,
        vertical_row_target, write_clipboard_image,
    };
    use gpui::{ImageFormat, px};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// The pack is embedded and used to be unreachable here (#2769): the list
    /// was built from the glyph crate alone, so `:ok:` answered a unicode
    /// character and never our own crab.
    #[test]
    fn the_emoji_list_leads_with_our_pack() {
        let rows = super::Editor::emoji_rows("ok");
        assert_eq!(rows[0].shortcode, "ok");
        assert!(rows[0].crab.is_some(), "the pack row carries its picture");
        assert_eq!(rows[0].glyph, "", "a crab inserts no character");
    }

    /// Every pack entry answers to the name it is stored under, and that name
    /// is the bare shortcode (#2769). The files used to carry a `crab_` prefix
    /// that nothing the user reads or types ever showed, so a row could be
    /// reachable in one place and invisible in the other.
    #[test]
    fn every_pack_name_is_its_bare_shortcode_and_findable() {
        for (name, _) in crate::topic_icons::CRAB_ICONS {
            assert!(!name.starts_with("crab_"), "{name} still has a prefix");
            let rows = super::Editor::emoji_rows(name);
            assert_eq!(rows.first().map(|row| row.shortcode), Some(*name));
            assert!(rows[0].crab.is_some(), "{name} is a picture row");
        }
    }

    /// A query no crab answers still lists glyphs, unchanged by the pack.
    #[test]
    fn the_emoji_list_still_answers_plain_glyphs() {
        let rows = super::Editor::emoji_rows("smile");
        assert!(!rows.is_empty(), "a glyph query still answers");
        assert!(rows.iter().all(|row| row.crab.is_none()));
    }

    /// A crab is an emoji written as text (#2771). Accepting one writes its
    /// shortcode, which the transcript draws as the picture, so nothing reaches
    /// the attachment pipeline and the message keeps its words.
    #[test]
    fn a_crab_is_inserted_as_its_shortcode() {
        for (name, _) in crate::topic_icons::CRAB_ICONS {
            let row = *super::Editor::emoji_rows(name)
                .iter()
                .find(|row| row.shortcode == *name)
                .expect("the pack row is listed");
            assert!(row.crab.is_some(), "{name} is a picture row");
            assert_eq!(super::emoji_insertion(&row), format!(":{name}:"));
        }
        let glyph = super::Editor::emoji_rows("smile")[0];
        assert!(glyph.crab.is_none());
        assert_eq!(super::emoji_insertion(&glyph), glyph.glyph);
    }

    /// A crab shortcode is shaped into a transparent run that keeps its width
    /// (#2794).
    ///
    /// The field paints the pack picture over the box those glyphs hold, so
    /// the run has to cover the token exactly and the runs have to cover the
    /// whole draft. A run of the wrong length moves the caret, the selection
    /// and every wrap offset after it, which is the class of bug the picture
    /// itself would hide.
    #[test]
    fn a_crab_shortcode_keeps_its_width_in_a_transparent_run() {
        let text = "hi :cry: there";
        let runs = super::draft_runs(text, &gpui::font("Helvetica"), gpui::hsla(0., 0., 0., 1.));
        assert_eq!(
            runs.iter().map(|run| run.len).sum::<usize>(),
            text.len(),
            "the runs cover the whole draft"
        );
        assert_eq!(runs.len(), 3, "text, token, text");
        let crab = runs
            .iter()
            .find(|run| run.color == gpui::transparent_black())
            .expect("the crab token is its own run");
        assert_eq!(crab.len, ":cry:".len(), "the run covers the token exactly");
    }

    /// A decoded picture names itself by the bytes it was decoded from
    /// (#2794): the paint pass looks the picture up by shortcode.
    #[test]
    fn a_picture_names_itself_by_address() {
        let bytes = crate::topic_icons::crab_icon("cry").expect("the pack holds cry");
        assert_eq!(crate::topic_icons::crab_name(bytes), Some("cry"));
    }

    /// The pack leads, but it does not grow the list: the eight-row cap is the
    /// terminal's, and a popup that outgrew the composer would cover it.
    #[test]
    fn the_emoji_list_keeps_its_eight_row_cap() {
        let rows = super::Editor::emoji_rows("a");
        assert_eq!(rows.len(), 8);
    }

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

    /// Every composer walks the shared list and writes new entries through
    /// the one file writer the terminal uses, so history is the same in the
    /// window, in a split pane, and after a restart (#2494).
    #[test]
    fn composer_history_is_shared_and_persisted() {
        let src = include_str!("editor.rs");
        let lock = ["lock_history(&self.", "history)"].concat();
        assert!(src.contains(&lock), "recall reads the shared list");
        let write = ["composer::append_history(&self.", "history_path"].concat();
        assert!(src.contains(&write), "a sent entry reaches the shared file");
        let ctor = ["pub fn new_with_", "history("].concat();
        assert!(
            src.contains(&ctor),
            "the handle is injected at construction"
        );
    }

    /// The composer must not re-shape the draft on every frame: the shaping
    /// call sits behind the `shaped_draft` cache, so a caret blink, selection
    /// or streaming answer reuses the last shaping (#2688).
    #[test]
    fn composer_shaping_is_cached_between_frames() {
        let src = include_str!("editor.rs");
        assert!(src.contains("shaped_draft: Option<ShapedDraft>"));
        let prepaint = src
            .split("fn prepaint(")
            .nth(1)
            .and_then(|rest| rest.split("fn paint(").next())
            .expect("editor prepaint");
        assert!(prepaint.contains(".filter(|cache| cache.key == key)"));
        assert!(prepaint.contains("let shaped = match cached"));
        assert!(prepaint.contains("input.shaped_draft = Some(ShapedDraft"));
    }

    /// The layout pass answers its height out of the same cache prepaint
    /// fills. Without that, `request_layout` shaped the whole draft again on
    /// every frame and the cache above bought nothing (#2688).
    #[test]
    fn composer_layout_reads_the_cached_rows() {
        let src = include_str!("editor.rs");
        let layout = src
            .split("fn request_layout(")
            .nth(1)
            .and_then(|rest| rest.split("fn prepaint(").next())
            .expect("editor request layout");
        assert!(
            layout.contains("input.shaped_draft") && layout.contains(".map(|cache| cache.rows)"),
            "request_layout must answer from the shaped cache"
        );
        assert!(
            layout.contains("wrapped_row_count(&content, wrap_width, window)"),
            "the uncached measure survives only as the miss path"
        );
    }

    /// The miss path is the keystroke path, and it used to shape the draft to
    /// count rows and then throw the shaping away, leaving prepaint to shape
    /// the same draft again. Keeping the lines is what makes a typed character
    /// cost one shaping instead of two (#2688).
    #[test]
    fn composer_layout_hands_its_shaping_to_prepaint() {
        let src = include_str!("editor.rs");
        let layout = src
            .split("fn request_layout(")
            .nth(1)
            .and_then(|rest| rest.split("fn prepaint(").next())
            .expect("editor request layout");
        assert!(
            layout.contains("shape_draft(&content, wrap_width, window)"),
            "the miss path shapes through the shared helper"
        );
        assert!(
            layout.contains("input.shaped_draft = Some(ShapedDraft { key, lines, rows })"),
            "the miss path keeps the lines it shaped for prepaint"
        );
    }

    /// The shaped lines are one allocation shared by the cache, the prepaint
    /// state and `last_layout`. Caching the shaping but deep-copying it into
    /// `last_layout` and back out of the cache still copied every glyph of the
    /// draft twice per frame, which is why a long draft stayed heavy after the
    /// shape call itself was cached (#2688).
    #[test]
    fn composer_shares_its_shaped_lines_between_frames() {
        let src = include_str!("editor.rs");
        assert!(
            src.contains("lines: Option<Arc<Vec<WrappedLine>>>"),
            "the cache holds one shaping the frame can share"
        );
        assert!(
            src.contains("input.last_layout = Arc::clone(&layout)"),
            "the frame's layout is shared with the field, not copied into it"
        );
        assert!(
            src.contains("let layout: Arc<Vec<WrappedLine>>"),
            "prepaint hands its layout to paint instead of rebuilding it"
        );
    }

    /// The cached height is the height the shaped lines cover. Shaping that
    /// failed keeps the logical line count, so a long draft never collapses to
    /// one row.
    #[test]
    fn cached_rows_match_the_shaped_lines() {
        assert_eq!(super::shaped_rows(None, "a\nb\nc"), 3);
        assert_eq!(super::shaped_rows(None, ""), 1);
        // A trailing newline is a real empty row the caret can sit on.
        assert_eq!(super::shaped_rows(None, "a\n"), 2);
    }

    /// The box caps its height and scrolls inside itself. Without the cap the
    /// field grew with the draft and painted over the pane below it, which is
    /// the overlap the owner reported on a long paste (#2773).
    #[test]
    fn composer_caps_its_height_and_scrolls_inside_the_box() {
        let src = include_str!("editor.rs");
        // Split so this assertion cannot match its own literal.
        let cap = concat!(
            ".max_h(window.line_height() * ",
            "COMPOSER_MAX_ROWS as f32)"
        );
        let scroll = concat!(
            ".overflow_y_scroll()",
            "\n                        .track_scroll(&"
        );
        // The scroll methods are on a stateful element, so an id has to come
        // first. Without it the gui binary does not compile (#2773).
        let stateful = concat!(
            ".id(\"composer-input\")",
            "\n                        .overflow_y_scroll()"
        );
        assert!(src.contains(cap), "the composer must cap its height");
        assert!(
            src.contains(scroll),
            "the composer must scroll inside itself"
        );
        assert!(
            src.contains(stateful),
            "the scroll box must be stateful or its scroll methods do not exist"
        );
    }

    /// A draft that fits is never scrolled; one that overflows follows the
    /// caret, and never scrolls past its own end.
    #[test]
    fn reveal_scroll_keeps_the_caret_in_the_capped_box() {
        let line = px(22.);
        // Short draft: the box stays at its top edge.
        assert_eq!(
            super::reveal_scroll(px(44.), px(220.), Some(px(0.)), line, px(0.)),
            px(0.)
        );
        // Caret below the fold: scroll just enough to show it.
        assert_eq!(
            super::reveal_scroll(px(440.), px(220.), Some(px(418.)), line, px(0.)),
            px(220.)
        );
        // Caret above the fold: pull the box up to it.
        assert_eq!(
            super::reveal_scroll(px(440.), px(220.), Some(px(22.)), line, px(198.)),
            px(22.)
        );
        // Never past the end of the draft.
        assert_eq!(
            super::reveal_scroll(px(440.), px(220.), None, line, px(999.)),
            px(220.)
        );
    }

    #[test]
    fn drag_copy_composer_keeps_cmd_c_and_adds_ctrl_c() {
        let src = include_str!("editor.rs");
        let bindings = src
            .split("pub fn bind_keys(")
            .nth(1)
            .and_then(|rest| rest.split("// Line math").next())
            .expect("editor key bindings");
        assert!(bindings.contains("KeyBinding::new(\"cmd-c\", Copy, None)"));
        assert!(bindings.contains("KeyBinding::new(\"ctrl-c\", Copy, None)"));
    }

    /// Word motion answers every chord the TUI documents: ctrl and alt on
    /// both arrows, and ctrl or alt backspace deletes the word behind.
    #[test]
    fn word_chords_answer_ctrl_and_alt() {
        let src = include_str!("editor.rs");
        let bindings = src
            .split("pub fn bind_keys(")
            .nth(1)
            .and_then(|rest| rest.split("// Line math").next())
            .expect("editor key bindings");
        for chord in [
            "ctrl-left",
            "alt-left",
            "ctrl-right",
            "alt-right",
            "ctrl-backspace",
            "alt-backspace",
        ] {
            assert!(
                bindings.contains(&format!("KeyBinding::new(\"{chord}\"")),
                "{chord} is not bound"
            );
        }
    }

    /// The single-line hint sits in the middle of the composer, with the
    /// same small inset at the text's left edge and the paperclip's right.
    #[test]
    fn composer_centres_text_and_insets_the_attachment_control() {
        let src = include_str!("editor.rs");
        let render = src
            .split("impl Render for Editor")
            .nth(1)
            .and_then(|rest| rest.split("fn attachment_strip(").next())
            .expect("editor render");
        assert!(render.contains(".items_center()"));
        assert!(render.contains(".pl_1()"));
        let clip = src
            .split("fn paperclip(")
            .nth(1)
            .and_then(|rest| rest.split("/// The keybindings").next())
            .expect("paperclip");
        assert!(clip.contains(".mr_1()"));
    }

    /// A chip's thumbnail and its ✕ are two elements with two ids and two
    /// handlers, so the preview click cannot eat the remove click. Both are
    /// built inside `attachment_strip`, which needs a window to run, so this
    /// pins what that function writes instead of what it renders.
    #[test]
    fn an_attachment_chip_previews_without_stealing_the_remove_click() {
        let src = include_str!("editor.rs");
        // Distinct ids: the two hit areas are addressable separately, so a
        // future selector or test cannot confuse one for the other.
        assert!(
            src.contains("attachment-preview-{index}"),
            "the thumbnail is its own element"
        );
        assert!(
            src.contains("attachment-remove-{index}"),
            "the remove button is its own element"
        );
        assert!(
            src.contains("cx.emit(EditorEvent::Preview(path.clone()));"),
            "the thumbnail hands the path to whoever owns the overlay"
        );
        assert!(
            src.contains("cx.stop_propagation();\n            this.remove_attachment(index, cx);"),
            "the ✕ swallows its click before removing, so neither the chip nor the field fires"
        );
        // The preview handler must not remove, or clicking a picture would
        // detach it.
        let preview_handler = src
            .split("let open_preview = {")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("the preview handler is built in one block");
        assert!(!preview_handler.contains("remove_attachment"));
        // Opened on a completed click, never on the mouse down that starts it: one
        // gesture, one meaning, the same shape a transcript tile uses, and no
        // overlay mounted between a press and its release (#2395).
        assert!(
            src.contains(&[".on_click(open_", "preview)"].concat()),
            "the thumbnail opens on the click that finished, like a tile"
        );
        assert!(
            !src.contains(&[".on_mouse_down(MouseButton::Left, open_", "preview)"].concat()),
            "nothing opens the overlay on the way down"
        );
    }

    /// #2443: the chips stay inside the composer. An `img` that owns its own
    /// size bled over the input's corner and ran past the window, so the tile
    /// is a fixed clipped box, the strip is bounded, and the name is the only
    /// part allowed to shrink.
    #[test]
    fn attachment_chips_stay_inside_the_composer() {
        let src = include_str!("editor.rs");
        let strip = src
            .split("fn attachment_strip(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("attachment_strip is one function");
        assert!(
            strip.contains(".w_full()") && strip.contains(".min_w_0()"),
            "the strip is bounded by the composer instead of its content"
        );
        assert!(
            strip.contains(".overflow_hidden()"),
            "a chip clips its own contents"
        );
        // The picture is drawn inside a box that owns both dimensions; the
        // bare `img(...).w(...).h(...)` shape is what overflowed.
        let image_branch = strip
            .split("if attachment.is_image() {")
            .nth(1)
            .and_then(|rest| rest.split("} else {").next())
            .expect("the image branch is identifiable");
        assert!(
            image_branch.contains("crate::thumbs::picture(&attachment.path"),
            "the picture comes from the square cache, not a cover fit"
        );
        assert!(
            !image_branch.contains("Cover"),
            "a cover fit paints a larger quad than the chip and bleeds (#2796)"
        );
        assert!(
            image_branch.contains(".flex_shrink_0()"),
            "the tile keeps its 34px box when the row runs out of width"
        );
        assert!(
            strip.contains(".truncate()") && strip.contains(".flex_shrink()"),
            "the name is what gives way, not the chip"
        );
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

    /// "one two three four" wrapped at 8 bytes, then a hard newline line.
    fn wrapped_rows() -> Vec<(usize, usize)> {
        vec![(0, 8), (8, 9), (17, 4), (22, 5)]
    }

    #[test]
    fn up_moves_one_visual_row_and_keeps_the_column() {
        let rows = wrapped_rows();
        // From column 3 of row 2 up to column 3 of row 1.
        assert_eq!(vertical_row_target(&rows, 11, -1), 3);
        // Column beyond the destination row clamps to its end.
        assert_eq!(vertical_row_target(&rows, 25, -1), 20);
    }

    #[test]
    fn up_off_the_first_row_goes_to_the_start() {
        let rows = wrapped_rows();
        assert_eq!(vertical_row_target(&rows, 0, -1), 0);
        assert_eq!(vertical_row_target(&rows, 5, -1), 0);
    }

    #[test]
    fn down_moves_one_visual_row_and_keeps_the_column() {
        let rows = wrapped_rows();
        assert_eq!(vertical_row_target(&rows, 3, 1), 11);
        // A caret parked at the end of a row belongs to that row, so Down from
        // the wrap point advances a row instead of re-entering the same one.
        assert_eq!(vertical_row_target(&rows, 8, 1), 16);
    }

    #[test]
    fn down_off_the_last_row_goes_to_the_end() {
        let rows = wrapped_rows();
        let end = rows.last().map(|(start, len)| start + len).unwrap_or(0);
        assert_eq!(end, 27);
        assert_eq!(vertical_row_target(&rows, 27, 1), 27);
        assert_eq!(vertical_row_target(&rows, 24, 1), 27);
        // No layout yet is not a crash: the caret stays home.
        assert_eq!(vertical_row_target(&[], 4, 1), 0);
        assert_eq!(vertical_row_target(&[], 4, -1), 0);
    }

    /// Alt+backspace selects back to the word start and replaces the
    /// selection, so what it removes is exactly the range below. The halves
    /// come from `get` so a boundary that ever came out mid character would
    /// fail the assertion instead of panicking the runner.
    fn delete_word_back_once(text: &str, cursor: usize) -> String {
        let start = previous_word_start(text, cursor);
        let mut kept = String::with_capacity(text.len());
        kept.push_str(text.get(..start).unwrap_or(""));
        kept.push_str(text.get(cursor..).unwrap_or(""));
        kept
    }

    #[test]
    fn alt_left_stops_at_a_word_boundary() {
        let text = "alpha beta gamma";
        assert_eq!(previous_word_start(text, text.len()), 11);
        assert_eq!(previous_word_start(text, 11), 6);
        assert_eq!(previous_word_start(text, 0), 0);
        // Separators before the word are skipped, not counted as the word.
        assert_eq!(previous_word_start("alpha   ", 8), 0);
        // Multibyte text is counted in utf8 bytes, never split mid character.
        assert_eq!(previous_word_start("olá mundo", "olá mundo".len()), 5);
    }

    #[test]
    fn alt_right_lands_on_the_next_word() {
        let text = "alpha beta gamma";
        assert_eq!(next_word_end(text, 0), 6);
        assert_eq!(next_word_end(text, 6), 11);
        assert_eq!(next_word_end(text, text.len()), text.len());
        assert_eq!(next_word_end("olá mundo", 0), 5);
    }

    #[test]
    fn alt_backspace_removes_a_whole_word() {
        assert_eq!(delete_word_back_once("alpha beta gamma", 16), "alpha beta ");
        assert_eq!(delete_word_back_once("alpha beta gamma", 6), "beta gamma");
        assert_eq!(delete_word_back_once("olá mundo", 10), "olá ");
        // A caret at the very start has nothing to remove and must not panic.
        assert_eq!(delete_word_back_once("alpha", 0), "alpha");
    }

    #[test]
    fn an_empty_field_paints_a_caret_at_the_start_of_its_row() {
        // A focused, empty composer shows the placeholder and nothing else
        // unless the caret paints, so the field looks dead (#2446).
        assert_eq!(caret_in_row(true, &(0..0), 0, 0, 0), Some(0));
        assert_eq!(caret_in_row(true, &(0..0), 0, 0, 12), Some(0));
    }

    #[test]
    fn the_caret_lands_only_in_the_row_that_holds_it() {
        // "one\ntwo": row two starts at byte 4, so a caret at 4 is row two's.
        assert_eq!(caret_in_row(false, &(0..0), 2, 0, 3), Some(2));
        assert_eq!(caret_in_row(false, &(0..0), 5, 4, 7), Some(1));
        assert_eq!(caret_in_row(false, &(0..0), 4, 0, 3), None);
    }

    #[test]
    fn a_selection_takes_the_row_over_from_the_caret() {
        // Selecting text paints the selection; a caret on top of it would
        // draw two marks for one position.
        assert_eq!(caret_in_row(false, &(2..5), 5, 0, 7), None);
    }

    /// #2479: a screenshot copies picture bytes with no text next to them.
    /// The paste writes those bytes to a scratch file so the path-based
    /// attachment pipeline can carry it, the same door a Finder drop takes.
    #[test]
    fn a_clipboard_image_lands_on_disk_for_the_attachment_door() {
        let bytes = b"\x89PNG\r\n\x1a\nfake".to_vec();
        let path = write_clipboard_image(&(ImageFormat::Png, bytes.clone()))
            .expect("the image reaches a file");
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(
            std::fs::read(&path).expect("the file holds the bytes"),
            bytes
        );
        assert!(
            path.starts_with(std::env::temp_dir()),
            "a clipboard paste never writes into the checkout"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The format the clipboard reports decides the suffix, so a browser
    /// handover is not saved as a mislabelled `.png` (#2479).
    #[test]
    fn every_clipboard_image_format_maps_to_its_own_extension() {
        let pairs = [
            (ImageFormat::Png, "png"),
            (ImageFormat::Jpeg, "jpg"),
            (ImageFormat::Webp, "webp"),
            (ImageFormat::Gif, "gif"),
            (ImageFormat::Svg, "svg"),
            (ImageFormat::Bmp, "bmp"),
            (ImageFormat::Tiff, "tiff"),
        ];
        let mut seen = Vec::new();
        for (format, want) in pairs {
            assert_eq!(image_format_extension(format), want);
            assert!(!seen.contains(&want), "{want} is used by two formats");
            seen.push(want);
        }
    }

    /// A paste with text beside the picture stays a text paste: dropping the
    /// typed words would swallow what the user copied (#2479).
    #[test]
    fn the_paste_reads_an_image_only_clipboard() {
        let src = include_str!("editor.rs");
        let helper = src
            .split("pub fn clipboard_image(")
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .expect("the clipboard read is one helper");
        assert!(
            helper.contains("if item.text().is_some()"),
            "text wins over the picture, like every paste before this"
        );
        let paste = src
            .split("fn paste(&mut self, _: &Paste")
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .expect("paste is one function");
        assert!(
            paste.contains("Self::clipboard_image(cx)"),
            "the paste asks the clipboard for a picture first"
        );
        assert!(
            paste.contains("self.attach_paths([path]) > 0"),
            "the file goes through the same attachment door a drop uses"
        );
        assert!(
            paste.find("attach_paths").unwrap_or(usize::MAX)
                < paste.find("read_from_clipboard").unwrap_or(usize::MAX),
            "an attached picture returns before the text branch is reached"
        );
    }

    /// The hint lives in the composer, not a toast: an empty field says a
    /// screenshot is waiting, and it says which key pastes it (#2479).
    #[test]
    fn the_composer_names_the_waiting_clipboard_image() {
        assert!(
            CLIPBOARD_IMAGE_HINT.contains("Image in clipboard"),
            "the hint names what is waiting: {CLIPBOARD_IMAGE_HINT}"
        );
        assert!(
            CLIPBOARD_IMAGE_HINT.contains("V to paste"),
            "the hint names the key that pastes it: {CLIPBOARD_IMAGE_HINT}"
        );
        let src = include_str!("editor.rs");
        assert!(
            src.contains("if empty && bare && Editor::clipboard_image(cx).is_some()"),
            "the hint only stands in while the field is empty and bare"
        );
    }

    /// A copy is acknowledged on the terminal's own clock, so the same gesture
    /// cannot read as instant in one surface and sticky in the other (#2587).
    #[test]
    fn copy_notice_expires_on_the_terminal_two_second_clock() {
        let shown = Instant::now();
        assert_eq!(NOTICE_TTL, Duration::from_secs(2), "the terminal's TTL");
        assert!(!notice_expired(shown, shown, NOTICE_TTL), "fresh");
        assert!(
            !notice_expired(shown, shown + Duration::from_millis(1999), NOTICE_TTL),
            "still up just before the second"
        );
        assert!(
            notice_expired(shown, shown + NOTICE_TTL, NOTICE_TTL),
            "gone on the second"
        );
        assert!(
            notice_expired(shown, shown + Duration::from_secs(9), NOTICE_TTL),
            "and gone for good"
        );
        // A clock that went backwards must not hold a notice forever.
        assert!(!notice_expired(
            shown,
            shown - Duration::from_secs(1),
            NOTICE_TTL
        ));
    }

    /// The words and the slot: a copy says the terminal's line, an empty field
    /// owns it, and a draft gets one row under the text (#2587).
    #[test]
    fn copy_notice_says_the_terminal_line_and_picks_its_slot() {
        assert_eq!(COPIED_NOTICE, "Copied to clipboard");
        assert_ne!(
            COPIED_NOTICE, CLIPBOARD_IMAGE_HINT,
            "a copy must not offer a paste that has no image behind it"
        );
        let src = include_str!("editor.rs");
        let (_, after) = src
            .split_once(
                ["input.", "active_notice(Instant::now())"]
                    .concat()
                    .as_str(),
            )
            .expect("the placeholder slot reads the live notice");
        let (slot, _) = after.split_once("\n        let style").expect("slot ends");
        assert!(
            slot.contains(["Some(notice) => ", "notice,"].concat().as_str()),
            "an empty field shows the notice where the name was"
        );
        assert!(
            src.contains(
                ["body = body.child(", "notice_row(&notice))"]
                    .concat()
                    .as_str()
            ),
            "a draft gets the notice on its own row under the text"
        );
        assert!(
            src.contains(
                ["cx.background_executor().", "timer(NOTICE_TTL)"]
                    .concat()
                    .as_str()
            ),
            "and the line clears itself on a timer"
        );
    }
}
