//! Plan-completion notification sound (#2255).
//!
//! Two pieces, deliberately separate:
//!
//! - [`announcement_pending`] decides whether a plan just finished. It is a
//!   pure function over the plan document, so the never-fire cases (Editing,
//!   partly done, failed, discarded) are unit-testable with no audio device.
//! - [`play_completion_sound`] plays the embedded notification chime once, on
//!   its own thread. Sound is a TUI surface concern: the settle hook emits
//!   `ProgressEvent::PlanCompleted` for every surface, and only the TUI turns
//!   that fact into audio.

use crate::tui::plan::{PlanDocument, PlanStatus};

/// The only asset this module plays: the notification chime the owner picked.
#[cfg(feature = "notification-sound")]
const COMPLETION_WAV: &[u8] = include_bytes!("../assets/sound/notification.wav");

/// Report the resolved-task count when `plan` just finished, else None.
///
/// Mirrors the settle hook's archive condition: an Active plan whose every
/// task is Completed or Skipped. Editing plans never announce (their
/// checklist is not executing yet), and a partly done or failed plan stays
/// silent until it actually closes.
pub fn announcement_pending(plan: Option<&PlanDocument>) -> Option<usize> {
    let plan = plan?;
    if plan.status != PlanStatus::Active || !plan.is_complete() {
        return None;
    }
    Some(plan.tasks.len())
}

/// Play the completion sound once, detached from the caller.
///
/// Never blocks the turn and never panics: a machine with no output device
/// (a server, a container, a Mac with no audio route) logs a warning and
/// carries on. Only the TUI calls this, and only when
/// `[agent] completion_notification_enabled = true`.
#[cfg(feature = "notification-sound")]
pub fn play_completion_sound() {
    std::thread::spawn(|| {
        if let Err(e) = play_blocking() {
            tracing::warn!("Completion sound skipped: {e}");
        }
    });
}

#[cfg(feature = "notification-sound")]
fn play_blocking() -> Result<(), String> {
    use rodio::{Decoder, OutputStream, Sink};
    use std::io::Cursor;

    let (_stream, handle) =
        OutputStream::try_default().map_err(|e| format!("no audio output device: {e}"))?;
    let sink = Sink::try_new(&handle).map_err(|e| format!("no audio sink: {e}"))?;
    let source = Decoder::new(Cursor::new(COMPLETION_WAV))
        .map_err(|e| format!("embedded wav did not decode: {e}"))?;
    sink.append(source);
    sink.sleep_until_end();
    Ok(())
}
