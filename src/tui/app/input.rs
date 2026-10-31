//! Input handling — keyboard events, history, and approval interception.

use super::events::{AppMode, ToolApprovalResponse, TuiEvent};
use super::mission_control::McPanel;
use super::*;
use crate::tui::input_layout;
use anyhow::Result;
use tokio::sync::mpsc;
use uuid::Uuid;

/// After a double-escape abort, decide whether to pull the just-sent query back
/// into the input and drop it from the transcript (#698). True only when the
/// agent produced NOTHING this turn — no visible text, no reasoning, no tool
/// activity, no intermediate — i.e. the user cancelled before any reply, so the
/// unanswered query should not linger (and would otherwise duplicate on resend).
/// When the agent DID produce output, everything on screen survives the cancel.
pub(crate) fn should_restore_cancelled_query(
    has_visible_text: bool,
    has_visible_reasoning: bool,
    had_tool_activity: bool,
    had_intermediate: bool,
) -> bool {
    !has_visible_text && !has_visible_reasoning && !had_tool_activity && !had_intermediate
}

/// Find the byte offset in `text` where the cumulative display width reaches `target_col`.
fn byte_offset_at_display_col(text: &str, target_col: usize) -> usize {
    let mut width = 0usize;
    for (idx, ch) in text.char_indices() {
        if width >= target_col {
            return idx;
        }
        width += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    text.len()
}

impl App {
    /// Remove mouse tracking / CSI escape fragments from `input_buffer`.
    ///
    /// When tmux pane-switches while mouse capture is active, raw mouse
    /// sequences get split across reads and crossterm delivers them as
    /// individual `Key(Char(...))` events. Both formats leak this way:
    ///
    /// - SGR mode (1006): `\x1b[<35;116;77M` → `[`, `<`, digits, `;`, `M`
    /// - URXVT mode (1015): `\x1b[35;116;77M` → `[`, digits, `;`, `M`
    ///
    /// On `FocusGained` we scrub anything that looks like these fragments
    /// so the user doesn't see garbage.
    pub fn clear_escape_garbage(&mut self) {
        if self.input_buffer.is_empty() {
            return;
        }
        // Fast path: detect dominance by either of the leak signatures.
        // The SGR set (`\x1b`, `[`, `<`, `M`, `m`, `^`) was the original
        // trigger; URXVT motion bursts produce mostly digits and `;` so
        // a separate "lots of `[NN;` clusters" check catches them too.
        let dominated_by_garbage = {
            let total = self.input_buffer.len();
            let sgr_chars = self
                .input_buffer
                .chars()
                .filter(|c| matches!(c, '\x1b' | '[' | '<' | 'M' | 'm' | '^'))
                .count();
            let urxvt_chars = self
                .input_buffer
                .chars()
                .filter(|c| c.is_ascii_digit() || matches!(c, ';' | '[' | 'M' | 'm'))
                .count();
            // 30% threshold for SGR (small alphabet, harder to false-fire),
            // 80% for URXVT (digits + `;` + `[` cover so much of normal
            // typing that we need to be confident before scrubbing).
            total > 5 && (sgr_chars * 100 / total > 30 || urxvt_chars * 100 / total > 80)
        };
        if !dominated_by_garbage {
            return;
        }
        // Strip escape sequences via the same helper used for paste events
        let cleaned = Self::strip_terminal_escapes(&self.input_buffer);
        // Also remove leftover mouse-sequence fragment chars that arrived as
        // individual key events. Digits and `;` are added when the buffer
        // is dominated by URXVT-style fragments — otherwise we'd nuke any
        // legit text the user mixed in (e.g. "5; ").
        let strip_urxvt_chars = {
            let total = cleaned.chars().count().max(1);
            let urxvt_run = cleaned
                .chars()
                .filter(|c| c.is_ascii_digit() || matches!(c, ';' | '[' | 'M' | 'm'))
                .count();
            urxvt_run * 100 / total > 80
        };
        let cleaned: String = cleaned
            .chars()
            .filter(|c| {
                if matches!(c, '[' | '<' | '>' | 'M' | 'm' | '^') {
                    return false;
                }
                if strip_urxvt_chars && (c.is_ascii_digit() || *c == ';') {
                    return false;
                }
                true
            })
            .collect();
        if cleaned.trim().is_empty() {
            tracing::debug!(
                "Cleared {} bytes of escape garbage from input buffer",
                self.input_buffer.len()
            );
            self.input_buffer.clear();
            self.cursor_position = 0;
        } else {
            self.input_buffer = cleaned;
            self.cursor_position = self.input_buffer.len().min(self.cursor_position);
        }
    }

    /// Returns (line_start_byte, column_bytes) for the cursor's current logical line.
    /// `line_start_byte` is the byte offset where the current line begins (after `\n`).
    /// `column_bytes` is how many bytes into the line the cursor is.
    fn cursor_line_position(&self) -> (usize, usize) {
        let line_start = self.input_buffer[..self.cursor_position]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let col = self.cursor_position - line_start;
        (line_start, col)
    }

    /// Text width of one input visual line, in display columns.
    ///
    /// This has to be the width the renderer wraps at, or Alt+Up/Down lands on
    /// a different column than the row it moved to. The box has no side
    /// borders, but every row spends two columns on its prefix (`❯ ` or two
    /// spaces), so the text wraps at `input_area_width - 2`. That is the same
    /// number `input_layout::content_width` hands to click and drag.
    fn input_visual_line_width(&self) -> usize {
        let area_width = if self.input_area_width > 0 {
            self.input_area_width
        } else {
            crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80)
        };
        input_layout::content_width(area_width).max(1)
    }

    /// Read-only predicate: is there anywhere for the cursor to move up to?
    /// True whenever the cursor isn't already at the very start of the
    /// buffer — this covers visual rows, logical lines, and "move to 0"
    /// when we're already on the first line but mid-line.
    fn can_cursor_move_up(&self) -> bool {
        self.cursor_position > 0
    }

    /// Read-only predicate: is there anywhere for the cursor to move down to?
    /// True whenever the cursor isn't already at the very end of the buffer.
    fn can_cursor_move_down(&self) -> bool {
        self.cursor_position < self.input_buffer.len()
    }

    /// Move cursor up one visual line within a logical line, accounting for soft wrapping.
    /// Returns true if the cursor moved, false if already on the first visual line.
    fn cursor_visual_up(&mut self) -> bool {
        use unicode_width::UnicodeWidthStr;

        let (line_start, _col_bytes) = self.cursor_line_position();
        let text_before = &self.input_buffer[line_start..self.cursor_position];
        let display_col = text_before.width();
        let vw = self.input_visual_line_width();

        let visual_row = display_col / vw;
        if visual_row == 0 {
            // Already on first visual line of this logical line
            return false;
        }

        // Target: same display column but one visual row up
        let target_display_col = (visual_row - 1) * vw + (display_col % vw);
        // Find byte offset at that display column within this logical line
        let line_text = &self.input_buffer[line_start..];
        let line_end = line_text.find('\n').unwrap_or(line_text.len());
        let line_text = &line_text[..line_end];
        self.cursor_position =
            line_start + byte_offset_at_display_col(line_text, target_display_col);
        true
    }

    /// Move cursor down one visual line within a logical line, accounting for soft wrapping.
    /// Returns true if the cursor moved, false if already on the last visual line.
    fn cursor_visual_down(&mut self) -> bool {
        use unicode_width::UnicodeWidthStr;

        let (line_start, _col_bytes) = self.cursor_line_position();
        let text_before = &self.input_buffer[line_start..self.cursor_position];
        let display_col = text_before.width();
        let vw = self.input_visual_line_width();

        // Get full logical line
        let line_text = &self.input_buffer[line_start..];
        let line_end = line_text.find('\n').unwrap_or(line_text.len());
        let line_text = &line_text[..line_end];
        let total_width = line_text.width();

        let visual_row = display_col / vw;
        let max_visual_row = if total_width == 0 {
            0
        } else {
            (total_width.saturating_sub(1)) / vw
        };

        if visual_row >= max_visual_row {
            // Already on last visual line of this logical line
            return false;
        }

        // Target: same display column but one visual row down
        let target_display_col = ((visual_row + 1) * vw + (display_col % vw)).min(total_width);
        self.cursor_position =
            line_start + byte_offset_at_display_col(line_text, target_display_col);
        true
    }

    /// Map terminal row to message index using the render-time line mapping
    fn row_to_msg_idx(&self, row: u16) -> Option<usize> {
        let row_in_chat = row.saturating_sub(self.chat_area_y + 1) as usize;
        let line_idx = self.chat_render_scroll + row_in_chat;
        self.chat_line_to_msg.get(line_idx).copied().flatten()
    }

    /// Pin message `idx`'s header to its current screen row so the next render
    /// keeps it in place when its block expands or collapses (#728). Computed
    /// from the *current* (pre-toggle) line map, so it captures where the header
    /// sits right now; `render_chat` restores it there after the height change.
    fn set_expand_anchor(&mut self, idx: usize) {
        if let Some(line_top) = self.chat_line_to_msg.iter().position(|m| *m == Some(idx)) {
            self.chat_expand_growth_pending = true;
            self.chat_expand_anchor =
                crate::tui::render::chat::turn_anchor_row(line_top, self.chat_render_scroll)
                    .map(|row| (idx, row));
        }
    }

    /// Pin a turn header to the screen row it currently occupies, so folding or
    /// unfolding the turn grows it in place instead of jumping the viewport.
    ///
    /// The message-keyed [`Self::set_expand_anchor`] cannot do this: a header
    /// line maps to no message, so it is absent from `chat_line_to_msg`.
    fn set_turn_expand_anchor(&mut self, anchor: &str) {
        if let Some(line_top) = self
            .chat_line_to_turn
            .iter()
            .position(|t| t.as_deref() == Some(anchor))
        {
            self.chat_expand_growth_pending = true;
            self.chat_expand_anchor_turn =
                crate::tui::render::chat::turn_anchor_row(line_top, self.chat_render_scroll)
                    .map(|row| (anchor.to_string(), row));
        }
    }

    /// The newest turn's anchor id, if there is one. `ctrl+o` is scoped to this
    /// turn so a long transcript does not reflow under the user.
    fn newest_turn_anchor(&self) -> Option<String> {
        let turns = crate::tui::render::chat::turn_ranges(&self.messages);
        let last = turns.last()?;
        self.messages
            .get(last.start)
            .map(crate::tui::render::chat::turn_anchor)
    }

    /// `ctrl+o`: open or close the newest turn, working-out and all.
    ///
    /// The fold is the outer layer, so it drives the toggle: expanding a tool
    /// group while its turn is folded changes nothing on screen, which is what
    /// made `ctrl+o` look broken once folding shipped. Flipping the fold and
    /// the turn's groups together keeps one key honest.
    ///
    /// `cycle_details` distinguishes the two call sites. The normal handler
    /// also advances Thinking blocks through their 3-state cycle (#727); the
    /// approval-mode handler must not, since its whole purpose is collapsing
    /// far enough to see the pending prompt.
    fn toggle_newest_turn(&mut self, cycle_details: bool) {
        let Some(anchor) = self.newest_turn_anchor() else {
            return;
        };
        let turn = crate::tui::render::chat::turn_ranges(&self.messages)
            .into_iter()
            .next_back();
        let target = !self.turn_expanded.get(&anchor).copied().unwrap_or(false);

        self.set_turn_expand_anchor(&anchor);
        self.turn_expanded.insert(anchor, target);

        if let Some(ref mut group) = self.active_tool_group {
            group.expanded = target;
        }
        let Some(turn) = turn else {
            return;
        };
        let end = turn.end.min(self.messages.len());
        for msg in &mut self.messages[turn.start.min(end)..end] {
            if let Some(ref mut group) = msg.tool_group {
                group.expanded = target;
            }
            if cycle_details && msg.details.is_some() {
                msg.cycle_details_expand();
            }
        }
    }

    /// The turn anchor whose HEADER sits on `row`, if any (#758).
    fn row_to_turn_anchor(&self, row: u16) -> Option<String> {
        let row_in_chat = row.saturating_sub(self.chat_area_y + 1) as usize;
        let line_idx = self.chat_render_scroll + row_in_chat;
        self.chat_line_to_turn.get(line_idx).cloned().flatten()
    }

    /// Left-click: select/highlight a message
    pub(crate) fn handle_click_select(&mut self, row: u16) {
        // A fresh click clears any in-flight drag selection.
        self.drag_anchor = None;
        self.drag_current = None;

        // A turn header folds/unfolds the whole turn (#758). Checked before
        // message routing because header rows map to no message.
        if let Some(anchor) = self.row_to_turn_anchor(row) {
            // Pin the header BEFORE toggling, the same way the tool-group and
            // details paths below do. Without it, unfolding inserts the turn's
            // whole working-out above the fixed-from-bottom offset and the
            // view jumps pages away from what was clicked (#728).
            self.set_turn_expand_anchor(&anchor);
            // Folded is the default, so an unclicked header opens its turn.
            let currently_expanded = self.turn_expanded.get(&anchor).copied().unwrap_or(false);
            self.turn_expanded.insert(anchor, !currently_expanded);
            return;
        }

        let msg_idx = self.row_to_msg_idx(row);
        // Click on an expandable block (tool group or reasoning details)
        // toggles that single block's expand state, mirroring Ctrl+O but
        // scoped to the one message under the cursor. Live/streaming groups
        // live in active_tool_group and aren't in `messages`; those stay on
        // Ctrl+O. Plain text rows keep the highlight-select behavior.
        if let Some(idx) = msg_idx {
            // Resolve the action from an immutable borrow first so we can pin the
            // scroll anchor (#728) before taking the mutable borrow to toggle.
            match self.messages.get(idx).map(|m| m.click_action()) {
                Some(ClickAction::ToggleToolGroup) => {
                    self.set_expand_anchor(idx);
                    if let Some(msg) = self.messages.get_mut(idx)
                        && let Some(ref mut group) = msg.tool_group
                    {
                        group.expanded = !group.expanded;
                    }
                    return;
                }
                Some(ClickAction::ToggleDetails) => {
                    // 3-state cycle: collapsed -> capped -> full -> collapsed (#727).
                    self.set_expand_anchor(idx);
                    if let Some(msg) = self.messages.get_mut(idx) {
                        msg.cycle_details_expand();
                    }
                    return;
                }
                Some(ClickAction::Select) | None => {}
            }
        }
        // Toggle: click same message deselects, click different selects
        if self.selected_message_idx == msg_idx {
            self.selected_message_idx = None;
        } else {
            self.selected_message_idx = msg_idx;
        }
    }

    /// Left-button drag — update the live drag selection.
    /// The anchor is the click position (captured on the first drag event
    /// because crossterm does not deliver drags until the pointer moves).
    pub(crate) fn handle_mouse_drag(&mut self, col: u16, row: u16) {
        if self.drag_anchor.is_none() {
            self.drag_anchor = Some((col, row));
            // While drag-selecting we suppress the message highlight so the
            // two visual cues don't fight.
            self.selected_message_idx = None;
        }
        self.drag_current = Some((col, row));
    }

    /// Left-button released — finalize selection, extract text, copy, notify.
    pub(crate) fn handle_mouse_up(&mut self, col: u16, row: u16) {
        // If this was a plain click (no drag motion), treat it as a click-select.
        let Some(anchor) = self.drag_anchor.take() else {
            self.drag_current = None;
            // Click-to-open (#1772): a token under the cursor resolving to a
            // URL or an existing path launches and consumes the click.
            // Anywhere else keeps the existing fold/select semantics.
            if self.try_open_clicked(col, row) {
                return;
            }
            self.handle_click_select(row);
            return;
        };
        let end = (col, row);
        self.drag_current = None;

        let text = self.extract_drag_selection(anchor, end);
        if text.trim().is_empty() {
            return;
        }
        if Self::copy_to_clipboard(&text) {
            self.notification = Some("Copied to clipboard".to_string());
            self.notification_shown_at = Some(std::time::Instant::now());
        }
    }

    /// Screen coordinates → rendered line under the cursor → clickable token
    /// → launch. Returns true when the click opened something (or copied the
    /// path after a failed launch), consuming it. Coordinate math mirrors
    /// `extract_drag_selection`: `Padding(1,1,1,0)` puts the first text row at
    /// `chat_area_y + 1` and content one cell past `chat_area_x`.
    fn try_open_clicked(&mut self, col: u16, row: u16) -> bool {
        let chat_height = self.chat_area_height as usize;
        if chat_height == 0 {
            return false;
        }
        let top_pad = 1u16;
        let Some(row_in_chat) = row.checked_sub(self.chat_area_y + top_pad) else {
            return false;
        };
        let row_in_chat = row_in_chat as usize;
        if row_in_chat >= chat_height.saturating_sub(top_pad as usize) {
            return false;
        }
        let line_idx = self.chat_render_scroll + row_in_chat;
        let Some(line) = self.chat_rendered_lines.get(line_idx) else {
            return false;
        };
        let content_left = self.chat_area_x + 1;
        let col_in_line = col.saturating_sub(content_left) as usize;
        let Some(target) = super::clickable::target_at(line, col_in_line) else {
            return false;
        };
        match super::clickable::open(&target, self.event_sender()) {
            Ok(()) => {
                self.notification = Some(format!("Opened {}", target.label()));
                self.notification_shown_at = Some(std::time::Instant::now());
            }
            // No launcher on this box (or it refused): the token was still
            // the click's intent, so copy it instead of a dead end — same
            // fallback the session-files overlay uses.
            Err(_) => {
                let copied = Self::copy_to_clipboard(&target.label());
                self.notification = Some(if copied {
                    format!("Copied {} (no launcher available)", target.label())
                } else {
                    format!("Could not open {}", target.label())
                });
                self.notification_shown_at = Some(std::time::Instant::now());
            }
        }
        true
    }

    /// Turn a pair of terminal-screen coordinates into the plain-text that was
    /// drawn between them. Strips leading chat padding and code-block gutter
    /// (e.g. `"  1 │ "`) so the copied text matches what the user visually sees.
    fn extract_drag_selection(&self, a: (u16, u16), b: (u16, u16)) -> String {
        // Normalize so (start) precedes (end) in reading order.
        let (start, end) = if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        };

        let chat_left = self.chat_area_x;
        let chat_top = self.chat_area_y;
        let chat_height = self.chat_area_height as usize;
        if chat_height == 0 {
            return String::new();
        }

        // Screen row → logical line index in `chat_rendered_lines`.
        // Render uses `Padding::new(1,1,1,0)` on the inner block, so the first
        // line of text is drawn at `chat_top + 1`.
        let top_pad = 1u16;
        let row_to_line = |row: u16| -> Option<usize> {
            let row_in_chat = row.checked_sub(chat_top + top_pad)? as usize;
            if row_in_chat >= chat_height.saturating_sub(top_pad as usize) {
                return None;
            }
            Some(self.chat_render_scroll + row_in_chat)
        };

        // Content starts one cell in (Padding left = 1).
        let content_left = chat_left + 1;
        let col_in_line = |col: u16| -> usize { col.saturating_sub(content_left) as usize };

        let start_line = row_to_line(start.1);
        let end_line = row_to_line(end.1);
        let (Some(start_line), Some(end_line)) = (start_line, end_line) else {
            return String::new();
        };

        let strip_gutter = |s: &str| -> String {
            // Trim leading spaces first.
            let trimmed = s.trim_start();
            // Code-block lines look like "  1 │ fn main()" → after trim_start:
            // "1 │ fn main()". Strip the "<digits> │ " prefix if present.
            if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
                let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
                if let Some(after_bar) = rest.strip_prefix(" │ ") {
                    return after_bar.to_string();
                }
                if let Some(after_bar) = rest.strip_prefix("│ ") {
                    return after_bar.to_string();
                }
            }
            trimmed.to_string()
        };

        // Helper to slice a line by display-column range (char-based is fine
        // for monospaced ASCII; for multi-byte we fall back to char indices).
        let slice_line = |line: &str, from: usize, to: usize| -> String {
            let chars: Vec<char> = line.chars().collect();
            let from = from.min(chars.len());
            let to = to.min(chars.len());
            if from >= to {
                return String::new();
            }
            chars[from..to].iter().collect()
        };

        let mut out = String::new();
        if start_line == end_line {
            let Some(line) = self.chat_rendered_lines.get(start_line) else {
                return String::new();
            };
            let from = col_in_line(start.0);
            let to = col_in_line(end.0).max(from);
            let piece = slice_line(line, from, to + 1);
            out.push_str(&strip_gutter(&piece));
        } else {
            for (i, line_idx) in (start_line..=end_line).enumerate() {
                let Some(line) = self.chat_rendered_lines.get(line_idx) else {
                    continue;
                };
                let piece = if i == 0 {
                    // First line: from start.col to EOL
                    let from = col_in_line(start.0);
                    slice_line(line, from, line.chars().count())
                } else if line_idx == end_line {
                    // Last line: from 0 to end.col (inclusive)
                    let to = col_in_line(end.0) + 1;
                    slice_line(line, 0, to)
                } else {
                    line.clone()
                };
                let cleaned = strip_gutter(&piece);
                // Skip purely-whitespace lines to match opencode's "no empty lines" UX.
                if !cleaned.trim().is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&cleaned);
                }
            }
        }
        out
    }

    /// Right-click: copy the clicked (or selected) message to clipboard
    pub(crate) fn handle_right_click_copy(&mut self, row: u16) {
        // Use clicked message, or fall back to already-selected message
        let msg_idx = self.row_to_msg_idx(row).or(self.selected_message_idx);

        let Some(idx) = msg_idx else { return };
        let content = match self.messages.get(idx) {
            Some(msg) if !msg.content.trim().is_empty() => msg.content.clone(),
            _ => return,
        };

        if Self::copy_to_clipboard(&content) {
            self.notification = Some("Copied to clipboard".to_string());
            self.notification_shown_at = Some(std::time::Instant::now());
            self.selected_message_idx = None;
        }
    }

    /// Is `row` a content row of the input box, i.e. not one of its borders?
    fn row_is_input_content(&self, row: u16) -> bool {
        if self.input_area_height < 3 {
            return false;
        }
        let first = self.input_area_y + 1;
        let last = self.input_area_y + self.input_area_height - 2;
        row >= first && row <= last
    }

    /// Handle a left click inside the input box: put the caret under the
    /// pointer. Returns true when the click landed in the box, so the caller
    /// knows the chat gesture handling must not run for it.
    ///
    /// The same press also seeds the drag anchor, so a selection starts at the
    /// exact cell the user pressed instead of a cell or two later at the first
    /// motion event. A press with no drag before the release stays a plain
    /// caret placement; `handle_input_mouse_up` drops the anchor.
    pub(crate) fn handle_input_mouse_click(&mut self, col: u16, row: u16) -> bool {
        if !self.row_is_input_content(row) {
            return false;
        }
        self.input_drag_selecting = false;
        self.input_drag_current = None;
        self.input_drag_anchor = Some((col, row));
        // A press in the input box never starts a chat gesture, and a chat
        // anchor left over from an earlier gesture must not survive it.
        self.drag_anchor = None;
        self.drag_current = None;

        if self.input_buffer.is_empty() {
            // The caret is already at 0. Consume the click anyway so it does
            // not fall through to the chat block handling underneath.
            return true;
        }
        let Some((visual_row, disp_col)) = input_layout::screen_to_visual(
            row,
            col,
            self.input_area_x,
            self.input_area_y,
            self.input_scroll_y,
        ) else {
            // Border column or prefix padding: the caret stays where it is.
            return true;
        };
        let rows = input_layout::visual_rows(
            &self.input_buffer,
            input_layout::content_width(self.input_area_width),
        );
        self.cursor_position =
            input_layout::byte_at_visual_pos(&rows, &self.input_buffer, visual_row, disp_col);
        true
    }

    /// Handle mouse drag in the input area — track text selection.
    pub(crate) fn handle_input_mouse_drag(&mut self, col: u16, row: u16) {
        if self.input_area_height == 0 {
            return;
        }
        // Check if mouse is within input area
        if row < self.input_area_y || row >= self.input_area_y + self.input_area_height {
            return;
        }
        if !self.input_drag_selecting {
            self.input_drag_selecting = true;
            // The press already recorded the anchor. Only a drag that began
            // without a press (mouse capture turned on mid-gesture) falls back
            // to the first motion event.
            if self.input_drag_anchor.is_none() {
                self.input_drag_anchor = Some((col, row));
            }
            // Clear any chat drag selection
            self.drag_anchor = None;
            self.drag_current = None;
        }
        self.input_drag_current = Some((col, row));
    }

    /// Handle mouse up in the input area — finalize selection and copy.
    /// Returns true if text was copied.
    pub(crate) fn handle_input_mouse_up(&mut self, col: u16, row: u16) -> bool {
        if !self.input_drag_selecting {
            // A plain click: the caret was already placed on press, so drop
            // the press anchor instead of letting it leak into the next drag.
            self.input_drag_anchor = None;
            self.input_drag_current = None;
            return false;
        }
        self.input_drag_selecting = false;
        let Some(anchor) = self.input_drag_anchor.take() else {
            self.input_drag_current = None;
            return false;
        };
        self.input_drag_current = None;

        let text = self.extract_input_drag_selection(anchor, (col, row));
        if text.trim().is_empty() {
            return false;
        }
        Self::copy_to_clipboard(&text)
    }

    /// Extract text from input buffer based on drag selection coordinates.
    pub(crate) fn extract_input_drag_selection(&self, a: (u16, u16), b: (u16, u16)) -> String {
        input_layout::selection_text(
            &self.input_buffer,
            input_layout::content_width(self.input_area_width),
            self.input_area_x,
            self.input_area_y,
            self.input_scroll_y,
            a,
            b,
        )
    }

    /// Copy text to system clipboard
    fn copy_to_clipboard(text: &str) -> bool {
        #[cfg(windows)]
        use std::io::Write;
        #[cfg(windows)]
        use std::process::{Command, Stdio};

        // Windows first, and only its own backends. The old arm fell through
        // to pbcopy/xclip/xsel, all of which fail to spawn here, so every
        // copy silently returned false.
        #[cfg(windows)]
        {
            // PowerShell Set-Clipboard, text riding a UTF-8 temp file:
            // argv quoting is hostile, and the console codepage on stdin
            // would mangle non-ASCII. The .NET reader gets an explicit
            // encoding, so Indonesian/emoji payloads survive.
            let path = std::env::temp_dir().join(format!("oc-clip-{}.txt", std::process::id()));
            if std::fs::write(&path, text).is_ok() {
                let ps = format!(
                    "Set-Clipboard -Value ([IO.File]::ReadAllText('{}', [Text.Encoding]::UTF8))",
                    // PS single-quoted strings escape ' by doubling; a temp
                    // path under a profile like C:\Users\O'Brien would
                    // otherwise break the script line.
                    path.display().to_string().replace('\'', "''")
                );
                use std::os::windows::process::CommandExt;
                let ok = Command::new("powershell.exe")
                    .args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-Command",
                        &ps,
                    ])
                    // CREATE_NO_WINDOW: no console flash on every Ctrl+C.
                    .creation_flags(0x0800_0000)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success());
                let _ = std::fs::remove_file(&path);
                if ok {
                    return true;
                }
            }
            // clip.exe last resort: it wants UTF-16LE, NUL-terminated, and
            // eats a trailing newline, which is strictly better than nothing.
            let mut utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            utf16.extend(0u16.to_le_bytes());
            if let Ok(mut child) = Command::new("clip.exe")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(&utf16);
                }
                // Close the pipe before waiting: clip.exe reads to EOF, and
                // the child handle keeps stdin open until dropped, so a
                // wait-with-pipe-open hangs the TUI on the last-resort path.
                drop(child.stdin.take());
                return child.wait().is_ok_and(|s| s.success());
            }
            return false;
        }

        // Unix backends, now gated: on Windows the arm above always returns.
        // Every backend is tried in platform order and a backend that spawns
        // but fails does not end the search (#2171), so a `true` here means
        // the text really reached a clipboard owner.
        #[cfg(not(windows))]
        {
            copy_with_backends(clipboard_backends(), text)
        }
    }

    /// True if `bytes` start with a known raster-image magic number. Used to
    /// reject non-image clipboard payloads before attaching.
    pub(crate) fn looks_like_image(bytes: &[u8]) -> bool {
        bytes.len() > 12
            && (bytes.starts_with(&[0x89, b'P', b'N', b'G'])      // PNG
                || bytes.starts_with(&[0xFF, 0xD8, 0xFF])         // JPEG
                || bytes.starts_with(b"GIF8")                     // GIF
                || bytes.starts_with(b"BM")                       // BMP
                || (&bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP"))
    }

    /// File extension (no dot) inferred from image magic bytes; `png` default.
    fn image_ext(bytes: &[u8]) -> &'static str {
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "jpg"
        } else if bytes.starts_with(b"GIF8") {
            "gif"
        } else if bytes.starts_with(b"BM") {
            "bmp"
        } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            "webp"
        } else {
            "png"
        }
    }

    /// Read raw image bytes from the OS clipboard, if it currently holds an
    /// image. Mirrors `copy_to_clipboard`'s shell-out approach (no new crates):
    /// the bracketed-paste path only ever delivers text, so an image copied
    /// from a browser / WhatsApp / a screenshot never reaches it. Returns the
    /// encoded image bytes (PNG on every backend).
    // Each platform's `return` is the exit of its own cfg block; on the one
    // platform being compiled it reads as the tail, hence the allow.
    #[allow(clippy::needless_return)]
    fn read_clipboard_image() -> Option<Vec<u8>> {
        // Platform backends below (macOS/Linux/Windows) all spawn; the
        // import would be unused (and warned) on any other target.
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        use std::process::{Command, Stdio};

        // macOS: pbpaste is text-only, so dump the clipboard PNG to a scratch
        // file via osascript (built in). Fails cleanly when there's no image.
        #[cfg(target_os = "macos")]
        {
            let scratch = std::env::temp_dir().join("opencrabs-clip-read.png");
            let script = format!(
                "set thePng to (the clipboard as «class PNGf»)\n\
                 set fp to open for access POSIX file \"{}\" with write permission\n\
                 set eof fp to 0\n\
                 write thePng to fp\n\
                 close access fp",
                scratch.display()
            );
            let ok = Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                // #1740: clipboard holds no PNGf class (or osascript failed).
                // Say so, or logs can never distinguish "never invoked" from
                // "invoked and refused".
                tracing::debug!(
                    "clipboard image: osascript PNGf read failed or clipboard has no image"
                );
            }
            if ok {
                match std::fs::read(&scratch) {
                    Ok(bytes) => {
                        if Self::looks_like_image(&bytes) {
                            let _ = std::fs::remove_file(&scratch);
                            return Some(bytes);
                        }
                        tracing::debug!(
                            "clipboard image: scratch file {} bytes are not an image",
                            bytes.len()
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "clipboard image: cannot read scratch file {}: {e}",
                            scratch.display()
                        );
                    }
                }
            }
            return None;
        }

        // Linux: try Wayland (wl-paste) then X11 (xclip, already our copy tool).
        // Both write the raw image bytes to stdout for the image/png target.
        #[cfg(target_os = "linux")]
        {
            let attempts: [(&str, &[&str]); 2] = [
                ("wl-paste", &["--type", "image/png"]),
                (
                    "xclip",
                    &["-selection", "clipboard", "-t", "image/png", "-o"],
                ),
            ];
            for (cmd, cmd_args) in attempts {
                if let Ok(out) = Command::new(cmd)
                    .args(cmd_args)
                    .stderr(Stdio::null())
                    .output()
                {
                    if out.status.success() && Self::looks_like_image(&out.stdout) {
                        return Some(out.stdout);
                    }
                    // #1740: distinguish "backend missing" from "backend ran
                    // but the clipboard was not an image".
                    tracing::debug!(
                        "clipboard image: {cmd} exit={} bytes={} (not a usable image)",
                        out.status,
                        out.stdout.len()
                    );
                } else {
                    tracing::debug!("clipboard image: {cmd} not available");
                }
            }
            return None;
        }

        // Windows: the clipboard holds an HBITMAP, not bytes — no CLI hands
        // us a PNG directly. PowerShell marshals GetImage() and encodes PNG
        // to a scratch file, re-entering the same magic-byte gate as the
        // macOS arm. -STA is mandatory: PowerShell 7 defaults to MTA and OLE
        // clipboard calls throw there.
        #[cfg(windows)]
        {
            let scratch =
                std::env::temp_dir().join(format!("oc-paste-img-{}.png", std::process::id()));
            let ps = format!(
                "Add-Type -AssemblyName System.Windows.Forms; $i = [Windows.Forms.Clipboard]::GetImage(); \
                 if ($i) {{ $i.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png); $i.Dispose(); exit 0 }} \
                 else {{ exit 1 }}",
                // PS single-quote escaping is doubling; O'Brien profiles exist.
                scratch.display().to_string().replace('\'', "''")
            );
            let out = {
                use std::os::windows::process::CommandExt;
                Command::new("powershell.exe")
                    .args([
                        "-STA",
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-Command",
                        &ps,
                    ])
                    .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
            };
            let ok = out.is_ok_and(|o| o.status.success());
            if !ok {
                tracing::debug!(
                    "clipboard image: powershell GetImage failed or clipboard has no image"
                );
                let _ = std::fs::remove_file(&scratch);
                return None;
            }
            let bytes = std::fs::read(&scratch).ok();
            let _ = std::fs::remove_file(&scratch);
            match bytes {
                Some(b) if Self::looks_like_image(&b) => Some(b),
                Some(b) => {
                    tracing::debug!(
                        "clipboard image: scratch {} bytes are not an image",
                        b.len()
                    );
                    None
                }
                None => None,
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            None
        }
    }

    /// If the OS clipboard holds an image, write it to `~/.opencrabs/tmp/` and
    /// return an attachment pointing at it (the rest of the pipeline is
    /// path-based). Returns `None` when there's no image or the write fails.
    pub(crate) fn attach_clipboard_image() -> Option<super::state::ImageAttachment> {
        let bytes = Self::read_clipboard_image()?;
        let dir = crate::config::opencrabs_home().join("tmp");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("clipboard image: cannot create {}: {e}", dir.display());
            return None;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!("pasted-{stamp}.{}", Self::image_ext(&bytes));
        let path = dir.join(&name);
        if let Err(e) = std::fs::write(&path, &bytes) {
            tracing::warn!("clipboard image: cannot write {}: {e}", path.display());
            return None;
        }
        tracing::info!(
            "clipboard image attached: {} ({} bytes)",
            path.display(),
            bytes.len()
        );
        Some(super::state::ImageAttachment {
            name,
            path: path.to_string_lossy().to_string(),
            is_video: false,
        })
    }

    /// Attach the clipboard's raw image bytes, if any, and track the file in
    /// the session. Shared by the empty bracketed paste and Ctrl+V so both
    /// routes attach, track and announce identically. `what` names the source
    /// in the notification. Returns whether an image was attached.
    pub(crate) fn attach_image_from_clipboard(&mut self, what: &str) -> bool {
        let Some(att) = Self::attach_clipboard_image() else {
            return false;
        };
        let label = att.name.clone();
        if let Some(session) = &self.current_session {
            let file_svc = self.file_service.clone();
            let sid = session.id;
            let path = std::path::PathBuf::from(&att.path);
            tokio::spawn(async move {
                if let Err(e) = file_svc.get_or_create_file(sid, path, None).await {
                    tracing::warn!("Failed to track pasted image: {e}");
                }
            });
        }
        self.attachments.push(att);
        self.notification = Some(format!("📎 Attached {what}: {label}"));
        self.notification_shown_at = Some(std::time::Instant::now());
        // The paste resolves the hint for this clipboard content; the probe
        // re-arms it only when the content changes (#1816).
        self.clipboard_hint = None;
        self.clipboard_hint_shown_at = None;
        self.clipboard_hint_suppressed = true;
        true
    }

    /// Read TEXT from the OS clipboard (pbpaste / wl-paste / xclip). `None`
    /// when every backend fails or the clipboard holds nothing usable.
    // Each platform's `return` is the exit of its own cfg block; on the one
    // platform being compiled it reads as the tail, hence the allow.
    #[allow(clippy::needless_return)]
    fn read_clipboard_text() -> Option<String> {
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        use std::process::{Command, Stdio};

        #[cfg(target_os = "macos")]
        {
            if let Ok(out) = Command::new("pbpaste").stderr(Stdio::null()).output()
                && out.status.success()
            {
                let s = String::from_utf8_lossy(&out.stdout).to_string();
                if !s.trim().is_empty() {
                    return Some(s);
                }
            }
            return None;
        }

        #[cfg(target_os = "linux")]
        {
            // Wayland first, X11 fallback: mirrors read_clipboard_image's
            // backend order.
            for (cmd, args) in [
                ("wl-paste", vec!["--no-newline"]),
                ("xclip", vec!["-selection", "clipboard", "-o"]),
            ] {
                if let Ok(out) = Command::new(cmd).args(args).stderr(Stdio::null()).output()
                    && out.status.success()
                {
                    let s = String::from_utf8_lossy(&out.stdout).to_string();
                    if !s.trim().is_empty() {
                        return Some(s);
                    }
                }
            }
            return None;
        }

        // Windows: Get-Clipboard. PowerShell's stdout arrives in the console
        // codepage, which mangles non-ASCII, so route through an explicit
        // UTF-8 file like the copy path does (Out-File -Encoding utf8 writes
        // a BOM on 5.1 and none on 7; strip unconditionally).
        #[cfg(windows)]
        {
            let path =
                std::env::temp_dir().join(format!("oc-paste-txt-{}.txt", std::process::id()));
            let ps = format!(
                // -NoNewline: Out-File appends a line terminator by default,
                // which the reader preserves as a phantom trailing newline in
                // the user's pasted text. -Raw already carries the exact
                // clipboard content; do not mutate it.
                "Get-Clipboard -Raw | Out-File -LiteralPath '{}' -Encoding utf8 -NoNewline",
                path.display().to_string().replace('\'', "''")
            );
            let ran = {
                use std::os::windows::process::CommandExt;
                Command::new("powershell.exe")
                    .args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-Command",
                        &ps,
                    ])
                    .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            };
            let text = if ran {
                std::fs::read(&path).ok().map(|b| {
                    String::from_utf8_lossy(&b)
                        .trim_start_matches('\u{FEFF}')
                        .to_string()
                })
            } else {
                None
            };
            let _ = std::fs::remove_file(&path);
            match text {
                Some(s) if !s.trim().is_empty() => Some(s),
                _ => None,
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            None
        }
    }

    /// Ctrl+V handler (#1740): if the clipboard text resolves to a real local
    /// file, attach it through the same `extract_attachments` pipeline a
    /// drag-drop uses, so every file type and every receipt matches. Plain
    /// text (or an unresolvable path) inserts at the cursor, keeping Ctrl+V a
    /// working paste key on terminals that never bracket-paste.
    ///
    /// No usable text (a screenshot copied to the clipboard carries image
    /// classes only, #1811) falls back to the clipboard's raw image bytes.
    pub(crate) fn attach_from_clipboard(&mut self) {
        let clip = Self::read_clipboard_text();
        let text = match super::clipboard_route::route_for_clipboard_text(clip.as_deref()) {
            super::clipboard_route::ClipboardRoute::Text(t) => t.to_string(),
            super::clipboard_route::ClipboardRoute::Image => {
                if !self.attach_image_from_clipboard("clipboard image") {
                    tracing::debug!("Ctrl+V: clipboard holds neither text nor an image");
                }
                return;
            }
        };
        let trimmed = text.trim().to_string();

        if Self::resolve_dropped_path(&trimmed).is_some() {
            // File-shaped: ride the full pipeline so classification,
            // receipts and project-file tracking all match a real drop.
            let extraction = Self::extract_attachments(&trimmed);
            for notice in extraction.notices {
                self.push_system_message(notice);
            }
            if !extraction.attachments.is_empty() {
                if let Some(session) = &self.current_session {
                    for att in &extraction.attachments {
                        let file_svc = self.file_service.clone();
                        let sid = session.id;
                        let path = std::path::PathBuf::from(&att.path);
                        tokio::spawn(async move {
                            if let Err(e) = file_svc.get_or_create_file(sid, path, None).await {
                                tracing::warn!("Failed to track Ctrl+V file: {e}");
                            }
                        });
                    }
                }
                let label = extraction.attachments[0].name.clone();
                self.attachments.extend(extraction.attachments);
                self.notification = Some(format!("📎 Attached from clipboard: {label}"));
                self.notification_shown_at = Some(std::time::Instant::now());
            }
            // Notes for doc/audio/archive/unknown/text files flow into the
            // input buffer below, exactly like a bracketed paste of the same
            // path does; with no clean text there is nothing to insert.
            // Bracketed paste scrubs at the bottom of this fn; the
            // Ctrl+V file-extraction path must not skip it (#1983).
            self.insert_text_at_cursor(&Self::strip_terminal_escapes(&extraction.text));
            return;
        }

        // Plain text: normal paste. Strip escape sequences and mouse-report
        // bursts first: the bracketed-paste handler already does, and a
        // clipboard copy of a leaked sequence would otherwise poison the
        // buffer the same way (#1943).
        let text = Self::strip_terminal_escapes(&text);
        self.insert_text_at_cursor(&text);
    }

    /// Insert `text` at the cursor (char-boundary safe, issue #69) and move
    /// the cursor to the end of what was inserted.
    pub(crate) fn insert_text_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.cursor_position = self
            .input_buffer
            .floor_char_boundary(self.cursor_position.min(self.input_buffer.len()));
        self.input_buffer.insert_str(self.cursor_position, text);
        self.cursor_position += text.len();
    }

    /// Delete the word before the cursor (for Ctrl+Backspace and Alt+Backspace)
    pub(crate) fn delete_last_word(&mut self) {
        if self.cursor_position == 0 {
            return;
        }
        let before = &self.input_buffer[..self.cursor_position];
        // Skip trailing whitespace
        let trimmed = before.trim_end();
        // Find the last whitespace boundary in the trimmed portion.
        // Use ceil_char_boundary to handle multi-byte whitespace safely.
        let word_start = trimmed
            .rfind(char::is_whitespace)
            .map(|pos| trimmed.ceil_char_boundary(pos + 1))
            .unwrap_or(0);
        // Remove from word_start to cursor_position
        self.input_buffer.drain(word_start..self.cursor_position);
        self.cursor_position = word_start;
    }

    /// History file path: ~/.opencrabs/history.txt
    fn history_path() -> Option<std::path::PathBuf> {
        Some(crate::config::opencrabs_home().join("history.txt"))
    }

    /// Load input history from disk (one entry per line, most recent last)
    pub(crate) fn load_history() -> Vec<String> {
        let Some(path) = Self::history_path() else {
            return Vec::new();
        };
        match std::fs::read_to_string(&path) {
            Ok(content) => content
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Append a single entry to the history file (and trim to 500 entries)
    fn save_history_entry(&self, entry: &str) {
        let Some(path) = Self::history_path() else {
            return;
        };
        // Ensure directory exists
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Cap at 500 entries: keep last 499 + new entry
        let max_entries = 500;
        if self.input_history.len() > max_entries {
            // Rewrite the whole file with only the last max_entries
            let start = self.input_history.len().saturating_sub(max_entries);
            let trimmed: Vec<&str> = self.input_history[start..]
                .iter()
                .map(|s| s.as_str())
                .collect();
            let _ = std::fs::write(&path, trimmed.join("\n") + "\n");
        } else {
            // Just append
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "{}", entry);
            }
        }
    }

    pub fn has_pending_approval(&self) -> bool {
        self.messages.iter().rev().any(|msg| {
            msg.approval
                .as_ref()
                .is_some_and(|a| a.state == ApprovalState::Pending)
        })
    }

    pub(crate) fn has_pending_approve_menu(&self) -> bool {
        self.messages.iter().rev().any(|msg| {
            msg.approve_menu
                .as_ref()
                .is_some_and(|m| m.state == ApproveMenuState::Pending)
        })
    }

    /// The dialog currently occupying input focus, if any (#1775).
    ///
    /// Single source for the Ctrl+C expanded command panel: whichever
    /// surface owns the keyboard reports its scope, and the panel renders
    /// that scope's keymap. Order matters — popup-style intercepts
    /// (pickers, password prompts, approval prompts) shadow the mode
    /// beneath them, mirroring the handler order in
    /// `handle_key_event`.
    pub(crate) fn active_dialog_scope(&self) -> Option<super::dialog_keys::DialogScope> {
        use super::dialog_keys::DialogScope;
        use super::events::AppMode;

        if self.theme_picker.is_some() {
            return Some(DialogScope::ThemePicker);
        }
        if self.ssh_pending.is_some() || self.sudo_pending.is_some() {
            return Some(DialogScope::SshPassword);
        }
        if self.has_pending_approve_menu() {
            return Some(DialogScope::ApprovePolicyMenu);
        }
        if self.has_pending_approval() {
            return Some(DialogScope::ToolApproval);
        }
        match self.mode {
            AppMode::Help => Some(DialogScope::Help),
            AppMode::Settings => Some(DialogScope::Settings),
            AppMode::UsageDashboard => Some(
                if self
                    .dashboard_state
                    .as_ref()
                    .is_some_and(|d| d.cache_detail_open)
                {
                    // The price breakdown sits over the grid, so it is the
                    // surface holding the keyboard (#2027).
                    DialogScope::UsageCacheBreakdown
                } else {
                    DialogScope::UsageDashboard
                },
            ),
            AppMode::FilePicker => Some(DialogScope::FilePicker),
            AppMode::DirectoryPicker => Some(DialogScope::DirectoryPicker),
            AppMode::RestartPending => Some(DialogScope::RestartPending),
            AppMode::UpdatePrompt => Some(DialogScope::UpdatePrompt),
            AppMode::PlanOverlay => Some(DialogScope::PlanOverlay),
            AppMode::SkillsList => Some(DialogScope::SkillsDialog),
            AppMode::Profiles => Some(DialogScope::ProfilesDialog),
            AppMode::MissionControl => Some(if self.mc.log_viewer.is_some() {
                DialogScope::McLogViewer
            } else if self.mc.detail_open {
                match self.mc.focused_panel {
                    McPanel::Inbox => DialogScope::McDetailPopup,
                    _ => DialogScope::McDetailRead,
                }
            } else {
                DialogScope::MissionControl
            }),
            // The sessions list is a full-screen surface with its own keys,
            // so it belongs in the census. Consequence: Ctrl+C inside it
            // expands the command panel instead of starting the quit flow,
            // which is what every other full-screen dialog already does;
            // Esc still returns to chat.
            AppMode::Sessions => Some(DialogScope::Sessions),
            // Chat has no dialog chrome; Projects/SessionFiles stay outside
            // the #1775 census; Onboarding keeps its contextual hints.
            AppMode::Chat | AppMode::Projects | AppMode::SessionFiles | AppMode::Onboarding => None,
        }
    }

    /// Handle keys in chat mode
    /// Abort the in-flight turn, preserving everything already on screen.
    ///
    /// Shared by double-Escape and by a typed stop intent (#965), so the two
    /// cannot drift: streamed text and tool groups are committed to history
    /// BEFORE the task dies, queued messages are dropped so they cannot leak
    /// into the next turn, and pending approvals are denied so agent callbacks
    /// do not hang waiting on a turn that no longer exists.
    pub(crate) async fn abort_active_turn(&mut self) {
        // Second Escape within 3 seconds — abort
        // PERSIST visible streaming content to DB BEFORE killing the task.
        // This prevents data loss: everything shown on screen survives cancel.
        if let Some(ref session) = self.current_session {
            self.persist_streaming_state(session.id).await;
        }
        // Cancel the active cancel token (cooperative)
        if let Some(token) = &self.cancel_token {
            token.cancel();
        }
        // Hard-abort the agent task as a backstop. Only THIS session's task:
        // the handle used to be app-level, so cancelling one session could
        // kill a turn still running in another (#2155).
        if let Some(sid) = self.current_session.as_ref().map(|s| s.id) {
            self.abort_session_task(sid);
        }
        // Also cancel any stashed session token (e.g. from session switch)
        if let Some(ref session) = self.current_session {
            let session_id = session.id;
            if let Some(stashed) = self.session_cancel_tokens.remove(&session_id) {
                stashed.cancel();
            }
            self.processing_sessions.remove(&session_id);
            // Clear the session's live state wherever it lives. Clearing only
            // the foreground fields below is enough in single-pane, where the
            // sidecar is never rendered, but in split view `panes.rs` draws the
            // sidecar entry as a live turn — so a cancelled session kept showing
            // "is thinking" forever (#1342).
            self.background_sessions.remove(&session_id);
        }
        self.is_processing = false;
        self.processing_started_at = None;
        // Leave a trace: the abort path emitted nothing, so a report of "cancel
        // did not work" could not be checked against the logs at all (#1342).
        tracing::info!(
            "TUI: turn aborted by user for session {:?}",
            self.current_session.as_ref().map(|s| s.id)
        );
        // Flush any in-flight tool group into the message list
        // BEFORE touching streaming state. Per-tool DB persistence
        // already wrote them to the message row, but the TUI's
        // DisplayMessage for the group only exists as
        // `active_tool_group` until the next flush point. Without
        // this, the user watches tools execute, hits Escape-twice,
        // and the whole group vanishes from view — only
        // reappearing on session reload from DB. Commit it to
        // `self.messages` so the history stays stable across cancel.
        let had_tool_activity = self
            .active_tool_group
            .as_ref()
            .is_some_and(|g| !g.calls.is_empty());
        if let Some(group) = self.active_tool_group.take()
            && !group.calls.is_empty()
        {
            let count = group.calls.len();
            self.messages.push(DisplayMessage {
                id: Uuid::new_v4(),
                role: "tool_group".to_string(),
                content: format!("{} tool call{}", count, if count == 1 { "" } else { "s" }),
                timestamp: chrono::Utc::now(),
                token_count: None,
                cost: None,
                approval: None,
                approve_menu: None,
                details: None,
                expanded: false,
                expanded_full: false,
                tool_group: Some(group),
                duration_secs: None,
            });
        }
        // Promote any in-progress streaming text + reasoning to
        // a permanent assistant DisplayMessage BEFORE clearing
        // them. Without this the user sees streaming content
        // during the turn, hits Escape-twice, and the whole
        // response vanishes from the in-memory history — the
        // DB still has it (persist_streaming_state above wrote
        // it) but rendering pulls from `self.messages`, so the
        // chat appears to reset to just the user's message
        // until restart. Push now so the render matches what
        // the user actually saw on screen.
        let stash_text = self.streaming_response.take();
        let stash_reasoning = self.streaming_reasoning.take();
        let has_visible_text = stash_text
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let has_visible_reasoning = stash_reasoning
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if has_visible_text || has_visible_reasoning {
            let content = stash_text
                .as_deref()
                .map(crate::utils::sanitize::strip_llm_artifacts)
                .unwrap_or_default();
            self.messages.push(DisplayMessage {
                id: Uuid::new_v4(),
                role: "assistant".to_string(),
                content,
                timestamp: chrono::Utc::now(),
                token_count: None,
                cost: None,
                approval: None,
                approve_menu: None,
                details: stash_reasoning,
                expanded: false,
                expanded_full: false,
                tool_group: None,
                duration_secs: None,
            });
        }
        // Cancelled before the agent produced ANYTHING: pull the
        // just-sent query back into the input (editable, ready to
        // resend) and remove it from the transcript + DB, so an
        // unanswered query doesn't linger and duplicate on resend
        // (#698). Only when the input is empty (don't clobber a
        // new draft) and the trailing message is that user query.
        if should_restore_cancelled_query(
            has_visible_text,
            has_visible_reasoning,
            had_tool_activity,
            self.intermediate_text_received,
        ) && self.input_buffer.trim().is_empty()
            && self.messages.last().is_some_and(|m| m.role == "user")
        {
            if let Some(m) = self.messages.pop() {
                // Scrub like every other buffer write: a queued message
                // captured before the gate existed could still carry a
                // burst (#1983).
                self.input_buffer = Self::strip_terminal_escapes(&m.content);
                self.cursor_position = self.input_buffer.len();
            }
            if let Some(sid) = self.current_session.as_ref().map(|s| s.id) {
                // Drop the unanswered query AND its empty assistant
                // placeholder as a pair — the placeholder is the
                // trailing DB row, so deleting only the last user
                // row would miss the query and leave it lingering
                // in the next turn's context, duplicating on
                // resend (#730).
                let repo =
                    crate::db::repository::MessageRepository::new(self.session_service.pool());
                if let Err(e) = repo.delete_trailing_unanswered_pair(sid).await {
                    tracing::warn!("Cancel-restore: failed to delete unanswered message pair: {e}");
                }
            }
        }
        self.streaming_render_cache = None;
        self.cancel_token = None;
        self.escape_pending_at = None;
        self.streaming_output_tokens = 0;
        self.intermediate_text_received = false;
        // Drop any queued user message — otherwise it would
        // survive the cancel and get injected into the NEXT
        // unrelated turn, appearing as a duplicate in chat.
        if let Some(sid) = self.current_session.as_ref().map(|s| s.id)
            && let Ok(mut q) = self.queued_messages.lock()
        {
            q.remove(&sid);
        }
        // Deny any pending approvals so agent callbacks don't hang
        for msg in &mut self.messages {
            if let Some(ref mut approval) = msg.approval
                && approval.state == ApprovalState::Pending
            {
                let _ = approval.response_tx.send(ToolApprovalResponse {
                    request_id: approval.request_id,
                    approved: false,
                    reason: Some("Operation cancelled".to_string()),
                });
                approval.state = ApprovalState::Denied("Operation cancelled".to_string());
            }
        }
    }

    pub(crate) async fn handle_chat_key(
        &mut self,
        event: crossterm::event::KeyEvent,
    ) -> Result<()> {
        use super::events::keys;
        use crossterm::event::{KeyCode, KeyModifiers};

        // Intercept keys when /approve menu is pending
        if self.has_pending_approve_menu() {
            if keys::is_up(&event) {
                if let Some(menu) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find_map(|m| m.approve_menu.as_mut())
                    .filter(|m| m.state == ApproveMenuState::Pending)
                {
                    menu.selected_option = menu.selected_option.saturating_sub(1);
                }
                return Ok(());
            } else if keys::is_down(&event) {
                if let Some(menu) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find_map(|m| m.approve_menu.as_mut())
                    .filter(|m| m.state == ApproveMenuState::Pending)
                {
                    menu.selected_option = (menu.selected_option + 1).min(2);
                }
                return Ok(());
            } else if keys::is_enter(&event) || keys::is_submit(&event) {
                let selected = self
                    .messages
                    .iter()
                    .rev()
                    .find_map(|m| m.approve_menu.as_ref())
                    .filter(|m| m.state == ApproveMenuState::Pending)
                    .map(|m| m.selected_option)
                    .unwrap_or(0);

                // Apply policy
                match selected {
                    0 => {
                        // Reset to approve-only
                        self.approval_auto_session = false;
                        self.approval_auto_always = false;
                    }
                    1 => {
                        // Allow all for this session
                        self.approval_auto_session = true;
                        self.approval_auto_always = false;
                    }
                    _ => {
                        // Yolo mode
                        self.approval_auto_session = false;
                        self.approval_auto_always = true;
                    }
                }

                let label = match selected {
                    0 => "Approve-only (always ask)",
                    1 => "Allow all for this session",
                    _ => "Yolo mode (execute without approval)",
                };

                // Mark menu as resolved
                if let Some(menu) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find_map(|m| m.approve_menu.as_mut())
                    .filter(|m| m.state == ApproveMenuState::Pending)
                {
                    menu.state = ApproveMenuState::Selected(selected);
                }

                // Persist to config.toml
                let policy_str = match selected {
                    0 => "ask",
                    1 => "auto-session",
                    _ => "auto-always",
                };
                if let Err(e) =
                    crate::config::Config::write_key("agent", "approval_policy", policy_str)
                {
                    tracing::warn!("Failed to persist approval policy: {}", e);
                }

                // Rebuild the agent service so tool_loop.auto_approve_tools
                // matches the new policy — otherwise the compaction/continuation
                // system prompt keeps injecting "AUTO-APPROVE OFF" even after
                // the user switches to yolo mode.
                if let Err(e) = self.rebuild_agent_service().await {
                    tracing::warn!("Failed to rebuild agent service after /approve: {}", e);
                }

                self.push_system_message(format!("Approval policy set to: {}", label));
                return Ok(());
            } else if keys::is_cancel(&event) {
                // Cancel — dismiss menu without changing policy
                if let Some(menu) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find_map(|m| m.approve_menu.as_mut())
                    .filter(|m| m.state == ApproveMenuState::Pending)
                {
                    menu.state = ApproveMenuState::Selected(99); // sentinel for cancelled
                }
                self.push_system_message("Approval policy unchanged.".to_string());
                return Ok(());
            }
            return Ok(());
        }

        // Intercept keys when an inline approval is pending
        // Options: Yes(0), Always(1), No(2)
        if self.has_pending_approval() {
            if keys::is_left(&event) || keys::is_up(&event) {
                // Navigate options left
                if let Some(approval) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find_map(|m| m.approval.as_mut())
                    .filter(|a| a.state == ApprovalState::Pending)
                {
                    approval.selected_option = approval.selected_option.saturating_sub(1);
                }
                return Ok(());
            } else if keys::is_right(&event) || keys::is_down(&event) {
                // Navigate options right
                if let Some(approval) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find_map(|m| m.approval.as_mut())
                    .filter(|a| a.state == ApprovalState::Pending)
                {
                    approval.selected_option = (approval.selected_option + 1).min(2);
                }
                return Ok(());
            } else if keys::is_enter(&event) || keys::is_submit(&event) {
                // Confirm: Yes(0)=approve once, Always(1)=approve always, No(2)=deny
                let approval_data: Option<(
                    Uuid,
                    usize,
                    mpsc::UnboundedSender<ToolApprovalResponse>,
                )> = self
                    .messages
                    .iter()
                    .rev()
                    .find_map(|m| m.approval.as_ref())
                    .filter(|a| a.state == ApprovalState::Pending)
                    .map(|a| (a.request_id, a.selected_option, a.response_tx.clone()));

                if let Some((request_id, selected, response_tx)) = approval_data {
                    if selected == 2 {
                        // "No" — deny
                        let response = ToolApprovalResponse {
                            request_id,
                            approved: false,
                            reason: Some("User denied permission".to_string()),
                        };
                        if let Err(e) = response_tx.send(response.clone()) {
                            tracing::error!(
                                "Failed to send denial response back to agent: {:?}",
                                e
                            );
                        }
                        let _ = self
                            .event_sender()
                            .send(TuiEvent::ToolApprovalResponse(response));
                    } else {
                        // "Yes" (0) or "Always" (1)
                        let option = if selected == 1 {
                            ApprovalOption::AllowAlways
                        } else {
                            ApprovalOption::AllowOnce
                        };
                        if matches!(option, ApprovalOption::AllowAlways) {
                            self.approval_auto_session = true;
                            crate::utils::persist_auto_session_policy();
                            self.push_system_message(
                                "Auto-approve enabled for this session. Use /approve to reset."
                                    .to_string(),
                            );
                        }
                        let response = ToolApprovalResponse {
                            request_id,
                            approved: true,
                            reason: None,
                        };
                        if let Err(e) = response_tx.send(response.clone()) {
                            tracing::error!(
                                "Failed to send approval response back to agent: {:?}",
                                e
                            );
                        }
                        let _ = self
                            .event_sender()
                            .send(TuiEvent::ToolApprovalResponse(response));
                    }
                    // Remove resolved approval messages to prevent channel accumulation
                    self.messages.retain(|m| {
                        m.approval
                            .as_ref()
                            .is_none_or(|a| a.request_id != request_id)
                    });
                }
                return Ok(());
            } else if event.code == KeyCode::Char('a') && event.modifiers == KeyModifiers::NONE {
                // `a` — approve once directly (#1775): single-key verb the
                // shared footer advertises. Independent of the highlighted
                // option (Yes/Always/No): always approves ONCE, never
                // touches the session auto policy.
                let approval_data: Option<(Uuid, mpsc::UnboundedSender<ToolApprovalResponse>)> =
                    self.messages
                        .iter()
                        .rev()
                        .find_map(|m| m.approval.as_ref())
                        .filter(|a| a.state == ApprovalState::Pending)
                        .map(|a| (a.request_id, a.response_tx.clone()));

                if let Some((request_id, response_tx)) = approval_data {
                    let response = ToolApprovalResponse {
                        request_id,
                        approved: true,
                        reason: None,
                    };
                    if let Err(e) = response_tx.send(response.clone()) {
                        tracing::error!("Failed to send approval response back to agent: {:?}", e);
                    }
                    let _ = self
                        .event_sender()
                        .send(TuiEvent::ToolApprovalResponse(response));
                    // Remove resolved approval message
                    self.messages.retain(|m| {
                        m.approval
                            .as_ref()
                            .is_none_or(|a| a.request_id != request_id)
                    });
                }
                return Ok(());
            } else if keys::is_deny(&event) || keys::is_cancel(&event) {
                // D/Esc shortcut — deny directly
                let approval_data: Option<(Uuid, mpsc::UnboundedSender<ToolApprovalResponse>)> =
                    self.messages
                        .iter()
                        .rev()
                        .find_map(|m| m.approval.as_ref())
                        .filter(|a| a.state == ApprovalState::Pending)
                        .map(|a| (a.request_id, a.response_tx.clone()));

                if let Some((request_id, response_tx)) = approval_data {
                    let response = ToolApprovalResponse {
                        request_id,
                        approved: false,
                        reason: Some("User denied permission".to_string()),
                    };
                    if let Err(e) = response_tx.send(response.clone()) {
                        tracing::error!("Failed to send denial response back to agent: {:?}", e);
                    }
                    let _ = self
                        .event_sender()
                        .send(TuiEvent::ToolApprovalResponse(response));
                    // Remove resolved approval message
                    self.messages.retain(|m| {
                        m.approval
                            .as_ref()
                            .is_none_or(|a| a.request_id != request_id)
                    });
                }
                return Ok(());
            } else if keys::is_view_details(&event) {
                // V key — toggle details
                if let Some(approval) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find_map(|m| m.approval.as_mut())
                    .filter(|a| a.state == ApprovalState::Pending)
                {
                    approval.show_details = !approval.show_details;
                }
                return Ok(());
            } else if event.code == KeyCode::Char('o') && event.modifiers == KeyModifiers::CONTROL {
                // Allow Ctrl+O during approval so user can collapse the newest
                // turn far enough to see the approval prompt. Details are left
                // alone here: cycling them can grow the view, which is the
                // opposite of what this key press is for.
                self.toggle_newest_turn(false);
                return Ok(());
            }
            // Other keys ignored while approval pending
            return Ok(());
        }

        // When slash suggestions are active, intercept navigation keys
        if self.slash_suggestions_active {
            if keys::is_up(&event) {
                self.slash_selected_index = self.slash_selected_index.saturating_sub(1);
                return Ok(());
            } else if keys::is_down(&event) {
                if !self.slash_filtered.is_empty() {
                    self.slash_selected_index =
                        (self.slash_selected_index + 1).min(self.slash_filtered.len() - 1);
                }
                return Ok(());
            } else if keys::is_enter(&event) || keys::is_submit(&event) {
                // Select the highlighted command and execute it
                if let Some(&cmd_idx) = self.slash_filtered.get(self.slash_selected_index) {
                    let cmd_name = self.slash_command_name(cmd_idx).unwrap_or("").to_string();
                    self.input_buffer.clear();
                    self.cursor_position = 0;
                    self.slash_suggestions_active = false;
                    self.handle_slash_command(&cmd_name).await;
                }
                return Ok(());
            } else if keys::is_cancel(&event) {
                // Dismiss dropdown but keep input
                self.slash_suggestions_active = false;
                return Ok(());
            }
            // Other keys fall through to normal handling
        }

        // When emoji picker is active, intercept navigation keys
        if self.emoji_picker_active {
            if keys::is_up(&event) {
                self.emoji_selected_index = self.emoji_selected_index.saturating_sub(1);
                return Ok(());
            } else if keys::is_down(&event) {
                if !self.emoji_filtered.is_empty() {
                    self.emoji_selected_index =
                        (self.emoji_selected_index + 1).min(self.emoji_filtered.len() - 1);
                }
                return Ok(());
            } else if event.code == KeyCode::Tab
                || keys::is_enter(&event)
                || keys::is_submit(&event)
            {
                self.accept_emoji();
                return Ok(());
            } else if keys::is_cancel(&event) {
                self.dismiss_emoji_picker();
                return Ok(());
            }
            // Other keys fall through to normal handling (typing more chars refines the filter)
        }

        // Follow-up suggestions (#596): ghost text (1 option) or pick-list (2+),
        // only while the input is empty. Accepting FILLS the input as editable
        // text and never submits, so the user can edit before sending. Typing
        // any other key falls through and the suggestions clear when the buffer
        // is no longer empty (both the render and this block gate on that).
        if !self.followup_suggestions.is_empty() && self.input_buffer.is_empty() {
            let multi = self.followup_suggestions.len() >= 2;
            if multi && keys::is_up(&event) {
                self.followup_selected_index = self.followup_selected_index.saturating_sub(1);
                return Ok(());
            } else if multi && keys::is_down(&event) {
                self.followup_selected_index =
                    (self.followup_selected_index + 1).min(self.followup_suggestions.len() - 1);
                return Ok(());
            } else if event.code == KeyCode::Tab
                || (multi && (keys::is_enter(&event) || keys::is_submit(&event)))
            {
                let idx = if multi {
                    self.followup_selected_index
                } else {
                    0
                };
                if let Some(text) = self.followup_suggestions.get(idx).cloned() {
                    // Same as the cancel-restore above: followup text can
                    // arrive from a model output that echoed a raw burst.
                    self.input_buffer = Self::strip_terminal_escapes(&text);
                    self.cursor_position = self.input_buffer.len();
                }
                self.followup_suggestions.clear();
                self.followup_selected_index = 0;
                return Ok(());
            } else if keys::is_cancel(&event) {
                self.followup_suggestions.clear();
                self.followup_selected_index = 0;
                return Ok(());
            }
            // Any other key falls through to normal handling.
        }

        // Any key other than Escape resets escape confirmation
        if !keys::is_cancel(&event) {
            self.escape_pending_at = None;
        }

        // --- Attachment focus navigation ---
        // When an attachment is focused, Up/Down navigate between attachments,
        // Backspace/Delete removes the focused one, any other key returns to input.
        if self.focused_attachment.is_some() {
            if keys::is_up(&event) {
                // Move to previous attachment (or stay at first)
                if let Some(idx) = self.focused_attachment
                    && idx > 0
                {
                    self.focused_attachment = Some(idx - 1);
                }
                return Ok(());
            } else if keys::is_down(&event) {
                // Move to next attachment, or return to input if at last
                if let Some(idx) = self.focused_attachment {
                    if idx + 1 < self.attachments.len() {
                        self.focused_attachment = Some(idx + 1);
                    } else {
                        self.focused_attachment = None; // back to input
                    }
                }
                return Ok(());
            } else if event.code == KeyCode::Backspace || event.code == KeyCode::Delete {
                // Remove the focused attachment
                if let Some(idx) = self.focused_attachment
                    && idx < self.attachments.len()
                {
                    self.attachments.remove(idx);
                    // Adjust focus: stay on same index if more remain, else move back
                    if self.attachments.is_empty() {
                        self.focused_attachment = None;
                    } else if idx >= self.attachments.len() {
                        self.focused_attachment = Some(self.attachments.len() - 1);
                    }
                }
                return Ok(());
            } else if keys::is_cancel(&event) {
                // Escape returns to input without removing
                self.focused_attachment = None;
                return Ok(());
            } else {
                // Any other key returns to input
                self.focused_attachment = None;
                // Fall through to handle the key normally
            }
        }

        if keys::is_newline(&event) {
            // Alt+Enter or Shift+Enter = insert newline for multi-line input
            self.input_buffer.insert(self.cursor_position, '\n');
            self.cursor_position += 1;
        } else if keys::is_submit(&event)
            && (!self.input_buffer.trim().is_empty() || !self.attachments.is_empty())
        {
            // Pre-send scrub (#1943): the per-char gate, the paste path and
            // the FocusGained cleaner each have blind spots: a burst split
            // across crossterm reads lands here verbatim, so take one more
            // pass over the buffer before it becomes a message or a slash
            // command. A poisoned buffer starts with `[<35;…`, which kills
            // every `starts_with('/')` dispatch check.
            let scrubbed = Self::strip_terminal_escapes(&self.input_buffer);
            if scrubbed != self.input_buffer {
                tracing::debug!(
                    "Scrubbed {} bytes of escape garbage before send",
                    self.input_buffer.len() - scrubbed.len()
                );
                self.input_buffer = scrubbed;
                self.cursor_position = self
                    .input_buffer
                    .floor_char_boundary(self.cursor_position.min(self.input_buffer.len()));
            }
            if self.input_buffer.trim().is_empty() && self.attachments.is_empty() {
                // Only garbage was in the buffer: swallow the submit.
                return Ok(());
            }
            // Check for slash commands before sending to LLM
            let content = self.input_buffer.clone();
            if self.handle_slash_command(content.trim()).await {
                // Don't clear input if it was a rejected unknown command (error_message is set)
                if self.error_message.is_none() {
                    self.input_buffer.clear();
                    self.cursor_position = 0;
                }
                self.slash_suggestions_active = false;
                self.dismiss_emoji_picker();
                return Ok(());
            }

            // A typed stop intent aborts the running turn instead of queueing
            // another message (#965). The TUI previously had no text-based
            // stop at all: cancelling was key-driven, so typing "stop" just
            // added to the pile the agent was already working through.
            //
            // Gated on a turn actually being in flight. With nothing running
            // there is nothing to cancel and the words are ordinary input.
            if self.is_processing && crate::utils::stop_intent::is_stop_intent(content.trim()) {
                self.input_buffer.clear();
                self.cursor_position = 0;
                self.slash_suggestions_active = false;
                self.dismiss_emoji_picker();
                self.abort_active_turn().await;
                self.push_system_message("Operation cancelled.".to_string());
                return Ok(());
            }

            // Bang operator: `!cmd args` runs a shell command directly in the
            // current working directory and pushes stdout/stderr as a system
            // message — no LLM, no tool approval. Mirrors Claude Code's `!`.
            if let Some(shell_cmd) = content.trim().strip_prefix('!') {
                let shell_cmd = shell_cmd.trim().to_string();
                if !shell_cmd.is_empty() {
                    self.push_system_message(format!("$ {}", shell_cmd));
                    let cwd = self.working_directory.clone();
                    let sender = self.event_sender();
                    // Capture the session id at submit time so the output
                    // lands in the same session that fired the command —
                    // even if the user Tabs to another pane while sh runs.
                    let origin_session = self
                        .current_session
                        .as_ref()
                        .map(|s| s.id)
                        .unwrap_or_else(uuid::Uuid::nil);
                    // Editor handoff (#1744): editors need the real tty.
                    // Pipe-capturing them dumps escape sequences into the
                    // chat. Park the request; the runner loop performs the
                    // terminal handoff before its next draw. Unix only
                    // (#1755): Windows keeps the pipe path below.
                    // $EDITOR/$VISUAL are honoured too (#2173): the user
                    // chose that program, so a `!kak` matching their editor
                    // gets the tty instead of dumping raw control codes.
                    #[cfg(unix)]
                    {
                        let env_editor = std::env::var("EDITOR")
                            .ok()
                            .or_else(|| std::env::var("VISUAL").ok());
                        if crate::tui::editor::handoff_target(&shell_cmd, env_editor.as_deref())
                            .is_some()
                        {
                            self.pending_editor_handoff = Some((shell_cmd, origin_session));
                            self.input_buffer.clear();
                            self.cursor_position = 0;
                            self.slash_suggestions_active = false;
                            self.dismiss_emoji_picker();
                            return Ok(());
                        }
                    }
                    tokio::spawn(async move {
                        use crate::utils::shell::PushShellCommand;
                        let (shell, shell_arg) = crate::utils::shell::shell_pair();
                        let output = tokio::process::Command::new(shell)
                            .push_shell_command(shell_arg, &shell_cmd)
                            .current_dir(&cwd)
                            .output()
                            .await;
                        let msg = match output {
                            Ok(out) => {
                                let stdout = String::from_utf8_lossy(&out.stdout);
                                let stderr = String::from_utf8_lossy(&out.stderr);
                                let mut combined = String::new();
                                if !stdout.is_empty() {
                                    combined.push_str(stdout.trim_end());
                                }
                                if !stderr.is_empty() {
                                    if !combined.is_empty() {
                                        combined.push('\n');
                                    }
                                    combined.push_str(stderr.trim_end());
                                }
                                if combined.is_empty() {
                                    if out.status.success() {
                                        "(no output)".to_string()
                                    } else {
                                        format!("(no output, exit {})", out.status)
                                    }
                                } else if !out.status.success() {
                                    format!("{}\n(exit {})", combined, out.status)
                                } else {
                                    combined
                                }
                            }
                            Err(e) => format!("Failed to run command: {}", e),
                        };
                        let _ = sender.send(TuiEvent::SystemMessage {
                            session_id: origin_session,
                            text: msg,
                        });
                    });
                }
                self.input_buffer.clear();
                self.cursor_position = 0;
                self.slash_suggestions_active = false;
                self.dismiss_emoji_picker();
                return Ok(());
            }

            // Also scan typed input for image paths at submit time
            let extraction = Self::extract_attachments(&content);
            // A file copied across the drop tunnel says so in the chat, with
            // where it landed (#1311).
            for notice in extraction.notices {
                self.push_system_message(notice);
            }
            let (clean_text, typed_attachments) = (extraction.text, extraction.attachments);
            let mut all_attachments = std::mem::take(&mut self.attachments);
            all_attachments.extend(typed_attachments);

            let final_content =
                if !all_attachments.is_empty() && clean_text.trim() != content.trim() {
                    clean_text
                } else {
                    content.clone()
                };

            // Enter = send message
            // Save to input history (dedup consecutive) and persist to disk
            let trimmed = content.trim().to_string();
            if self.input_history.last() != Some(&trimmed) {
                self.input_history.push(trimmed.clone());
                self.save_history_entry(&trimmed);
            }
            self.input_history_index = None;
            self.input_history_stash.clear();

            self.input_buffer.clear();
            self.cursor_position = 0;
            self.attachments.clear();
            self.focused_attachment = None;
            self.slash_suggestions_active = false;
            self.dismiss_emoji_picker();

            // Build message content with attachment markers for the agent.
            // Format: `<<IMG:/path>>` for images, `<<VID:/path>>` for videos —
            // handles spaces in paths. The marker tells the agent which tool
            // to call (`analyze_image` vs `analyze_video`).
            let send_content = if all_attachments.is_empty() {
                final_content
            } else {
                let mut msg = final_content.clone();
                for att in &all_attachments {
                    let prefix = if att.is_video { "VID" } else { "IMG" };
                    msg.push_str(&format!(" <<{}:{}>>", prefix, att.path));
                }
                msg
            };
            self.send_message(send_content).await?;
        } else if keys::is_cancel(&event) {
            // When processing, double-Escape aborts the operation
            if self.is_processing {
                if let Some(pending_at) = self.escape_pending_at {
                    if pending_at.elapsed() < std::time::Duration::from_secs(3) {
                        self.abort_active_turn().await;
                        // Reload session from DB so tool calls appear inline
                        // (matching the persisted order from expand_message) instead
                        // of being stacked at the bottom from in-memory finalization.
                        if let Some(ref session) = self.current_session {
                            let session_id = session.id;
                            self.load_session(session_id).await?;
                        }
                        // Only claimed once the turn is actually torn down: this
                        // used to print on the expired path too, telling the user
                        // the operation had stopped while it kept running (#1343).
                        self.push_system_message("Operation cancelled.".to_string());
                    } else {
                        // Expired — treat as a first Escape again. Without this the
                        // stale timestamp made every later Escape take the expired
                        // path, so the abort became unreachable until some other key
                        // reset it (#1343).
                        self.escape_pending_at = Some(std::time::Instant::now());
                        self.error_message = Some("Press Esc again to abort".to_string());
                        self.error_message_shown_at = Some(std::time::Instant::now());
                    }
                } else {
                    self.escape_pending_at = Some(std::time::Instant::now());
                    self.error_message = Some("Press Esc again to abort".to_string());
                    self.error_message_shown_at = Some(std::time::Instant::now());
                }
            } else if !self.auto_scroll {
                // User is scrolled up — scroll to bottom first
                self.scroll_offset = 0;
                self.auto_scroll = true;
                self.error_message = None;
                self.error_message_shown_at = None;
                self.escape_pending_at = None;
            } else if self.input_buffer.is_empty() {
                // Nothing to clear, just dismiss error
                self.error_message = None;
                self.error_message_shown_at = None;
                self.escape_pending_at = None;
            } else if let Some(pending_at) = self.escape_pending_at {
                if pending_at.elapsed() < std::time::Duration::from_secs(3) {
                    // Second Escape within 3 seconds — stash input then clear
                    // Arrow Up will recover the stashed text
                    if !self.input_buffer.is_empty() {
                        self.input_history_stash = self.input_buffer.clone();
                    }
                    self.input_buffer.clear();
                    self.cursor_position = 0;
                    self.attachments.clear();
                    self.focused_attachment = None;
                    self.error_message = None;
                    self.error_message_shown_at = None;
                    self.escape_pending_at = None;
                    self.slash_suggestions_active = false;
                    self.dismiss_emoji_picker();
                } else {
                    // Expired — treat as first Escape again
                    self.escape_pending_at = Some(std::time::Instant::now());
                    self.error_message = Some("Press Esc again to clear input".to_string());
                    self.error_message_shown_at = Some(std::time::Instant::now());
                }
            } else {
                // First Escape — show confirmation hint
                self.escape_pending_at = Some(std::time::Instant::now());
                self.error_message = Some("Press Esc again to clear input".to_string());
                self.error_message_shown_at = Some(std::time::Instant::now());
            }
        } else if event.code == KeyCode::Char('o') && event.modifiers == KeyModifiers::CONTROL {
            // Ctrl+O: open or close the newest turn, its fold, its tool groups
            // and its reasoning details. Scoped to that turn so a long
            // transcript does not reflow out from under the user.
            self.toggle_newest_turn(true);
        } else if keys::is_clipboard_paste(&event) {
            // Ctrl+V (#1740), or Cmd+V passed through as SUPER (#1812): paste
            // or attach from the OS clipboard. File-shaped clipboard text
            // attaches through the same pipeline as a drop (any file type);
            // plain text inserts at the cursor, so terminals that never
            // bracket-paste still get a working paste key.
            self.attach_from_clipboard();
        } else if keys::is_page_up(&event) {
            let before = self.scroll_offset;
            self.scroll_offset = self.scroll_offset.saturating_add(10);
            self.auto_scroll = false;
            tracing::debug!(
                "[SCROLL] PageUp: {} -> {} (auto_scroll=false)",
                before,
                self.scroll_offset
            );
            // Load more history when paging up if hidden messages exist
            if self.hidden_older_messages > 0
                && self.messages.len() < Self::MAX_DISPLAYED_MESSAGES
                && let Err(e) = self.load_more_history().await
            {
                tracing::warn!(error = %e, "failed to load more history");
            }
        } else if keys::is_page_down(&event) {
            let before = self.scroll_offset;
            self.scroll_offset = self.scroll_offset.saturating_sub(10);
            tracing::debug!("[SCROLL] PageDown: {} -> {}", before, self.scroll_offset);
            if self.scroll_offset == 0 {
                self.auto_scroll = true;
            }
        } else if (event.code == KeyCode::Backspace
            && event
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL))
            || (event.code == KeyCode::Char('w') && event.modifiers == KeyModifiers::CONTROL)
        {
            // Delete the word before the cursor. Alt+Backspace is the macOS
            // convention, but it only reaches us when the terminal sends
            // Option-as-Meta (ESC + DEL); terminals that don't (Warp's
            // default) never produce the ALT modifier. Ctrl+W (readline's
            // word-erase, a literal 0x17 every terminal sends) and
            // Ctrl+Backspace are the cross-terminal fallbacks.
            self.delete_last_word();
        } else if keys::is_up(&event)
            && !self.slash_suggestions_active
            && !self.input_buffer.is_empty()
            && self.can_cursor_move_up()
        {
            // Arrow Up — navigate lines within the buffer. Runs regardless
            // of whether we're inside a history browse, so a recalled
            // multiline entry is navigable. Only falls through to history
            // when the cursor is already at the top and can't move further.
            if !self.cursor_visual_up() {
                // Already on first visual line of this logical line
                let line_start = self.cursor_line_position().0;
                if line_start == 0 {
                    // First logical line — move to start
                    self.cursor_position = 0;
                } else {
                    // Move to previous logical line, try to land on its last visual row
                    // at the same column offset
                    use unicode_width::UnicodeWidthStr;
                    let prev_line_end = line_start - 1;
                    let prev_line_start = self.input_buffer[..prev_line_end]
                        .rfind('\n')
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    let prev_line = &self.input_buffer[prev_line_start..prev_line_end];
                    let prev_total_width = prev_line.width();
                    let vw = self.input_visual_line_width();
                    let last_row = if prev_total_width == 0 {
                        0
                    } else {
                        (prev_total_width.saturating_sub(1)) / vw
                    };
                    let current_col = self.input_buffer[line_start..self.cursor_position].width();
                    let target_col = last_row * vw
                        + (current_col % vw).min(prev_total_width.saturating_sub(last_row * vw));
                    self.cursor_position =
                        prev_line_start + byte_offset_at_display_col(prev_line, target_col);
                }
            }
        } else if keys::is_down(&event)
            && !self.slash_suggestions_active
            && !self.input_buffer.is_empty()
            && self.can_cursor_move_down()
        {
            // Arrow Down — navigate lines within the buffer. Runs regardless
            // of whether we're inside a history browse, so a recalled
            // multiline entry is navigable. Only falls through to history
            // when the cursor is already at the bottom.
            if !self.cursor_visual_down() {
                // Already on last visual line of this logical line
                let line_start = self.cursor_line_position().0;
                let line_end = self.input_buffer[line_start..]
                    .find('\n')
                    .map(|i| line_start + i)
                    .unwrap_or(self.input_buffer.len());
                if line_end == self.input_buffer.len() {
                    // Last logical line — move to end
                    self.cursor_position = self.input_buffer.len();
                } else {
                    // Move to next logical line, first visual row, same column offset
                    use unicode_width::UnicodeWidthStr;
                    let next_line_start = line_end + 1;
                    let next_line_end = self.input_buffer[next_line_start..]
                        .find('\n')
                        .map(|i| next_line_start + i)
                        .unwrap_or(self.input_buffer.len());
                    let next_line = &self.input_buffer[next_line_start..next_line_end];
                    let current_col = self.input_buffer[line_start..self.cursor_position].width();
                    let vw = self.input_visual_line_width();
                    let target_col = (current_col % vw).min(next_line.width());
                    self.cursor_position =
                        next_line_start + byte_offset_at_display_col(next_line, target_col);
                }
            }
        } else if keys::is_up(&event)
            && !self.slash_suggestions_active
            && !self.attachments.is_empty()
            && self.cursor_position == 0
            && self.input_history_index.is_none()
        {
            // Arrow Up at start of input with attachments — focus last attachment.
            // User can then Up/Down to navigate, Backspace/Delete to remove.
            self.focused_attachment = Some(self.attachments.len() - 1);
        } else if keys::is_up(&event)
            && !self.slash_suggestions_active
            // Only dequeue-for-edit when the input is empty — otherwise the
            // user is mid-typing their next message and Arrow Up must NOT
            // clobber it with the queued text (it falls through to normal
            // history navigation, which stashes the draft instead).
            && self.input_buffer.is_empty()
            && {
                let sid = self.current_session.as_ref().map(|s| s.id);
                sid.is_some()
                    && self
                        .queued_messages
                        .lock()
                        .map(|q| q.contains_key(&sid.unwrap()))
                        .unwrap_or(false)
            }
            && self.input_history_index.is_none()
        {
            // Arrow Up on EMPTY input while a message is queued — dequeue it
            // for editing. Moves it into the input_buffer, removing from the
            // per-session queue map.
            if let Some(sid) = self.current_session.as_ref().map(|s| s.id)
                && let Ok(mut q) = self.queued_messages.lock()
                && let Some(msgs) = q.remove(&sid)
            {
                drop(q); // release lock before touching self.input_buffer
                // Restore what the user typed, never the synthetic context a
                // system entry carries for the model.
                self.input_buffer = msgs
                    .into_iter()
                    .map(|m| m.display_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.cursor_position = self.input_buffer.len();
            }
        } else if keys::is_up(&event)
            && !self.slash_suggestions_active
            && self.input_buffer.is_empty()
            && !self.input_history_stash.is_empty()
            && self.input_history_index.is_none()
        {
            // Arrow Up on empty input — restore stashed text first (cleared via Esc)
            self.input_buffer = std::mem::take(&mut self.input_history_stash);
            self.cursor_position = self.input_buffer.len();
        } else if keys::is_up(&event)
            && !self.slash_suggestions_active
            && !self.input_history.is_empty()
            && (self.input_buffer.is_empty() || self.input_history_index.is_some())
        {
            // Arrow Up — browse input history (older). Only when the buffer
            // is empty (nothing to lose) or we're already inside history.
            match self.input_history_index {
                None => {
                    // Entering history — stash current input
                    self.input_history_stash = self.input_buffer.clone();
                    let idx = self.input_history.len() - 1;
                    self.input_history_index = Some(idx);
                    // Scrub on recall: history.txt predating #1943 can
                    // still carry a poisoned line, and Arrow-Up would
                    // reinstate it verbatim into the buffer.
                    self.input_buffer = Self::strip_terminal_escapes(&self.input_history[idx]);
                    self.cursor_position = self.input_buffer.len();
                }
                Some(idx) if idx > 0 => {
                    let idx = idx - 1;
                    self.input_history_index = Some(idx);
                    // Scrub on recall: history.txt predating #1943 can
                    // still carry a poisoned line, and Arrow-Up would
                    // reinstate it verbatim into the buffer.
                    self.input_buffer = Self::strip_terminal_escapes(&self.input_history[idx]);
                    self.cursor_position = self.input_buffer.len();
                }
                _ => {} // already at oldest
            }
        } else if keys::is_down(&event)
            && !self.slash_suggestions_active
            && self.input_history_index.is_some()
        {
            // Arrow Down — browse input history (newer)
            let idx = self.input_history_index.expect("checked is_some");
            if idx + 1 < self.input_history.len() {
                let idx = idx + 1;
                self.input_history_index = Some(idx);
                self.input_buffer = Self::strip_terminal_escapes(&self.input_history[idx]);
                self.cursor_position = self.input_buffer.len();
            } else {
                // Past newest — restore stashed input
                self.input_history_index = None;
                self.input_buffer = std::mem::take(&mut self.input_history_stash);
                self.cursor_position = self.input_buffer.len();
            }
        } else {
            // Regular character input
            match event.code {
                KeyCode::Char('@') => {
                    self.open_file_picker().await?;
                }
                KeyCode::Char(c) if keys::types_character(event.modifiers) => {
                    // Mouse-report fragments are already dropped by the
                    // dispatcher gate (`MouseFragGate`, #1983/#2233) before
                    // this handler runs, so anything reaching here is real
                    // typing - including `arr[0]` and `[0; 4]`.
                    self.input_buffer.insert(self.cursor_position, c);
                    self.cursor_position += c.len_utf8();
                }
                KeyCode::Backspace if event.modifiers.is_empty() && self.cursor_position > 0 => {
                    // Find the previous char boundary
                    let prev = self.input_buffer[..self.cursor_position]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.input_buffer.remove(prev);
                    self.cursor_position = prev;
                }
                KeyCode::Delete
                    if event.modifiers.is_empty()
                        && self.cursor_position < self.input_buffer.len() =>
                {
                    self.input_buffer.remove(self.cursor_position);
                }
                KeyCode::Left
                    if event.modifiers.is_empty()
                    // Move cursor left one character
                    && self.cursor_position > 0 =>
                {
                    let prev = self.input_buffer[..self.cursor_position]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.cursor_position = prev;
                }
                KeyCode::Right
                    if event.modifiers.is_empty()
                    // Move cursor right one character
                    && self.cursor_position < self.input_buffer.len() =>
                {
                    let next = self.input_buffer[self.cursor_position..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.cursor_position + i)
                        .unwrap_or(self.input_buffer.len());
                    self.cursor_position = next;
                }
                KeyCode::Home => {
                    // Jump to start of current line (not absolute start)
                    let line_start = self.input_buffer[..self.cursor_position]
                        .rfind('\n')
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    self.cursor_position = line_start;
                }
                KeyCode::End => {
                    // Jump to end of current line (not absolute end)
                    let line_end = self.input_buffer[self.cursor_position..]
                        .find('\n')
                        .map(|i| self.cursor_position + i)
                        .unwrap_or(self.input_buffer.len());
                    self.cursor_position = line_end;
                }
                KeyCode::Enter => {
                    // Fallback — if Enter didn't match is_submit (e.g., empty input)
                    // do nothing
                }
                _ => {}
            }
        }

        // Update slash autocomplete after any keystroke that modifies input
        self.update_slash_suggestions();

        // Update emoji picker after any keystroke that modifies input
        if !self.slash_suggestions_active {
            self.update_emoji_picker();
        }

        Ok(())
    }

    /// Handle keys in sessions mode
    pub(crate) async fn handle_sessions_key(
        &mut self,
        event: crossterm::event::KeyEvent,
    ) -> Result<()> {
        use super::events::keys;
        use crossterm::event::KeyCode;

        // Rename mode: typing the new name
        if self.session_renaming {
            match event.code {
                KeyCode::Enter => {
                    // Save the new name
                    if let Some(session) = self.sessions.get(self.selected_session_index) {
                        let new_title = if self.session_rename_buffer.trim().is_empty() {
                            None
                        } else {
                            Some(self.session_rename_buffer.trim().to_string())
                        };
                        let session_id = session.id;
                        self.session_service
                            .update_session_title(session_id, new_title)
                            .await?;
                        // Update current session if it's the one being renamed
                        if let Some(ref mut current) = self.current_session
                            && current.id == session_id
                        {
                            current.title = if self.session_rename_buffer.trim().is_empty() {
                                None
                            } else {
                                Some(self.session_rename_buffer.trim().to_string())
                            };
                        }
                        self.load_sessions().await?;
                    }
                    self.session_renaming = false;
                    self.session_rename_buffer.clear();
                }
                KeyCode::Esc => {
                    self.session_renaming = false;
                    self.session_rename_buffer.clear();
                }
                KeyCode::Backspace => {
                    self.session_rename_buffer.pop();
                }
                KeyCode::Char(c) => {
                    self.session_rename_buffer.push(c);
                }
                _ => {}
            }
            return Ok(());
        }

        // Search mode: typing to filter sessions
        if self.session_search_active {
            match event.code {
                KeyCode::Esc => {
                    self.session_search_active = false;
                    self.session_search.clear();
                    self.selected_session_index = 0;
                }
                KeyCode::Backspace => {
                    if self.session_search.is_empty() {
                        self.session_search_active = false;
                    } else {
                        self.session_search.pop();
                        self.selected_session_index = 0;
                    }
                }
                KeyCode::Enter => {
                    let visible = self.visible_session_indices();
                    if let Some(&actual_idx) = visible.get(self.selected_session_index)
                        && let Some(session) = self.sessions.get(actual_idx)
                    {
                        let session_id = session.id;
                        if let Some(project_id) = self.assigning_to_project {
                            let title = session
                                .title
                                .clone()
                                .unwrap_or_else(|| "Untitled".to_string());
                            match self
                                .project_service
                                .assign_session(session_id, project_id)
                                .await
                            {
                                Ok(()) => {
                                    self.notification = Some(format!("Assigned: {title}"));
                                    if let Some(s) =
                                        self.sessions.iter_mut().find(|s| s.id == session_id)
                                    {
                                        s.project_id = Some(project_id);
                                    }
                                }
                                Err(e) => {
                                    self.notification = Some(format!("Failed to assign: {e}"));
                                }
                            }
                            self.notification_shown_at = Some(std::time::Instant::now());
                        } else {
                            self.load_session(session_id).await?;
                            self.session_search_active = false;
                            self.session_search.clear();
                            self.switch_mode(AppMode::Chat).await?;
                        }
                    }
                }
                KeyCode::Char(c) if !c.is_control() => {
                    self.session_search.push(c);
                    self.selected_session_index = 0;
                }
                KeyCode::Up => {
                    self.selected_session_index = self.selected_session_index.saturating_sub(1);
                }
                KeyCode::Down => {
                    let count = self.visible_session_indices().len();
                    self.selected_session_index =
                        (self.selected_session_index + 1).min(count.saturating_sub(1));
                }
                _ => {}
            }
            return Ok(());
        }

        // Normal sessions mode
        if keys::is_cancel(&event) {
            if self.assigning_to_project.is_some() {
                // Exit assign mode, go back to projects
                self.assigning_to_project = None;
                // Refresh projects and switch back
                if let Ok(projects) = self.project_service.list_projects().await {
                    self.projects = projects;
                }
                self.switch_mode(AppMode::Projects).await?;
            } else {
                self.switch_mode(AppMode::Chat).await?;
            }
        } else if keys::is_up(&event) {
            self.selected_session_index = self.selected_session_index.saturating_sub(1);
        } else if keys::is_down(&event) {
            self.selected_session_index =
                (self.selected_session_index + 1).min(self.sessions.len().saturating_sub(1));
        } else if keys::is_enter(&event) {
            if let Some(session) = self.sessions.get(self.selected_session_index) {
                let session_id = session.id;

                if let Some(project_id) = self.assigning_to_project {
                    // Assign mode: assign this session to the project, stay in list
                    let title = session
                        .title
                        .clone()
                        .unwrap_or_else(|| "Untitled".to_string());
                    match self
                        .project_service
                        .assign_session(session_id, project_id)
                        .await
                    {
                        Ok(()) => {
                            self.notification = Some(format!("Assigned: {title}"));
                            // Reflect the assignment in the in-memory list right
                            // away so the project badge appears in real time,
                            // without leaving and re-entering the sessions list.
                            if let Some(s) = self.sessions.iter_mut().find(|s| s.id == session_id) {
                                s.project_id = Some(project_id);
                            }
                        }
                        Err(e) => {
                            self.notification = Some(format!("Failed to assign: {e}"));
                        }
                    }
                    self.notification_shown_at = Some(std::time::Instant::now());
                    // Stay in sessions list for more picks
                } else {
                    // Normal mode: load session and go to chat
                    // If this session is already in another pane, switch focus there
                    let existing_pane = self.pane_manager.panes.iter().find(|p| {
                        p.session_id == Some(session_id) && p.id != self.pane_manager.focused
                    });
                    if let Some(pane) = existing_pane {
                        let target_id = pane.id;
                        self.pane_manager.focused = target_id;
                    }

                    self.load_session(session_id).await?;
                    self.switch_mode(AppMode::Chat).await?;
                }
            }
        } else if event.code == KeyCode::Char('r') || event.code == KeyCode::Char('R') {
            // Start renaming the selected session
            if let Some(session) = self.sessions.get(self.selected_session_index) {
                self.session_renaming = true;
                self.session_rename_buffer = session.title.clone().unwrap_or_default();
            }
        } else if event.code == KeyCode::Char('n') || event.code == KeyCode::Char('N') {
            // Create a new session and switch to it
            self.create_new_session().await?;
            self.switch_mode(AppMode::Chat).await?;
        } else if event.code == KeyCode::Char('|') {
            // Split horizontal (left | right)
            // Ensure current session is pinned to the original pane before splitting
            if let Some(ref session) = self.current_session {
                let sid = session.id;
                if let Some(pane) = self.pane_manager.focused_pane_mut() {
                    pane.session_id = Some(sid);
                }
            }
            self.pane_manager
                .split(crate::tui::pane::SplitDirection::Horizontal);
            self.pane_manager.save_layout();
            // Stay on sessions screen — user picks which session goes in the new pane.
            // When they press Enter, load_session assigns it to the focused (new) pane.
        } else if event.code == KeyCode::Char('_') {
            // Split vertical (top / bottom)
            if let Some(ref session) = self.current_session {
                let sid = session.id;
                if let Some(pane) = self.pane_manager.focused_pane_mut() {
                    pane.session_id = Some(sid);
                }
            }
            self.pane_manager
                .split(crate::tui::pane::SplitDirection::Vertical);
            self.pane_manager.save_layout();
            // Stay on sessions screen — user picks which session goes in the new pane.
        } else if event.code == KeyCode::Char('d') || event.code == KeyCode::Char('D') {
            // Delete the selected session
            if let Some(session) = self.sessions.get(self.selected_session_index) {
                let session_id = session.id;
                let is_current = self
                    .current_session
                    .as_ref()
                    .map(|s| s.id == session_id)
                    .unwrap_or(false);
                self.session_service.delete_session(session_id).await?;
                // Clean up all cached state for this session
                self.pane_message_cache.remove(&session_id);
                // Also drop any background-session sidecar entry —
                // without this a session that was processing in
                // background at delete time would leak its
                // pending_messages and tps_tracker forever. See
                // background_session.rs doc for the routing model.
                self.background_sessions.remove(&session_id);
                if let Ok(mut q) = self.queued_messages.lock() {
                    q.remove(&session_id);
                }
                self.session_cancel_tokens.remove(&session_id);
                self.processing_sessions.remove(&session_id);
                if is_current {
                    self.current_session = None;
                    self.messages.clear();
                    self.render_cache.clear();
                    *self.shared_session_id.lock().await = None;
                }
                self.load_sessions().await?;
                // Adjust index if it's now out of bounds
                if self.selected_session_index >= self.sessions.len() {
                    self.selected_session_index = self.sessions.len().saturating_sub(1);
                }
            }
        } else if event.code == KeyCode::Char('f') || event.code == KeyCode::Char('F') {
            // Open session files browser
            if let Some(session) = self.sessions.get(self.selected_session_index) {
                let session_id = session.id;
                match self.file_service.list_files_for_session(session_id).await {
                    Ok(files) => {
                        self.session_files = files;
                        self.selected_file_index = 0;
                        self.switch_mode(AppMode::SessionFiles).await?;
                    }
                    Err(e) => {
                        self.notification = Some(format!("Failed to load files: {e}"));
                        self.notification_shown_at = Some(std::time::Instant::now());
                    }
                }
            }
        } else if event.code == KeyCode::Char('p') || event.code == KeyCode::Char('P') {
            // Open projects browser
            match self.project_service.list_projects().await {
                Ok(projects) => {
                    self.project_name_cache =
                        projects.iter().map(|p| (p.id, p.name.clone())).collect();
                    self.projects = projects;
                    self.selected_project_index = 0;
                    self.project_detail_view = false;
                    self.project_sessions.clear();
                    self.selected_project_session_index = 0;
                    self.project_name_input.clear();
                    self.project_name_input_active = false;
                    self.switch_mode(AppMode::Projects).await?;
                }
                Err(e) => {
                    self.notification = Some(format!("Failed to load projects: {e}"));
                    self.notification_shown_at = Some(std::time::Instant::now());
                }
            }
        } else if event.code == KeyCode::Char('/') {
            // Activate session search
            self.session_search_active = true;
            self.session_search.clear();
            self.selected_session_index = 0;
        }

        Ok(())
    }

    /// Handle keys in session files mode
    pub(crate) async fn handle_session_files_key(
        &mut self,
        event: crossterm::event::KeyEvent,
    ) -> Result<()> {
        use super::events::keys;
        use crossterm::event::KeyCode;

        if keys::is_cancel(&event) {
            // Back to sessions
            self.switch_mode(AppMode::Sessions).await?;
        } else if keys::is_up(&event) {
            self.selected_file_index = self.selected_file_index.saturating_sub(1);
        } else if keys::is_down(&event) {
            self.selected_file_index =
                (self.selected_file_index + 1).min(self.session_files.len().saturating_sub(1));
        } else if keys::is_enter(&event) {
            // Open file with default app
            if let Some(file) = self.session_files.get(self.selected_file_index) {
                let path = file.path.clone();
                // Detached (#2172): waiting on the viewer to close froze the
                // whole event loop, so Enter on a tracked file looked like a
                // hang until the user quit Preview or Finder. The exit status
                // now arrives as an event instead of being waited for here.
                let label = path.display().to_string();
                let tx = self.event_sender();
                let result = crate::tui::launcher::open_detached(&path, move |detail| {
                    let _ = tx.send(crate::tui::events::TuiEvent::LauncherFailed {
                        target: label,
                        detail,
                    });
                });
                match result {
                    Ok(()) => {
                        self.notification = Some(format!("Opened {}", path.display()));
                    }
                    Err(_) => {
                        // No launcher on this box: copy the path so the
                        // keystroke is not a dead end, and say which of the
                        // two actually happened. The old comment promised a
                        // clipboard fallback that never ran (#2172).
                        let shown = path.display().to_string();
                        let copied = Self::copy_to_clipboard(&shown);
                        self.notification = Some(if copied {
                            format!("Copied {shown} (no launcher available)")
                        } else {
                            format!("Could not open {shown}")
                        });
                    }
                }
                self.notification_shown_at = Some(std::time::Instant::now());
            }
        } else if event.code == KeyCode::Char('d') || event.code == KeyCode::Char('D') {
            // Delete tracking row (non-destructive, DB only)
            if let Some(file) = self.session_files.get(self.selected_file_index) {
                let file_id = file.id;
                let path_display = file.path.display().to_string();
                match self.file_service.delete_file(file_id).await {
                    Ok(()) => {
                        self.session_files.remove(self.selected_file_index);
                        if self.selected_file_index >= self.session_files.len() {
                            self.selected_file_index = self.session_files.len().saturating_sub(1);
                        }
                        self.notification = Some(format!(
                            "Removed tracking: {path_display} (file on disk untouched)"
                        ));
                    }
                    Err(e) => {
                        self.notification = Some(format!("Failed to remove tracking: {e}"));
                    }
                }
                self.notification_shown_at = Some(std::time::Instant::now());
            }
        } else if event.code == KeyCode::Char('o') || event.code == KeyCode::Char('O') {
            // Open containing folder
            if let Some(file) = self.session_files.get(self.selected_file_index)
                && let Some(parent) = file.path.parent()
            {
                let label = parent.display().to_string();
                let tx = self.event_sender();
                let result = crate::tui::launcher::open_detached(parent, move |detail| {
                    let _ = tx.send(crate::tui::events::TuiEvent::LauncherFailed {
                        target: label,
                        detail,
                    });
                });
                match result {
                    Ok(()) => {
                        self.notification = Some(format!("Opened {}", parent.display()));
                    }
                    Err(_) => {
                        // Same fallback as the file itself: copy the folder
                        // path rather than claiming it opened (#2172).
                        let shown = parent.display().to_string();
                        let copied = Self::copy_to_clipboard(&shown);
                        self.notification = Some(if copied {
                            format!("Copied {shown} (no launcher available)")
                        } else {
                            format!("Could not open {shown}")
                        });
                    }
                }
                self.notification_shown_at = Some(std::time::Instant::now());
            }
        }

        Ok(())
    }

    /// Handle keys in projects mode
    pub(crate) async fn handle_projects_key(
        &mut self,
        event: crossterm::event::KeyEvent,
    ) -> Result<()> {
        use super::events::keys;
        use crossterm::event::KeyCode;

        // If we're in name input mode, handle text input
        if self.project_name_input_active {
            match event.code {
                KeyCode::Esc => {
                    self.project_name_input_active = false;
                    self.project_name_input.clear();
                }
                KeyCode::Enter => {
                    let name = self.project_name_input.trim().to_string();
                    if name.is_empty() {
                        self.notification = Some("Project name cannot be empty".to_string());
                        self.notification_shown_at = Some(std::time::Instant::now());
                    } else {
                        match self
                            .project_service
                            .create_project(name.clone(), None)
                            .await
                        {
                            Ok(_project) => {
                                self.notification = Some(format!("Created project: {}", name));
                                // Refresh project list
                                if let Ok(projects) = self.project_service.list_projects().await {
                                    self.projects = projects;
                                    // Select the newly created project
                                    if let Some(pos) =
                                        self.projects.iter().position(|p| p.name == name)
                                    {
                                        self.selected_project_index = pos;
                                    }
                                }
                            }
                            Err(e) => {
                                self.notification = Some(format!("Failed to create project: {e}"));
                            }
                        }
                        self.notification_shown_at = Some(std::time::Instant::now());
                    }
                    self.project_name_input_active = false;
                    self.project_name_input.clear();
                }
                KeyCode::Char(c) => {
                    self.project_name_input.push(c);
                }
                KeyCode::Backspace => {
                    self.project_name_input.pop();
                }
                _ => {}
            }
            return Ok(());
        }

        // Normal navigation mode
        if keys::is_cancel(&event) {
            if self.project_detail_view {
                // Back to project list
                self.project_detail_view = false;
                self.project_sessions.clear();
                self.selected_project_session_index = 0;
            } else {
                // Back to sessions
                self.switch_mode(AppMode::Sessions).await?;
            }
        } else if self.project_detail_view {
            // Detail view: navigate sessions within a project
            if keys::is_up(&event) {
                self.selected_project_session_index =
                    self.selected_project_session_index.saturating_sub(1);
            } else if keys::is_down(&event) {
                self.selected_project_session_index = (self.selected_project_session_index + 1)
                    .min(self.project_sessions.len().saturating_sub(1));
            } else if event.code == KeyCode::Char('u') || event.code == KeyCode::Char('U') {
                // Unassign session from project
                if let Some(session) = self
                    .project_sessions
                    .get(self.selected_project_session_index)
                {
                    let session_id = session.id;
                    let title = session
                        .title
                        .clone()
                        .unwrap_or_else(|| "Untitled".to_string());
                    match self.project_service.unassign_session(session_id).await {
                        Ok(()) => {
                            self.project_sessions
                                .remove(self.selected_project_session_index);
                            if self.selected_project_session_index >= self.project_sessions.len() {
                                self.selected_project_session_index =
                                    self.project_sessions.len().saturating_sub(1);
                            }
                            // Refresh project list in background to update counts
                            if let Ok(projects) = self.project_service.list_projects().await {
                                self.projects = projects;
                            }
                            self.notification = Some(format!("Unassigned: {title}"));
                        }
                        Err(e) => {
                            self.notification = Some(format!("Failed to unassign: {e}"));
                        }
                    }
                    self.notification_shown_at = Some(std::time::Instant::now());
                }
            } else if keys::is_enter(&event) {
                // Select this session and go to chat
                if let Some(session) = self
                    .project_sessions
                    .get(self.selected_project_session_index)
                {
                    let session_id = session.id;
                    self.switch_mode(AppMode::Chat).await?;
                    self.load_session(session_id).await?;
                }
            }
        } else {
            // List view: navigate projects
            if keys::is_up(&event) {
                self.selected_project_index = self.selected_project_index.saturating_sub(1);
            } else if keys::is_down(&event) {
                self.selected_project_index =
                    (self.selected_project_index + 1).min(self.projects.len().saturating_sub(1));
            } else if keys::is_enter(&event) {
                // Enter project detail view
                if let Some(project) = self.projects.get(self.selected_project_index) {
                    let project_id = project.id;
                    match self
                        .project_service
                        .get_sessions_for_project(project_id)
                        .await
                    {
                        Ok(sessions) => {
                            self.project_sessions = sessions;
                            self.selected_project_session_index = 0;
                            self.project_detail_view = true;
                        }
                        Err(e) => {
                            self.notification = Some(format!("Failed to load sessions: {e}"));
                            self.notification_shown_at = Some(std::time::Instant::now());
                        }
                    }
                }
            } else if event.code == KeyCode::Char('n') || event.code == KeyCode::Char('N') {
                // Create new project
                self.project_name_input_active = true;
                self.project_name_input.clear();
            } else if event.code == KeyCode::Char('d') || event.code == KeyCode::Char('D') {
                // Delete project
                if let Some(project) = self.projects.get(self.selected_project_index) {
                    let project_id = project.id;
                    let name = project.name.clone();
                    match self.project_service.delete_project(project_id).await {
                        Ok(()) => {
                            self.projects.remove(self.selected_project_index);
                            if self.selected_project_index >= self.projects.len() {
                                self.selected_project_index = self.projects.len().saturating_sub(1);
                            }
                            self.notification = Some(format!("Deleted project: {name}"));
                        }
                        Err(e) => {
                            self.notification = Some(format!("Failed to delete project: {e}"));
                        }
                    }
                    self.notification_shown_at = Some(std::time::Instant::now());
                }
            } else if event.code == KeyCode::Char('a') || event.code == KeyCode::Char('A') {
                // Enter assign mode: switch to sessions list so user picks a session
                if let Some(project) = self.projects.get(self.selected_project_index) {
                    self.assigning_to_project = Some(project.id);
                    // Make sure cache has this project name
                    self.project_name_cache
                        .entry(project.id)
                        .or_insert_with(|| project.name.clone());
                    // Reload sessions so we see the full list
                    self.load_sessions().await?;
                    self.switch_mode(AppMode::Sessions).await?;
                }
            }
        }

        Ok(())
    }
}

/// Clipboard write backends for this platform, in the order they are tried.
///
/// Pure and testable so the ordering is pinned: on Linux the Wayland tool
/// goes first (#2171). The read path already prefers `wl-paste`
/// (`read_clipboard`), so a Wayland box could paste what it had copied with
/// a terminal shortcut while `copy_to_clipboard` had written to X11 and
/// silently lost it. `wl-copy` holds the selection until it is replaced, and
/// it takes no arguments.
#[cfg(not(windows))]
pub(crate) fn clipboard_backends() -> &'static [(&'static str, &'static [&'static str])] {
    #[cfg(target_os = "macos")]
    {
        &[("pbcopy", &[])]
    }
    #[cfg(not(target_os = "macos"))]
    {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    }
}

/// Try every backend in order until one actually owns the text.
///
/// A backend counts as successful only when it spawned, accepted the whole
/// payload on stdin and exited 0. Anything less falls through to the next
/// one, so a missing `wl-copy`, a dead X display or a half-written pipe can
/// no longer make `copy_to_clipboard` report success it did not have
/// (#2171).
#[cfg(not(windows))]
pub(crate) fn copy_with_backends(backends: &[(&str, &[&str])], text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};

    for (program, args) in backends {
        let Ok(mut child) = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        // A write failure means this backend never got the payload, so it
        // cannot be trusted even if it exits 0.
        let wrote = match child.stdin.as_mut() {
            Some(stdin) => stdin.write_all(text.as_bytes()).is_ok(),
            None => false,
        };
        // Close the pipe before waiting: the tools read to EOF, and holding
        // stdin open makes `wait` hang on the last backend.
        drop(child.stdin.take());
        if wrote && child.wait().is_ok_and(|s| s.success()) {
            return true;
        }
    }
    false
}

#[cfg(all(test, not(windows)))]
mod clipboard_backend_tests {
    use super::*;

    /// Linux must try Wayland before X11, matching the read path.
    #[test]
    fn wayland_comes_first_on_linux() {
        let backends = clipboard_backends();
        #[cfg(target_os = "macos")]
        assert_eq!(backends[0].0, "pbcopy");
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(backends[0].0, "wl-copy");
            assert_eq!(backends[1].0, "xclip");
            assert_eq!(backends[2].0, "xsel");
        }
    }

    /// A backend that spawns and exits non-zero must not end the search.
    /// Both fakes are `sh`, so no test depends on a clipboard tool existing.
    #[test]
    fn a_failing_backend_falls_through_to_the_next() {
        let backends: &[(&str, &[&str])] =
            &[("sh", &["-c", "exit 1"]), ("sh", &["-c", "cat >/dev/null"])];
        assert!(
            copy_with_backends(backends, "payload"),
            "the second backend must be tried after the first fails"
        );
    }

    /// Every backend failing means false, not a silent success.
    #[test]
    fn all_backends_failing_reports_failure() {
        let backends: &[(&str, &[&str])] = &[
            ("sh", &["-c", "exit 1"]),
            ("opencrabs-no-such-clipboard-xyz", &[]),
        ];
        assert!(!copy_with_backends(backends, "payload"));
    }
}
