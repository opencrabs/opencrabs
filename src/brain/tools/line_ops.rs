//! Byte-preserving line edits (#2227).
//!
//! `str::lines()` is lossy for a read-modify-write: it drops the terminator and
//! a trailing `\r`. A tool that rebuilds a file from `lines()` and `join("\n")`
//! can only put back the ending it was told to, so a CRLF file silently came
//! out LF and every file lost its final newline. Nothing in the tool result said
//! so, because the diff only lists changed lines.
//!
//! These helpers splice the edited byte range out of the original instead, so
//! every byte nobody asked about is preserved exactly. `fuzzy_replace_once` has
//! worked this way since #2212; the line operations of `edit_file` did not, and
//! `hashline_edit` still rebuilt the whole document.

/// Byte span of every line, terminator included.
///
/// `spans.len()` always equals `content.lines().count()`, so callers can keep
/// validating bounds against the line count they already compute.
pub(crate) fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (i, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            spans.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < content.len() {
        spans.push((start, content.len()));
    }
    spans
}

/// Borrow `content[start..end]`.
///
/// Every index these helpers pass comes from [`line_spans`], which only splits
/// at `b'\n'` or at `content.len()`. A `0x0A` byte is never a UTF-8
/// continuation byte, so both indices are char boundaries by construction and
/// the slice cannot land mid-character. `get` rather than `[..]` means a future
/// caller that breaks that invariant gets a named panic instead of a bare
/// slice panic with no explanation (#2227).
fn slice(content: &str, start: usize, end: usize) -> &str {
    content
        .get(start..end)
        .expect("line_ops indices come from line_spans, which yields char boundaries")
}

/// The line ending used by line `index`.
///
/// The line's own ending wins, which keeps a mixed-ending file mixed. An
/// unterminated last line carries no ending of its own, so the line before it
/// describes the file just as well.
pub(crate) fn eol_at(content: &str, spans: &[(usize, usize)], index: usize) -> &'static str {
    if index < spans.len() {
        let line = slice(content, spans[index].0, spans[index].1);
        if line.contains("\r\n") {
            return "\r\n";
        }
    }
    if index > 0
        && let Some(&(start, end)) = spans.get(index - 1)
        && slice(content, start, end).contains("\r\n")
    {
        return "\r\n";
    }
    "\n"
}

/// The ending a whole-document rebuild should use.
///
/// For callers that cannot splice because they replace the document wholesale
/// (`hashline_edit` applies edits bottom-up over a line vector). A file with
/// equal counts of both endings is treated as LF, the commoner case.
pub(crate) fn dominant_eol(content: &str) -> &'static str {
    let crlf = content.matches("\r\n").count();
    let lf = content.matches('\n').count().saturating_sub(crlf);
    if crlf > lf { "\r\n" } else { "\n" }
}

/// Rewrite caller-supplied text with `eol` as its line separator.
///
/// `read_file` shows the model CR-stripped text, so a multi-line replacement
/// arrives with bare LF even when the file is CRLF. The inserted lines have to
/// adopt the file's ending, or the untouched bytes are preserved and the new
/// ones still land as LF.
///
/// One trailing newline is treated as the caller terminating the block rather
/// than as a request for an extra blank line; the caller supplies the block's
/// terminator separately. This matches how `fuzzy_replace_once` reads its
/// replacement text.
pub(crate) fn with_eol(text: &str, eol: &str) -> String {
    let lf = text.replace("\r\n", "\n");
    let block = lf.strip_suffix('\n').unwrap_or(&lf);
    if eol == "\n" {
        block.to_string()
    } else {
        block.replace('\n', eol)
    }
}

fn splice(content: &str, start: usize, end: usize, body: &str) -> String {
    let mut out = String::with_capacity(content.len() - (end - start) + body.len());
    out.push_str(slice(content, 0, start));
    out.push_str(body);
    out.push_str(slice(content, end, content.len()));
    out
}

/// Replace lines `start..=end` (0-indexed) with `new_text`.
///
/// The region's own terminator decides whether the replacement ends with one,
/// so a file keeps its final newline (or keeps lacking one).
pub(crate) fn replace_lines(content: &str, start: usize, end: usize, new_text: &str) -> String {
    let spans = line_spans(content);
    let eol = eol_at(content, &spans, start);
    let region_start = spans[start].0;
    let region_end = spans[end].1;
    let region_terminated = slice(content, region_start, region_end).ends_with('\n');

    let mut body = with_eol(new_text, eol);
    if region_terminated {
        body.push_str(eol);
    }
    splice(content, region_start, region_end, &body)
}

/// Insert `text` before line `line` (0-indexed), or at EOF when `line` equals
/// the line count.
pub(crate) fn insert_line(content: &str, line: usize, text: &str) -> String {
    let spans = line_spans(content);
    let offset = if line < spans.len() {
        spans[line].0
    } else {
        content.len()
    };
    let eol = eol_at(content, &spans, line.min(spans.len().saturating_sub(1)));

    let at_eof = offset == content.len();
    let mut body = String::new();
    if offset > 0 && !slice(content, 0, offset).ends_with('\n') {
        // Appending to a file whose last line had no terminator: that line
        // needs one now, or the two would merge into one.
        body.push_str(eol);
    }
    body.push_str(&with_eol(text, eol));
    if !at_eof || content.ends_with('\n') {
        body.push_str(eol);
    }
    splice(content, offset, offset, &body)
}

/// Delete lines `start..=end` (0-indexed).
pub(crate) fn delete_lines(content: &str, start: usize, end: usize) -> String {
    let spans = line_spans(content);
    splice(content, spans[start].0, spans[end].1, "")
}
