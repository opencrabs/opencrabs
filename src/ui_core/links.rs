//! Click-to-open resolution for a rendered line (#1772, #2317).
//!
//! Both clients draw a path or a url as ordinary styled text, and both need
//! the same answer to one question: what did the reader just click? That
//! answer is a url for the browser, or a path that EXISTS on disk for the
//! file manager. It lives here so the terminal and the window cannot drift
//! apart, and so the window can reach it at all: the GUI is a separate binary
//! crate and cannot see the terminal's `pub(crate)` helpers.
//!
//! Two rules keep it safe:
//!
//! - Nothing resolves unless it is a real url scheme or a path that exists on
//!   disk. A prose word that happens to look like `e.g`, or a stale
//!   `old/path.md`, is inert: the same no-phantom-launch rule `dropped_path`
//!   follows for pasted paths (#1288).
//! - A token ends at whitespace and at quote/control boundaries, so a click
//!   can never grab a whole sentence. Paths containing spaces are out of
//!   scope for a click (`dropped_path` owns those for paste).
//!
//! Offsets differ by client. A terminal cell is a CHARACTER, while a shaped
//! text index in the window is a BYTE, so both entry points exist and share
//! one core.

use std::path::{Path, PathBuf};

/// What a click on a token should open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickTarget {
    /// Web URL, opened in the system's default handler.
    Url(String),
    /// File or directory verified to exist on disk.
    File(PathBuf),
}

impl ClickTarget {
    /// Human-readable form, for a log line or a notice.
    pub fn label(&self) -> String {
        match self {
            ClickTarget::Url(url) => url.clone(),
            ClickTarget::File(path) => path.display().to_string(),
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

/// Resolve the openable token at `col`, a character index into `line`. Uses
/// the process cwd and the user's home directory.
pub fn target_at(line: &str, col: usize) -> Option<ClickTarget> {
    let cwd = std::env::current_dir().unwrap_or_default();
    target_at_with(line, col, &cwd, dirs::home_dir().as_deref())
}

/// Injectable-cwd/home core, so tests resolve against tempdirs instead of the
/// real filesystem layout of whoever runs them.
pub fn target_at_with(
    line: &str,
    col: usize,
    cwd: &Path,
    home: Option<&Path>,
) -> Option<ClickTarget> {
    let token = token_at(line, col)?;
    classify(token, cwd, home)
}

/// The same, for a byte offset into `text`: the unit a shaped text layout
/// reports for a pointer position, so the window can ask without converting.
pub fn target_at_byte(text: &str, byte: usize) -> Option<ClickTarget> {
    let cwd = std::env::current_dir().unwrap_or_default();
    target_at_byte_with(text, byte, &cwd, dirs::home_dir().as_deref())
}

/// Injectable-cwd/home form of [`target_at_byte`].
pub fn target_at_byte_with(
    text: &str,
    byte: usize,
    cwd: &Path,
    home: Option<&Path>,
) -> Option<ClickTarget> {
    target_at_with(text, char_col(text, byte), cwd, home)
}

/// A byte offset from a shaped text layout, as the character index the token
/// walk counts in. A byte inside a character rounds down to its start, and an
/// offset past the end clamps to the end.
fn char_col(text: &str, byte: usize) -> usize {
    let mut cut = byte.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].chars().count()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir unique per test, holding the fixtures it names.
    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str, files: &[&str]) -> Self {
            let dir = std::env::temp_dir().join(format!("oc-links-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            for f in files {
                let p = dir.join(f);
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).expect("parent dirs");
                }
                std::fs::write(&p, b"fixture").expect("fixture write");
            }
            Self { dir }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_url_mid_sentence_resolves() {
        let line = "See https://opencrabs.com/docs for more.";
        let col = line.find("opencrabs").expect("needle") + 2;
        assert_eq!(
            target_at_with(line, col, Path::new("/nonexistent"), None),
            Some(ClickTarget::Url("https://opencrabs.com/docs".into()))
        );
    }

    #[test]
    fn an_existing_path_resolves_and_a_missing_one_does_not() {
        let box_ = Sandbox::new("path", &["src/gui/main.rs"]);
        let line = "open src/gui/main.rs and then src/gui/gone.rs please";
        let here = line.find("main.rs").expect("needle");
        assert_eq!(
            target_at_with(line, here, &box_.dir, None),
            Some(ClickTarget::File(box_.dir.join("src/gui/main.rs")))
        );
        let gone = line.find("gone.rs").expect("needle");
        assert_eq!(target_at_with(line, gone, &box_.dir, None), None);
    }

    #[test]
    fn a_line_suffix_is_not_part_of_the_file_name() {
        let box_ = Sandbox::new("line", &["src/gui/main.rs"]);
        let line = "see src/gui/main.rs:1012:5 for the handler";
        let col = line.find("main.rs").expect("needle");
        assert_eq!(
            target_at_with(line, col, &box_.dir, None),
            Some(ClickTarget::File(box_.dir.join("src/gui/main.rs")))
        );
    }

    #[test]
    fn a_byte_offset_resolves_the_token_under_it() {
        let box_ = Sandbox::new("byte", &["notes/a.md"]);
        // The arrow is three bytes in UTF-8, so the byte offset of the path
        // sits past its character index, and the two units disagree.
        let line = "→ notes/a.md ←";
        let char_col = line.find("notes").expect("needle");
        let byte_col = char_col + 3;
        assert_eq!(
            target_at_byte_with(line, byte_col, &box_.dir, None),
            Some(ClickTarget::File(box_.dir.join("notes/a.md")))
        );
        // A byte inside the arrow rounds down to its start, which is a break.
        assert_eq!(target_at_byte_with(line, 1, &box_.dir, None), None);
    }

    #[test]
    fn a_prose_word_is_inert() {
        let line = "for example, e.g. this sentence has no target";
        assert_eq!(target_at_with(line, 15, Path::new("/nonexistent"), None), None);
    }
}
