//! Click-to-open targets in the transcript (#1772).
//!
//! The TUI renders file paths and URLs as styled text, but a click on one did
//! nothing: plain clicks select messages, and the only launcher lived behind
//! the session-files overlay. This module resolves the token *under the
//! cursor* into something the OS can launch.
//!
//! Two rules keep it safe:
//!
//! - Nothing launches unless it is a real URL scheme or a path that EXISTS on
//!   disk. A prose word that happens to look like `e.g` or a stale
//!   `old/path.md` is inert, same no-phantom-launch rule `dropped_path`
//!   follows for pasted paths (#1288).
//! - Tokens end at whitespace and quote/control boundaries, so a click can
//!   never grab a whole sentence. Paths containing spaces are out of scope
//!   for a click (the extension-anchored scanner in `dropped_path` owns
//!   those for paste).

use std::path::{Path, PathBuf};

/// What a click on a token should launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClickTarget {
    /// Web URL, opened in the system's default handler.
    Url(String),
    /// File or directory verified to exist on disk.
    File(PathBuf),
}

impl ClickTarget {
    /// Human-readable form for the notification line.
    pub(crate) fn label(&self) -> String {
        match self {
            ClickTarget::Url(u) => u.clone(),
            ClickTarget::File(p) => p.display().to_string(),
        }
    }
}

/// Characters that end a token. Quotes and angle brackets can't be part of a
/// path or URL in the rendered transcript; brackets/parens are NOT breaks so
/// a markdown link's ` (https://x)` suffix tokenizes whole and is unwrapped by
/// the cleaner instead of chopped.
fn is_break(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '`' | '<' | '>')
}

/// Resolve the openable token at `col` (a character index into the rendered
/// line). Uses the process cwd and the user's home directory.
pub(crate) fn target_at(line: &str, col: usize) -> Option<ClickTarget> {
    let cwd = std::env::current_dir().unwrap_or_default();
    target_at_with(line, col, &cwd, dirs::home_dir().as_deref())
}

/// Injectable-cwd/home core, so tests resolve against tempdirs instead of the
/// real filesystem layout of whoever runs them.
pub(crate) fn target_at_with(
    line: &str,
    col: usize,
    cwd: &Path,
    home: Option<&Path>,
) -> Option<ClickTarget> {
    let token = token_at(line, col)?;
    classify(token, cwd, home)
}

/// Expand from `col` to the token surrounding it. A click on whitespace or
/// past the line's end resolves to nothing.
fn token_at(line: &str, col: usize) -> Option<&str> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    if col >= chars.len() || is_break(chars[col].1) {
        return None;
    }
    let mut a = col;
    while a > 0 && !is_break(chars[a - 1].1) {
        a -= 1;
    }
    let mut b = col;
    while b + 1 < chars.len() && !is_break(chars[b + 1].1) {
        b += 1;
    }
    let start = chars[a].0;
    let end = chars[b].0 + chars[b].1.len_utf8();
    Some(&line[start..end])
}

/// Strip rendering wrapper noise around a token.
///
/// `[text](url)` renders as `text (url)`, inline code keeps its backticks as
/// separate break-boundary tokens already, and prose trails punctuation:
/// `see ~/a/b.md.` must not look for `b.md.` on disk.
fn clean_token(raw: &str) -> &str {
    let mut s = raw;
    loop {
        let prev = s;
        // Balanced outer wrappers: "(https://x)", "[a.md]", "{p}".
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            if s.len() > 2 && s.starts_with(open) && s.ends_with(close) {
                s = &s[1..s.len() - 1];
                break;
            }
        }
        // Trailing sentence punctuation.
        s = s.trim_end_matches(['.', ',', ';', ':', '!', '?']);
        // A trailing closer only when nothing opened it (x.py) → x.py,
        // but a URL with a balanced inner "(bar)" keeps its ")".
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            if s.ends_with(close) && !s.contains(open) {
                s = &s[..s.len() - close.len_utf8()];
            }
        }
        if s == prev {
            return s;
        }
    }
}

/// Classify a token as URL / existing path / nothing.
fn classify(raw: &str, cwd: &Path, home: Option<&Path>) -> Option<ClickTarget> {
    let token = clean_token(raw);
    if token.is_empty() {
        return None;
    }
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // Require something host-shaped after the scheme: "https://" alone
        // (or "https://." from prose) must not launch the browser.
        let rest = &token[lower.find("://").unwrap() + 3..];
        if rest.contains('.') || rest.contains(':') || rest.eq_ignore_ascii_case("localhost") {
            return Some(ClickTarget::Url(token.to_string()));
        }
        return None;
    }
    if lower.starts_with("file://") {
        let path = &token["file://".len()..];
        return resolve_path(path, cwd, home);
    }
    if lower.starts_with("mailto:") && token.contains('@') {
        return Some(ClickTarget::Url(token.to_string()));
    }
    resolve_path(token, cwd, home)
}

/// Try the token as a local path: expand `~`, drop any `:line[:col]`
/// reference suffix, resolve relative to `cwd`, and require existence.
fn resolve_path(token: &str, cwd: &Path, home: Option<&Path>) -> Option<ClickTarget> {
    // Line references are not part of the name, and leaving them on blinds
    // the extension check below: `chat.rs:1618` would read as extension
    // `rs:1618` and never get probed.
    let token = strip_line_ref(token);
    // Only forms that plausibly name a file get a disk probe. Bare prose
    // words without a dot have no chance of resolving anyway; bare dotted
    // names ("README.md") DO get probed because clicking exactly the word
    // that names a real file in cwd is the point.
    let path_like = token.starts_with('/')
        || token.starts_with('~')
        || token.starts_with('.')
        || token.contains('/')
        || token.contains('\\')
        || looks_extended(token);
    if !path_like {
        return None;
    }
    let expanded = expand_tilde(token, home)?;
    let candidate = if Path::new(&expanded).is_absolute() {
        PathBuf::from(&expanded)
    } else {
        cwd.join(&expanded)
    };
    if candidate.exists() {
        Some(ClickTarget::File(candidate))
    } else {
        None
    }
}

/// `chat.rs:1618` and `chat.rs:1618:5` reference a line in a file; the
/// trailing numeric segments are not part of the name. Windows drive letters
/// (`C:`) are one char and never all-digits, so they survive.
fn strip_line_ref(token: &str) -> &str {
    let mut s = token;
    while let Some((head, tail)) = s.rsplit_once(':') {
        let tail_is_number = !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit());
        if !tail_is_number || head.is_empty() || head.chars().count() <= 1 {
            break;
        }
        s = head;
    }
    s
}

/// Ends in something that reads like an extension: `.md`, `.rs`, `.png`,
/// `.tar.gz` (last segment only matters), 1-8 alnum chars.
fn looks_extended(token: &str) -> bool {
    match token.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 8
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// Expand a leading `~` against the provided home. Returns None when the
/// token wants a home the environment does not have.
fn expand_tilde(token: &str, home: Option<&Path>) -> Option<String> {
    if token == "~" {
        return home.map(|h| h.display().to_string());
    }
    if let Some(rest) = token.strip_prefix("~/") {
        let h = home?;
        return Some(h.join(rest.trim_start_matches('/')).display().to_string());
    }
    Some(token.to_string())
}

/// Launch the target with the system default handler, detached (#2170).
///
/// `spawn` succeeding only proves the launcher binary existed. Whether the
/// open actually worked is only known from the child's exit status, which
/// arrives later as `TuiEvent::LauncherFailed` so the caller's optimistic
/// "Opened …" line gets corrected instead of standing as a lie. The draw
/// loop never blocks on a browser or Finder coming up.
pub(crate) fn open(
    target: &ClickTarget,
    tx: tokio::sync::mpsc::UnboundedSender<crate::tui::events::TuiEvent>,
) -> std::io::Result<()> {
    let arg = target.label();
    let label = arg.clone();
    crate::tui::launcher::open_detached(Path::new(&arg), move |detail| {
        let _ = tx.send(crate::tui::events::TuiEvent::LauncherFailed {
            target: label,
            detail,
        });
    })
}
