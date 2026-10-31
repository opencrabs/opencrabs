//! The window's own debug trail.
//!
//! gpui logs through the `log` crate and this bin wires no logger, so a crash
//! in here used to leave nothing behind but one zsh line ("illegal hardware
//! instruction") and a guess. Everything the window decides now lands in
//! `~/.opencrabs/logs/opencrabs-gui.log`: keys, their neutral translation, the
//! decision, every ledger fetch with its duration, and any panic with a full
//! backtrace.
//!
//! `OPENCRABS_GUI_DEBUG=1` additionally mirrors the trail to stderr and turns
//! on the per-keystroke detail.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The append-only sink. `None` when the log file could not be opened, which
/// must never stop the window from running: logging is a witness, not a
/// dependency.
static SINK: OnceLock<Mutex<Option<File>>> = OnceLock::new();

fn sink() -> MutexGuard<'static, Option<File>> {
    // A poisoned mutex means a previous write panicked mid-line; take the file
    // back rather than losing the rest of the trail.
    SINK.get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One line of the trail, mirrored to stderr in verbose runs.
pub fn info(msg: &str) {
    let line = format!("{} {msg}", stamp());
    write_raw(&line);
    if verbose() {
        eprintln!("{line}");
    }
}

/// Per-keystroke detail, only with `OPENCRABS_GUI_DEBUG=1`.
pub fn debug(msg: &str) {
    if verbose() {
        info(&format!("[debug] {msg}"));
    }
}

/// An error the user can also see on screen: always mirrored to stderr, so a
/// terminal run and the log file agree.
pub fn error(msg: &str) {
    let line = format!("{} ERROR {msg}", stamp());
    write_raw(&line);
    eprintln!("{line}");
}

/// Where the trail lives.
pub fn path() -> PathBuf {
    opencrabs::config::opencrabs_home()
        .join("logs")
        .join("opencrabs-gui.log")
}

/// Open the log file and install the panic hook. Call once, before the first
/// fetch, so a panic during startup is captured too.
pub fn init() {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = OpenOptions::new().create(true).append(true).open(&p).ok();
    if file.is_none() {
        eprintln!("opencrabs-gui: could not open {}", p.display());
    }
    *sink() = file;

    // A panic that kills a gpui worker can surface as SIGILL rather than a
    // readable message, so the hook writes the payload and a forced backtrace
    // to the file before the process goes anywhere.
    std::panic::set_hook(Box::new(|info| {
        let trace = std::backtrace::Backtrace::force_capture();
        let line = format!("{} PANIC {info}\n{trace}", stamp());
        write_raw(&line);
        eprintln!("{line}");
    }));

    info(&format!(
        "--- opencrabs-gui start, pid {}, log {} ---",
        std::process::id(),
        p.display()
    ));
}

fn write_raw(line: &str) {
    if let Some(f) = sink().as_mut() {
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
}

fn stamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S%.3fZ")
        .to_string()
}

fn verbose() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("OPENCRABS_GUI_DEBUG").is_ok())
}
