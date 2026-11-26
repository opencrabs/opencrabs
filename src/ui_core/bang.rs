//! The `!` operator (#2437): `!cmd` runs a shell command in the launch
//! directory and shows its output in the chat. Editors (vi, nano, ...) open in
//! the window's own buffer (#3163): the terminal hands them the real tty, the
//! window opens a dialog that edits and saves the file. Parsing lives here so
//! both sides share it.

use std::path::Path;

/// Editors the window opens in its own buffer (#3163). Mirrors the terminal's
/// allowlist (`src/tui/editor.rs`), which is crate-private to the terminal
/// build: the terminal hands the real tty to the editor, the window opens a
/// built-in dialog, and both agree on WHICH programs count as editors.
const EDITORS: &[&str] = &[
    "vi",
    "vim",
    "nvim",
    "nano",
    "emacs",
    "emacsclient",
    "helix",
    "hx",
    "micro",
];

/// An editor invocation, parsed out of a bang command (#3163).
///
/// The window does not run the editor in a terminal any more: it opens a
/// built-in buffer. To do that it has to know what the user asked for, and
/// the ask is three things: which editor, which file, and where to put the
/// cursor. A bare `!vi` is an empty buffer, exactly like `vi` on a shell with
/// no file.
///
/// The flags are the ones every editor in the list understands the same way
/// (`+N` to jump to a line, `-R` to open read-only). Anything else the user
/// typed is kept in `flags` so the dialog can show it rather than silently
/// swallowing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCommand {
    /// The editor's program, as typed (`vi`, `nano`, ...).
    pub program: String,
    /// The file to open, when the user named one.
    pub path: Option<String>,
    /// The line to put the cursor on (`+12`), when the user asked for one.
    pub line: Option<u32>,
    /// Whether the buffer opens read-only (`-R`).
    pub read_only: bool,
    /// The flags the window does not act on, kept for display.
    pub flags: Vec<String>,
}

/// The command after a leading `!`, trimmed. None when the line is not a bang
/// command or the command is empty.
pub fn bang_command(text: &str) -> Option<&str> {
    let cmd = text.trim().strip_prefix('!')?.trim();
    (!cmd.is_empty()).then_some(cmd)
}

/// Whether the command's program is an editor.
pub fn is_editor(cmd: &str) -> bool {
    let Some(first) = cmd.split_whitespace().next() else {
        return false;
    };
    let base = Path::new(first)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(first);
    EDITORS.contains(&base)
}

/// Parse a bang command into an editor invocation. `None` when the command
/// does not name an editor.
///
/// The path is the argument that names the file. Flags come in front of it,
/// the way every editor in the list takes them, so `vi -R notes.md` opens
/// `notes.md` read-only. `+12` sets the line to jump to, and `-R` (or
/// `--read-only`) opens the buffer read-only. A bare `vi` is an empty buffer:
/// no path, no line, not read-only.
///
/// `--` ends the flags, so anything after it is the file even when it starts
/// with a dash. A flag the window does not know (`-u NONE`, `-w`) is kept in
/// `flags`: it is shown, never silently dropped.
pub fn editor_command(cmd: &str) -> Option<EditorCommand> {
    let words = split_words(cmd);
    let first = words.first()?;
    let base = Path::new(first.as_str())
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(first.as_str());
    if !EDITORS.contains(&base) {
        return None;
    }

    let mut path: Option<String> = None;
    let mut line = None;
    let mut read_only = false;
    let mut flags: Vec<String> = Vec::new();
    let mut candidates: Vec<String> = Vec::new();
    let mut after_dashdash = false;
    for word in words.iter().skip(1) {
        if word == "--" {
            after_dashdash = true;
            flags.push(word.clone());
            continue;
        }
        if after_dashdash {
            if path.is_none() {
                path = Some(word.clone());
            } else {
                flags.push(word.clone());
            }
            continue;
        }
        if let Some(rest) = word.strip_prefix('+') {
            if let Ok(number) = rest.parse::<u32>()
                && number > 0
            {
                line = Some(number);
            } else {
                flags.push(word.clone());
            }
            continue;
        }
        if word == "-R" || word == "--read-only" {
            read_only = true;
            continue;
        }
        if word.starts_with('-') {
            flags.push(word.clone());
            continue;
        }
        candidates.push(word.clone());
    }

    // The file is the last plain argument: `vim -u NONE notes.md` names the
    // file last, and an earlier plain word is a flag's value, not a file.
    if path.is_none()
        && let Some(last) = candidates.pop()
    {
        path = Some(last);
    }
    flags.extend(candidates);

    Some(EditorCommand {
        program: first.to_string(),
        path,
        line,
        read_only,
        flags,
    })
}

/// Split a command into words, keeping a quoted path whole.
///
/// A file called `my notes.md` is one argument for every editor in the list,
/// so `!vi "my notes.md"` has to open that file and not two of them. The
/// quotes are dropped, like a shell would drop them.
fn split_words(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_word = false;
    for ch in cmd.chars() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
            }
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                has_word = true;
            }
            None if ch.is_whitespace() => {
                if has_word {
                    words.push(std::mem::take(&mut current));
                    has_word = false;
                }
            }
            None => {
                current.push(ch);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(current);
    }
    words
}

/// The text the chat shows for a finished command: its stdout then stderr,
/// trimmed, or a line saying it printed nothing.
pub fn output_text(stdout: &[u8], stderr: &[u8], success: bool) -> String {
    let mut out = String::from_utf8_lossy(stdout).trim_end().to_string();
    let err = String::from_utf8_lossy(stderr);
    let err = err.trim_end();
    if !err.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(err);
    }
    if out.is_empty() {
        return if success {
            "(no output)".to_string()
        } else {
            "(exited with an error, no output)".to_string()
        };
    }
    out
}
