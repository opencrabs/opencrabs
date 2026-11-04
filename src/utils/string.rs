//! String utility functions.

/// Truncate a string to at most `max_bytes` bytes, ensuring the cut lands on a
/// valid UTF-8 char boundary. Returns the longest prefix that fits.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The longest TAIL of `s` that fits in `max_bytes` bytes, snapped UP to a
/// valid char boundary. Sibling of [`truncate_str`], which keeps the head.
///
/// Every caller that wants "the last N bytes" is slicing `&s[len - n..]`, and
/// that panics the moment `len - n` lands inside a multi-byte character. A
/// response ending in an accent, a curly quote, an emoji or CJK text hits that
/// on ordinary input, so the guard belongs here rather than at each call site.
pub fn truncate_str_tail(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    &s[s.ceil_char_boundary(s.len() - max_bytes)..]
}

/// Everything before the final character of `s`, snapped down to a char
/// boundary. Used to peek at the punctuation that ends a string without
/// slicing inside a multi-byte character.
pub fn prefix_before_last_char(s: &str) -> &str {
    &s[..s.floor_char_boundary(s.len().saturating_sub(1))]
}

/// Truncate a string to at most `max_chars` characters. Unlike [`truncate_str`]
/// which operates on bytes (halving effective length for Cyrillic, quartering
/// for emoji), this counts actual characters so a budget of 2400 means 2400
/// visible characters regardless of language.
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    if s.chars().count() <= max_chars {
        return s;
    }
    // Find the byte offset of the max_chars-th character.
    let byte_end = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..byte_end]
}

/// Shorten a payload to at most `max_chars` CHARACTERS while keeping BOTH ends,
/// and report how many characters were dropped (#490).
///
/// Sibling of [`truncate_chars`] (head-only) and [`truncate_middle`]
/// (byte-based, path-oriented, silent). A notify body is prose in the reader's
/// language, so the head-only cut silently lost its TAIL — which is exactly
/// where the Disclosures and What-now/next blocks live, the part written for
/// the reader. The marker names the count, so a reader can tell that material
/// was removed rather than finding the message simply ends.
///
/// Returns the possibly-rewritten string and the dropped character count; a
/// payload that already fits comes back unchanged with `0` dropped, which is
/// what a caller branches on to decide whether the sender owes a warning.
pub fn truncate_chars_tail_preserving(s: &str, max_chars: usize) -> (String, usize) {
    let total = s.chars().count();
    if total <= max_chars {
        return (s.to_string(), 0);
    }
    // The marker sits INSIDE the budget, so its own width comes off the top.
    // The widest form is a six-digit count — "… (truncated 999999 chars) …" is
    // 30 chars — and the reserve leaves headroom above that.
    const MARKER_RESERVE: usize = 40;
    // A budget this small cannot hold both ends AND a marker, so fall back to
    // the head-only cut rather than emitting a marker that IS the output.
    if max_chars <= MARKER_RESERVE * 2 {
        let head = truncate_chars(s, max_chars);
        return (head.to_string(), total - head.chars().count());
    }
    let budget = max_chars - MARKER_RESERVE;
    // Head-heavy (2:1): the opening carries the headline, the tail carries the
    // ending that must survive. A third is ample for the Disclosures block.
    let head_len = budget * 2 / 3;
    let tail_len = budget - head_len;
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s.chars().skip(total - tail_len).collect();
    let dropped = total - head_len - tail_len;
    (
        format!("{head}\n… (truncated {dropped} chars) …\n{tail}"),
        dropped,
    )
}

/// Returns true if `s` looks like a file path rather than a slash command.
///
/// Slash commands are `/` followed by a single word with no additional slashes
/// and no file extension (e.g. `/help`, `/models`, `/deploy`).
///
/// File paths have additional `/` segments (e.g. `/Users/alice/file.pdf`)
/// or a recognizable file extension on the first word (e.g. `/report.pdf check this`).
///
/// This prevents drag-and-dropped file paths from triggering "Unknown command" errors.
pub fn looks_like_file_path(s: &str) -> bool {
    if !s.starts_with('/') {
        return false;
    }
    // If it contains another `/` after the leading slash, it's a path
    // (e.g. `/Users/...`, `/tmp/...`, `./` resolved to absolute)
    if s[1..].contains('/') {
        return true;
    }
    // If the first word (before any space) has a file extension, treat as path
    // e.g. `/report.pdf check this` → the `/report.pdf` part is a file
    let first_word = s.split_whitespace().next().unwrap_or(s);
    if let Some(ext) = std::path::Path::new(first_word).extension()
        && !ext.is_empty()
    {
        return true;
    }
    false
}

/// Collapse the current user's `$HOME` prefix to `~` in paths/commands.
/// `/Users/alice/srv/foo/bar.rs` → `~/srv/foo/bar.rs`. Keeps absolute
/// paths OUTSIDE home untouched so `/tmp/...` or `/etc/...` still render
/// faithfully. No-op if home isn't resolvable or doesn't appear in `s`.
pub fn tilde_home(s: &str) -> String {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return s.to_string(),
    };
    let home_str = home.to_string_lossy();
    if home_str.is_empty() {
        return s.to_string();
    }
    // Replace all occurrences — bash commands like `cd /Users/me/a && cp /Users/me/b /Users/me/c`
    // benefit from every instance being collapsed.
    s.replace(home_str.as_ref(), "~")
}

/// Shorten a string to fit `max_bytes` while preserving both ends —
/// essential for file paths where the filename (tail) is usually the
/// most informative part. `~/a/b/c/d/very_long_name.rs` truncated to
/// 30 bytes becomes `~/a/b/…/very_long_name.rs` rather than
/// `~/a/b/c/d/very_long_na` which loses the `.rs` extension entirely.
///
/// Respects UTF-8 char boundaries on both sides of the ellipsis.
pub fn truncate_middle(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    const ELLIPSIS: &str = "…"; // 3 bytes
    if max_bytes <= ELLIPSIS.len() + 2 {
        // Too small to preserve both ends meaningfully — fall back to head truncation.
        return truncate_str(s, max_bytes).to_string();
    }
    let budget = max_bytes - ELLIPSIS.len();
    // Slight bias toward keeping the tail since filenames / final args
    // carry more signal than the leading path components.
    let tail_bytes = budget.div_ceil(2);
    let head_bytes = budget - tail_bytes;

    let mut head_end = head_bytes;
    while head_end > 0 && !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len() - tail_bytes;
    while tail_start < s.len() && !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if tail_start <= head_end {
        return truncate_str(s, max_bytes).to_string();
    }
    format!("{}{}{}", &s[..head_end], ELLIPSIS, &s[tail_start..])
}

/// Format a token count as a compact human-readable string (e.g. "150K", "1.2M").
fn format_token_count(tokens: u32) -> String {
    let tokens = tokens as f64;
    if tokens >= 1_000_000.0 {
        format!("{:.1}M", tokens / 1_000_000.0)
    } else if tokens >= 1_000.0 {
        format!("{:.0}K", tokens / 1_000.0)
    } else if tokens > 0.0 {
        format!("{}", tokens as u32)
    } else {
        "0".to_string()
    }
}

/// Format a context budget footer line: "ctx: 8K/200K 4% | 45 tok/s".
///
/// Used by channel handlers to append a context usage indicator to the
/// final message delivered to the user. Plain text so it works across all
/// channel-specific formatters (Telegram HTML, Discord markdown, Slack mrkdwn, WhatsApp).
pub fn format_ctx_footer(used: u32, max: u32, tps: Option<f64>) -> String {
    let pct = if max > 0 {
        (used as f64 / max as f64) * 100.0
    } else {
        0.0
    };
    let base = format!(
        "ctx: {}/{} {:.0}%",
        format_token_count(used),
        format_token_count(max),
        pct
    );
    if let Some(rate) = tps {
        format!("{} | {:.0} tok/s", base, rate)
    } else {
        base
    }
}

/// Build a GitHub-flavored markdown table (`| h | h |` + `| --- | --- |` +
/// rows). Telegram's native rich renderer (`sendRichMessage`) turns this into a
/// real bordered table; the HTML fallback renders a phone-friendly grid /
/// key-value list. Returns `""` for no rows so callers can skip empty sections.
///
/// Cell contents have `|` and newlines neutralized so a stray pipe in a value
/// can't shift columns or break the row.
pub fn md_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let clean = |s: &str| s.replace(['|', '\n'], " ");
    let mut out = String::new();
    out.push_str("| ");
    out.push_str(
        &headers
            .iter()
            .map(|h| clean(h))
            .collect::<Vec<_>>()
            .join(" | "),
    );
    out.push_str(" |\n|");
    for _ in headers {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in rows {
        out.push_str("| ");
        out.push_str(&row.iter().map(|c| clean(c)).collect::<Vec<_>>().join(" | "));
        out.push_str(" |\n");
    }
    out
}

/// Strip ctx footer lines (e.g. "ctx: 84K/200K 42% | 406 tok/s") from text.
/// Used on incoming reply quotes so the metadata never leaks into agent context.
pub fn strip_ctx_footer(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("ctx:") || !trimmed.contains('/') || !trimmed.contains('%')
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end_matches('\n')
        .to_string()
}

/// A short, human-readable status excerpt of the model's live reasoning (#742),
/// at the one-line budget the Telegram flow header uses.
///
/// The TUI wants more room and calls [`thinking_excerpt_capped`] directly, so
/// widening it there does not quietly turn every Telegram status edit into a
/// paragraph (#768).
pub fn thinking_excerpt(thinking: &str) -> Option<String> {
    thinking_excerpt_capped(thinking, THINKING_STATUS_CHARS)
}

/// One-line budget for a chat status line, where the message is edited
/// repeatedly and length is noise.
pub const THINKING_STATUS_CHARS: usize = 80;

/// As [`thinking_excerpt`], with the caller choosing how much to keep.
///
/// Walks the reasoning right-to-left, picks the latest non-trivial sentence,
/// strips a leading "I am / I'm / I will / Let me / Let us", capitalises it, and
/// caps at `max_chars`. Returns `None` for reasoning too short to summarise.
pub fn thinking_excerpt_capped(thinking: &str, max_chars: usize) -> Option<String> {
    let trimmed = thinking.trim();
    if trimmed.len() < 20 {
        return None;
    }
    // Walk sentences right-to-left, pick the latest non-trivial one.
    let mut sentences: Vec<&str> = trimmed
        .split(['.', '?', '!', '\n'])
        .map(str::trim)
        .filter(|s| s.len() >= 12)
        .collect();
    let last = sentences.pop()?;
    let cleaned = last
        .strip_prefix("I am ")
        .or_else(|| last.strip_prefix("I'm "))
        .or_else(|| last.strip_prefix("I will "))
        .or_else(|| last.strip_prefix("Let me "))
        .or_else(|| last.strip_prefix("Let us "))
        .unwrap_or(last)
        .trim();
    if cleaned.is_empty() {
        return None;
    }
    // Capitalise the first letter so "assessing X" -> "Assessing X".
    let mut chars = cleaned.chars();
    let first = chars.next()?;
    let rest: String = chars.collect();
    let pretty = format!("{}{}", first.to_uppercase(), rest);
    let capped: String = pretty.chars().take(max_chars).collect();
    Some(if pretty.chars().count() > max_chars {
        format!("{capped}…")
    } else {
        capped
    })
}

/// How much of the latest reasoning sentence the live summary keeps.
///
/// 80 cut a multi-step chain off mid-thought ("… → commit #766 → comment+close
/// #…"), which is the one moment the line is meant to be useful. 300 is about
/// three wrapped lines on a normal terminal, and the renderer caps the wrap at
/// three, so this stays a fixed budget rather than the scrolling window #742
/// replaced (#768).
pub const THINKING_EXCERPT_CHARS: usize = 300;

/// An unambiguous UTC timestamp for anything persisted or logged (#826).
///
/// The codebase wrote two shapes that look identical and are not:
/// `Local::now().format("%Y-%m-%dT%H:%M:%S")` and
/// `Utc::now().format("%Y-%m-%dT%H:%M:%SZ")`. The first carries no zone, so a
/// local time was stored beside UTC epochs everywhere else and read back as
/// though it were UTC — an hour of silent skew, in whichever direction the
/// host happens to sit.
///
/// One clock, stated once, with the `Z` present so a reader never has to
/// guess. Correlating a stored time with a log line or a chat timestamp is
/// exactly when the guess gets made, and getting it wrong pulls the wrong
/// records while looking authoritative.
pub fn utc_timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A coarse, human age from a whole number of seconds: `3d 2h`, `5h 12m`,
/// `42m`, `<1m` (#480).
///
/// One home: the RSI staleness line and the goal driver line both render an
/// age, and two formatters would drift on where the unit boundaries sit.
/// Negative input clamps to `<1m` rather than rendering a future age.
pub fn humanize_age(secs: i64) -> String {
    let secs = secs.max(0);
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        "<1m".to_string()
    }
}
