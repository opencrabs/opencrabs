//! Click-to-open targets in the terminal transcript (#1772).
//!
//! The TUI renders file paths and URLs as styled text, but a click on one did
//! nothing: plain clicks select messages, and the only launcher lived behind
//! the session-files overlay. Resolving the token under the cursor now lives
//! in `ui_core::links`, because the window needs the same answer about the
//! same rendered text and a `pub(crate)` terminal helper is invisible to it
//! (#2317). What stays here is the terminal half: the launcher, and the
//! failure event that corrects an optimistic "Opened …" line.

use std::path::Path;

use crate::ui_core::links::ClickTarget;

/// Resolve the openable token at `col`, a character index into the rendered
/// line, against the process cwd and the user's home directory.
pub(crate) fn target_at(line: &str, col: usize) -> Option<ClickTarget> {
    crate::ui_core::links::target_at(line, col)
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
