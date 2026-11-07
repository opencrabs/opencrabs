//! Plan-completion notification sound (#2255).
//!
//! Two pieces, deliberately separate:
//!
//! - [`announcement_pending`] decides whether a plan just finished. It is a
//!   pure function over the plan document, so the never-fire cases (Editing,
//!   partly done, failed, discarded) are unit-testable with no audio device.
//! - [`play_completion_sound`] plays the embedded notification chime once, on
//!   its own thread. Sound is a surface concern: the settle hook emits
//!   `ProgressEvent::PlanCompleted` for every surface, and the TUI and the GUI
//!   each turn that fact into audio behind the same opt-in (#2691).

use std::collections::HashSet;

use uuid::Uuid;

use crate::tui::plan::{PlanDocument, PlanStatus, TaskStatus};

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
/// carries on. The TUI and the GUI call this, and only when
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

/// Task ids that have settled (Completed or Skipped) in `plan`.
///
/// The per-TASK chime needs a set, not a count: a plan reload can land twice
/// for one completion (the tool rewrites the file, the pane re-reads it), and
/// a count would chime on every read. A set makes "already announced" a fact
/// about which task, not how many.
pub fn resolved_task_ids(plan: Option<&PlanDocument>) -> HashSet<Uuid> {
    let Some(plan) = plan else {
        return HashSet::new();
    };
    plan.tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
        .map(|t| t.id)
        .collect()
}

/// How many tasks in `plans` settled that `seen` had not recorded yet, adding
/// them to `seen` as it goes (#2574).
///
/// Pure on purpose: the TUI calls it from its tick with the focused plan plus
/// every pane plan, and the chime decision is testable without an audio
/// device. Freshly loaded plans whose tasks were already done when the plan
/// first appeared count as new once, which is the honest reading: the operator
/// never heard about them.
pub fn newly_resolved(seen: &mut HashSet<Uuid>, plans: &[Option<&PlanDocument>]) -> usize {
    let mut fresh = 0;
    for plan in plans {
        for id in resolved_task_ids(*plan) {
            if seen.insert(id) {
                fresh += 1;
            }
        }
    }
    fresh
}
