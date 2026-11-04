//! Pure half of the WhatsApp live tool block (#2023).
//!
//! Folds the agent's progress events into an ordered list of steps and renders
//! that list as one WhatsApp text body. Nothing here touches the client, so
//! every choice the user sees is covered by a plain unit test. The network
//! half lives in `flow_message.rs`.

use std::time::Duration;

use crate::brain::agent::ProgressEvent;

/// Steps shown in the block. Older steps fold into one "earlier steps" line so
/// a long turn never outgrows a single WhatsApp message.
pub(crate) const MAX_VISIBLE_STEPS: usize = 12;

/// Character ceiling for one rendered step line.
const MAX_LINE_CHARS: usize = 140;

/// Character ceiling for the narration preview.
const MAX_NOTE_CHARS: usize = 160;

/// One entry in the block, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// A tool call. `status` is `None` while running, then `Some(success)`.
    Tool {
        name: String,
        context: String,
        status: Option<bool>,
        /// Set for a detached background task row (#2145, #2150): the id the
        /// finish event matches on, so a finish settles its own row even when
        /// two tasks share a label. `None` for an ordinary tool row.
        task_id: Option<String>,
    },
    /// Narration the agent wrote between tool calls.
    Note(String),
}

/// Where the turn stands. The block shows the matching header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Running,
    Finished,
    Failed,
}

/// Apply one progress event to the step list. Returns true when the visible
/// block changed, so the caller knows whether a re-render is worth an edit.
pub(crate) fn fold(steps: &mut Vec<Step>, event: &ProgressEvent) -> bool {
    match event {
        ProgressEvent::ToolStarted {
            tool_name,
            tool_input,
            ..
        } => {
            steps.push(Step::Tool {
                name: tool_name.clone(),
                // A bash command can carry a token in its arguments, so the
                // hint is scrubbed like narration before it is shown.
                context: crate::utils::sanitize::redact_secrets(&crate::utils::tool_context_hint(
                    tool_name, tool_input,
                )),
                status: None,
                task_id: None,
            });
            true
        }
        ProgressEvent::ToolCompleted {
            tool_name, success, ..
        } => {
            // Close the most recent running call with this name. A completion
            // with no matching start still shows up, so no result is lost.
            let open = steps.iter_mut().rev().find_map(|step| match step {
                Step::Tool { name, status, .. }
                    if status.is_none() && name.as_str() == tool_name.as_str() =>
                {
                    Some(status)
                }
                _ => None,
            });
            match open {
                Some(status) => *status = Some(*success),
                None => steps.push(Step::Tool {
                    name: tool_name.clone(),
                    context: String::new(),
                    status: Some(*success),
                    task_id: None,
                }),
            }
            true
        }
        ProgressEvent::DetachedTaskStarted { task_id, label } => {
            // A detached task is a tool row, so it folds into the same block
            // as everything else (#2150). Its label is empty context: the row
            // names itself through `detached_row_label`.
            steps.push(Step::Tool {
                name: label.clone(),
                context: String::new(),
                status: None,
                task_id: Some(task_id.clone()),
            });
            true
        }
        ProgressEvent::DetachedTaskFinished {
            task_id, success, ..
        } => {
            // Match by task id, never by label: two detached tasks can both
            // be `cargo test`, and a label match would settle whichever row it
            // hit first, leaving a finished task showing as running.
            let row = steps.iter_mut().rev().find(|step| {
                matches!(
                    step,
                    Step::Tool { task_id: Some(id), .. } if id == task_id
                )
            });
            match row {
                Some(Step::Tool { status, .. }) => {
                    *status = Some(*success);
                    true
                }
                // Nothing to update: the start never reached this block.
                _ => false,
            }
        }
        ProgressEvent::IntermediateText { text, .. } => {
            let clean = clean_text(text);
            if clean.is_empty() {
                return false;
            }
            steps.push(Step::Note(clean));
            true
        }
        _ => false,
    }
}

/// Whether a block posted this long ago can still be edited.
///
/// WhatsApp refuses an edit older than 15 minutes, so the block checks the
/// clock itself. A closed window then costs one fresh post instead of a round
/// trip the server is guaranteed to reject (#2150).
pub(crate) fn within_edit_window(age: Duration) -> bool {
    age < super::outbox::EDIT_WINDOW
}

/// What one flush of the live block does (#2150).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlushAction {
    /// Nothing visible changed, so send nothing.
    Skip,
    /// Edit the tracked message in place.
    Edit,
    /// Post a new message: the first row of the turn, or the single final row
    /// after the edit window closed.
    Post,
}

/// The edit-window state of one live block, with no network in it (#2150).
///
/// WhatsApp can only edit a message for 15 minutes. A detached task outlives
/// its turn, so a build that finishes late lands outside that window: the
/// block must stop trying to edit, stop posting running updates, and settle
/// with exactly one fresh row that does not look live.
///
/// `Block::flush` delegates every branch here, so that fallback is provable
/// without a bot: a test drives a whole sequence of ticks through this state
/// and counts the posts. A message id and a clock cannot be asserted against.
#[derive(Debug, Default)]
pub(crate) struct WindowPlan {
    /// A posted message exists and is the target of future edits.
    has_message: bool,
    /// Editing is over for this block: the window closed or an edit was
    /// rejected. Running updates stop; the settled render still posts once.
    expired: bool,
    /// Text the posted message currently shows.
    last_text: Option<String>,
}

impl WindowPlan {
    /// Decide what this flush does. `settled` is the turn's last render,
    /// `window_open` says whether the posted message is still young enough to
    /// edit. The only state change made here is the expiry itself, because
    /// that decision must be visible to the very next tick.
    pub(crate) fn decide(&mut self, text: &str, settled: bool, window_open: bool) -> FlushAction {
        // Once editing is over, running updates stop. Posting one every tick
        // would restart the clock and split the row this issue is about.
        if self.expired && !settled {
            return FlushAction::Skip;
        }
        if self.last_text.as_deref() == Some(text) {
            return FlushAction::Skip;
        }
        if self.has_message && !self.expired && !window_open {
            self.expired = true;
            if !settled {
                return FlushAction::Skip;
            }
        }
        if self.has_message && !self.expired {
            FlushAction::Edit
        } else {
            FlushAction::Post
        }
    }

    /// Record that a message now shows `text`: either an edit landed or a new
    /// row was posted. Posting restarts the edit clock, so expiry clears.
    pub(crate) fn landed(&mut self, text: &str) {
        self.has_message = true;
        self.expired = false;
        self.last_text = Some(text.to_string());
    }

    /// Record a refused edit. The message exists but can no longer be
    /// changed, so only the settled render may post again.
    pub(crate) fn edit_refused(&mut self) {
        self.expired = true;
    }

    /// Record a refused post. No message exists, so the next flush tries
    /// again; running updates stay off until then.
    pub(crate) fn post_refused(&mut self) {
        self.has_message = false;
        self.expired = true;
    }
}

/// The tool whose `SuggestedOptions` surface halts the turn.
pub(crate) const OPTIONS_TOOL: &str = "suggest_options";

/// Remove the answer a surface halted on from the block, and return it.
///
/// The answer is the run of narration directly before the options tool step.
/// Any other tool step in between means that narration belonged to an earlier
/// step, so it stays in the block (#2030).
pub(crate) fn take_trailing_answer(steps: &mut Vec<Step>) -> Option<String> {
    let end = steps
        .iter()
        .rposition(|s| matches!(s, Step::Tool { name, .. } if name == OPTIONS_TOOL))
        .unwrap_or(steps.len());
    let start = steps[..end]
        .iter()
        .rposition(|s| !matches!(s, Step::Note(_)))
        .map_or(0, |i| i + 1);
    let notes: Vec<String> = steps[start..end]
        .iter()
        .filter_map(|s| match s {
            Step::Note(text) => Some(text.clone()),
            Step::Tool { .. } => None,
        })
        .collect();
    if notes.is_empty() {
        return None;
    }
    steps.drain(start..end);
    Some(notes.join("\n\n"))
}

/// Drop the paragraphs of `content` that were already posted as the held
/// answer, so a sign-off that repeats it does not post it twice (#2030).
pub(crate) fn drop_held(content: &str, held: &[String]) -> String {
    let held: Vec<String> = held
        .iter()
        .flat_map(|h| h.split("\n\n").map(|p| p.trim().to_string()))
        .filter(|p| !p.is_empty())
        .collect();
    content
        .split("\n\n")
        .filter(|p| !held.iter().any(|h| h == p.trim()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Render the block as one WhatsApp text body.
pub(crate) fn render(steps: &[Step], phase: Phase, elapsed: Duration) -> String {
    let tools = steps
        .iter()
        .filter(|step| matches!(step, Step::Tool { .. }))
        .count();
    let calls = if tools == 1 {
        "1 tool call".to_string()
    } else {
        format!("{tools} tool calls")
    };
    let clock = fmt_elapsed(elapsed);
    let header = match phase {
        Phase::Running => format!("⚙️ *Working* · {calls} · ⏱ {clock}"),
        Phase::Finished => format!("✅ *Finished* · {calls} · ⏱ {clock}"),
        Phase::Failed => format!("❌ *Failed* · {calls} · ⏱ {clock}"),
    };

    let mut lines = vec![header];
    let hidden = steps.len().saturating_sub(MAX_VISIBLE_STEPS);
    if hidden > 0 {
        lines.push(format!("… {hidden} earlier steps"));
    }
    lines.extend(steps[hidden..].iter().map(render_step));
    lines.join("\n")
}

fn render_step(step: &Step) -> String {
    match step {
        Step::Tool {
            name,
            context,
            status,
            task_id,
        } => {
            let icon = match status {
                None => "⚙️",
                Some(true) => "✅",
                Some(false) => "❌",
            };
            // A detached task row names itself; an ordinary tool row keeps
            // its tool name untouched (#2150).
            let name = crate::channels::background_work::detached_row_label(
                name,
                task_id.as_deref(),
                *status,
            );
            let label = if context.is_empty() {
                name
            } else {
                format!("{name}: {context}")
            };
            format!("{icon} {}", clip(&label, MAX_LINE_CHARS))
        }
        Step::Note(text) => {
            let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("💬 {}", clip(&one_line, MAX_NOTE_CHARS))
        }
    }
}

/// Same scrubbing the streamed answer gets, so a tool's arguments or the
/// agent's narration cannot leak a secret into a group chat.
fn clean_text(text: &str) -> String {
    let (clean, _) = crate::utils::extract_img_markers(text);
    let clean = crate::utils::sanitize::redact_secrets(&clean);
    crate::utils::sanitize::strip_llm_artifacts(&clean)
        .trim()
        .to_string()
}

/// Cut to `max` characters, marking the cut. Counts characters, not bytes, so
/// accented or CJK text is never split mid-character.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn fmt_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}
