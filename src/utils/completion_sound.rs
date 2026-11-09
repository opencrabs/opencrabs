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
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::tui::plan::{PlanDocument, PlanStatus, TaskStatus};

/// How long the rail's plan bar stays lit after a task settles.
///
/// Long enough to catch the eye from across the desk, short enough that a
/// row which finished while the operator was reading something else stops
/// shouting instead of blinking for minutes.
pub const FLASH_MS: u64 = 6_000;

/// The flash window itself, so callers add it once instead of rebuilding it.
pub const FLASH_WINDOW: Duration = Duration::from_millis(FLASH_MS);

/// How long each half of the blink lasts: six seconds is roughly thirteen
/// on/off pairs, which reads as a pulse rather than a strobe.
pub const FLASH_BLINK_MS: u64 = 450;

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

/// Whether a flash window is still open at `now`.
///
/// Pure over the two instants, so the paint path never owns a timer: the rail
/// stores the deadline once per session and asks this on every repaint.
pub fn flash_lit(until: Option<Instant>, now: Instant) -> bool {
    until.is_some_and(|end| now < end)
}

/// Whether the flash is in its lit half at `now`, which is what decides the
/// colour: orange while lit, the normal accent while dark. `false` outside
/// the window, so the caller's paint collapses to the resting look.
///
/// The phase is measured from the START of the window (the deadline minus
/// [`FLASH_WINDOW`]) using saturating arithmetic, so a clock that moved
/// backwards dims the bar instead of panicking on the paint path.
pub fn flash_blink_on(until: Option<Instant>, now: Instant) -> bool {
    let Some(end) = until else {
        return false;
    };
    if now >= end {
        return false;
    }
    let elapsed = FLASH_WINDOW.saturating_sub(end.saturating_duration_since(now));
    (elapsed.as_millis() / u128::from(FLASH_BLINK_MS)).is_multiple_of(2)
}

/// How many tasks in this ONE plan settled that `seen` had not recorded yet.
///
/// Same dedupe as [`newly_resolved`], but reported per plan so the caller can
/// say WHICH session's task finished. The rail flashes that row's plan bar,
/// and the chime rings once for the batch: one event, two surfaces.
pub fn newly_resolved_in(seen: &mut HashSet<Uuid>, plan: Option<&PlanDocument>) -> usize {
    resolved_task_ids(plan)
        .into_iter()
        .filter(|id| seen.insert(*id))
        .count()
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
    plans
        .iter()
        .fold(0, |fresh, plan| fresh + newly_resolved_in(seen, *plan))
}
