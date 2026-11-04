//! Which shell commands are long enough to run detached (#722).
//!
//! A command is detached when it *starts* one of the known long tasks. It is
//! deliberately not enough for the text to contain one: a heredoc writing a
//! report that names a build step, a `grep` for the phrase, or an interpreter
//! script whose body inserts one into a source file all finish in
//! milliseconds. Detaching those buys nothing and costs a full turn, because
//! the tool returns immediately and the completion arrives as an injected
//! message that starts another one. Detaching a file-writing heredoc is worse
//! than useless: the turn ends before the write has landed.
//!
//! Anything ambiguous stays inline. That is the pre-#722 behaviour and the
//! safe direction to fail in: an inline command that turns out to be long only
//! risks the 600s cap, while a wrongly detached one costs a turn every time.

use crate::utils::shell_scan;

/// Command prefixes that mark a genuinely long-running task.
const KNOWN_LONG_MARKERS: &[&str] = &[
    "cargo test",
    "cargo build",
    "cargo run",
    "cargo clippy",
    "npm test",
    "npm run build",
    "pnpm test",
    "pnpm build",
    "yarn test",
    "yarn build",
    "npx remotion render",
    "remotion render",
    "gh run watch",
    "gh pr checks --watch",
];

/// What bash should do with a command, and why.
///
/// The `why` exists for the log line: when a command that looks long runs
/// inline anyway, the reason has to be visible without a rebuild. Working out
/// which of these two cases had fired cost a full log archaeology session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detach {
    /// `marker` starts a command here: run it detached.
    Yes { marker: &'static str },
    /// `marker` appears, but only as data. Run inline.
    Mentioned { marker: &'static str },
    /// No marker at all. Run inline.
    No,
}

/// Decide how `command` should run, and record why.
pub fn classify(command: &str) -> Detach {
    let shell = shell_scan::blank_quoted(&strip_heredoc_bodies(command)).to_lowercase();
    let lower = command.to_lowercase();
    let mut mentioned = None;

    for marker in KNOWN_LONG_MARKERS {
        if marker_starts_a_command(&shell, marker) {
            return Detach::Yes { marker };
        }
        if mentioned.is_none() && lower.contains(marker) {
            mentioned = Some(*marker);
        }
    }
    match mentioned {
        Some(marker) => Detach::Mentioned { marker },
        None => Detach::No,
    }
}

/// Is `command` a known long-running task that should run in the background?
pub fn is_known_long(command: &str) -> bool {
    matches!(classify(command), Detach::Yes { .. })
}

/// Drop heredoc bodies, keeping the line that opens them. A body is data
/// written to a file or fed to an interpreter, never a command this shell runs.
fn strip_heredoc_bodies(command: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut lines = command.lines();
    while let Some(line) = lines.next() {
        out.push(line);
        let Some(delim) = heredoc_delimiter(line) else {
            continue;
        };
        for body in lines.by_ref() {
            if body.trim() == delim {
                break;
            }
        }
    }
    out.join("\n")
}

/// Byte ranges of every heredoc body in `command`, including the delimiter
/// line that closes it.
///
/// Same rule as [`strip_heredoc_bodies`]: a body is data, never a command. It
/// differs only in shape, returning ranges over the original string so a caller
/// can keep the byte offsets it already has. `sudo` detection needs exactly
/// that: it has to rewrite the `sudo` word it finds, and must not touch one
/// that only appears inside a body (#2157).
pub(crate) fn heredoc_body_ranges(command: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut pending: Option<String> = None;
    let mut body_start: Option<usize> = None;
    let mut offset = 0usize;

    for line in command.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let text = line.trim_end_matches(['\n', '\r']);

        match pending.as_deref() {
            Some(delim) => {
                body_start.get_or_insert(line_start);
                if text.trim() == delim {
                    ranges.push(body_start.take().unwrap_or(line_start)..offset);
                    pending = None;
                }
            }
            None => {
                if let Some(delim) = heredoc_opened_outside_quotes(text) {
                    pending = Some(delim);
                }
            }
        }
    }

    // An unterminated heredoc runs to the end of the input, which is how the
    // shell reads it too.
    if let Some(start) = body_start {
        ranges.push(start..command.len());
    }
    ranges
}

/// The delimiter of a heredoc opened on `line`, but only when the `<<` is shell
/// syntax. `heredoc_delimiter` scans the raw text, so `echo "see <<EOF"` would
/// open a body that does not exist and hide every later line from a caller.
fn heredoc_opened_outside_quotes(line: &str) -> Option<String> {
    let scanned = shell_scan::scan(line);
    let chars: Vec<char> = line.chars().collect();
    let positions: Vec<usize> = line.char_indices().map(|(at, _)| at).collect();

    for i in 0..chars.len().saturating_sub(1) {
        if chars[i] != '<' || chars[i + 1] != '<' {
            continue;
        }
        if scanned[i].literal || scanned[i + 1].literal {
            continue;
        }
        // `<<<` feeds a word to stdin; nothing after it opens a body.
        if chars.get(i + 2) == Some(&'<') {
            continue;
        }
        if let Some(delim) = heredoc_delimiter(&line[positions[i]..]) {
            return Some(delim);
        }
    }
    None
}

/// The delimiter word of the first heredoc opened on `line`, if any. Handles
/// `<<EOF`, `<<-EOF`, `<<'EOF'` and `<<"EOF"`; `<<<` is a here-string, not one.
fn heredoc_delimiter(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b'<' || bytes[i + 1] != b'<' {
            i += 1;
            continue;
        }
        if bytes.get(i + 2) == Some(&b'<') {
            i += 3;
            continue;
        }
        let mut j = i + 2;
        if bytes.get(j) == Some(&b'-') {
            j += 1;
        }
        while matches!(bytes.get(j), Some(&b' ') | Some(&b'\t')) {
            j += 1;
        }
        let quote = match bytes.get(j) {
            Some(&b'\'') => Some(b'\''),
            Some(&b'"') => Some(b'"'),
            _ => None,
        };
        if quote.is_some() {
            j += 1;
        }
        let start = j;
        while let Some(&c) = bytes.get(j) {
            match quote {
                Some(q) if c == q => break,
                Some(_) => j += 1,
                None if c.is_ascii_alphanumeric() || c == b'_' => j += 1,
                None => break,
            }
        }
        if j > start {
            return Some(line[start..j].to_string());
        }
        i = j.max(i + 2);
    }
    None
}

/// Does `marker` appear at the start of a command inside `shell`?
fn marker_starts_a_command(shell: &str, marker: &str) -> bool {
    shell
        .match_indices(marker)
        .any(|(at, _)| is_command_position(&shell[..at]))
}

/// Is the text preceding a marker the end of a command boundary?
fn is_command_position(prefix: &str) -> bool {
    let trimmed = prefix.trim_end_matches([' ', '\t']);
    let Some(last) = trimmed.chars().last() else {
        return true;
    };
    if matches!(last, ';' | '&' | '|' | '(' | '{' | '\n' | '`') {
        return true;
    }
    let last_word = trimmed.rsplit([' ', '\t', '\n']).next().unwrap_or_default();
    matches!(
        last_word,
        "do" | "then" | "else" | "time" | "exec" | "nohup"
    )
}
