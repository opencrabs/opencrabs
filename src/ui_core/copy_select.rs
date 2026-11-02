//! The rule for drag-to-copy over transcript rows (#2383): which rows a drag
//! covers, and the text a copy takes. The window keeps the drag state; this
//! decides what it means.

/// The rows a drag from `anchor` to `end` covers, first to last. A drag that
/// never left its row covers nothing, so a plain click copies nothing.
pub fn covered_rows(anchor: usize, end: usize) -> Option<(usize, usize)> {
    (anchor != end).then(|| (anchor.min(end), anchor.max(end)))
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
