//! The `!` operator (#2437): `!cmd` runs a shell command in the launch
//! directory and shows its output in the chat. Editors (vi, nano, ...) need a
//! real terminal, so the window opens them in one. Parsing lives here so the
//! window and the tests share it.

use std::path::Path;

/// Editors that open in a terminal window. Mirrors the terminal's allowlist
/// (`src/tui/editor.rs`), which is crate-private to the terminal build.
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

/// The command after a leading `!`, trimmed. None when the line is not a bang
/// command or the command is empty.
pub fn bang_command(text: &str) -> Option<&str> {
    let cmd = text.trim().strip_prefix('!')?.trim();
    (!cmd.is_empty()).then_some(cmd)
}

/// Whether the command's program is an editor that needs a terminal.
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
