//! Folding one progress event into a live card, and the card's
//! single line.
//!
//! Moved out of the crate root as pure code motion: the functions, their
//! comments and their tests are unchanged, only the module boundary is new.

use super::*;
use opencrabs::channels::voice::spoken;

/// The card's single line for a turn running with no pane of its own (#2718).
///
/// The pane paints the lead in the accent colour and the tail muted, and a
/// card line has one colour to spend, so the halves are joined with the same
/// separator the terminal uses. Empty means the turn has said nothing yet: the
/// card then keeps its preview instead of blanking a line to show a blank.
///
/// The prose leads and the call follows (#2816). Narration is occasional, not
/// constant: most turns run from call to call and never speak, and on those
/// the call is still the whole line. When a turn does speak, the paragraph is
/// what it concluded and the call is only what it ran, so a card that printed
/// the call alone hid the sentence the chat above it was showing.
pub(crate) fn rail_activity_line(live: &transcript::LiveTurn) -> String {
    // The paragraph the turn last spoke, clipped by the pane's own rule so the
    // two surfaces never show a different slice of the same sentence.
    let narration = live
        .narration()
        .last()
        // A spoken block that arrived as intermediate text is hidden on the
        // rail too, or the leak just moves one surface over (#3222).
        .map(|text| transcript::collapsed_paragraph(&spoken::while_streaming(text)))
        .filter(|text| !text.is_empty());
    // `live_status_line` invents "OpenCrabs is thinking..." for a turn that has
    // produced nothing at all, which on a card would out-shout the preview it
    // is replacing. The rail waits for the first real event (#2718), and a
    // paragraph counts as one: a turn whose only output so far is prose still
    // has something to say.
    let idle = !live.thinking
        && live.tools.is_empty()
        && live.chunks == 0
        && live.stream.is_empty()
        && live.reasoning.is_empty()
        && narration.is_none();
    if idle {
        return String::new();
    }
    let (lead, tail) = transcript::live_status_line(live);
    let mut parts: Vec<String> = Vec::new();
    parts.extend(narration);
    // Then what it ran. The call-count and clock tail is appended after both,
    // so the line reads the way the chat does: prose, the call, then the tally.
    match transcript::live_activity(live) {
        Some(call) => parts.push(call),
        // A turn that spoke but has not called anything yet gets no second
        // "thinking" label: the prose is the line.
        None if parts.is_empty() && !lead.is_empty() => parts.push(lead),
        None => {}
    }
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts.join(" \u{b7} ")
}

/// The line a card shows while a send is being retried (#3152).
///
/// The wording is `send_label`'s, the same function the terminal and every
/// channel print, so a retry reads the same wherever it is seen. It is built in
/// one place because the window's own turn folds its events arm by arm instead
/// of through `fold_live`, and two copies of a format string drift.
pub(crate) fn retry_notice(attempt: u32, max: u32, reason: &str) -> String {
    format!(
        "{}: {}",
        opencrabs::utils::retry::send_label(attempt, max),
        reason
    )
}

/// Fold one progress event into a live card.
///
/// Free because two owners draw the same card: a split leaf's pane (#2614) and
/// a turn parked under a conversation the user switched away from (#2616). The
/// fold has to be identical for both or the card would tell a different story
/// depending on where the session happens to be sitting.
pub(crate) fn fold_live(
    live: &mut transcript::LiveTurn,
    progress: opencrabs::brain::agent::ProgressEvent,
    at: std::time::Instant,
) {
    use opencrabs::brain::agent::ProgressEvent;
    match progress {
        ProgressEvent::Thinking => live.thinking = true,
        ProgressEvent::StreamingChunk { text } => {
            live.thinking = false;
            // The arrival stamp, so a batch folded in one loop still measures
            // the time the provider actually spent streaming (#2703).
            live.note_chunk_at(&text, at);
        }
        ProgressEvent::IntermediateText { text, reasoning } => {
            live.thinking = false;
            if let Some(reasoning) = reasoning {
                live.push_reasoning_at(&reasoning, at);
            }
            live.push_narration(&text);
        }
        ProgressEvent::ToolStarted {
            tool_name,
            tool_input,
            ..
        } => {
            live.thinking = false;
            let description =
                opencrabs::tui::app::App::format_tool_description(&tool_name, &tool_input);
            live.push_tool(transcript::ToolCall::running(tool_name, description));
        }
        ProgressEvent::ToolCompleted {
            tool_name, success, ..
        } => {
            let open = live
                .tools
                .iter_mut()
                .rev()
                .find(|c| !c.done && c.name == tool_name && c.task_id.is_none());
            match open {
                Some(call) => {
                    call.done = true;
                    call.success = success;
                }
                None => live.push_tool(transcript::ToolCall {
                    name: tool_name.clone(),
                    description: tool_name,
                    success,
                    done: true,
                    task_id: None,
                }),
            }
        }
        ProgressEvent::DetachedTaskStarted { task_id, label } => {
            live.thinking = false;
            live.push_tool(transcript::ToolCall::detached(task_id, label));
        }
        ProgressEvent::DetachedTaskFinished {
            task_id,
            success,
            elapsed_secs,
            ..
        } => {
            // The same settle the window's card runs, so a pane, a parked turn
            // and a rail card keep the runtime too (#2952). This used to drop
            // elapsed_secs and mark the row done with no duration, which left
            // two paths settling one event and only one keeping the number.
            // A finish with no row is not an error: the turn may have settled.
            transcript::settle_detached(&mut live.tools, &task_id, success, elapsed_secs);
        }
        // The scaffold strip keeps a pasted compaction summary out of the
        // live card: the event text is whatever the user queued, and a copy
        // that rode in with the banner must not paint the whole summary back
        // (2026-11-26 GUI leak). Same rule the reload path applies.
        ProgressEvent::QueuedUserMessage { text } => {
            live.injected
                .push(opencrabs::utils::strip_embedded_compaction(&text).unwrap_or(text));
        }
        // The service discarded what it had already streamed: a phantom
        // iteration, a gaslighting preamble, a thinking-loop timeout. The card
        // has to drop it too, or the working-out that just got retried stays on
        // screen as the answer until the turn settles (#3021).
        ProgressEvent::StripStreamedContent { bytes, .. } => live.strip_streamed(bytes),
        // A retry ladder is silent by design (#3152): `utils::retry` does not
        // fast-fail a dead link, it spends the whole budget and then walks the
        // fallback chain, so the user stares at a counting clock with no word
        // about the network. The terminal and every channel already print this
        // line; the GUI was the one surface that dropped it.
        //
        // This arm covers a split leaf's pane, a parked turn and a rail card,
        // because they all fold through here. The window's own turn does not:
        // it folds its events arm by arm in `dashboard_turns`, so it carries the
        // same notice in its own arm rather than silently keeping the old
        // behaviour on the one card the user is looking at.
        ProgressEvent::RetryAttempt {
            attempt,
            max,
            reason,
        } => live.notices.push(retry_notice(attempt, max, &reason)),
        _ => {}
    }
}
