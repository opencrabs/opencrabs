//! Detached process launcher for TUI side effects (#2170, #2172).
//!
//! Two things must both be true when the TUI hands a path or a URL to the
//! operating system:
//!
//! - the draw loop must not block, because a browser or a file manager can
//!   take seconds to come up, and
//! - a launcher that fails *after* spawning must not be reported as a
//!   success. `spawn()` only says the binary existed; the exit status is the
//!   real answer, and the old code threw it away.
//!
//! `spawn_and_reap` spawns with every stdio handle nulled, hands the child to
//! a background thread and reports a non-zero exit through a callback. The
//! caller keeps its optimistic "Opened …" line, and a failure corrects it
//! asynchronously instead of leaving a lie on screen.
//!
//! This module deliberately knows nothing about the TUI: callers pass a
//! closure that captures their event sender.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

/// The system default-handler command for this platform, as a pure fn so a
/// test can pin it. Windows uses `explorer` rather than `cmd /C start` to
/// match the `logs open` CLI (`src/cli/commands.rs`) and to avoid flashing a
/// console window.
pub(crate) fn opener_command() -> &'static str {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let cmd = "xdg-open";
    cmd
}

/// Spawn `program` detached and reap it on a background thread.
///
/// Returns `Err` only when the spawn itself failed, which is the one failure
/// the caller can report synchronously. A non-zero exit, or a `wait` error,
/// arrives later through `on_failure`.
///
/// Windows caveat: `explorer` exits 1 even when it succeeded, so the exit
/// status carries no signal there. The child is still reaped (no zombie) but
/// the callback is compiled out, and only a spawn failure is ever reported.
/// That is strictly better than the previous behaviour, which reported
/// nothing at all, and it avoids a permanent false "Could not open" toast.
pub(crate) fn spawn_and_reap<F>(program: &str, args: &[&OsStr], on_failure: F) -> io::Result<()>
where
    F: FnOnce(String) + Send + 'static,
{
    let mut cmd = Command::new(program);
    // stderr is piped, not nulled: the opener's own message is the useful
    // part ("no \"view\" rule for type text/markdown"), and the old code sent
    // it to /dev/null, which is why a dead launch looked identical to a live
    // one (#2170).
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    // Owned so the reaper thread can name the program in its message: the
    // `&str` argument does not live long enough to cross the thread boundary.
    let program = program.to_string();

    std::thread::spawn(move || {
        use std::io::Read;
        // Drain stderr to EOF before waiting. Reading it here is what keeps a
        // chatty launcher from filling the pipe and blocking on write; the
        // read returns as soon as the child closes the handle, so `wait`
        // never races a full buffer.
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        let status = child.wait();
        #[cfg(windows)]
        {
            // `explorer` exits 1 on success: no exit status is trustworthy
            // here, so nothing is reported. Keep the callback consumed so the
            // signature stays identical across platforms.
            let _ = status;
            let _ = on_failure;
            let _ = program;
        }
        #[cfg(not(windows))]
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let code = status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string());
                on_failure(describe_failure(&program, &code, &stderr));
            }
            Err(err) => on_failure(format!("{program} failed: {err}")),
        }
    });

    Ok(())
}

/// Build the failure detail: the exit code, plus the launcher's own stderr
/// when it said anything. Capped and single-lined so a runaway child cannot
/// paste a wall of text into a one-line notification.
#[cfg_attr(windows, allow(dead_code))]
fn describe_failure(program: &str, code: &str, stderr: &str) -> String {
    const MAX_DETAIL: usize = 200;
    let said = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if said.is_empty() {
        return format!("{program} exited {code}");
    }
    let mut detail = said;
    if detail.chars().count() > MAX_DETAIL {
        detail = detail.chars().take(MAX_DETAIL).collect::<String>() + "…";
    }
    format!("{program} exited {code}: {detail}")
}

/// Open `target` with the platform's default handler, detached.
///
/// Thin wrapper over [`spawn_and_reap`] that picks [`opener_command`].
pub(crate) fn open_detached<F>(target: &Path, on_failure: F) -> io::Result<()>
where
    F: FnOnce(String) + Send + 'static,
{
    spawn_and_reap(opener_command(), &[target.as_os_str()], on_failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A launcher that exits non-zero after spawning must report it. Uses a
    /// shell as the fake launcher so no test depends on a real `xdg-open`,
    /// `open` or `explorer` being installed.
    #[cfg(unix)]
    #[test]
    fn non_zero_exit_reports_failure() {
        let (tx, rx) = mpsc::channel();
        spawn_and_reap(
            "sh",
            &[OsStr::new("-c"), OsStr::new("exit 3")],
            move |detail| {
                let _ = tx.send(detail);
            },
        )
        .expect("spawn sh");
        let detail = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("failure callback must fire for a non-zero exit");
        assert!(
            detail.contains("exited 3"),
            "detail must name the exit code, got: {detail}"
        );
    }

    /// A clean exit must never call the callback, or every successful open
    /// would be followed by a bogus failure notification.
    #[cfg(unix)]
    #[test]
    fn success_never_calls_back() {
        let (tx, rx) = mpsc::channel::<String>();
        spawn_and_reap(
            "sh",
            &[OsStr::new("-c"), OsStr::new("true")],
            move |detail| {
                let _ = tx.send(detail);
            },
        )
        .expect("spawn sh");
        // Disconnected (thread finished without calling back) or timed out
        // (thread still running) both mean no failure was reported.
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "a successful launch must not report a failure"
        );
    }

    /// The event the TUI actually receives: a non-zero child must surface as
    /// `TuiEvent::LauncherFailed` carrying the target and the detail, which is
    /// what turns the optimistic "Opened ..." line into a correction (#2170).
    #[cfg(unix)]
    #[test]
    fn non_zero_exit_surfaces_as_launcher_failed_event() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_and_reap(
            "sh",
            &[OsStr::new("-c"), OsStr::new("exit 3")],
            move |detail| {
                let _ = tx.send(crate::tui::events::TuiEvent::LauncherFailed {
                    target: "notes.md".to_string(),
                    detail,
                });
            },
        )
        .expect("spawn sh");
        let event = rx.blocking_recv().expect("an event must arrive");
        match event {
            crate::tui::events::TuiEvent::LauncherFailed { target, detail } => {
                assert_eq!(target, "notes.md");
                assert!(
                    detail.contains("exited 3"),
                    "detail must name the exit code, got: {detail}"
                );
            }
            _ => panic!("a failed launcher must surface as LauncherFailed"),
        }
    }

    /// The launcher's own error text must reach the user. This is the exact
    /// shape of the #2170 report: `xdg-open` exits 0-or-nonzero while saying
    /// "no \"view\" rule for type text/markdown" on stderr, and the old code
    /// sent that line to /dev/null.
    #[cfg(unix)]
    #[test]
    fn failure_detail_carries_the_launchers_own_stderr() {
        let (tx, rx) = mpsc::channel();
        spawn_and_reap(
            "sh",
            &[
                OsStr::new("-c"),
                OsStr::new("echo 'no view rule for text/markdown' >&2; exit 4"),
            ],
            move |detail| {
                let _ = tx.send(detail);
            },
        )
        .expect("spawn sh");
        let detail = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("failure callback must fire");
        assert!(
            detail.contains("exited 4"),
            "detail must name the exit code, got: {detail}"
        );
        assert!(
            detail.contains("no view rule for text/markdown"),
            "detail must carry the launcher's stderr, got: {detail}"
        );
    }

    /// A silent failure still reports, and multi-line stderr is flattened so
    /// the notification stays one line.
    #[test]
    fn failure_detail_is_single_line_and_capped() {
        assert_eq!(describe_failure("xdg-open", "3", ""), "xdg-open exited 3");
        let multi = describe_failure("xdg-open", "3", "first\n\n  second  \n");
        assert_eq!(multi, "xdg-open exited 3: first second");
        assert!(!multi.contains('\n'), "detail must not carry newlines");
        let long = describe_failure("xdg-open", "3", &"x".repeat(500));
        assert!(
            long.chars().count() <= "xdg-open exited 3: ".chars().count() + 201,
            "detail must be capped, got {} chars",
            long.chars().count()
        );
    }

    /// A missing launcher is the synchronous error path the callers already
    /// handle by falling back to the clipboard.
    #[test]
    fn missing_program_is_a_spawn_error() {
        let err = spawn_and_reap("opencrabs-no-such-launcher-xyz", &[], |_| {})
            .expect_err("a missing program must fail at spawn");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
