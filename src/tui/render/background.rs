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
