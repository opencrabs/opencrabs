//! The correction injected when a turn claims work it never executed
//! (#796, #797).
//!
//! The previous wording stated what was missing: "your last response produced
//! ZERO tool_use blocks". That is accurate and argues against a position the
//! model does not hold. It is not withholding a call it knows it skipped; it
//! believes the work already happened, having simulated the call while
//! reasoning and lost the distinction between the simulation and a result.
//! Told it emitted no tool_use blocks, a model in that state reads a
//! formatting complaint and leaves the belief intact.
//!
//! So the correction states the mechanism instead: nothing executes inside
//! reasoning, and the only evidence a tool ran is a result present in the
//! conversation. That is checkable, and it is checkable by the model.
//!
//! Pure so the wording is testable without a provider.

/// The escape hatch every variant must carry.
///
/// Without it the nudge becomes a loop: a model that genuinely finished, and
/// said so, gets told to call tools, calls something pointless to comply, and
/// is nudged again. Real completion has to have an exit that is not a tool
/// call.
const FINISHED_ESCAPE: &str = " If the work is genuinely done and you have already reported it, \
     reply with a short confirmation and stop; do not run extra tool calls to re-verify it.";

/// The mechanism, stated once and shared by every variant.
const NO_EXECUTION_WHILE_REASONING: &str = "Tools execute only between turns; nothing runs inside your reasoning. If you saw that \
     output while thinking, you imagined it. The only evidence a tool ran is its result \
     present in this conversation.";

/// Correction naming the exact commands claimed but never run (#797).
///
/// This is the one correction that does not infer. Every other phantom check
/// reads wording for signals, and wording is arguable; the loop knows what it
/// executed, so "you claimed X ran and X did not run" is a matter of fact and
/// cannot be talked around. Quoting it turns a category into a citation.
pub fn uncalled_commands_nudge(commands: &[String]) -> String {
    let quoted = commands
        .iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let subject = if commands.len() == 1 {
        "output from"
    } else {
        "output from these commands:"
    };
    format!(
        "[System: You reported {subject} {quoted}. No such call ran this turn. \
         {NO_EXECUTION_WHILE_REASONING} Call the tool now through the structured tool-call API, \
         or retract the claim and tell the user it has not been run.{FINISHED_ESCAPE}]"
    )
}

/// Correction for claims about the session's own record that the record
/// contradicts (#2122).
///
/// Distinct from the other two fact checks because the contradiction is
/// internal: the text says a command never ran while the calls show it did, or
/// reports an identifier as produced with nothing in the evidence behind it. No
/// external source is needed to settle it, and the model cannot answer it by
/// pointing at a tool it is about to run. The wording therefore asks for a
/// retraction rather than a rerun, and only falls back to running when the
/// identifier is genuinely missing because the call was never made.
pub fn unbacked_actions_nudge(actions: &[String]) -> String {
    let quoted = actions
        .iter()
        .map(|a| format!("`{a}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let subject = if actions.len() == 1 {
        "this claim:"
    } else {
        "these claims:"
    };
    format!(
        "[System: You reported {subject} {quoted}. That contradicts this session's own record: \
         either the call is in your history, or nothing in any tool result shows the work was \
         done. Do not restate it. Correct it in your next message with what the record actually \
         shows, and if the work was never done, do it now through the structured tool-call API \
         before claiming it again.{NO_EXECUTION_WHILE_REASONING}{FINISHED_ESCAPE}]"
    )
}

/// Correction naming facts the turn asserted that exist nowhere in the
/// conversation's evidence (#1423).
///
/// A commit sha or a test tally has to have come from somewhere. When it is in
/// no tool result and no message, it was written rather than read, and quoting
/// it back is what makes the correction unarguable: the model cannot reword its
/// way past a token that does not exist (#797).
pub fn unbacked_facts_nudge(facts: &[String]) -> String {
    let quoted = facts
        .iter()
        .map(|f| format!("`{f}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let subject = if facts.len() == 1 {
        "this fact:"
    } else {
        "these facts:"
    };
    format!(
        "[System: You reported {subject} {quoted}. None of it appears in any tool result or \
         message in this conversation, so it was written, not read. \
         {NO_EXECUTION_WHILE_REASONING} Run the tool that produces it through the structured \
         tool-call API, or retract the claim and say it has not been verified.{FINISHED_ESCAPE}]"
    )
}

/// Correction for a turn that produced no tool calls, when no specific
/// fabricated command was identified.
///
/// `local_model` selects wording for Qwen/Kimi/DeepSeek-class models, which
/// need two things the cloud variant does not. They read "STOP" as "wait for
/// further instruction" and reply with an acknowledgement instead of calling
/// anything, so the word is avoided. And they write `{"tool_call": {...}}` as
/// message text believing that IS the invocation, so the structured API is
/// named explicitly.
pub fn no_tool_calls_nudge(local_model: bool) -> String {
    let channel = if local_model {
        "Invoke it through the provider's structured tool-call API, the same channel the function \
         schemas were registered on. JSON or markdown written into your message is text and does \
         not execute."
    } else {
        "Call the tool through the structured tool-call API rather than describing it."
    };
    format!(
        "[System: Your last response claimed work but produced no tool calls, so nothing was \
         executed. {NO_EXECUTION_WHILE_REASONING} {channel} Pick the tool you need and call it \
         now.{FINISHED_ESCAPE}]"
    )
}

// ── Pre-compaction context-pressure warning (#909) ──
//
// Compaction is reactive today: Tier-1 fires at 65% and clips the conversation
// with no prior warning. The model never gets a chance to deliberately finish
// its current sub-task and persist critical state before the clip.
//
// These helpers define a warning band (55-64%) just below the trigger. When
// usage enters the band, a behavioural nudge is appended to the system brain
// telling the model to persist state to disk NOW. It is a nudge, not a number,
// because `context.token_count` is a tiktoken estimate for all providers at
// the point this runs - a precise percentage would repeat the units-confusion
// that caused #896, and the model can't trigger compaction anyway, so the only
// useful signal is the instruction itself.
//
// The nudge is transient: it rides on `system_brain`, which is rebuilt from
// disk every turn, so it never lands in the DB. A per-session throttle flag
// (held on AgentService) suppresses repeat warnings while usage lingers in the
// band and re-arms when it drops below the band floor.

/// Lower edge of the warning band (inclusive). Below this the throttle re-arms
/// so a fresh entry into the band warns again.
pub(crate) const PRESSURE_WARN_FLOOR: f64 = 55.0;

/// Upper edge of the warning band (exclusive). At 65% Tier-1 compaction fires
/// (see `compaction.rs`), so the warning band runs right up to the trigger.
pub(crate) const PRESSURE_WARN_CEILING: f64 = 65.0;

/// The behavioural nudge text appended to the system brain when usage is in the
/// warning band. Public to the crate so the wiring site and tests can reference
/// the exact wording.
pub fn context_pressure_warning() -> &'static str {
    "\n\n[SYSTEM WARNING - Context is filling up and auto-compaction will trigger soon. \
     If you have critical state - active plan, key findings, partial work, decisions made - \
     persist it to disk NOW (MEMORY.md, plan JSON, or a project file) so it survives the \
     upcoming compaction. Do NOT stop or change task; just checkpoint your state.]"
}

/// Whether the current usage percentage is inside the warning band.
/// Pure - testable without a provider or a full AgentService.
pub fn in_pressure_warning_band(usage_pct: f64) -> bool {
    (PRESSURE_WARN_FLOOR..PRESSURE_WARN_CEILING).contains(&usage_pct)
}

/// Decide whether to emit the pressure warning this turn.
///
/// Returns `Some(warning)` when usage is in the band AND the warning has not
/// already been emitted for this band entry, `None` otherwise. The caller owns
/// the `already_emitted` flag and must set it true on emit, false when usage
/// drops below the floor (so the next band entry warns again).
///
/// Pure - testable in isolation.
pub fn should_emit_pressure_warning(usage_pct: f64, already_emitted: bool) -> Option<&'static str> {
    if in_pressure_warning_band(usage_pct) && !already_emitted {
        Some(context_pressure_warning())
    } else {
        None
    }
} // ── Mermaid regen nudge (#37) ──

/// In-loop correction for a model whose mermaid fence failed the render
/// preflight with a deterministic parse error (#37). Quotes the renderer's
/// own error text — it names the offending line or token — and states the
/// regen budget so the model knows how many attempts remain. Same shape as
/// the other nudges: a `[System: …]` bracket the loop injects as a user
/// message after echoing the assistant's broken text.
#[cfg(feature = "telegram")]
pub(crate) fn mermaid_regen_nudge(errors: &[String], attempt: u32, max: u32) -> String {
    let quoted = errors.join("\n");
    let rules = crate::channels::telegram::rich::mermaid::unified_mermaid_rules();
    format!(
        "[System: Your mermaid diagram failed to render — mermaid.ink returned:\n{quoted}\n\
         {rules}\n\
         Re-emit the COMPLETE corrected fence in your reply — keep everything else you wrote. \
         Regen attempt {attempt}/{max}.]"
    )
}

// ── Shared variation directive (#32) ──
//
// Born from the 2026-08-29 incident: a ship call recurred through the
// loop-guard ladder (nudge at 3-in-8, break at 4-in-8) and the turn broke
// silently. The reason the call failed sat in the first tool result — the
// work had already been done by a sibling lane — but no guard message made
// the model read it and act on it. The directive is that missing instruction,
// stated once so every loop-breaker that fires on a recurring call teaches
// the same lesson in the same words. Callers keep their own mechanism
// sentence and compose this around it; the shared part is the behavioural
// core (read the result, verify state, vary or report completion).

/// The behavioural core shared by every guard that fires on a recurring
/// call (#32): stop re-issuing, read the result already in hand, verify
/// current state, then vary the action or report completion.
///
/// Pure so the wording is testable without a provider.
pub fn variation_directive() -> &'static str {
    "Do not re-issue the same call. Read the reason in the result you already have: it often \
     says the work is already done or the input is wrong. Verify current state before acting \
     again, then take a genuinely different action — different flags, a different command, or \
     report completion if there is nothing left to do."
}
