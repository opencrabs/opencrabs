//! One text field, one editing engine.
//!
//! The Seeker grew a real caret, a selection and word walks (#2534), and the
//! composer has had them all along. The name prompts did not: renaming a
//! conversation or a topic appended characters to a `String`, so the caret was
//! welded to the end, `cmd-a` selected nothing, and `alt-backspace` was just a
//! backspace (#2892). The math lives here once. The palette holds a
//! [`TextField`], the window's topic card holds one, and the terminal's two
//! prompts hold one, so a chord cannot work in one place and silently do
//! nothing in another.
//!
//! The field never touches a clipboard and never knows about rows: a frontend
//! owns those, and [`apply`] hands back a [`ClipboardRequest`] for the one
//! part it cannot do itself.

use unicode_segmentation::UnicodeSegmentation;

/// Byte offset where the word before `offset` begins. The walk skips
/// separators and then the word, matching the terminal and GUI composers.
/// Grapheme boundaries keep the walk from splitting a multibyte character.
pub fn previous_word_start(content: &str, offset: usize) -> usize {
    let clamped = offset.min(content.len());
    let graphemes: Vec<(usize, &str)> = content.grapheme_indices(true).collect();
    let mut index = graphemes
        .iter()
        .position(|(start, _)| *start >= clamped)
        .unwrap_or(graphemes.len());
    while index > 0 && !is_word_grapheme(graphemes[index - 1].1) {
        index -= 1;
    }
    while index > 0 && is_word_grapheme(graphemes[index - 1].1) {
        index -= 1;
    }
    graphemes.get(index).map(|(start, _)| *start).unwrap_or(0)
}

/// Byte offset the word after `offset` ends on, past the word and the
/// separators that follow it, matching the terminal composer.
pub fn next_word_end(content: &str, offset: usize) -> usize {
    let clamped = offset.min(content.len());
    let graphemes: Vec<(usize, &str)> = content.grapheme_indices(true).collect();
    let mut index = graphemes
        .iter()
        .position(|(start, _)| *start >= clamped)
        .unwrap_or(graphemes.len());
    while index < graphemes.len() && is_word_grapheme(graphemes[index].1) {
        index += 1;
    }
    while index < graphemes.len() && !is_word_grapheme(graphemes[index].1) {
        index += 1;
    }
    graphemes
        .get(index)
        .map(|(start, _)| *start)
        .unwrap_or(content.len())
}

/// Letters and digits are word content; whitespace and punctuation separate
/// words. A grapheme counts as a word when any of its characters is a letter.
fn is_word_grapheme(grapheme: &str) -> bool {
    grapheme.chars().any(|ch| ch.is_alphanumeric())
}

/// Byte index of a character offset, clamped to the end. Every caret in this
/// crate is counted in chars so it can never land inside a UTF-8 codepoint.
pub(crate) fn char_to_byte(content: &str, offset: usize) -> usize {
    content
        .char_indices()
        .nth(offset)
        .map(|(index, _)| index)
        .unwrap_or(content.len())
}

/// Character index of a byte offset produced by a word walk or a shaped-text
/// mouse hit. Public so the GUI can translate GPUI's byte index without doing
/// UTF-8 arithmetic of its own.
pub fn byte_to_char(content: &str, offset: usize) -> usize {
    content
        .char_indices()
        .take_while(|(index, _)| *index < offset)
        .count()
}

/// A single line of text with a caret and a selection, edited the same way
/// everywhere. Offsets are character counts, never bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextField {
    text: String,
    caret: usize,
    /// Where a shift or drag selection began. `None` means no selection; the
    /// live end is always `caret`.
    anchor: Option<usize>,
}

impl TextField {
    pub fn new() -> Self {
        Self::default()
    }

    /// A field holding `text` with the caret at the end, which is what a
    /// rename prompt wants: the old name is there to edit, not to select.
    pub fn from_text(text: &str) -> Self {
        Self {
            caret: text.chars().count(),
            text: text.to_string(),
            anchor: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The trimmed value, which is what a name prompt stores.
    pub fn trimmed(&self) -> &str {
        self.text.trim()
    }

    /// Replace the whole text and put the caret at the end, as seeding a
    /// prompt with an existing name does.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.caret = self.text.chars().count();
        self.anchor = None;
    }

    /// Move by whole characters and clamp at both ends. With a selection, an
    /// ordinary arrow collapses to its near edge, as the composer does.
    pub fn move_caret(&mut self, delta: isize) {
        if let Some((start, end)) = self.selection() {
            if delta != 0 {
                self.caret = if delta < 0 { start } else { end };
            }
            self.collapse();
            return;
        }
        let len = self.text.chars().count();
        self.caret = if delta < 0 {
            self.caret.saturating_sub(delta.unsigned_abs())
        } else {
            self.caret.saturating_add(delta.unsigned_abs()).min(len)
        };
        self.collapse();
    }

    /// Move to an absolute character offset and collapse any selection.
    /// Home, End and mouse clicks use the same clamped landing rule.
    pub fn move_caret_to(&mut self, offset: usize) {
        self.caret = offset.min(self.text.chars().count());
        self.collapse();
    }

    /// Move by the same word boundaries as the composer. The field stores
    /// chars, while the shared word walk speaks bytes, so convert at the seam.
    pub fn move_caret_word(&mut self, delta: isize) {
        let target = self.word_target(delta);
        self.caret = byte_to_char(&self.text, target);
        self.collapse();
    }

    /// Byte offset one word walk away from the caret, in the walk's units.
    fn word_target(&self, delta: isize) -> usize {
        let byte_caret = char_to_byte(&self.text, self.caret);
        if delta < 0 {
            previous_word_start(&self.text, byte_caret)
        } else if delta > 0 {
            next_word_end(&self.text, byte_caret)
        } else {
            byte_caret
        }
    }

    /// Extend a selection to an absolute character offset. The first call
    /// anchors at the current caret; later calls keep that anchor, including
    /// when the direction reverses (#2534).
    pub fn select_to(&mut self, offset: usize) {
        let len = self.text.chars().count();
        let anchor = *self.anchor.get_or_insert(self.caret);
        self.caret = offset.min(len);
        if self.caret == anchor {
            self.anchor = None;
        }
    }

    /// Extend the selection by one composer word boundary. The original
    /// anchor survives repeated presses and direction reversals (#2534).
    pub fn select_word_to(&mut self, delta: isize) {
        let target = self.word_target(delta);
        self.select_to(byte_to_char(&self.text, target));
    }

    /// Select the whole text, ready for copy, cut, or replacement typing.
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.chars().count();
    }

    /// Drop the selection without moving the caret.
    pub fn collapse(&mut self) {
        self.anchor = None;
    }

    /// Selected character offsets in ascending order. An empty selection is
    /// no selection, so editing falls back to the caret.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.caret).then_some((anchor.min(self.caret), anchor.max(self.caret)))
    }

    /// The selected text without changing anything, for copy.
    pub fn selected_text(&self) -> Option<&str> {
        let (start, end) = self.selection()?;
        let start = char_to_byte(&self.text, start);
        let end = char_to_byte(&self.text, end);
        self.text.get(start..end)
    }

    /// The caret as a byte offset, so a frontend that lays text out itself can
    /// split at the insertion point without doing UTF-8 math (#2892).
    pub fn caret_byte(&self) -> usize {
        char_to_byte(&self.text, self.caret)
    }

    /// The selection as ascending byte offsets, when one exists. Both land on
    /// char boundaries, so slicing the text at them cannot panic.
    pub fn selection_bytes(&self) -> Option<(usize, usize)> {
        self.selection()
            .map(|(start, end)| (char_to_byte(&self.text, start), char_to_byte(&self.text, end)))
    }

    /// Text slices before the caret or selection, inside the selection, and
    /// after it. With no selection the middle slice is empty and the split is
    /// the caret, which lets a frontend paint all three without UTF-8 math.
    pub fn render_spans(&self) -> (&str, &str, &str) {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        let start = char_to_byte(&self.text, start);
        let end = char_to_byte(&self.text, end);
        (
            self.text.get(..start).unwrap_or(""),
            self.text.get(start..end).unwrap_or(""),
            self.text.get(end..).unwrap_or(""),
        )
    }

    /// Replace a character range and put the caret after the replacement.
    /// Every real edit funnels through here. Returns whether anything
    /// changed, so a caller can skip work for a no-op.
    pub(crate) fn replace_range(&mut self, start: usize, end: usize, replacement: &str) -> bool {
        let len = self.text.chars().count();
        let start = start.min(len);
        let end = end.min(len).max(start);
        let byte_start = char_to_byte(&self.text, start);
        let byte_end = char_to_byte(&self.text, end);
        self.text.replace_range(byte_start..byte_end, replacement);
        self.caret = start + replacement.chars().count();
        self.anchor = None;
        true
    }

    /// Insert text at the caret, replacing any selection. Paste uses the same
    /// path as typing, so both frontends inherit identical editing (#2534).
    pub fn insert_text(&mut self, text: &str) -> bool {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        if text.is_empty() && start == end {
            return false;
        }
        self.replace_range(start, end, text)
    }

    /// A typed character inserts at the caret rather than always appending.
    pub fn type_char(&mut self, c: char) -> bool {
        if c.is_control() {
            return false;
        }
        let mut encoded = [0; 4];
        let text = c.encode_utf8(&mut encoded);
        self.insert_text(text)
    }

    /// Remove the selection, if one exists.
    pub fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((start, end)) => self.replace_range(start, end, ""),
            None => false,
        }
    }

    /// Remove and return the selected text, for cut. Copy uses
    /// [`TextField::selected_text`] and leaves the text alone.
    pub fn take_selected_text(&mut self) -> Option<String> {
        let taken = self.selected_text().map(str::to_owned);
        if taken.is_some() {
            self.delete_selection();
        }
        taken
    }

    pub fn backspace(&mut self) -> bool {
        if self.selection().is_some() {
            self.delete_selection()
        } else if self.caret > 0 {
            self.replace_range(self.caret - 1, self.caret, "")
        } else {
            false
        }
    }

    /// Delete the character after the caret, or the whole selection.
    pub fn delete_forward(&mut self) -> bool {
        if self.selection().is_some() {
            self.delete_selection()
        } else if self.caret < self.text.chars().count() {
            self.replace_range(self.caret, self.caret + 1, "")
        } else {
            false
        }
    }

    /// Delete back to the previous composer word boundary.
    pub fn delete_word_back(&mut self) -> bool {
        if self.selection().is_some() {
            return self.delete_selection();
        }
        let start = byte_to_char(&self.text, self.word_target(-1));
        if start < self.caret {
            self.replace_range(start, self.caret, "")
        } else {
            false
        }
    }

    /// Delete forward through the next composer word boundary.
    pub fn delete_word_forward(&mut self) -> bool {
        if self.selection().is_some() {
            return self.delete_selection();
        }
        let end = byte_to_char(&self.text, self.word_target(1));
        if end > self.caret {
            self.replace_range(self.caret, end, "")
        } else {
            false
        }
    }

    /// Delete everything before the caret on this line, which is what
    /// `ctrl-u` does in the composer.
    pub fn delete_to_line_start(&mut self) -> bool {
        if let Some((start, end)) = self.selection() {
            return self.replace_range(start, end, "");
        }
        if self.caret > 0 {
            self.replace_range(0, self.caret, "")
        } else {
            false
        }
    }

    /// Empty the field and put the caret back at the start.
    pub fn clear(&mut self) -> bool {
        if self.text.is_empty() && self.caret == 0 {
            return false;
        }
        self.text.clear();
        self.caret = 0;
        self.anchor = None;
        true
    }
}

/// One editing intent, translated from a keystroke. Frontends hold the same
/// table, so the chords cannot drift between the window and the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFieldEdit {
    Move {
        delta: isize,
        word: bool,
        select: bool,
    },
    Home {
        select: bool,
    },
    End {
        select: bool,
    },
    Backspace {
        word: bool,
    },
    Delete {
        word: bool,
    },
    DeleteToLineStart,
    SelectAll,
    Copy,
    Cut,
    Paste,
}

/// The part of an edit a field cannot do by itself. The window writes to
/// GPUI's clipboard, the terminal to the platform one, so the engine names
/// what it wants and the frontend does it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardRequest {
    Write(String),
    Read,
}

/// Translate the editing keys the composer answers. Command is the primary
/// modifier on macOS; Control does the same clipboard work elsewhere. Alt or
/// Control walks words, Command+Left/Right reaches the line edges, and Shift
/// extends the selection instead of collapsing it. `ctrl-u` and `ctrl-w` are
/// the composer's line and word kills, so a name prompt has them too (#2892).
pub fn editing_chord(
    key: &str,
    control: bool,
    platform: bool,
    alt: bool,
    shift: bool,
) -> Option<TextFieldEdit> {
    let primary = control || platform;
    if primary && !alt {
        match key {
            "a" => return Some(TextFieldEdit::SelectAll),
            "c" => return Some(TextFieldEdit::Copy),
            "x" => return Some(TextFieldEdit::Cut),
            "v" => return Some(TextFieldEdit::Paste),
            _ => {}
        }
    }
    // The composer's two line kills. Command stays out of them: `cmd-u` and
    // `cmd-w` mean something else to the window manager.
    if control && !alt && !platform {
        match key {
            "u" => return Some(TextFieldEdit::DeleteToLineStart),
            "w" => return Some(TextFieldEdit::Backspace { word: true }),
            _ => {}
        }
    }
    match key {
        "left" | "right" => {
            let delta = if key == "left" { -1 } else { 1 };
            if platform && !control && !alt {
                Some(if delta < 0 {
                    TextFieldEdit::Home { select: shift }
                } else {
                    TextFieldEdit::End { select: shift }
                })
            } else {
                Some(TextFieldEdit::Move {
                    delta,
                    word: alt || control,
                    select: shift,
                })
            }
        }
        "home" => Some(TextFieldEdit::Home { select: shift }),
        "end" => Some(TextFieldEdit::End { select: shift }),
        "backspace" => Some(TextFieldEdit::Backspace {
            word: alt || control,
        }),
        "delete" => Some(TextFieldEdit::Delete {
            word: alt || control,
        }),
        _ => None,
    }
}

/// Apply one edit. Movement and deletion happen here; copy, cut and paste
/// come back as a [`ClipboardRequest`] for the frontend to satisfy.
pub fn apply(field: &mut TextField, edit: TextFieldEdit) -> Option<ClipboardRequest> {
    match edit {
        TextFieldEdit::Move {
            delta,
            word,
            select,
        } => match (word, select) {
            (true, true) => field.select_word_to(delta),
            (true, false) => field.move_caret_word(delta),
            (false, true) => {
                let len = field.text.chars().count();
                let target = if delta < 0 {
                    field.caret().saturating_sub(delta.unsigned_abs())
                } else {
                    field.caret().saturating_add(delta.unsigned_abs()).min(len)
                };
                field.select_to(target);
            }
            (false, false) => field.move_caret(delta),
        },
        TextFieldEdit::Home { select } => {
            if select {
                field.select_to(0);
            } else {
                field.move_caret_to(0);
            }
        }
        TextFieldEdit::End { select } => {
            let end = field.text.chars().count();
            if select {
                field.select_to(end);
            } else {
                field.move_caret_to(end);
            }
        }
        TextFieldEdit::Backspace { word: true } => {
            field.delete_word_back();
        }
        TextFieldEdit::Backspace { word: false } => {
            field.backspace();
        }
        TextFieldEdit::Delete { word: true } => {
            field.delete_word_forward();
        }
        TextFieldEdit::Delete { word: false } => {
            field.delete_forward();
        }
        TextFieldEdit::DeleteToLineStart => {
            field.delete_to_line_start();
        }
        TextFieldEdit::SelectAll => field.select_all(),
        TextFieldEdit::Copy => {
            return field
                .selected_text()
                .map(|text| ClipboardRequest::Write(text.to_string()));
        }
        TextFieldEdit::Cut => {
            return field.take_selected_text().map(ClipboardRequest::Write);
        }
        TextFieldEdit::Paste => return Some(ClipboardRequest::Read),
    }
    None
}
