//! Find the next real tool ledger in a persisted assistant row.
//!
//! The tool loop writes a ledger as `\n<!-- tools-v2: [json] -->\n`, so a
//! real opener always starts a line. The model, reading its own history
//! through `session_search`, sometimes quotes a ledger opener mid-sentence
//! in its reasoning or answer (#1587). Reload used to take the first
//! `<!-- tools-v2:` it found, walk prose looking for the closing bracket,
//! fail, and dump everything after the quoted opener as the answer.
//!
//! Two rules keep a quoted marker where it belongs, inside the text of the
//! block it sits in: an opener counts only at the start of a line, and a
//! candidate that does not parse is skipped rather than ending the scan.

const V2_OPEN: &str = "<!-- tools-v2:";
const V1_OPEN: &str = "<!-- tools:";
const CLOSE: &str = "-->";

/// One parsed ledger: where it starts in the scanned text, its body (the
/// JSON array for v2, the `a | b` list for v1) and the offset just past
/// its closing arrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger<'a> {
    pub start: usize,
    pub is_v2: bool,
    pub body: &'a str,
    pub end: usize,
}

/// The earliest ledger in `s` that both starts a line and parses.
pub fn next_ledger(s: &str) -> Option<Ledger<'_>> {
    let mut from = 0;
    while from <= s.len() {
        let v2 = find_at_line_start(s, V2_OPEN, from).map(|i| (i, true, V2_OPEN.len()));
        let v1 = find_at_line_start(s, V1_OPEN, from).map(|i| (i, false, V1_OPEN.len()));
        let (start, is_v2, open_len) = match (v2, v1) {
            (Some(a), Some(b)) => {
                if a.0 <= b.0 {
                    a
                } else {
                    b
                }
            }
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return None,
        };
        if let Some((body, end)) = parse_body(s, start + open_len, is_v2) {
            return Some(Ledger {
                start,
                is_v2,
                body,
                end,
            });
        }
        // A quoted or truncated opener: leave it as text and keep looking.
        from = start + open_len;
    }
    None
}

/// First occurrence of `marker` at or after `from` that begins a line.
pub(crate) fn find_at_line_start(s: &str, marker: &str, from: usize) -> Option<usize> {
    let mut at = from;
    while at <= s.len() {
        let rel = s[at..].find(marker)?;
        let abs = at + rel;
        if abs == 0 || s.as_bytes()[abs - 1] == b'\n' {
            return Some(abs);
        }
        at = abs + 1;
    }
    None
}

/// Body and end offset for an opener whose text starts at `after`.
fn parse_body(s: &str, after: usize, is_v2: bool) -> Option<(&str, usize)> {
    let rest = &s[after..];
    if is_v2 {
        let trimmed = rest.trim_start();
        let lead = rest.len() - trimmed.len();
        let array_end = find_balanced_json_end(trimmed)?;
        let tail = &trimmed[array_end..];
        let post = tail.trim_start();
        if !post.starts_with(CLOSE) {
            return None;
        }
        let tail_lead = tail.len() - post.len();
        let end = after + lead + array_end + tail_lead + CLOSE.len();
        Some((trimmed[..array_end].trim(), end))
    } else {
        let close = rest.find(CLOSE)?;
        Some((rest[..close].trim(), after + close + CLOSE.len()))
    }
}

/// Byte length of a balanced JSON array starting at `s[0] == '['`.
///
/// Tracks string and escape state so `-->` or `]` inside string values
/// (cargo diagnostics like `--> src/main.rs:42` in a tool output) do not
/// end the scan early. `None` when the input does not start with `[` or
/// never balances.
pub(crate) fn find_balanced_json_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (idx, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            match b {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx + 1);
                }
            }
            _ => {}
        }
    }
    None
}
