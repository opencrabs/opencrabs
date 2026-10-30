//! Fuzzy line-sequence matching for str_replace.
//!
//! Ported from OpenAI Codex `seek_sequence` (apply-patch/src/seek_sequence.rs)
//! and Alexey Leshchenko's Python port (agent-eval-matrix). Tolerates minor
//! whitespace and Unicode punctuation differences between the agent's
//! `old_text` and the actual file content.

use super::line_ops::line_spans;
use unicode_normalization::UnicodeNormalization;

/// Normalize Unicode punctuation/whitespace to ASCII equivalents so that
/// smart quotes, em-dashes, non-breaking spaces, etc. don't break matching.
fn normalise_unicode(s: &str) -> String {
    let nfc: String = s.nfc().collect();
    nfc.chars()
        .map(|c| match c {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{00a0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200a}' | '\u{202f}' | '\u{205f}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Returns true if the two lines match under one of the progressively looser
/// comparators: exact, rstrip, strip, unicode-normalised.
fn line_matches(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.trim_end_matches(|c: char| c.is_ascii_whitespace())
        == b.trim_end_matches(|c: char| c.is_ascii_whitespace())
    {
        return true;
    }
    if a.trim() == b.trim() {
        return true;
    }
    normalise_unicode(a) == normalise_unicode(b)
}

/// Find all starting indices where `pattern` matches `lines` fuzzily.
/// Empty pattern matches only at `start` (if within bounds).
pub fn seek_sequence(lines: &[&str], pattern: &[&str], start: usize) -> Vec<usize> {
    if pattern.is_empty() {
        return if start <= lines.len() {
            vec![start]
        } else {
            Vec::new()
        };
    }
    if pattern.len() > lines.len() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let end = lines.len() - pattern.len() + 1;
    for i in start..end {
        if (0..pattern.len()).all(|j| line_matches(lines[i + j], pattern[j])) {
            matches.push(i);
        }
    }
    matches
}

/// Replace the first unique fuzzy line-sequence match of `old_str` with
/// `new_str` in `content`. Returns `Ok(new_content)` on success, or
/// `Err(message)` describing why the replace couldn't be applied.
pub fn fuzzy_replace_once(content: &str, old_str: &str, new_str: &str) -> Result<String, String> {
    if old_str.is_empty() {
        return Err("old_str must not be empty.".into());
    }

    // Fast path: exact substring match.
    if content.contains(old_str) {
        let count = content.matches(old_str).count();
        if count > 1 {
            return Err(format!(
                "old_str appears {count} times as exact substring. Include more context to make a unique match."
            ));
        }
        return Ok(content.replacen(old_str, new_str, 1));
    }

    // Slow path: fuzzy line-sequence match.
    let content_lines: Vec<&str> = content.lines().collect();
    let search_lines: Vec<&str> = old_str.lines().collect();

    let indices = seek_sequence(&content_lines, &search_lines, 0);
    if indices.is_empty() {
        return Err(
            "old_str not found (exact or fuzzy line match). Check whitespace and indentation."
                .into(),
        );
    }
    if indices.len() > 1 {
        return Err(format!(
            "old_str matches {} locations. Include more context to make a unique match.",
            indices.len()
        ));
    }

    let start = indices[0];
    let end_exclusive = start + search_lines.len();

    // Splice the matched bytes out of the original instead of rebuilding the
    // file from `lines()` (#2212). `lines()` drops each terminator and a
    // trailing `\r`, so a join can only put back the ending it was told to:
    // on a CRLF file it silently converted every untouched line to LF, and on
    // a mixed-ending file it normalised the lot.
    let spans = line_spans(content);
    let region_start = spans[start].0;
    let region_end = spans[end_exclusive - 1].1;
    let region = &content[region_start..region_end];

    // The matched region's own ending wins for the replacement lines too.
    // `read_file` shows the model CR-stripped text, so `new_str` normally
    // arrives with bare LF even when the file is CRLF. Keying on the region
    // rather than the whole file also keeps a mixed-ending file mixed.
    let eol = if region.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    // `lines()` on the caller's text keeps the old semantics for a
    // replacement of "\n" (one blank line) and for "" (the block is removed),
    // then re-joins with the file's own ending.
    let normalized_new = new_str.replace("\r\n", "\n");
    let new_lines: Vec<&str> = normalized_new.lines().collect();
    let mut body = new_lines.join(eol);
    // The region's own terminator belongs to the file, not to the caller's
    // text: without this, replacing the last line of a block would glue it
    // to the following one.
    if !new_lines.is_empty() && region.ends_with('\n') {
        body.push_str(eol);
    }

    let mut new_content = String::with_capacity(content.len() - region.len() + body.len());
    new_content.push_str(&content[..region_start]);
    new_content.push_str(&body);
    new_content.push_str(&content[region_end..]);

    Ok(new_content)
}
