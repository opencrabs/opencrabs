//! What a tool-call loop guard does when it gives up on the current model
//! (#1397).
//!
//! Three guards in the tool loop end a turn when the model keeps re-issuing
//! the same call: the strictly-consecutive modification guard, the
//! near-match guard, and the identical-call dominance guard. Until #1397
//! each of them appended a breadcrumb and returned the partial response as
//! the turn's answer. That dropped the pending call on the floor and never
//! consulted the fallback chain, while every sibling failure (stream drop,
//! phantom calls, reasoning-only turns, announcement loops) walks that
//! chain before the turn is allowed to end. A different model does not
//! reproduce the same loop, which is the whole reason the chain exists.
//!
//! The break is now the provider-attributable [`ProviderError::ToolLoop`]
//! the #1023 rotation wrapper already reacts to: it promotes the next
//! provider and replays the turn, and only when `force_next_fallback` has
//! nowhere left to go does the error reach the user. The message carries the
//! raw pending call so a break is never silent about what did not run.

use serde_json::Value;

use crate::brain::agent::error::AgentError;
use crate::brain::provider::ProviderError;

/// Longest rendering of one pending call in the log line and the error.
const PENDING_CALL_MAX_CHARS: usize = 240;

/// The raw tool call(s) a guard is about to discard: name plus compact JSON
/// arguments, one per pending call. The normalized signature the guard
/// counted on has its digits and punctuation stripped, so it cannot tell
/// the user (or the log) which call was dropped; this can.
pub(crate) fn describe_pending_calls(tool_uses: &[(String, String, Value)]) -> String {
    tool_uses
        .iter()
        .map(|(_, name, input)| {
            let args = serde_json::to_string(input).unwrap_or_default();
            let full = format!("{name} {args}");
            if full.chars().count() <= PENDING_CALL_MAX_CHARS {
                full
            } else {
                let mut cut: String = full.chars().take(PENDING_CALL_MAX_CHARS - 1).collect();
                cut.push('…');
                cut
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The error a loop guard returns in place of ending the turn. Routed as
/// `ToolLoop` on purpose: that is the variant the rotation wrapper
/// in `run_tool_loop` matches to hand the turn to the next provider.
pub(crate) fn loop_break_error(
    guard: &str,
    label: &str,
    count: usize,
    window: usize,
    pending: &str,
) -> AgentError {
    AgentError::Provider(ProviderError::ToolLoop(format!(
        "{guard}: '{label}' recurred {count}x in the last {window} steps; dropped call: {pending}"
    )))
}

/// Rewrites a loop-detector error once the fallback chain is exhausted so the
/// user learns that every provider was tried and that nothing is queued. Any
/// other error passes through untouched.
pub(crate) fn chain_exhausted(err: AgentError, providers_tried: u32) -> AgentError {
    match err {
        AgentError::Provider(ProviderError::ToolLoop(msg)) => {
            AgentError::Provider(ProviderError::ToolLoop(format!(
                "{msg}; every provider in the fallback chain ({providers_tried} tried) hit the \
                 same loop. Nothing is queued, say the word to resume"
            )))
        }
        AgentError::Provider(ProviderError::AnnouncementLoop(msg)) => {
            AgentError::Provider(ProviderError::AnnouncementLoop(format!(
                "{msg}; every provider in the fallback chain ({providers_tried} tried) hit the \
                 same loop. Nothing is queued, say the word to resume"
            )))
        }
        other => other,
    }
}
