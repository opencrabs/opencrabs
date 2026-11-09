//! Footer indicator for detached background commands (#762).
//!
//! A genuinely long command (`cargo test`, a build) runs detached so it does
//! not churn toward the bash 600s cap, and the turn ends immediately. That
//! left the TUI with nothing to draw: the agent says it is waiting, the
//! spinner is gone because no turn is active, and a five-minute build is
//! indistinguishable from a hang.
//!
//! Pure formatting, so the shape is testable without a terminal or a live
//! task. Elapsed seconds are passed in rather than read from a clock here.
//!
//! The GUI reuses [`format_background_tasks`] for its own running strip: one
//! implementation means the two surfaces cannot drift on what a running task
//! looks like.

/// How much of a command label the indicator shows.
///
/// The stored label allows 60 characters, which is fine for a log line and far
/// too long for a border title: a full `cargo test --locked --profile ci --lib
/// 2>&1 | tail -40` crowded out everything beside it. The leading words are
/// what identify the command, so the tail is what gets dropped.
pub const LABEL_CHARS: usize = 28;

/// The running-task text for ONE session, shaped for a border title (#2576).
///
/// Decorating it (`⏳`, padding, colour) is the caller's job: the focused
/// composer and the read-only pane composer style the same fact differently,
/// and one helper that baked in the glyph made the second caller show it
/// twice.
///
/// Per-pane split view needs the roster scoped to the pane's own session: the
/// same manager holds every session's detached commands, so formatting the
/// whole roster would put a neighbouring pane's build on this pane's border.
/// `None` when that session has nothing running, which is what makes the row
/// disappear instead of showing an empty slot.
pub fn pane_background_title(
    manager: &crate::brain::agent::service::background_tasks::BackgroundTaskManager,
    session_id: uuid::Uuid,
) -> Option<String> {
    let tasks: Vec<(String, u64)> = manager
        .running_tasks(session_id)
        .into_iter()
        .map(|t| (t.label, t.started.elapsed().as_secs()))
        .collect();
    format_background_tasks(&tasks, LABEL_CHARS)
}

/// `cargo test 32s`, or `cargo test 32s +2` when other tasks are also running.
///
/// The oldest task is the one named: it is the one the user has been waiting
/// on longest, and it is the one whose elapsed time answers "is this stuck".
/// `None` when nothing is running, so the caller omits the field entirely
/// rather than showing an empty slot.
///
/// `max_label` truncates the command, never the elapsed time or the overflow
/// count: those are short, fixed-width, and the whole point of the indicator.
pub fn format_background_tasks(tasks: &[(String, u64)], max_label: usize) -> Option<String> {
    let (label, secs) = tasks.first()?;
    let mut out = truncate_label(label, max_label);
    out.push(' ');
    out.push_str(&humanize_elapsed(*secs));
    if tasks.len() > 1 {
        out.push_str(&format!(" +{}", tasks.len() - 1));
    }
    Some(out)
}

/// Cut `label` to `max` characters, marking the cut. Counts chars, not bytes,
/// so a command carrying non-ASCII does not split mid-character.
fn truncate_label(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_string();
    }
    let kept: String = label.chars().take(max).collect();
    format!("{kept}…")
}

/// `32s` / `4m 20s` / `1h 5m`, matching the turn header's duration style so the
/// two read as the same kind of number.
fn humanize_elapsed(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => {
            let (m, r) = (s / 60, s % 60);
            if r == 0 {
                format!("{m}m")
            } else {
                format!("{m}m {r}s")
            }
        }
        s => {
            let (h, m) = (s / 3600, (s % 3600) / 60);
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h {m}m")
            }
        }
    }
}

/// The compact `/sessions` status badge for a session's detached commands, or
/// `None` when nothing is running.
///
/// The session list is exactly one terminal row per conversation, so this
/// cannot carry the command label the composer strip shows: it answers the
/// only question the list needs, "is work still going here, and for how
/// long". The oldest task names the duration because it is the one the
/// operator has been waiting on longest.
pub fn row_background_badge(tasks: &[(String, u64)]) -> Option<String> {
    let (_, secs) = tasks.first()?;
    let mut out = format!("\u{23f3} {}", compact_elapsed(*secs));
    if tasks.len() > 1 {
        out.push_str(&format!(" +{}", tasks.len() - 1));
    }
    Some(out)
}

/// Seconds as one short unit: `45s`, `16m`, `2h`.
///
/// Deliberately coarser than [`humanize_elapsed`]: the status column is
/// capped at eighteen cells and already shares them with the spinner, the
/// pending-approval mark and the pane index.
fn compact_elapsed(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}
