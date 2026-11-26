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
//!   can never grab a whole sentence. A name that holds a space is read as a
//!   bounded span of adjacent words, and only when the joined text exists on
//!   disk, so prose stays inert (#2846).
//!
//! Offsets differ by client. A terminal cell is a CHARACTER, while a shaped
//! text index in the window is a BYTE, so both entry points exist and share
//! one core.

use std::ops::Range;
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
    target_at_roots(line, col, &[cwd], home)
}

/// Resolve a click against an ordered list of roots (#2846).
///
/// The first root is the session's working directory, which a window does not
/// own: a desktop launch has no shell cwd, so every relative name in the
/// transcript resolved against `/tmp` and the click was a silent no-op. The
/// process cwd stays available to the terminal as its own root.
///
/// A file the transcript names can hold a space, and a space ends a token, so
/// the single-token read is tried first and a bounded span of adjacent words
/// second. A span only wins when the joined text exists on disk, which is what
/// keeps ordinary prose inert.
pub fn target_at_roots(
    line: &str,
    col: usize,
    roots: &[&Path],
    home: Option<&Path>,
) -> Option<ClickTarget> {
    if let Some(target) = token_at(line, col).and_then(|token| classify_in(token, roots, home)) {
        return Some(target);
    }
    joined_spans(line, col)
        .into_iter()
        .find_map(|span| classify_in(span, roots, home))
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
    text.get(..cut).unwrap_or_default().chars().count()
}

/// The destination a click at `offset` names, given the link labels drawn on
/// the same text (#2317).
///
/// A label wins over token resolution because the label alone carries no
/// destination: `[docs](https://x)` draws `docs`, and only the parser knows
/// where it points. Off every label the text is treated as ordinary rendered
/// text, so a link never makes its whole line clickable.
///
/// `offset` is a byte index, the unit a shaped text layout reports.
pub fn target_for_click(
    text: &str,
    links: &[(Range<usize>, String)],
    offset: usize,
) -> Option<ClickTarget> {
    let cwd = std::env::current_dir().unwrap_or_default();
    target_for_click_with(text, links, offset, &cwd, dirs::home_dir().as_deref())
}

/// Injectable-cwd/home form of [`target_for_click`]. An explicit Markdown
/// destination uses the same local-path rules as a bare token instead of
/// assuming every label points at the web.
pub fn target_for_click_with(
    text: &str,
    links: &[(Range<usize>, String)],
    offset: usize,
    cwd: &Path,
    home: Option<&Path>,
) -> Option<ClickTarget> {
    target_for_click_roots(text, links, offset, &[cwd], home)
}

/// Resolve the target of a click against an ordered list of roots (#2846).
///
/// The label read comes first and uses the same roots, so a markdown
/// destination that names a local file resolves exactly as a bare token does.
pub fn target_for_click_roots(
    text: &str,
    links: &[(Range<usize>, String)],
    offset: usize,
    roots: &[&Path],
    home: Option<&Path>,
) -> Option<ClickTarget> {
    for (span, dest) in links {
        if span.contains(&offset) {
            return classify_link(dest, roots, home);
        }
    }
    target_at_roots(text, char_col(text, offset), roots, home)
}

/// Classify an explicit Markdown destination. Non-file URI schemes retain the
/// existing external-handler behavior; only a verified local path becomes a
/// file target.
fn classify_link(raw: &str, roots: &[&Path], home: Option<&Path>) -> Option<ClickTarget> {
    let token = clean_token(raw);
    if token.is_empty() {
        return None;
    }
    if token.to_ascii_lowercase().starts_with("file://") {
        return resolve_path_in(token.get("file://".len()..)?, roots, home);
    }
    if has_uri_scheme(token) {
        return Some(ClickTarget::Url(token.to_string()));
    }
    resolve_path_in(token, roots, home)
}

/// A URI scheme is `alpha *( alpha / digit / "+" / "-" / "." )` followed by
/// `:`. One-letter schemes are left to path resolution so Windows drive paths
/// such as `C:\\notes.md` remain files.
fn has_uri_scheme(token: &str) -> bool {
    let Some((scheme, _)) = token.split_once(':') else {
        return false;
    };
    scheme.len() > 1
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
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
    line.get(start..end)
}

/// The token a click at `byte` sits inside, expanded both ways along the text.
///
/// This is the same span [`target_at_roots`] tries first, so a miss can name
/// what the resolver actually looked at. The pointer's own tail is not that
/// span: a click in the middle of `brain-audit-2026-11-17.md` used to log
/// `26-11-17.md`, a string no resolver ever considered (#3085).
pub fn token_at_byte(text: &str, byte: usize) -> Option<&str> {
    token_at(text, char_col(text, byte))
}

/// Whether a click at `byte` named anything the resolver would even try.
///
/// The miss line exists to explain a click that named a file or a url and did
/// not open it. A click on ordinary prose named nothing, so writing it into
/// the log only buried the lines that matter: `bisa`, `etap` and an empty
/// token were all reported as misses (#3085).
///
/// This asks the same question the resolver asks before it probes the disk,
/// so a token that was probed and failed still logs, and a word that never
/// reached a probe stays silent.
pub fn click_names_target(text: &str, byte: usize) -> bool {
    let col = char_col(text, byte);
    if token_at(text, col).is_some_and(names_target) {
        return true;
    }
    joined_spans(text, col)
        .iter()
        .any(|span| names_target(span))
}

/// The candidate test behind [`click_names_target`]: a url with something
/// host-shaped after its scheme, or a token shaped like a path.
fn names_target(raw: &str) -> bool {
    let token = clean_token(raw);
    if token.is_empty() {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let scheme_end = lower.find("://").map_or(lower.len(), |at| at + 3);
        return token.get(scheme_end..).is_some_and(|rest| {
            rest.contains('.') || rest.contains(':') || rest.eq_ignore_ascii_case("localhost")
        });
    }
    if lower.starts_with("file://") {
        return token.len() > "file://".len();
    }
    if lower.starts_with("mailto:") {
        return token.contains('@');
    }
    names_path(strip_line_ref(token))
}

/// The shapes worth a disk probe: an absolute, home or relative path, a
/// separator, or a plausible extension. Bare prose words fail here, which is
/// what keeps them out of both the resolver and the log.
fn names_path(token: &str) -> bool {
    token.starts_with('/')
        || token.starts_with('~')
        || token.starts_with('.')
        || token.contains('/')
        || token.contains('\\')
        || looks_extended(token)
}

/// How many adjacent words one click may join when it names a file that holds
/// a space (#2846). Six covers the shape the agent writes most: a generated
/// artifact whose name is separated from its timestamp by one space.
const MAX_JOINED_WORDS: usize = 6;

/// Candidate spans around `col`, longest first, each joining adjacent
/// non-break runs across their original gaps.
///
/// A space ends a token, so `AUDIT-GUI-BEBAN 121126-1837.md` reads as two
/// tokens and neither of them exists on disk. The joined span is the whole
/// name. Every span still has to survive `classify_in`, which probes the disk,
/// so prose that merely contains spaces never becomes a click target.
fn joined_spans(line: &str, col: usize) -> Vec<&str> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    if col >= chars.len() || is_break(chars[col].1) {
        return Vec::new();
    }
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if is_break(chars[i].1) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !is_break(chars[i].1) {
            i += 1;
        }
        runs.push((start, i - 1));
    }
    let Some(hit) = runs.iter().position(|(a, b)| *a <= col && col <= *b) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    for width in (2..=MAX_JOINED_WORDS).rev() {
        for back in 0..width {
            let forward = width - 1 - back;
            if hit < back || hit + forward >= runs.len() {
                continue;
            }
            let (first, _) = runs[hit - back];
            let (_, last) = runs[hit + forward];
            let start = chars[first].0;
            let end = chars[last].0 + chars[last].1.len_utf8();
            if let Some(span) = line.get(start..end) {
                spans.push(span);
            }
        }
    }
    spans
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
                s = s.get(1..s.len() - 1).unwrap_or(s);
                break;
            }
        }
        // Trailing sentence punctuation.
        s = s.trim_end_matches(['.', ',', ';', ':', '!', '?']);
        // A trailing closer only when nothing opened it (x.py) → x.py,
        // but a URL with a balanced inner "(bar)" keeps its ")".
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            if s.ends_with(close) && !s.contains(open) {
                s = s.get(..s.len() - close.len_utf8()).unwrap_or(s);
            }
        }
        if s == prev {
            return s;
        }
    }
}

/// Classify a token as URL / existing path / nothing.
fn classify_in(raw: &str, roots: &[&Path], home: Option<&Path>) -> Option<ClickTarget> {
    let token = clean_token(raw);
    if token.is_empty() {
        return None;
    }
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // Require something host-shaped after the scheme: "https://" alone
        // (or "https://." from prose) must not launch the browser.
        let scheme_end = lower.find("://").unwrap() + 3;
        let rest = token.get(scheme_end..)?;
        if rest.contains('.') || rest.contains(':') || rest.eq_ignore_ascii_case("localhost") {
            return Some(ClickTarget::Url(token.to_string()));
        }
        return None;
    }
    if lower.starts_with("file://") {
        let path = token.get("file://".len()..)?;
        return resolve_path_in(path, roots, home);
    }
    if lower.starts_with("mailto:") && token.contains('@') {
        return Some(ClickTarget::Url(token.to_string()));
    }
    resolve_path_in(token, roots, home)
}

/// Try the token as a local path: expand `~`, drop any `:line[:col]`
/// reference suffix, resolve relative to each root in turn, and require
/// existence.
fn resolve_path_in(token: &str, roots: &[&Path], home: Option<&Path>) -> Option<ClickTarget> {
    // Line references are not part of the name, and leaving them on blinds
    // the extension check below: `chat.rs:1618` would read as extension
    // `rs:1618` and never get probed.
    let token = strip_line_ref(token);
    // Only forms that plausibly name a file get a disk probe. Bare prose
    // words without a dot have no chance of resolving anyway; bare dotted
    // names ("README.md") DO get probed because clicking exactly the word
    // that names a real file in the root is the point.
    if !names_path(token) {
        return None;
    }
    let expanded = expand_tilde(token, home)?;
    let path = Path::new(&expanded);
    if path.is_absolute() {
        return path.exists().then(|| ClickTarget::File(path.to_path_buf()));
    }
    // Roots are tried in order, so the session's directory wins over the
    // process cwd when both hold a file of the same name (#2846).
    roots.iter().find_map(|root| {
        let candidate = root.join(&expanded);
        candidate.exists().then_some(ClickTarget::File(candidate))
    })
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
