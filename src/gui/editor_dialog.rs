//! The built-in editor the bang operator opens (#3163).
//!
//! `!vi notes.md` used to hand the file to Terminal.app, which took the file
//! out of the window the user asked for it in. The window owns the buffer now:
//! one dialog per conversation, keyed by the session that asked for it, so a
//! split keeps every pane's text apart and the dialog lands on the pane that
//! ran the command. The shared parser (`ui_core::bang`) decides WHAT was
//! asked; this module decides what the window does about it.
//!
//! The state sits on the window rather than in a `PaneView` entity because the
//! dialog is a window-level modal like the preview: it covers the panes, so a
//! pane's own cache path would never repaint under it. Keying the map by
//! session keeps the isolation the panes had anyway; the focused id decides
//! which one draws and which one answers a keystroke.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gpui::{
    AnyElement, App, BorderStyle, Bounds, ClickEvent, ClipboardItem, Context, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta,
    ScrollHandle, ScrollWheelEvent, Window, canvas, div, fill, font, point, prelude::*, px, rgb,
    rgba, size,
};
use opencrabs::ui_core::bang::EditorCommand;
use opencrabs::ui_core::card::concentric_radius;
use opencrabs::ui_core::editor_buffer::EditorBuffer;
use opencrabs::ui_core::local_document::DocumentKind;
use opencrabs::ui_core::scroll::vertical_scroll_thumb;
use uuid::Uuid;

use crate::agent_bridge;
use crate::logging;
use crate::markdown::{self, MdBlock};
use crate::modal_shell;
use crate::palette;
use crate::transcript;

/// How many buffer lines the dialog draws around the caret. The buffer can
/// hold far more than a screen, and a full repaint of every line on every
/// keystroke is the cost the panes just escaped (#2805), so the dialog draws a
/// window that follows the caret instead of the whole file.
const VISIBLE_LINES: usize = 30;

/// Which face of the dialog is up. A file the user opens to look at shows as
/// a document, the way the preview reads it; the raw text only comes up once
/// the user asks to edit, and goes back to the document the moment a save
/// lands (#3163).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorMode {
    /// The file rendered as rich markdown, read-only. The default.
    RichPreview,
    /// The raw text: caret, selection, typing, scrolling.
    RawEdit,
}

/// What a click needs to land on the right character: the text column's
/// bounds in window coordinates, and how wide one character of the mono font
/// renders at the editor's fixed size. The render pass fills it through a
/// canvas that draws nothing, and the mouse handlers read it, so the two
/// never have to agree on a layout neither can see (#3163).
#[derive(Clone)]
pub(crate) struct EditorHitGrid {
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) char_width: f32,
}

/// The raw view's fixed text geometry. Rendering and hit testing share these
/// constants, so a click's pixel offset always divides out to the character
/// the user aimed at, whatever the theme or rem size does (#3163). The
/// padding and border are explicit pixels rather than `py_1()`/`border_1()`
/// shorthands precisely so the height sum below and the hit test cannot
/// drift apart when zoom changes the rem size (#3163).
const RAW_TEXT_SIZE: f32 = 13.0;
const RAW_LINE_HEIGHT: f32 = 20.0;
const RAW_GUTTER_WIDTH: f32 = 48.0;
const RAW_PADDING: f32 = 4.0;
const RAW_BORDER: f32 = 1.0;

/// One conversation's editor: its text, its path, and what the user is
/// mid-way through doing to it.
pub(crate) struct EditorDialog {
    /// The editor program as the user typed it, for the title.
    pub(crate) program: String,
    /// The file behind the buffer, resolved absolute. `None` for a bare
    /// `!vi`, which is an empty buffer until the first save names a file.
    pub(crate) path: Option<PathBuf>,
    pub(crate) buffer: EditorBuffer,
    /// `-R` on the command, or a file that is not valid UTF-8: an edit here
    /// would corrupt bytes the buffer cannot represent, so the dialog draws
    /// but refuses every write.
    pub(crate) read_only: bool,
    /// Whether the buffer has changed since the last save.
    pub(crate) dirty: bool,
    /// Flags the window does not act on (`-u NONE`), shown so nothing typed
    /// is silently swallowed (#3163).
    pub(crate) unsupported_flags: Vec<String>,
    /// The line a `+N` asked for, consumed once on the first paint.
    pub(crate) pending_line: Option<u32>,
    /// A load in flight or failed. The text arrives on the background
    /// executor, so the dialog must be able to show itself before it arrives.
    pub(crate) loading: bool,
    /// The load's failure, if any.
    pub(crate) load_error: Option<String>,
    /// The name being typed for a first save. `Some` while the prompt is up.
    pub(crate) save_prompt: Option<String>,
    /// The user pressed Esc on an unsaved buffer; one more Esc discards.
    pub(crate) confirm_discard: bool,
    /// The last thing the dialog told the user: saved here, read-only, a
    /// failed save. Cleared by the next action that answers for itself.
    pub(crate) status: Option<String>,
    /// Which file load this dialog is waiting on. A generation rejects a
    /// response for a file the user has already closed or replaced.
    pub(crate) generation: u64,
    /// Which face is up. See [`EditorMode`].
    pub(crate) mode: EditorMode,
    /// First visible line of the raw view, counted from one. The wheel moves
    /// it, caret motion drags it along, and the render draws the lines that
    /// start here instead of re-deriving the window from the caret every
    /// frame (#3163).
    pub(crate) raw_scroll: usize,
    /// The click-to-caret grid, filled by the render pass and read by the
    /// mouse handlers. It is shared rather than owned so a handler built in
    /// the render pass can hold a clone of it without borrowing the dialog
    /// the handler is about to mutate (#3163). `None` until the first paint
    /// of the raw view.
    pub(crate) hit_grid: Arc<Mutex<Option<EditorHitGrid>>>,
    /// The rich preview's own scroller, so the rendered document wheels like
    /// the preview modal does instead of standing still (#3163).
    pub(crate) rich_scroll: ScrollHandle,
    /// How many raw rows the body can show at the current card size. The
    /// render pass measures it from the viewport, and the caret-sync uses it
    /// without a window at hand (#3163).
    pub(crate) rows_visible: usize,
    /// A left press is down inside the raw view and the mouse is dragging a
    /// selection: moves extend the selection until the release (#3163).
    pub(crate) dragging: bool,
    /// The rich preview's parsed blocks, keyed by the text they were parsed
    /// from. Parsing runs once per text change, which only happens in raw
    /// edit mode, so wheeling or reading the document is a cache hit (#3163).
    pub(crate) preview_cache: Option<(String, Vec<MdBlock>)>,
}

impl EditorDialog {
    fn empty(command: &EditorCommand, path: Option<PathBuf>) -> Self {
        Self {
            program: command.program.clone(),
            path,
            buffer: EditorBuffer::new(),
            read_only: command.read_only,
            dirty: false,
            unsupported_flags: command
                .flags
                .iter()
                .filter(|flag| flag.as_str() != "--")
                .cloned()
                .collect(),
            pending_line: command.line,
            loading: false,
            load_error: None,
            save_prompt: None,
            confirm_discard: false,
            status: None,
            generation: 0,
            mode: EditorMode::RichPreview,
            raw_scroll: 1,
            hit_grid: Arc::new(Mutex::new(None)),
            rich_scroll: ScrollHandle::new(),
            rows_visible: VISIBLE_LINES,
            dragging: false,
            preview_cache: None,
        }
    }

    /// Pull the raw view's line window along so the caret stays inside it.
    /// Every caret motion goes through here, so a key at the window's edge or
    /// a drag past it always shows the line the user is on (#3163).
    fn scroll_to_caret(&mut self) {
        let (line, _) = self.buffer.line_col();
        let rows = self.rows_visible.max(1);
        if line < self.raw_scroll {
            self.raw_scroll = line;
        } else if line >= self.raw_scroll + rows {
            self.raw_scroll = line + 1 - rows;
        }
        let last_first = self.buffer.line_count().saturating_sub(rows - 1).max(1);
        self.raw_scroll = self.raw_scroll.clamp(1, last_first);
    }
}

/// Read a file for the buffer, off the UI thread. A file whose bytes are not
/// valid UTF-8 comes back with `read_only`, because a buffer that cannot
/// represent every byte must never be allowed to rewrite the ones it cannot
/// see: saving after an edit would corrupt the file (#3163).
pub(crate) fn load_file(path: PathBuf) -> Result<(String, bool), String> {
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok((text, false)),
        Err(_) => Ok((String::new(), true)),
    }
}

/// Write the buffer's text back to its file. A synchronous write on purpose:
/// the save is the user's one deliberate act and the dialog answers the chord
/// with the result in the same frame, so "did it save" is never a guess.
pub(crate) fn write_file(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|error| error.to_string())
}

impl super::Dashboard {
    /// Open the built-in editor for the conversation that asked for it
    /// (#3163). The id travels on the request, so a split shows the buffer of
    /// the pane that ran `!vi`, and the file is read against that pane's own
    /// working directory.
    pub(crate) fn open_editor(
        &mut self,
        session: Option<Uuid>,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = session.or(self.chat_state.session()) else {
            self.turn
                .live
                .notices
                .push("editor: no conversation to open it in, start or pick one first".to_string());
            cx.notify();
            return;
        };
        // The session's own directory decides where a relative path points.
        // The process's start directory is a guess that has nothing to do
        // with the conversation the user cd'd around in.
        let cwd = agent_bridge::bridge()
            .map(|b| b.service.get_working_directory_for_session(id))
            .unwrap_or_else(std::env::temp_dir);
        let path = command.path.as_ref().map(|raw| {
            let raw = Path::new(raw.as_str());
            if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                cwd.join(raw)
            }
        });
        let generation = self.editor_generation.wrapping_add(1);
        self.editor_generation = generation;
        let mut dialog = EditorDialog::empty(&command, path.clone());
        dialog.generation = generation;
        let exists = path.as_ref().is_some_and(|p| p.is_file());
        dialog.loading = exists;
        if !exists {
            // A file that is not there yet is a new buffer, the way `vi
            // notes.md` on a shell opens an empty file named notes.md. The
            // status says so once, so an empty screen is never mistaken for
            // a failed read.
            dialog.status = Some("new file".to_string());
        }
        self.editor_dialogs.insert(id, dialog);
        self.editor_focus = Some(id);
        self.refocus_overlay();
        cx.notify();
        logging::info(&format!(
            "editor: open for {id} {:?}",
            self.editor_dialogs.get(&id).and_then(|d| d.path.clone())
        ));
        if exists {
            let load_path = path.clone().expect("exists implies a path");
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { load_file(load_path.clone()) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.editor_load_done(id, generation, result, cx);
                });
            })
            .detach();
        }
    }

    /// A background load came back. It lands only when the dialog on top is
    /// still this file's: closing the editor or opening another file must not
    /// have a late read write over it (#2395, the preview's own generation
    /// rule).
    fn editor_load_done(
        &mut self,
        session: Uuid,
        generation: u64,
        result: Result<(String, bool), String>,
        cx: &mut Context<Self>,
    ) {
        let on_top = self.editor_focus == Some(session);
        let Some(dialog) = self.editor_dialogs.get_mut(&session) else {
            return;
        };
        if !on_top || dialog.generation != generation {
            return;
        }
        match result {
            Ok((text, binary)) => {
                dialog.loading = false;
                dialog.read_only = dialog.read_only || binary;
                dialog.buffer.set_text(&text);
                if let Some(line) = dialog.pending_line.take() {
                    dialog.buffer.goto_line(line as usize);
                }
                if binary {
                    // A binary file has nothing to preview as text, so it
                    // goes straight to the raw face rather than rendering a
                    // document out of bytes it cannot parse (#3163).
                    dialog.mode = EditorMode::RawEdit;
                    dialog.status = Some("not valid text, opened read-only".to_string());
                } else {
                    dialog.status = None;
                }
                dialog.preview_cache = None;
                dialog.scroll_to_caret();
            }
            Err(error) => {
                dialog.loading = false;
                dialog.load_error = Some(error);
            }
        }
        cx.notify();
    }

    /// Flip the focused dialog between the rich document and the raw text.
    /// A prompt, a load in flight, or a failed or read-only file all keep
    /// their face: the toggle is only for real text the user can act on
    /// (#3163).
    pub(crate) fn toggle_editor_mode(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.editor_focus else {
            return;
        };
        let Some(dialog) = self.editor_dialogs.get_mut(&id) else {
            return;
        };
        if dialog.loading
            || dialog.load_error.is_some()
            || dialog.save_prompt.is_some()
            || dialog.read_only
        {
            return;
        }
        dialog.mode = match dialog.mode {
            EditorMode::RichPreview => EditorMode::RawEdit,
            EditorMode::RawEdit => EditorMode::RichPreview,
        };
        if dialog.mode == EditorMode::RawEdit {
            dialog.scroll_to_caret();
        }
        logging::info(&format!("editor: face {:?} for {id}", dialog.mode));
        cx.notify();
    }

    /// Land the caret on the character under a window point, using the hit
    /// grid the render pass recorded. The guards mirror the keyboard's: a
    /// click never edits while the load is in flight, failed, behind a
    /// prompt, or in a read-only buffer (#3163).
    fn editor_caret_to(&mut self, position: &Point<Pixels>, select: bool) {
        let Some(id) = self.editor_focus else {
            return;
        };
        let (grid, raw_scroll) = {
            let Some(dialog) = self.editor_dialogs.get(&id) else {
                return;
            };
            if dialog.mode != EditorMode::RawEdit
                || dialog.loading
                || dialog.load_error.is_some()
                || dialog.save_prompt.is_some()
                || dialog.read_only
            {
                return;
            }
            (dialog.hit_grid.clone(), dialog.raw_scroll)
        };
        let Some(grid) = grid.lock().ok().and_then(|guard| guard.clone()) else {
            return;
        };
        if !grid.bounds.contains(position) {
            return;
        }
        let rel_x = f32::from(position.x - grid.bounds.left());
        // The canvas bounds are the raw view's padding box, so the text rows
        // start exactly RAW_PADDING below the bounds' top. Without this the
        // last four pixels of every line would read as the next line down
        // (#3163).
        let rel_y = (f32::from(position.y - grid.bounds.top()) - RAW_PADDING).max(0.0);
        let row = (rel_y / RAW_LINE_HEIGHT).floor() as usize;
        let column = ((rel_x - RAW_GUTTER_WIDTH) / grid.char_width.max(1.0))
            .round()
            .max(0.0) as usize
            + 1;
        let Some(dialog) = self.editor_dialogs.get_mut(&id) else {
            return;
        };
        dialog
            .buffer
            .goto_line_col(raw_scroll + row, column, select);
        dialog.scroll_to_caret();
    }

    /// A left press in the raw view: land the caret on the clicked
    /// character and arm the drag-select (#3163).
    pub(crate) fn editor_mouse_down(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        if ev.button != MouseButton::Left {
            return;
        }
        self.editor_caret_to(&ev.position, false);
        let Some(id) = self.editor_focus else {
            return;
        };
        if let Some(dialog) = self.editor_dialogs.get_mut(&id) {
            dialog.dragging = true;
        }
        cx.notify();
    }

    /// A press moving over the raw view extends the selection while a drag
    /// is live, following the mouse line by line (#3163).
    pub(crate) fn editor_mouse_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(id) = self.editor_focus else {
            return;
        };
        let dragging = self
            .editor_dialogs
            .get(&id)
            .is_some_and(|d| d.dragging && d.mode == EditorMode::RawEdit);
        if !dragging {
            return;
        }
        self.editor_caret_to(&ev.position, true);
        cx.notify();
    }

    /// The drag ends: further moves leave the selection alone (#3163).
    pub(crate) fn editor_mouse_up(&mut self, _ev: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(id) = self.editor_focus else {
            return;
        };
        if let Some(dialog) = self.editor_dialogs.get_mut(&id) {
            dialog.dragging = false;
        }
        cx.notify();
    }

    /// The wheel inside the raw view moves the line window a few rows per
    /// notch, in whichever direction the platform delta leans, and a tiny
    /// trackpad delta still pays one row (#3163).
    pub(crate) fn editor_wheel(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(id) = self.editor_focus else {
            return;
        };
        let Some(dialog) = self.editor_dialogs.get_mut(&id) else {
            return;
        };
        if dialog.mode != EditorMode::RawEdit {
            return;
        }
        let delta_y = match ev.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y),
            ScrollDelta::Lines(delta) => delta.y,
        };
        let mut shift = (delta_y / RAW_LINE_HEIGHT).trunc() as i32;
        if shift == 0 && delta_y != 0.0 {
            shift = delta_y.signum() as i32;
        }
        let rows = dialog.rows_visible.max(1) as i32;
        let last_first = (dialog.buffer.line_count() as i32 - rows + 1).max(1);
        dialog.raw_scroll = (dialog.raw_scroll as i32 - shift).clamp(1, last_first) as usize;
        cx.notify();
    }

    /// Close the focused editor and hand the keyboard back (#3163).
    pub(crate) fn close_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.editor_focus.take() {
            self.editor_dialogs.remove(&id);
            self.editor_generation = self.editor_generation.wrapping_add(1);
            logging::info(&format!("editor: closed for {id}"));
        }
        self.refocus_screen();
        cx.notify();
    }

    /// Write the focused dialog's buffer to its file (#3163).
    pub(crate) fn save_editor(&mut self, session: &Uuid, cx: &mut Context<Self>) {
        let Some(dialog) = self.editor_dialogs.get_mut(session) else {
            return;
        };
        let Some(path) = dialog.path.clone() else {
            // A buffer with no file behind it cannot save into nothing, so
            // the chord opens the name prompt instead. Enter there picks up
            // the save.
            dialog.save_prompt = Some(String::new());
            cx.notify();
            return;
        };
        if dialog.read_only {
            dialog.status = Some("read-only".to_string());
            cx.notify();
            return;
        }
        let text = dialog.buffer.text().to_string();
        match write_file(&path, &text) {
            Ok(()) => {
                dialog.dirty = false;
                // Back to the document face: the raw edit is done the moment
                // the file lands, which is the flow the report asked for
                // (#3163).
                dialog.mode = EditorMode::RichPreview;
                // Drop the cache: the text just changed, so the next paint
                // re-parses the document out of the saved buffer (#3163).
                dialog.preview_cache = None;
                dialog.status = Some(format!("saved {}", path.display()));
                logging::info(&format!("editor: saved {}", path.display()));
            }
            Err(error) => {
                dialog.status = Some(format!("save failed: {error}"));
            }
        }
        cx.notify();
    }

    /// Confirm the save-as prompt: resolve the typed name against the
    /// session's own directory and save there (#3163).
    fn save_editor_as(&mut self, session: &Uuid, name: &str, cx: &mut Context<Self>) {
        let trimmed = name.trim().to_string();
        let Some(dialog) = self.editor_dialogs.get_mut(session) else {
            return;
        };
        if trimmed.is_empty() {
            dialog.status = Some("give the file a name first".to_string());
            dialog.save_prompt = Some(String::new());
            cx.notify();
            return;
        }
        let cwd = agent_bridge::bridge()
            .map(|b| b.service.get_working_directory_for_session(*session))
            .unwrap_or_else(std::env::temp_dir);
        let raw = Path::new(&trimmed);
        let resolved = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            cwd.join(raw)
        };
        dialog.path = Some(resolved);
        dialog.save_prompt = None;
        self.save_editor(session, cx);
    }

    /// Every keystroke the editor dialog owns while it is open. It reads
    /// ahead of the composer and every pane, the way the preview and the
    /// settings modal do: a dialog that lets a character through to a field
    /// underneath is the bug class #2455 came from.
    pub(crate) fn editor_key(
        &mut self,
        ev: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &ev.keystroke;
        let key = ks.key.as_str();
        let primary = ks.modifiers.platform || ks.modifiers.control;
        let Some(id) = self.editor_focus else {
            return;
        };
        // The save chord first, so it works from every state the dialog can
        // be in, prompt included.
        if primary && !ks.modifiers.alt && key == "s" {
            self.save_editor(&id, cx);
            return;
        }
        if !self.editor_dialogs.contains_key(&id) {
            self.editor_focus = None;
            return;
        }
        if key == "escape" {
            // The prompt closes first, then the discard guard, then the
            // dialog itself: one Esc never throws away typed text on its own.
            // The close decision is taken while the dialog is borrowed and
            // run after the borrow ends, so `self` is free for it and no
            // reference has to be "dropped" to fake a release.
            let close = {
                let Some(dialog) = self.editor_dialogs.get_mut(&id) else {
                    return;
                };
                if dialog.save_prompt.take().is_some() {
                    cx.notify();
                    false
                } else if dialog.dirty && !dialog.confirm_discard {
                    dialog.confirm_discard = true;
                    dialog.status = Some("unsaved changes, esc again to discard".to_string());
                    cx.notify();
                    false
                } else {
                    true
                }
            };
            if close {
                self.close_editor(cx);
            }
            return;
        }
        // While the name prompt is up, the typing belongs to it: the buffer
        // underneath must not take a character meant for the filename. The
        // typed name is read inside the borrow and the save runs after the
        // borrow ends, which is what needs `self` back.
        let prompt_up = self
            .editor_dialogs
            .get(&id)
            .is_some_and(|d| d.save_prompt.is_some());
        if prompt_up {
            let save_as = {
                let Some(dialog) = self.editor_dialogs.get_mut(&id) else {
                    return;
                };
                match key {
                    "enter" => Some(dialog.save_prompt.clone().unwrap_or_default()),
                    "backspace" => {
                        if let Some(name) = dialog.save_prompt.as_mut() {
                            name.pop();
                        }
                        cx.notify();
                        None
                    }
                    _ => {
                        if !primary
                            && !ks.modifiers.alt
                            && let Some(ch) = ks.key_char.as_ref().and_then(|s| s.chars().next())
                            && !ch.is_control()
                            && let Some(name) = dialog.save_prompt.as_mut()
                        {
                            name.push(ch);
                            cx.notify();
                        }
                        None
                    }
                }
            };
            if let Some(typed) = save_as {
                self.save_editor_as(&id, &typed, cx);
            }
            return;
        }
        let Some(dialog) = self.editor_dialogs.get_mut(&id) else {
            self.editor_focus = None;
            return;
        };
        // Movement answers even in read-only and even while a load is in
        // flight, because looking around a file never writes anything.
        let select = ks.modifiers.shift;
        match key {
            "left" => dialog.buffer.move_left(select),
            "right" => dialog.buffer.move_right(select),
            "up" => dialog.buffer.move_up(select),
            "down" => dialog.buffer.move_down(select),
            "home" => dialog.buffer.line_start(select),
            "end" => dialog.buffer.line_end(select),
            "a" if primary && !ks.modifiers.alt => dialog.buffer.select_all(),
            "c" if primary && !ks.modifiers.alt => {
                if let Some(text) = dialog.buffer.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                }
            }
            _ if dialog.loading || dialog.read_only => {
                // Edits are refused while the text is still arriving and in a
                // buffer the window promised not to rewrite. The status says
                // why rather than eating the keystroke silently.
                if dialog.read_only && !dialog.loading {
                    dialog.status = Some("read-only, cannot edit".to_string());
                    cx.notify();
                }
                return;
            }
            "enter" => {
                dialog.buffer.insert("\n");
                dialog.dirty = true;
                dialog.confirm_discard = false;
                cx.notify();
            }
            "tab" => {
                dialog.buffer.insert("\t");
                dialog.dirty = true;
                dialog.confirm_discard = false;
                cx.notify();
            }
            "backspace" => {
                if dialog.buffer.backspace() {
                    dialog.dirty = true;
                    dialog.confirm_discard = false;
                    cx.notify();
                }
            }
            "delete" => {
                if dialog.buffer.delete_forward() {
                    dialog.dirty = true;
                    dialog.confirm_discard = false;
                    cx.notify();
                }
            }
            _ => {
                if !primary
                    && !ks.modifiers.alt
                    && let Some(ch) = ks.key_char.as_ref().and_then(|s| s.chars().next())
                    && dialog.buffer.type_char(ch)
                {
                    dialog.dirty = true;
                    dialog.confirm_discard = false;
                    cx.notify();
                }
            }
        }
        // The raw view's line window follows the caret on every motion, and
        // the movement arms above never repaint by themselves, so one sync
        // and one notify here cover the whole match (#3163).
        dialog.scroll_to_caret();
        cx.notify();
    }
}

/// One gesture listener from the window, boxed so the editor's draw code can
/// carry the window's answers without naming six closure types. The shape is
/// the one `command_palette`'s `PickHandler` carries for its row presses,
/// written as an alias because a `Box<dyn Fn(...)>` spelled out in a struct
/// field is what clippy counts as a very complex type (#3163).
pub(crate) type EditorHandler<E> = Box<dyn Fn(&E, &mut Window, &mut App) + 'static>;

/// The window's answers to the editor's gestures, carried as one value.
/// Each listener used to be its own `impl Fn` parameter, which had the draw
/// call eight arguments deep; clippy's `too_many_arguments` (seven) is the
/// lint that says a function has stopped being readable, and the honest fix
/// is one argument per concern, not a silencing allow. Boxing each handler
/// keeps the struct one concrete type instead of six generic ones: the
/// dashboard's `cx.listener` closures are already `'static` and `Fn`, and
/// the same boxed shape is what `command_palette`'s `PickHandler` carries
/// for its row presses (#3163).
pub(crate) struct EditorHandlers {
    pub(crate) dismiss: EditorHandler<ClickEvent>,
    pub(crate) toggle_mode: EditorHandler<ClickEvent>,
    pub(crate) mouse_down: EditorHandler<MouseDownEvent>,
    pub(crate) mouse_move: EditorHandler<MouseMoveEvent>,
    pub(crate) mouse_up: EditorHandler<MouseUpEvent>,
    pub(crate) wheel: EditorHandler<ScrollWheelEvent>,
}

/// The dialog itself, drawn with the window's shared modal chrome so the
/// editor reads as one of the window's surfaces instead of a page pasted in.
/// The card is measured from the live viewport instead of guessing a fixed
/// size, which is what let the old one run off the bottom-right corner and
/// clip (#3163). The handlers are the Dashboard's listeners, bundled into
/// one struct and passed in so this free function stays free of a `self`
/// borrow (#3163).
pub(crate) fn overlay(
    dialog: &mut EditorDialog,
    window: &Window,
    handlers: EditorHandlers,
) -> AnyElement {
    let EditorHandlers {
        dismiss,
        toggle_mode,
        mouse_down,
        mouse_move,
        mouse_up,
        wheel,
    } = handlers;
    let shown = palette::current();
    let viewport = window.viewport_size();
    // The scrim pads 24 on each side and the card 20 more, so the usable
    // area is the viewport minus that chrome. The clamps keep an extreme
    // window size from squeezing the card into unusability, and the measured
    // body height is what decides how many raw rows the dialog can show
    // (#3163).
    let width = (f32::from(viewport.width) - 120.0).clamp(420.0, 1040.0);
    let height = (f32::from(viewport.height) - 120.0).clamp(320.0, 760.0);
    let body_height = (height - 170.0).max(160.0);
    let rows_visible =
        (((body_height - 24.0) / RAW_LINE_HEIGHT).floor() as usize).clamp(6, VISIBLE_LINES);
    dialog.rows_visible = rows_visible;
    let named = dialog
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "[no name]".to_string());
    let dirty_mark = if dialog.dirty { " *" } else { "" };
    let mut subtitle = if dialog.loading {
        "loading...".to_string()
    } else if let Some(error) = &dialog.load_error {
        format!("could not read: {error}")
    } else if dialog.mode == EditorMode::RawEdit {
        let (line, col) = dialog.buffer.line_col();
        format!("{}:{}, {}", line, col, dialog.buffer.text().chars().count())
    } else {
        let chars = dialog.buffer.text().chars().count();
        format!("{} characters", chars)
    };
    if dialog.read_only {
        subtitle.push_str(" · read-only");
    }
    if !dialog.unsupported_flags.is_empty() {
        subtitle.push_str(&format!(
            " · ignored: {}",
            dialog.unsupported_flags.join(" ")
        ));
    }
    if let Some(status) = &dialog.status {
        subtitle.push_str(&format!(" · {status}"));
    }
    let body = if dialog.save_prompt.is_some() {
        let typed = dialog.save_prompt.clone().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(shown.text_muted))
                    .child("name the file to save into"),
            )
            .child(
                div()
                    .font_family(crate::fonts::current_mono())
                    .text_sm()
                    .text_color(rgb(shown.text))
                    .px_2()
                    .py_1()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(shown.track))
                    .bg(rgb(shown.panel_hi))
                    .child(if typed.is_empty() {
                        "file name...".to_string()
                    } else {
                        typed
                    }),
            )
            .into_any_element()
    } else if let Some(error) = &dialog.load_error {
        div()
            .text_sm()
            .text_color(rgb(shown.red))
            .child(error.clone())
            .into_any_element()
    } else {
        match dialog.mode {
            EditorMode::RichPreview => preview_view(dialog, body_height),
            EditorMode::RawEdit => buffer_view(dialog, mouse_down, mouse_move, mouse_up, wheel),
        }
    };
    // The face toggle sits in the header next to the close door: the report
    // asked for document until an explicit edit click, and raw until a save
    // or the same click returns (#3163).
    let (face, toggle_label) = match dialog.mode {
        EditorMode::RichPreview => ("document", "edit"),
        EditorMode::RawEdit => ("raw", "view"),
    };
    let actions = div()
        .id("editor-face-toggle")
        .flex_none()
        .px_2()
        .py_1()
        .rounded(px(6.0))
        .text_sm()
        .text_color(rgb(shown.text_muted))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(shown.panel_hi)).text_color(rgb(shown.text)))
        .on_click(move |ev, window, app| toggle_mode(ev, window, app))
        .child(toggle_label)
        .into_any_element();
    let footer_text = match dialog.mode {
        EditorMode::RichPreview => {
            format!(
                "{} · edit to change the text · cmd-s save · esc close",
                face
            )
        }
        EditorMode::RawEdit => format!(
            "{} · click to place the caret · drag to select · wheel to scroll · cmd-s save · esc close",
            face
        ),
    };
    let view = modal_shell::ModalView {
        id: "editor",
        title: format!("{} {}{}", dialog.program, named, dirty_mark),
        subtitle,
        actions: Some(actions),
        header_close: true,
        body,
        footer: Some(modal_shell::footer(footer_text).into_any_element()),
        width,
        max_height: height,
    };
    modal_shell::render(view, dismiss).into_any_element()
}

/// The rich face: the file parsed the way the transcript renders a document,
/// inside its own scroller, so a long document wheels like the preview does.
/// The cache holds the text the blocks were parsed from; a fresh parse only
/// happens when the text changed, which only the raw face can cause (#3163).
fn preview_view(dialog: &mut EditorDialog, body_height: f32) -> AnyElement {
    let shown = palette::current();
    let text = dialog.buffer.text().to_string();
    let stale = dialog
        .preview_cache
        .as_ref()
        .is_none_or(|(cached, _)| *cached != text);
    if stale {
        let flow = match dialog.path.as_deref().and_then(DocumentKind::of_path) {
            Some(DocumentKind::Markdown) => markdown::MdFlow::Prose,
            _ => markdown::MdFlow::Verbatim,
        };
        let blocks = markdown::blocks_flow(&text, flow);
        dialog.preview_cache = Some((text, blocks));
    }
    let blocks = dialog
        .preview_cache
        .as_ref()
        .map(|(_, blocks)| blocks.clone())
        .unwrap_or_default();
    let reader = div()
        .id("editor-preview-doc")
        .overflow_y_scroll()
        .scrollbar_width(px(8.0))
        .track_scroll(&dialog.rich_scroll)
        .h(px(body_height))
        .rounded(px(concentric_radius(8.0)))
        .border_1()
        .border_color(rgb(shown.track))
        .bg(rgb(shown.bg))
        .p_5()
        .child(transcript::document_blocks(&blocks));
    div()
        .relative()
        .w_full()
        .h(px(body_height))
        .child(reader)
        .child(preview_thumb(dialog.rich_scroll.clone()))
        .into_any_element()
}

/// Paint after the reader so the thumb uses this frame's measured content.
/// The same quiet track and thumb the document preview draws, so the two
/// scrollables read as one surface (#3163).
fn preview_thumb(scroll: ScrollHandle) -> impl IntoElement {
    let shown = palette::current();
    div()
        .id("editor-preview-thumb")
        .absolute()
        .top_3()
        .bottom_3()
        .right_2()
        .w(px(4.0))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    window.paint_quad(fill(bounds, rgba((shown.track << 8) | 0x66)));
                    let Some(thumb) = vertical_scroll_thumb(
                        f32::from(bounds.size.height),
                        f32::from(scroll.max_offset().height),
                        f32::from(scroll.offset().y),
                    ) else {
                        return;
                    };
                    let thumb_bounds = Bounds::new(
                        point(bounds.origin.x, bounds.origin.y + px(thumb.top)),
                        size(bounds.size.width, px(thumb.height)),
                    );
                    window.paint_quad(gpui::quad(
                        thumb_bounds,
                        px(2.0),
                        rgb(shown.text_muted),
                        px(0.0),
                        rgba(0x00000000),
                        BorderStyle::default(),
                    ));
                },
            )
            .size_full(),
        )
}

/// The raw face: the caret's line window, one row per line, each character
/// carrying its own selection or caret paint so a click can land on any of
/// them through the hit grid the paint pass records. Long lines do not wrap,
/// so rows keep the fixed geometry the hit test divides by, and the card
/// clips them instead of shoving the layout around (#3163).
fn buffer_view(
    dialog: &mut EditorDialog,
    mouse_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    mouse_move: impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static,
    mouse_up: impl Fn(&MouseUpEvent, &mut Window, &mut App) + 'static,
    wheel: impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let shown = palette::current();
    let rows = dialog.rows_visible;
    let caret_line = dialog.buffer.line_col().0;
    let total = dialog.buffer.line_count();
    let start = dialog.raw_scroll.max(1).min(total);
    let end = (start + rows - 1).min(total);
    let widest = end.to_string().len();
    let selection = dialog.buffer.selection();
    // The caret's character index within its own line, zero-based, may sit
    // one past the last character (a caret at the line's end or on an empty
    // line). Only the caret's line gets a caret paint: a global offset would
    // otherwise read as "past this line's end" on every line above (#3163).
    let (_, caret_col) = dialog.buffer.line_col();
    let caret_in_line = if caret_line >= start && caret_line <= end {
        Some(caret_col - 1)
    } else {
        None
    };
    let grid = dialog.hit_grid.clone();
    let mut rows_div = div().flex().flex_col();
    for number in start..=end {
        let line = dialog.buffer.line_text(number).unwrap_or("");
        let line_start = dialog
            .buffer
            .line_span(number)
            .map(|span| span.start)
            .unwrap_or(0);
        let is_caret_line = number == caret_line;
        let mut row = div()
            .flex()
            .flex_row()
            .flex_none()
            .font_family(crate::fonts::current_mono())
            .text_size(px(RAW_TEXT_SIZE))
            .line_height(px(RAW_LINE_HEIGHT))
            .whitespace_nowrap();
        let gutter = format!("{:>width$}", number, width = widest);
        row = row.child(
            div()
                .w(px(RAW_GUTTER_WIDTH))
                .flex_none()
                .text_color(rgb(shown.text_faint))
                .child(gutter),
        );
        let on_line_caret = if is_caret_line { caret_in_line } else { None };
        if line.is_empty() {
            if on_line_caret.is_some() {
                // An empty caret line still owes the user a visible caret: a
                // single space paints the block (#3163).
                row = row.child(
                    div()
                        .bg(rgb(shown.text))
                        .text_color(rgb(shown.bg))
                        .child(" "),
                );
            }
        } else {
            for (text, selected, on_caret) in
                row_segments(line, line_start, selection, on_line_caret)
            {
                let mut cell = div().flex_none();
                if on_caret {
                    // The caret is a block of the paper colour over the ink
                    // colour, the way a terminal draws one (#3163).
                    cell = cell.bg(rgb(shown.text)).text_color(rgb(shown.bg));
                } else if selected {
                    cell = cell.bg(rgb(shown.selection)).text_color(rgb(shown.text));
                } else if is_caret_line {
                    cell = cell.text_color(rgb(shown.text));
                } else {
                    cell = cell.text_color(rgb(shown.text_normal));
                }
                row = row.child(cell.child(text));
            }
        }
        rows_div = rows_div.child(row);
    }
    let raw = div()
        .id("editor-raw-view")
        .relative()
        .flex()
        .flex_col()
        .overflow_hidden()
        // The height is the border box: the rows themselves, the padding
        // this view adds, and the one-pixel border each side. Sizing the
        // content box exactly to the rows keeps the last line whole; the
        // shorter box before this clipped half of it (#3163).
        .h(px(rows as f32 * RAW_LINE_HEIGHT
            + 2.0 * RAW_PADDING
            + 2.0 * RAW_BORDER))
        .rounded(px(concentric_radius(8.0)))
        .border_1()
        .border_color(rgb(shown.track))
        .bg(rgb(shown.bg))
        .py(px(RAW_PADDING))
        .child(rows_div)
        // The hit grid is filled in paint, the only pass that knows where the
        // text column really landed. The canvas draws nothing; it exists so
        // the bounds reach the mouse handlers without either side guessing
        // (#3163).
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let mono = font(crate::fonts::current_mono());
                    let font_id = window.text_system().resolve_font(&mono);
                    let char_width = window
                        .text_system()
                        .advance(font_id, px(RAW_TEXT_SIZE), 'M')
                        .map(|size| f32::from(size.width))
                        .unwrap_or(7.2);
                    if let Ok(mut slot) = grid.lock() {
                        *slot = Some(EditorHitGrid { bounds, char_width });
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .on_mouse_down(MouseButton::Left, move |ev, window, app| {
            mouse_down(ev, window, app);
            app.stop_propagation();
        })
        .on_mouse_move(move |ev, window, app| mouse_move(ev, window, app))
        .on_mouse_up(MouseButton::Left, move |ev, window, app| {
            mouse_up(ev, window, app);
        })
        .on_scroll_wheel(move |ev, window, app| {
            wheel(ev, window, app);
            app.stop_propagation();
        });
    raw.into_any_element()
}

/// Split one displayed line into runs of text, marking which are inside the
/// selection and which carry the caret, all in the buffer's character
/// offsets. The caret index is within the line, zero-based, and may equal
/// the line's length: a caret past the last character gets a one-space
/// block, which is the only way it can be visible at all (#3163).
fn row_segments(
    line: &str,
    line_start: usize,
    selection: Option<(usize, usize)>,
    caret: Option<usize>,
) -> Vec<(String, bool, bool)> {
    let line_len = line.chars().count();
    let line_end = line_start + line_len;
    let selected_span = selection
        .filter(|(s, e)| *s < line_end && *e > line_start)
        .map(|(s, e)| (s.max(line_start), e.min(line_end)));
    let mut segments: Vec<(String, bool, bool)> = Vec::new();
    for (index, ch) in line.chars().enumerate() {
        let at = line_start + index;
        let selected = selected_span.is_some_and(|(s, e)| at >= s && at < e);
        let on_caret = caret == Some(index);
        match segments.last_mut() {
            Some((text, run_selected, run_caret))
                if *run_selected == selected && *run_caret == on_caret =>
            {
                text.push(ch);
            }
            _ => segments.push((ch.to_string(), selected, on_caret)),
        }
    }
    if caret == Some(line_len) {
        segments.push((" ".to_string(), false, true));
    }
    segments
}
