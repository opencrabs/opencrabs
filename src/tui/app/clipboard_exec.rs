//! Bounded clipboard commands (#2702).
//!
//! The clipboard tools are cheap when the desktop is healthy and can hang for
//! ever when it is not: `wl-paste` blocks until the clipboard owner answers the
//! selection request, and a compositor that never services it leaves the child
//! waiting. Every call on this path used `Command::output()` or
//! `Command::wait()`, which carry no deadline, and the caller is the TUI's
//! event loop, so one wedged clipboard froze the whole app: no input, no
//! rendering.
//!
//! [`capture`] spawns the child, reads its stdout on a separate thread and
//! polls until a deadline, then kills and reaps. Draining on a thread is not
//! optional: an image is larger than the 64 KiB pipe buffer, so a
//! poll-then-read design would deadlock on a perfectly healthy payload.
//!
//! A timeout degrades to "this backend said nothing", never to an error the
//! reader has to dismiss: a probe that misses its deadline just means no hint
//! this tick, and a read that misses means the next backend is tried.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How often the child is polled. The healthy probe costs 4 to 38 ms, so a
/// 5 ms beat answers well inside every deadline while burning nothing.
const POLL: Duration = Duration::from_millis(5);

/// How long to wait for the reader thread once the child has exited. It has
/// already reached EOF at that point, so this only covers scheduling.
const READ_GRACE: Duration = Duration::from_secs(1);

/// `wl-paste --list-types` costs 22 to 38 ms and `xclip` 4 ms, so the probe
/// budget is generous without being patient: the call runs on focus and tick.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const LINUX_PROBE: Duration = Duration::from_millis(250);

/// An AppleScript probe pays for compiling the script first.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const APPLESCRIPT_PROBE: Duration = Duration::from_secs(1);

/// PowerShell pays for starting the runtime before it does any work.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const POWERSHELL: Duration = Duration::from_secs(5);

/// Reading text is one tool call with no compile step.
pub(crate) const TEXT_READ: Duration = Duration::from_millis(1500);

/// An image read moves more bytes than text but no more work.
pub(crate) const IMAGE_READ: Duration = Duration::from_secs(3);

/// Writing must not outlast the keystroke that asked for it.
pub(crate) const WRITE: Duration = Duration::from_millis(1500);

/// What a clipboard command did before its deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Capture {
    /// The child exited on its own within the budget.
    Finished { success: bool, stdout: Vec<u8> },
    /// The child was still running at the deadline. It has been killed and
    /// reaped by the time this is returned.
    TimedOut,
    /// The program is not on this machine, or the child could not be waited
    /// on. Normal on a box without Wayland or X11.
    Unavailable,
}

/// Run `cmd` with no stdin, collecting stdout, and give up at `timeout`.
pub(crate) fn capture(cmd: Command, timeout: Duration) -> Capture {
    run(cmd, None, timeout)
}

/// Run `cmd` with `bytes` on its stdin and give up at `timeout`. Returns the
/// same shape as [`capture`]: a write backend is trusted when it exits 0.
pub(crate) fn write_bytes(cmd: Command, bytes: Vec<u8>, timeout: Duration) -> Capture {
    run(cmd, Some(bytes), timeout)
}

/// The one implementation both doors share.
fn run(mut cmd: Command, payload: Option<Vec<u8>>, timeout: Duration) -> Capture {
    cmd.stdin(if payload.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::null());

    let Ok(mut child) = cmd.spawn() else {
        return Capture::Unavailable;
    };

    // The pipe is drained on its own thread from the first byte, so the child
    // can never fill the buffer and block while we are polling it.
    let (tx, rx) = std::sync::mpsc::channel();
    match child.stdout.take() {
        Some(mut pipe) => {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
                let _ = tx.send(buf);
            });
        }
        None => {
            let _ = tx.send(Vec::new());
        }
    }

    // The payload is written on its own thread for the same reason: a child
    // that never reads would otherwise block us on the first bufferful.
    let mut writer = payload.map(|bytes| {
        let pipe = child.stdin.take();
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let _ = std::io::Write::write_all(&mut pipe, &bytes);
            }
            // Dropping the handle closes the pipe, which is the EOF every
            // clipboard tool waits for.
        })
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = rx.recv_timeout(READ_GRACE).unwrap_or_default();
                if let Some(writer) = writer.take() {
                    let _ = writer.join();
                }
                return Capture::Finished {
                    success: status.success(),
                    stdout,
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(writer) = writer.take() {
                        // The pipe is gone, so a blocked write returns EPIPE
                        // and the thread ends here.
                        let _ = writer.join();
                    }
                    tracing::debug!("clipboard command missed its deadline, killed");
                    return Capture::TimedOut;
                }
                std::thread::sleep(POLL);
            }
            Err(e) => {
                tracing::debug!("clipboard command could not be waited on: {e}");
                return Capture::Unavailable;
            }
        }
    }
}

/// The probe budget for this platform.
pub(crate) fn probe_timeout() -> Duration {
    #[cfg(target_os = "macos")]
    {
        APPLESCRIPT_PROBE
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        LINUX_PROBE
    }
    #[cfg(windows)]
    {
        POWERSHELL
    }
}

/// Clipboard bytes as text, or `None` when the payload is not text at all.
///
/// `wl-paste` with no `--type` hands back whatever the clipboard offers, and an
/// image-only clipboard comes back as raw PNG bytes with exit 0 (#2702). The
/// read paths now ask for `--type text`, and this gate is the backstop for any
/// backend that ignores the request: binary payloads carry NUL bytes, so they
/// are refused rather than turned into replacement characters and inserted
/// into the composer.
pub(crate) fn text_from_clipboard_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.contains(&0) {
        tracing::debug!("clipboard read: binary payload refused as text");
        return None;
    }
    let text = String::from_utf8_lossy(bytes).to_string();
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    /// A child that never exits must be killed at the deadline, not waited on.
    #[test]
    fn a_child_that_never_exits_is_killed_at_the_deadline() {
        let started = Instant::now();
        let outcome = capture(sh("exec sleep 30"), Duration::from_millis(120));
        let elapsed = started.elapsed();
        assert_eq!(outcome, Capture::TimedOut);
        assert!(
            elapsed < Duration::from_secs(3),
            "the deadline is the only budget that counts, took {elapsed:?}"
        );
    }

    /// 512 KiB is eight times the pipe buffer, so a design that polls before
    /// it reads would hang here instead of returning the payload.
    #[test]
    fn a_payload_larger_than_the_pipe_buffer_does_not_deadlock() {
        let outcome = capture(
            sh("head -c 524288 /dev/zero | tr '\\0' 'a'"),
            Duration::from_secs(5),
        );
        match outcome {
            Capture::Finished { success, stdout } => {
                assert!(success, "the fake backend exits 0");
                assert_eq!(stdout.len(), 524_288, "every byte is collected");
            }
            other => panic!("large stdout must complete, got {other:?}"),
        }
    }

    /// A non-zero exit is reported as a finished, failed run: the caller
    /// decides whether to fall through to the next backend.
    #[test]
    fn a_failing_child_reports_its_status_not_a_timeout() {
        assert_eq!(
            capture(sh("exit 3"), Duration::from_secs(1)),
            Capture::Finished {
                success: false,
                stdout: Vec::new()
            }
        );
    }

    /// A missing program is normal on a box without that clipboard tool.
    #[test]
    fn a_missing_program_is_unavailable() {
        let cmd = Command::new("opencrabs-no-such-clipboard-tool");
        assert_eq!(capture(cmd, Duration::from_secs(1)), Capture::Unavailable);
    }

    /// The write door feeds stdin and still honours the deadline: the payload
    /// has to reach the child for its exit code to mean anything.
    #[test]
    fn a_write_reaches_the_child_and_falls_through_when_it_does_not() {
        assert_eq!(
            write_bytes(
                sh("cat >/dev/null"),
                b"hello".to_vec(),
                Duration::from_secs(1)
            ),
            Capture::Finished {
                success: true,
                stdout: Vec::new()
            }
        );
        assert_eq!(
            write_bytes(
                sh("exec sleep 30"),
                b"hello".to_vec(),
                Duration::from_millis(120)
            ),
            Capture::TimedOut
        );
    }

    /// PNG bytes are not text. Without this gate the lossy conversion returns
    /// replacement characters, the result is non-empty, and the paste inserts
    /// binary into the composer instead of attaching the image (#2702).
    #[test]
    fn image_bytes_are_refused_as_clipboard_text() {
        let png = [
            0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H',
            b'D', b'R',
        ];
        assert_eq!(text_from_clipboard_bytes(&png), None);
    }

    /// Plain text survives the gate, whitespace included.
    #[test]
    fn plain_text_survives_the_gate() {
        assert_eq!(
            text_from_clipboard_bytes(b"  two words  "),
            Some("  two words  ".to_string())
        );
        assert_eq!(text_from_clipboard_bytes(b"   \n"), None);
    }

    /// The text read names its tool per platform, so a box only ever pays for
    /// the backend that exists there.
    #[cfg(unix)]
    #[test]
    fn the_text_backends_name_the_platform_tool_first() {
        let backends = crate::tui::app::input::unix_text_backends();
        let first = backends.first().expect("every unix has a text backend");
        #[cfg(target_os = "macos")]
        assert_eq!(first.0, "pbpaste");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(first.0, "wl-paste");
    }

    /// The Linux text read asks for the text type, which is what stops an
    /// image-only clipboard from being read as bytes (#2702).
    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_text_read_asks_for_the_text_type() {
        let backends = crate::tui::app::input::unix_text_backends();
        let wayland = backends
            .iter()
            .find(|(program, _)| *program == "wl-paste")
            .expect("Wayland comes first on Linux");
        assert!(
            wayland.1.contains(&"--type") && wayland.1.contains(&"text"),
            "wl-paste must be told which type to read, got {:?}",
            wayland.1
        );
    }
}
