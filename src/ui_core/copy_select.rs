//! The rule for drag-to-copy over transcript rows (#2383, #2529): which rows
//! and byte ranges a drag covers, and the text a copy takes. The window keeps
//! the drag state; this decides what it means.

/// The rows a drag from `anchor` to `end` covers, first to last. A drag that
/// never left its row covers nothing, so the original whole-row click copies
/// nothing. Character selection uses [`ordered_points`] below instead.
pub fn covered_rows(anchor: usize, end: usize) -> Option<(usize, usize)> {
    (anchor != end).then(|| (anchor.min(end), anchor.max(end)))
}

/// One end of a text selection: a transcript row and a UTF-8 byte offset in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPoint {
    pub row: usize,
    pub offset: usize,
}

impl TextPoint {
    pub fn new(row: usize, offset: usize) -> Self {
        Self { row, offset }
    }

    fn order_key(self) -> (usize, usize) {
        (self.row, self.offset)
    }
}

/// Whether a key is one of the two transcript copy chords. The composer has
/// its own GPUI action bindings; the window uses this same predicate when the
/// current selection belongs to the transcript.
pub fn is_copy_chord(key: &str, control: bool, platform: bool) -> bool {
    key == "c" && (control || platform)
}

/// Order a drag without losing which byte was grabbed inside either row.
/// Equal points are a click, not a selection. Equal rows with different
/// offsets are the within-block case #2529 fixes.
pub fn ordered_points(anchor: TextPoint, end: TextPoint) -> Option<(TextPoint, TextPoint)> {
    if anchor == end {
        return None;
    }
    if anchor.order_key() <= end.order_key() {
        Some((anchor, end))
    } else {
        Some((end, anchor))
    }
}

/// Clamp a pointer-derived byte offset to a valid character boundary.
/// GPUI returns boundaries already; this also keeps synthetic events and
/// out-of-range end points from panicking while copying UTF-8 text.
fn boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The exact dragged text. Within one row this is only the selected range.
/// Across rows it keeps #2383's blank-line join and takes the partial first
/// and last rows rather than snapping the whole gesture to block boundaries.
pub fn copied_range(texts: &[String], anchor: TextPoint, end: TextPoint) -> Option<String> {
    let (start, end) = ordered_points(anchor, end)?;
    if start.row >= texts.len() || end.row >= texts.len() {
        return None;
    }

    if start.row == end.row {
        let text = &texts[start.row];
        let lo = boundary(text, start.offset);
        let hi = boundary(text, end.offset);
        return (lo < hi)
            .then(|| text[lo..hi].to_string())
            .filter(|text| !text.is_empty());
    }

    let mut picked = Vec::with_capacity(end.row - start.row + 1);
    for (row, text) in texts.iter().enumerate().take(end.row + 1).skip(start.row) {
        let lo = if row == start.row {
            boundary(text, start.offset)
        } else {
            0
        };
        let hi = if row == end.row {
            boundary(text, end.offset)
        } else {
            text.len()
        };
        picked.push(text[lo..hi].to_string());
    }
    (!picked.iter().all(String::is_empty)).then(|| picked.join("\n\n"))
}

/// The text of rows `lo..=hi`, joined by blank lines. None when the range is
/// empty or outside the rows.
pub fn copied_text(texts: &[String], lo: usize, hi: usize) -> Option<String> {
    let picked: Vec<&str> = texts
        .iter()
        .skip(lo)
        .take(hi.checked_sub(lo)? + 1)
        .map(String::as_str)
        .collect();
    (!picked.is_empty()).then(|| picked.join("\n\n"))
}
