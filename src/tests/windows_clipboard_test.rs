//! Windows clipboard support (#1822).
//!
//! Copy and paste in the TUI spoke only pbcopy/xclip/xsel. On Windows every
//! one of those spawns fails, so `copy_to_clipboard` fell through to its
//! `false` tail and the user got a silent no-op with the selection already
//! cleared — nothing to retry, nothing to explain. Paste was worse: the image
//! and text readers were compiled out entirely, so Ctrl+V had no backend at
//! all on the platform most of our users type on.
//!
//! The Windows arms shell out to PowerShell, so they cannot be exercised from
//! a macOS or Linux test run. These sentinels pin the wiring that makes the
//! path real (a backend exists, the Unix arms are gated so they cannot be
//! reached, and the two hazards that would hang or flash a console are
//! handled) so a refactor that quietly drops one fails here instead of on a
//! user's machine.

use std::path::Path;

fn input_src() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tui/app/input.rs");
    let src = std::fs::read_to_string(&path).expect("input.rs readable");
    assert!(src.len() > 500, "fixture guard: {} bytes", src.len());
    src
}

/// Copy must have a real Windows backend, and the Unix trio must be gated
/// behind `not(windows)` so it can never be the arm that runs there.
#[test]
fn copy_to_clipboard_has_windows_backend_and_gated_unix_arms() {
    let src = input_src();

    // Primary: Set-Clipboard via PowerShell, payload riding a UTF-8 temp file
    // so non-ASCII (accents, emoji, CJK) survives an arbitrary console
    // codepage.
    assert!(src.contains("Set-Clipboard"), "PowerShell copy backend");
    assert!(
        src.contains("[Text.Encoding]::UTF8"),
        "copy payload read back as UTF-8, not console codepage"
    );

    // Last resort: clip.exe, which wants NUL-terminated UTF-16LE.
    assert!(src.contains("clip.exe"), "clip.exe fallback backend");
    assert!(
        src.contains("encode_utf16"),
        "clip.exe payload encoded UTF-16LE, not raw UTF-8"
    );

    // The Unix helpers must be unreachable on Windows. Since #2171 they live
    // in one platform-gated backend list, so the gate is the attribute on
    // `clipboard_backends` itself: ungated, they compile on Windows and
    // reintroduce the silent fall-through this issue is about.
    let gate = src
        .find("#[cfg(not(windows))]\npub(crate) fn clipboard_backends")
        .expect("unix backend list gated behind not(windows)");
    let fn_start = src
        .find("pub(crate) fn clipboard_backends")
        .expect("clipboard_backends present");
    let fn_end = src[fn_start..]
        .find("\n}\n")
        .map(|i| fn_start + i)
        .expect("clipboard_backends body ends");
    let body = &src[fn_start..fn_end];
    assert!(gate < fn_start, "the cfg must sit directly on the function");
    for backend in ["\"pbcopy\"", "\"wl-copy\"", "\"xclip\"", "\"xsel\""] {
        assert!(
            body.contains(backend),
            "{backend} must live inside the gated backend list, not somewhere ungated"
        );
    }
}

/// Both paste readers must have a Windows arm. Before #1822 the fallback was
/// a bare `None` on this platform, so Ctrl+V silently did nothing.
#[test]
fn clipboard_paste_has_windows_arms() {
    let src = input_src();

    // Images: the clipboard holds an HBITMAP, so PowerShell has to marshal
    // GetImage() and encode PNG to a scratch file. -STA is mandatory because
    // PowerShell 7 defaults to MTA and OLE clipboard calls throw there.
    assert!(src.contains("GetImage()"), "image paste backend");
    assert!(src.contains("\"-STA\""), "image paste runs single-threaded");

    // Text: Get-Clipboard, routed through a file for the same codepage
    // reason as copy.
    assert!(src.contains("Get-Clipboard"), "text paste backend");
}

/// The two ways a shell-out clipboard kills the TUI: a console window
/// flashing on every copy, and a pipe held open past the child's EOF read.
#[test]
fn windows_shellouts_are_windowless_and_never_hold_the_pipe() {
    let src = input_src();

    // CREATE_NO_WINDOW on every PowerShell spawn.
    let spawns = src.matches("powershell.exe").count();
    let windowless = src.matches("creation_flags(0x0800_0000)").count();
    assert!(spawns >= 3, "expected 3 Windows shell-outs, found {spawns}");
    assert_eq!(
        windowless, spawns,
        "every PowerShell spawn must set CREATE_NO_WINDOW"
    );

    // clip.exe reads stdin to EOF; the child handle keeps the pipe open, so
    // stdin must be dropped before waiting or the last-resort path hangs.
    assert!(
        src.contains("drop(child.stdin.take())"),
        "clip.exe stdin closed before wait"
    );
}

/// A profile path containing an apostrophe (`C:\Users\O'Brien\...`) must not
/// break the generated PowerShell line: single-quoted PS strings escape `'`
/// by doubling it.
#[test]
fn temp_paths_are_escaped_for_powershell_quoting() {
    let src = input_src();
    let escapes = src.matches("replace('\\'', \"''\")").count();
    assert!(
        escapes >= 3,
        "every interpolated temp path must double single quotes, found {escapes}"
    );
}
