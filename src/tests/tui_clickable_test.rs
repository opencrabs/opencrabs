//! Click-to-open resolution for transcript tokens (#1772).
//!
//! A path or URL rendered in the TUI was decoration only: clicks went to
//! fold/select/copy and nothing launched. These tests pin the pure resolver
//! (injectable cwd/home against tempdirs, never the real home layout) and
//! the wiring that consumes it before click-select.

use std::path::{Path, PathBuf};

use crate::tui::app::clickable::{self, ClickTarget};

/// A temp dir unique per test, holding `files`, used as fake cwd or fake home.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(tag: &str, files: &[&str]) -> Self {
        let dir = std::env::temp_dir().join(format!("oc-click-{tag}-{}", std::process::id()));
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

    fn path_str(&self, rel: &str) -> String {
        self.dir.join(rel).display().to_string()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn resolve(line: &str, col: usize, cwd: &Path, home: Option<&Path>) -> Option<ClickTarget> {
    clickable::target_at_with(line, col, cwd, home)
}

/// Char index of the middle of the first occurrence of `needle` in `hay`.
fn mid_col(hay: &str, needle: &str) -> usize {
    let byte = hay.find(needle).expect("needle present");
    hay[..byte].chars().count() + needle.chars().count() / 2
}

#[test]
fn url_mid_sentence_resolves() {
    let line = "See https://opencrabs.com/docs for more.";
    let col = mid_col(line, "opencrabs");
    let got = resolve(line, col, Path::new("/nonexistent-cwd"), None);
    assert_eq!(
        got,
        Some(ClickTarget::Url("https://opencrabs.com/docs".into()))
    );
}

#[test]
fn markdown_url_parenthesis_suffix_unwraps() {
    // `TagEnd::Link` renders `[text](url)` as `text (url)`.
    let line = "truelens (https://truelens.tech) is it";
    let col = mid_col(line, "truelens.tech");
    let got = resolve(line, col, Path::new("/nonexistent-cwd"), None);
    assert_eq!(got, Some(ClickTarget::Url("https://truelens.tech".into())));
}

#[test]
fn url_trailing_period_trimmed_but_balanced_inner_paren_kept() {
    let line = "open https://x.dev/about. now";
    let col = mid_col(line, "x.dev");
    let got = resolve(line, col, Path::new("/nonexistent-cwd"), None);
    assert_eq!(got, Some(ClickTarget::Url("https://x.dev/about".into())));

    let wiki = "https://en.wikipedia.org/wiki/Foo_(bar)";
    let col = mid_col(wiki, "wiki");
    let got = resolve(wiki, col, Path::new("/nonexistent-cwd"), None);
    assert_eq!(
        got,
        Some(ClickTarget::Url(
            "https://en.wikipedia.org/wiki/Foo_(bar)".into()
        ))
    );
}

#[test]
fn bare_scheme_does_not_launch() {
    for line in ["https://", "http:// ...", "https://."] {
        let col = 3;
        assert_eq!(resolve(line, col, Path::new("/"), None), None, "{line}");
    }
}

#[test]
fn existing_relative_path_resolves_against_cwd() {
    let cwd = Sandbox::new("rel", &["docs/report.md"]);
    let line = "written in docs/report.md, done";
    let col = mid_col(line, "report.md");
    let got = resolve(line, col, &cwd.dir, None);
    assert_eq!(got, Some(ClickTarget::File(cwd.dir.join("docs/report.md"))));
}

#[test]
fn existing_absolute_path_resolves() {
    let sb = Sandbox::new("abs", &["theme-background-audit.md"]);
    let p = sb.path_str("theme-background-audit.md");
    let line = format!("Audit with every file:line receipt: {p}");
    let col = mid_col(&line, "audit.md");
    let got = resolve(&line, col, Path::new("/"), None);
    assert_eq!(got, Some(ClickTarget::File(PathBuf::from(&p))));
}

#[test]
fn trailing_period_on_path_is_trimmed() {
    let sb = Sandbox::new("dot", &["note.md"]);
    let p = sb.path_str("note.md");
    let line = format!("see {p}. Done");
    let col = mid_col(&line, "note.md");
    let got = resolve(&line, col, Path::new("/"), None);
    assert_eq!(got, Some(ClickTarget::File(PathBuf::from(&p))));
}

#[test]
fn tilde_expands_only_with_home() {
    let home = Sandbox::new("home", &["research/note.md"]);
    let line = "read ~/research/note.md now";
    let col = mid_col(line, "note.md");
    let got = resolve(line, col, Path::new("/"), Some(&home.dir));
    assert_eq!(
        got,
        Some(ClickTarget::File(home.dir.join("research/note.md")))
    );
    // No home in the environment: inert, never a phantom launch.
    assert_eq!(resolve(line, col, Path::new("/"), None), None);
}

#[test]
fn file_line_reference_opens_the_file() {
    let cwd = Sandbox::new("lineref", &["chat.rs"]);
    let line = "receipt chat.rs:1618 proves it";
    let col = mid_col(line, ":1618");
    let got = resolve(line, col, &cwd.dir, None);
    assert_eq!(got, Some(ClickTarget::File(cwd.dir.join("chat.rs"))));
}

#[test]
fn nonexistent_path_is_inert() {
    let line = "legacy notes in /definitely/not/here_42.md, old";
    let col = mid_col(line, "here_42.md");
    assert_eq!(resolve(line, col, Path::new("/"), None), None);
}

#[test]
fn prose_words_are_inert() {
    for (line, needle) in [
        ("the audit is done.", "audit"),
        ("e.g. run it", "e.g"),
        ("Meeting at 14:30 soon", "14:30"),
        ("plain", "plain"),
    ] {
        let col = mid_col(line, needle);
        assert_eq!(
            resolve(line, col, Path::new("/"), None),
            None,
            "must not launch: {line:?}"
        );
    }
}

#[test]
fn click_on_whitespace_or_past_eol_is_inert() {
    let line = "x https://a.dev";
    assert_eq!(resolve(line, 1, Path::new("/"), None), None); // on the space
    assert_eq!(resolve(line, line.len() + 4, Path::new("/"), None), None);
}

#[test]
fn unicode_columns_still_hit() {
    // CJK before the token shifts byte offsets; char indexing must not care.
    let line = "日本語See https://x.dev/a now";
    let col = mid_col(line, "x.dev");
    let got = resolve(line, col, Path::new("/"), None);
    assert_eq!(got, Some(ClickTarget::Url("https://x.dev/a".into())));
}

#[test]
fn backtick_wrapped_path_resolves() {
    let sb = Sandbox::new("btick", &["README.md"]);
    let p = sb.path_str("README.md");
    let line = format!("run `{p}` now");
    let col = mid_col(&line, "README.md");
    let got = resolve(&line, col, Path::new("/"), None);
    assert_eq!(got, Some(ClickTarget::File(PathBuf::from(&p))));
}

#[test]
fn file_scheme_and_mailto() {
    let sb = Sandbox::new("fscheme", &["a.md"]);
    let p = sb.path_str("a.md");
    let line = format!("open file://{p} here");
    let col = mid_col(&line, "a.md");
    let got = resolve(&line, col, Path::new("/"), None);
    assert_eq!(got, Some(ClickTarget::File(PathBuf::from(&p))));

    let line = "mail mailto:ada@example.com";
    let col = mid_col(line, "ada@example.com");
    let got = resolve(line, col, Path::new("/"), None);
    assert_eq!(got, Some(ClickTarget::Url("mailto:ada@example.com".into())));
    // A bare scheme with no address is prose.
    let line = "mailto: is a scheme";
    let col = mid_col(line, "mailto:");
    assert_eq!(resolve(line, col, Path::new("/"), None), None);
}

// ── Structural pins ────────────────────────────────────────────────────

/// The click path must consult the resolver BEFORE falling through to
/// click-select, and `handle_mouse_up`'s no-anchor branch must be where it
/// happens.
#[test]
fn mouse_up_resolves_before_click_select() {
    let src = include_str!("../tui/app/input.rs");
    let up = src
        .find("pub(crate) fn handle_mouse_up")
        .expect("handle_mouse_up exists");
    let body_end = src[up..]
        .find("\n    /// ")
        .map(|e| up + e)
        .unwrap_or(src.len());
    let body = &src[up..body_end];
    let resolve_at = body
        .find("try_open_clicked(col, row)")
        .expect("handle_mouse_up calls try_open_clicked");
    let select_at = body
        .find("self.handle_click_select(row)")
        .expect("handle_mouse_up still falls back to click-select");
    assert!(
        resolve_at < select_at,
        "resolver must run before the click-select fallback"
    );
    assert!(
        src.contains("super::clickable::target_at"),
        "try_open_clicked must go through the clickable resolver"
    );
}

/// The module must stay registered and the launcher must keep spawning
/// detached (a blocking `status()` on the draw loop would freeze the TUI).
#[test]
fn clickable_module_registered_and_launcher_detached() {
    let mods = include_str!("../tui/app/mod.rs");
    assert!(
        mods.contains("pub(crate) mod clickable;"),
        "module registered"
    );
    let src = include_str!("../tui/app/clickable.rs");
    assert!(
        src.contains("launcher::open_detached"),
        "clickable must delegate to the detached launcher"
    );
    assert!(
        !src.contains("spawn().map(|_child| ())"),
        "the spawn-and-forget launcher (#2170) must not come back"
    );
    let launcher = include_str!("../tui/launcher.rs");
    assert!(
        launcher.contains("spawn()"),
        "launcher uses spawn, not status"
    );
}

/// The launcher must pick the platform's default-handler command. Pinned per
/// OS so a silent rename here cannot break one platform while the suite still
/// passes on the others. Windows expects `explorer` (same choice as the
/// `logs open` CLI), not `cmd /C start`.
#[test]
fn opener_command_matches_platform() {
    let cmd = crate::tui::launcher::opener_command();
    #[cfg(target_os = "macos")]
    let expected = "open";
    #[cfg(target_os = "linux")]
    let expected = "xdg-open";
    #[cfg(target_os = "windows")]
    let expected = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let expected = "xdg-open";
    assert_eq!(cmd, expected);
    assert!(
        matches!(cmd, "open" | "xdg-open" | "explorer"),
        "unexpected launcher: {cmd}"
    );
}

/// The user-facing docs must mention click-to-open (README table + /help).
#[test]
fn docs_advertise_click_to_open() {
    let readme = include_str!("../../README.md");
    let row = readme
        .lines()
        .find(|l| l.contains("`Mouse click`"))
        .expect("README documents Mouse click row");
    assert!(
        row.contains("open") && row.contains("URL"),
        "README row must describe opening URLs/paths: {row}"
    );
    let help = include_str!("../tui/render/help.rs");
    assert!(
        help.contains("kv(\"Mouse click\""),
        "/help dialog must list the Mouse click binding"
    );
}

/// The session-files launcher must never wait on the viewer (#2172).
/// `.status()` there blocked the event loop until the user closed
/// Preview/Finder, which is what froze the whole TUI on Enter.
#[test]
fn session_files_launcher_never_blocks_the_loop() {
    let src = include_str!("../tui/app/input.rs");
    let start = src
        .find("pub(crate) async fn handle_session_files_key")
        .expect("session-files key handler must exist");
    let body = &src[start..];
    let end = body
        .find("/// Handle keys in projects mode")
        .unwrap_or(body.len());
    let body = &body[..end];
    assert!(
        !body.contains(".status()"),
        "the session-files launcher must be detached, not blocking on .status()"
    );
    assert!(
        body.contains("launcher::open_detached"),
        "session files must go through the detached launcher"
    );
    // The failure branch used to promise a clipboard fallback in a comment
    // and copy nothing (#2172). Pin the behaviour, not the comment.
    assert!(
        body.contains("copy_to_clipboard"),
        "the no-launcher branch must actually copy the path, not just name it"
    );
    assert!(
        !body.contains("Fallback: copy path to clipboard"),
        "the comment promised a fallback the code never ran"
    );
}
