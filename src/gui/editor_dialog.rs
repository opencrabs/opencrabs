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

use gpui::{AnyElement, ClipboardItem, Context, KeyDownEvent, Window, div, prelude::*, px, rgb};
use opencrabs::ui_core::bang::EditorCommand;
use opencrabs::ui_core::editor_buffer::EditorBuffer;
use uuid::Uuid;

use crate::agent_bridge;
use crate::logging;
use crate::modal_shell;
use crate::palette;

/// How many buffer lines the dialog draws around the caret. The buffer can
/// hold far more than a screen, and a full repaint of every line on every
/// keystroke is the cost the panes just escaped (#2805), so the dialog draws a
/// window that follows the caret instead of the whole file.
const VISIBLE_LINES: usize = 30;

/// One conversation's editor: its text, its path, and what the user is
/// mid-way through doing to it.
pub(crate) struct EditorDialog {
    /// The conversation that asked for the editor. Relative paths resolve
    /// against this session's working directory, never the process launch
    /// directory: each session owns its own `/cd` path, so pane A's
    /// `!vi notes.md` must not read pane B's directory (#3163).
    pub(crate) session: Uuid,
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
}

impl EditorDialog {
    fn empty(session: Uuid, command: &EditorCommand, path: Option<PathBuf>) -> Self {
        Self {
            session,
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
        }
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
            self.turn.live.notices.push(
                "editor: no conversation to open it in, start or pick one first".to_string(),
            );
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
        let mut dialog = EditorDialog::empty(id, &command, path.clone());
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
                    dialog.status =
                        Some("not valid text, opened read-only".to_string());
                } else {
                    dialog.status = None;
                }
            }
            Err(error) => {
                dialog.loading = false;
                dialog.load_error = Some(error);
            }
        }
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
        let Some(dialog) = self.editor_dialogs.get_mut(&id) else {
            self.editor_focus = None;
            return;
        };
        if key == "escape" {
            // The prompt closes first, then the discard guard, then the
            // dialog itself: one Esc never throws away typed text on its own.
            if dialog.save_prompt.take().is_some() {
                cx.notify();
                return;
            }
            if dialog.dirty && !dialog.confirm_discard {
                dialog.confirm_discard = true;
                dialog.status = Some("unsaved changes, esc again to discard".to_string());
                cx.notify();
                return;
            }
            drop(dialog);
            self.close_editor(cx);
            return;
        }
        // While the name prompt is up, the typing belongs to it: the buffer
        // underneath must not take a character meant for the filename. Each
        // branch re-reaches into the option rather than holding a borrow
        // across the save call, which needs `self` back.
        if dialog.save_prompt.is_some() {
            match key {
                "enter" => {
                    let typed = dialog.save_prompt.clone().unwrap_or_default();
                    drop(dialog);
                    self.save_editor_as(&id, &typed, cx);
                }
                "backspace" => {
                    if let Some(name) = dialog.save_prompt.as_mut() {
                        name.pop();
                    }
                    cx.notify();
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
                }
            }
            return;
        }
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
    }
}

/// The dialog itself, drawn with the window's shared modal chrome so the
/// editor reads as one of the window's surfaces instead of a page pasted in.
pub(crate) fn overlay(
    dialog: &EditorDialog,
    dismiss: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let shown = palette::current();
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
    } else {
        let (line, col) = dialog.buffer.line_col();
        format!("{}:{}, {}", line, col, dialog.buffer.text().chars().count())
    };
    if dialog.read_only {
        subtitle.push_str(" · read-only");
    }
    if !dialog.unsupported_flags.is_empty() {
        subtitle.push_str(&format!(" · ignored: {}", dialog.unsupported_flags.join(" ")));
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
        buffer_view(dialog)
    };
    let view = modal_shell::ModalView {
        id: "editor",
        title: format!("{} {}{}", dialog.program, named, dirty_mark),
        subtitle,
        actions: None,
        header_close: true,
        body,
        footer: Some(modal_shell::footer(
            "cmd-s save · esc close · arrows move · shift+arrows select".to_string(),
        )
        .into_any_element()),
        width: 860.0,
        max_height: 640.0,
    };
    modal_shell::render(view, dismiss).into_any_element()
}

/// The buffer: the lines around the caret, the caret line ringed in the
/// accent, each row carrying its line number. A window that follows the caret
/// keeps a keystroke from repainting a whole file (#3163).
fn buffer_view(dialog: &EditorDialog) -> AnyElement {
    let shown = palette::current();
    let caret_line = dialog.buffer.line_col().0;
    let total = dialog.buffer.line_count();
    let start = caret_line.saturating_sub(VISIBLE_LINES / 2).max(1);
    let end = (start + VISIBLE_LINES - 1).min(total);
    let widest = end.to_string().len();
    let mut rows = div().flex().flex_col();
    for number in start..=end {
        let text = dialog.buffer.line_text(number).unwrap_or("");
        let is_caret = number == caret_line;
        let mut row = div()
            .flex()
            .flex_row()
            .gap_2()
            .px_1()
            .font_family(crate::fonts::current_mono())
            .text_sm();
        if is_caret {
            row = row.border_l_2().border_color(rgb(shown.accent));
        } else {
            row = row.border_l_2().border_color(rgb(shown.panel));
        }
        let gutter = format!("{:>width$}", number, width = widest);
        row = row.child(
            div()
                .min_w_10()
                .text_color(rgb(shown.text_faint))
                .child(gutter),
        );
        row = row.child(
            div()
                .flex_1()
                .text_color(rgb(if is_caret {
                    shown.text
                } else {
                    shown.text_normal
                }))
                .child(if text.is_empty() { " " } else { text.to_string() }),
        );
        rows = rows.child(row);
    }
    rows.into_any_element()
}
