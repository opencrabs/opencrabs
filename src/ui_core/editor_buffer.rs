//! The text behind the built-in editor the bang operator opens (#3163).
//!
//! One text, one caret, one selection. Offsets are character counts and never
//! bytes, so a caret can never land inside a UTF-8 codepoint. The buffer
//! knows nothing about the window: it draws nowhere, it never touches a
//! clipboard, and it never touches the disk. The window owns those, so the
//! same buffer can be tested without a screen.
//!
//! The single-line field in [`super::text_field`] cannot be reused as it
//! stands: a file has many lines, and "home" means the start of THIS line,
//! not the start of the text. The word walks and the chord table stay shared,
//! so a key that works in the composer works here too.

use super::text_field::char_to_byte;

/// A multi-line text buffer with a caret and a selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorBuffer {
    text: String,
    /// The caret, as a character offset into `text`.
    caret: usize,
    /// Where a shifted selection began. `None` means there is no selection.
    anchor: Option<usize>,
}

impl EditorBuffer {
    /// An empty buffer, which is what a bare `!vi` opens.
    pub fn new() -> Self {
        Self::default()
    }

    /// A buffer holding `text` with the caret on its first character: a file
    /// opens at its top, the way every editor in the list opens it.
    pub fn from_text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            caret: 0,
            anchor: None,
        }
    }

    /// The text in the buffer.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The caret, as a character offset.
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// How many characters the buffer holds.
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Whether the buffer holds nothing.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replace the text and put the caret back on the first character.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.caret = 0;
        self.anchor = None;
    }

    /// How many lines the buffer holds. An empty buffer still has one line.
    pub fn line_count(&self) -> usize {
        self.text.chars().filter(|ch| *ch == '\n').count() + 1
    }

    /// The caret's line and column, both counted from one.
    pub fn line_col(&self) -> (usize, usize) {
        let mut line = 1;
        let mut start = 0;
        for (index, ch) in self.text.chars().enumerate() {
            if ch == '\n' {
                if self.caret <= index {
                    return (line, self.caret - start + 1);
                }
                line += 1;
                start = index + 1;
            }
        }
        (line, self.caret - start + 1)
    }

    /// The first character offset of a line and how many characters it holds,
    /// without the newline that ends it. Lines are counted from one.
    fn line_bounds(&self, line: usize) -> Option<(usize, usize)> {
        if line == 0 {
            return None;
        }
        let mut current = 1;
        let mut start = 0;
        for (index, ch) in self.text.chars().enumerate() {
            if ch == '\n' {
                if current == line {
                    return Some((start, index - start));
                }
                current += 1;
                start = index + 1;
            }
        }
        if current == line {
            return Some((start, self.text.chars().count() - start));
        }
        None
    }

    /// The text of a line, counted from one, without its newline.
    pub fn line_text(&self, line: usize) -> Option<&str> {
        let (start, len) = self.line_bounds(line)?;
        let byte_start = char_to_byte(&self.text, start);
        let byte_end = char_to_byte(&self.text, start + len);
        self.text.get(byte_start..byte_end)
    }

    /// Put the caret on a line, counted from one, at its first character. A
    /// line past the end of the buffer lands on the last line, the way `+99`
    /// lands at the end of a short file.
    pub fn goto_line(&mut self, line: usize) {
        let last = self.line_count();
        let target = line.clamp(1, last);
        if let Some((start, _)) = self.line_bounds(target) {
            self.caret = start;
            self.anchor = None;
        }
    }

    /// The line's own character-offset span, counted from one: where the
    /// line starts in the buffer and how far it reaches, newline excluded.
    /// The raw view's per-row selection paints work in these offsets, so a
    /// global selection range can be clipped against a row without a second
    /// scan (#3163).
    pub fn line_span(&self, line: usize) -> Option<std::ops::Range<usize>> {
        let (start, len) = self.line_bounds(line)?;
        Some(start..start + len)
    }

    /// Put the caret on a line and column, both counted from one, keeping
    /// the selection when the caller asks and dropping it otherwise. The
    /// column clamps to the line's own length, so a click past the end of a
    /// short line lands at that line's end instead of drifting into the
    /// next one (#3163).
    pub fn goto_line_col(&mut self, line: usize, column: usize, select: bool) {
        let last = self.line_count();
        let target = line.clamp(1, last);
        let Some((start, len)) = self.line_bounds(target) else {
            return;
        };
        let offset = start + column.saturating_sub(1).min(len);
        self.move_to(offset, select);
    }

    /// The selected character offsets, ascending. No selection is `None`.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.caret).then_some((anchor.min(self.caret), anchor.max(self.caret)))
    }

    /// The selected text, for copy and cut.
    pub fn selected_text(&self) -> Option<&str> {
        let (start, end) = self.selection()?;
        let byte_start = char_to_byte(&self.text, start);
        let byte_end = char_to_byte(&self.text, end);
        self.text.get(byte_start..byte_end)
    }

    /// Put the caret on a character offset, keeping the selection when the
    /// caller asks for it and dropping it when they do not.
    fn move_to(&mut self, offset: usize, select: bool) {
        let len = self.text.chars().count();
        let offset = offset.min(len);
        if select {
            let anchor = *self.anchor.get_or_insert(self.caret);
            self.caret = offset;
            if self.caret == anchor {
                self.anchor = None;
            }
        } else {
            self.caret = offset;
            self.anchor = None;
        }
    }

    /// Replace a character range and leave the caret after the replacement.
    /// Every real edit goes through here, so the caret can never end up
    /// outside the text.
    fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        let len = self.text.chars().count();
        let start = start.min(len);
        let end = end.min(len).max(start);
        let byte_start = char_to_byte(&self.text, start);
        let byte_end = char_to_byte(&self.text, end);
        self.text.replace_range(byte_start..byte_end, replacement);
        self.caret = start + replacement.chars().count();
        self.anchor = None;
    }

    /// Insert text at the caret, replacing any selection.
    pub fn insert(&mut self, text: &str) -> bool {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        if text.is_empty() && start == end {
            return false;
        }
        self.replace_range(start, end, text);
        true
    }

    /// Type one character. A control character is refused: the window turns
    /// Enter and Tab into their own keys before they reach the buffer.
    pub fn type_char(&mut self, ch: char) -> bool {
        if ch.is_control() {
            return false;
        }
        self.insert(&ch.to_string())
    }

    /// Remove the selection, or the character before the caret.
    pub fn backspace(&mut self) -> bool {
        if self.selection().is_some() {
            self.delete_selection();
            return true;
        }
        if self.caret > 0 {
            self.replace_range(self.caret - 1, self.caret, "");
            return true;
        }
        false
    }

    /// Remove the selection, or the character after the caret.
    pub fn delete_forward(&mut self) -> bool {
        if self.selection().is_some() {
            self.delete_selection();
            return true;
        }
        if self.caret < self.text.chars().count() {
            self.replace_range(self.caret, self.caret + 1, "");
            return true;
        }
        false
    }

    /// Remove the selection.
    pub fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((start, end)) => {
                self.replace_range(start, end, "");
                true
            }
            None => false,
        }
    }

    /// One character to the left. A selection collapses to its near edge.
    pub fn move_left(&mut self, select: bool) {
        if let Some((start, _)) = self.selection() {
            let target = if select {
                start.saturating_sub(1)
            } else {
                start
            };
            self.move_to(target, select);
            return;
        }
        let target = self.caret.saturating_sub(1);
        self.move_to(target, select);
    }

    /// One character to the right. A selection collapses to its far edge.
    pub fn move_right(&mut self, select: bool) {
        let len = self.text.chars().count();
        if let Some((_, end)) = self.selection() {
            let target = if select { (end + 1).min(len) } else { end };
            self.move_to(target, select);
            return;
        }
        let target = (self.caret + 1).min(len);
        self.move_to(target, select);
    }

    /// One line up, keeping the column. The column is clamped to the shorter
    /// line, the way every editor clamps it.
    pub fn move_up(&mut self, select: bool) {
        let (line, column) = self.line_col();
        if line <= 1 {
            return;
        }
        let Some((start, len)) = self.line_bounds(line - 1) else {
            return;
        };
        let offset = start + (column - 1).min(len);
        self.move_to(offset, select);
    }

    /// One line down, keeping the column.
    pub fn move_down(&mut self, select: bool) {
        let (line, column) = self.line_col();
        let Some((start, len)) = self.line_bounds(line + 1) else {
            return;
        };
        let offset = start + (column - 1).min(len);
        self.move_to(offset, select);
    }

    /// The first character of the caret's line.
    pub fn line_start(&mut self, select: bool) {
        let (line, _) = self.line_col();
        let start = self.line_bounds(line).map(|(start, _)| start).unwrap_or(0);
        self.move_to(start, select);
    }

    /// The last character of the caret's line.
    pub fn line_end(&mut self, select: bool) {
        let (line, _) = self.line_col();
        let end = self
            .line_bounds(line)
            .map(|(start, len)| start + len)
            .unwrap_or(0);
        self.move_to(end, select);
    }

    /// Select the whole buffer.
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.chars().count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_buffer_has_one_line_and_a_caret_at_the_top() {
        let buffer = EditorBuffer::new();
        assert_eq!(buffer.line_count(), 1);
        assert_eq!(buffer.line_col(), (1, 1));
        assert!(buffer.is_empty());
    }

    #[test]
    fn typing_moves_the_caret_and_the_line_follows() {
        let mut buffer = EditorBuffer::new();
        buffer.insert("hello");
        assert_eq!(buffer.text(), "hello");
        assert_eq!(buffer.caret(), 5);
        assert_eq!(buffer.line_col(), (1, 6));
    }

    #[test]
    fn a_newline_moves_the_caret_to_the_next_line() {
        let mut buffer = EditorBuffer::new();
        buffer.insert("one\ntwo");
        assert_eq!(buffer.line_count(), 2);
        assert_eq!(buffer.line_col(), (2, 4));
        assert_eq!(buffer.line_text(1), Some("one"));
        assert_eq!(buffer.line_text(2), Some("two"));
    }

    #[test]
    fn up_and_down_keep_the_column() {
        let mut buffer = EditorBuffer::from_text("abc\ndefgh\nij");
        buffer.goto_line(2);
        buffer.move_to(7, false);
        assert_eq!(buffer.line_col(), (2, 4));
        buffer.move_up(false);
        assert_eq!(buffer.line_col(), (1, 4));
        buffer.move_down(false);
        assert_eq!(buffer.line_col(), (2, 4));
    }

    #[test]
    fn up_clamps_to_the_shorter_line() {
        let mut buffer = EditorBuffer::from_text("abc\nd\n");
        buffer.goto_line(1);
        buffer.move_to(3, false);
        buffer.move_down(false);
        assert_eq!(buffer.line_col(), (2, 2));
    }

    #[test]
    fn home_and_end_reach_the_edges_of_the_line_not_the_text() {
        let mut buffer = EditorBuffer::from_text("abc\ndef");
        buffer.goto_line(2);
        buffer.line_end(false);
        assert_eq!(buffer.line_col(), (2, 4));
        buffer.line_start(false);
        assert_eq!(buffer.line_col(), (2, 1));
    }

    #[test]
    fn a_selection_keeps_its_text_and_can_be_cut() {
        let mut buffer = EditorBuffer::from_text("hello world");
        buffer.move_to(0, false);
        buffer.select_all();
        assert_eq!(buffer.selected_text(), Some("hello world"));
        assert!(buffer.delete_selection());
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn a_shift_selection_grows_from_the_anchor() {
        let mut buffer = EditorBuffer::from_text("abc");
        buffer.move_to(0, false);
        buffer.move_right(true);
        buffer.move_right(true);
        assert_eq!(buffer.selected_text(), Some("ab"));
    }

    #[test]
    fn backspace_removes_the_character_before_the_caret() {
        let mut buffer = EditorBuffer::from_text("ab");
        buffer.move_to(2, false);
        assert!(buffer.backspace());
        assert_eq!(buffer.text(), "a");
        assert_eq!(buffer.caret(), 1);
    }

    #[test]
    fn goto_line_lands_on_the_first_character_of_the_line() {
        let mut buffer = EditorBuffer::from_text("one\ntwo\nthree");
        buffer.goto_line(3);
        assert_eq!(buffer.line_col(), (3, 1));
    }

    #[test]
    fn goto_line_past_the_end_lands_on_the_last_line() {
        let mut buffer = EditorBuffer::from_text("one\ntwo");
        buffer.goto_line(99);
        assert_eq!(buffer.line_col(), (2, 1));
    }

    #[test]
    fn a_caret_never_lands_inside_a_multibyte_character() {
        let mut buffer = EditorBuffer::from_text("héllo");
        buffer.move_to(0, false);
        buffer.move_right(false);
        buffer.insert("x");
        assert_eq!(buffer.text(), "hxéllo");
    }

    #[test]
    fn line_span_gives_each_lines_own_offsets() {
        let buffer = EditorBuffer::from_text("one\ntwo\nthree");
        // Counted from one, newline excluded: line two starts at char
        // offset four and holds three characters, the newline itself
        // never belongs to a line (#3163).
        assert_eq!(buffer.line_span(2), Some(4..7));
        assert_eq!(buffer.line_span(1), Some(0..3));
        assert_eq!(buffer.line_span(9), None);
    }

    #[test]
    fn goto_line_col_lands_on_the_asked_character() {
        let mut buffer = EditorBuffer::from_text("one\ntwo");
        buffer.goto_line_col(2, 3, false);
        assert_eq!(buffer.line_col(), (2, 3));
    }

    #[test]
    fn goto_line_col_clamps_the_column_to_the_line() {
        let mut buffer = EditorBuffer::from_text("one\ntwo");
        // A click past a short line's end lands at the line's end, not
        // on the next line's first character (#3163).
        buffer.goto_line_col(1, 99, false);
        assert_eq!(buffer.line_col(), (1, 4));
    }

    #[test]
    fn goto_line_col_with_select_extends_a_selection() {
        let mut buffer = EditorBuffer::from_text("one\ntwo");
        buffer.goto_line_col(1, 2, false);
        buffer.goto_line_col(2, 2, true);
        assert_eq!(buffer.selected_text(), Some("ne\nt"));
    }
}
