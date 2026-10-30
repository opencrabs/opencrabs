//! Shared "turn ended but work is still running" helpers (#1985).
//!
//! The waiting-state logic was pure but lived in Telegram-only modules, so
//! Discord, Slack and WhatsApp re-derived it partially or not at all. This is
//! the shared home: it depends only on [`AgentService`] and its two registries
//! (background tasks, sub-agents) and never on channel types, so every channel
//! can fold the same two counts into one waiting phrase (#1987, #1988, #1989
//! build on this).

use uuid::Uuid;

use crate::brain::agent::{AgentError, AgentService};

/// Alive sub-agent counts captured at settle (#1183): `working` counts
/// children mid-round (`Running`), `awaiting` counts children parked at a
/// round boundary whose output is ready to collect (`AwaitingInput`). The
/// settle card distinguishes the two because they need different things from
/// the user: working agents just need time, parked ones need a
/// `wait_agent`/`send_input`/`close_agent` decision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SubagentCounts {
    pub(crate) working: usize,
    pub(crate) awaiting: usize,
}

impl SubagentCounts {
    /// Total alive agents, working or parked.
    pub(crate) fn total(self) -> usize {
        self.working + self.awaiting
    }

    /// True when no alive agents belong to this session's settle.
    pub(crate) fn is_empty(self) -> bool {
        self.total() == 0
    }
}

/// The sub-agent share of the waiting verb (#1183): "N working agents",
/// "N agents awaiting collection", or the split form when both exist. Pure so
/// the header grammar is pinnable without live managers.
pub(crate) fn subagent_waiting_phrase(agents: SubagentCounts) -> String {
    let n = agents.total();
    let noun = if n == 1 { "agent" } else { "agents" };
    match (agents.working, agents.awaiting) {
        (working, 0) => format!("{working} working {noun}"),
        (0, awaiting) => format!("{awaiting} {noun} awaiting collection"),
        (working, awaiting) => {
            format!("{n} {noun} ({working} working, {awaiting} awaiting collection)")
        }
    }
}

/// The shared waiting-state decision (#1985): a turn that finished while
/// background work is still alive must read "Waiting for N background
/// task(s) + M agents" instead of its terminal verb (#1144, #1183).
/// `Some(phrase)` when either registry still holds work for this session,
/// `None` when the turn is genuinely done. Callers then render their own
/// outcome (Telegram's `settled_icon_verb` in `flow.rs` pairs this with the
/// flow icon). The counts come from [`bg_indicator_for`] and
/// [`subagent_counts_for`].
pub(crate) fn waiting_verb(bg_count: Option<usize>, agents: SubagentCounts) -> Option<String> {
    let bg = bg_count.unwrap_or(0);
    if bg == 0 && agents.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if bg > 0 {
        parts.push(if bg == 1 {
            "1 background task".to_string()
        } else {
            format!("{bg} background tasks")
        });
    }
    if !agents.is_empty() {
        parts.push(subagent_waiting_phrase(agents));
    }
    Some(format!("Waiting for {}", parts.join(" + ")))
}

/// How a row in a turn's tool block names a detached task (#2145, #2147,
/// #2148, #2149, #2150). A background task outlives the turn that started
/// it, so its row is the only place its liveness is visible; a plain tool
/// row keeps its tool name untouched.
///
/// One implementation for every surface. Telegram's flow rows, Discord's
/// card rows, Slack and WhatsApp all render through this, so the wording
/// cannot drift channel by channel.
pub(crate) fn detached_row_label(
    name: &str,
    task_id: Option<&str>,
    completed: Option<bool>,
) -> String {
    if task_id.is_none() {
        return name.to_string();
    }
    let state = match completed {
        None => "running",
        Some(true) => "finished",
        Some(false) => "failed",
    };
    format!("detached task {state}: {name}")
}

/// Background-task indicator for the settled flow footer (#1054): the first
/// task's label when exactly one is running, a count when several, `None`
/// when nothing is detached or no manager is wired (#722). A settled turn
/// that ends with detached work looks identical to a complete one without
/// this, and the typing indicator staying alive is too easy to miss.
///
/// Returns the footer label **and** the numeric count (the settled header
/// needs the number to read "Waiting for N background task(s)" (#1144).
/// Both come from the single `running_tasks(session_id)` read so settle does
/// not hit the manager twice.
pub(crate) fn bg_indicator_for(
    agent: &AgentService,
    session_id: Uuid,
) -> (Option<String>, Option<usize>) {
    let Some(bm) = agent.background_manager() else {
        return (None, None);
    };
    let tasks = bm.running_tasks(session_id);
    match tasks.len() {
        0 => (None, Some(0)),
        1 => (Some(format!("{} running", tasks[0].label)), Some(1)),
        n => (Some(format!("{n} tasks running")), Some(n)),
    }
}

/// Alive sub-agent counts for the settled header (#1183): how many of THIS
/// session's children are still working vs parked awaiting collection. The
/// sub-agent registry is separate from `BackgroundTaskManager`, so the #1144
/// header gate never saw it — a turn ending with agents mid-work still read
/// "✅ Finished". Empty when no manager is wired or every child already
/// terminated; the header then falls back to the background-task-only (or
/// plain Finished) form.
pub(crate) fn subagent_counts_for(agent: &AgentService, session_id: Uuid) -> SubagentCounts {
    let Some(mgr) = agent.subagent_manager() else {
        return SubagentCounts::default();
    };
    let (working, awaiting) = mgr.alive_counts_for(session_id);
    SubagentCounts { working, awaiting }
}

/// Terminal state of a turn, shown by the settled chrome on the surfaces that
/// stamp one (#480 on Telegram, #1911 on Discord). One vocabulary so a lane
/// cannot invent its own word for a timeout and have it read as a crash.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FlowOutcome {
    Finished,
    Failed,
    TimedOut,
    /// The user stopped the turn (#1987). Telegram never reaches this state
    /// through the enum: its cancel arm deletes the bubble instead of
    /// settling it, so only the lanes that stamp a word onto a live bubble
    /// carry a cancelled turn.
    Cancelled,
}

impl FlowOutcome {
    /// Icon and verb for the settled header, e.g. `("⏱", "Timed out")`.
    pub(crate) fn icon_verb(self) -> (&'static str, &'static str) {
        match self {
            FlowOutcome::Finished => ("✅", "Finished"),
            FlowOutcome::Failed => ("❌", "Failed"),
            FlowOutcome::TimedOut => ("⏱", "Timed out"),
            FlowOutcome::Cancelled => ("❌", "Cancelled"),
        }
    }
}

/// Classify a failed turn (#1911). A timeout reaches the channel as a
/// provider error whose text carries the deadline wording, so this is a
/// string match rather than a typed variant. Kept in one place because
/// Telegram read the same three substrings inline in two files and Discord
/// read none: every failed turn there settled to one blunt word, so a 500
/// and a 10-minute stall looked identical on the bubble.
pub(crate) fn outcome_for_error(err: &AgentError) -> FlowOutcome {
    let text = err.to_string().to_lowercase();
    if text.contains("timed out") || text.contains("timeout") || text.contains("deadline") {
        FlowOutcome::TimedOut
    } else {
        FlowOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents(working: usize, awaiting: usize) -> SubagentCounts {
        SubagentCounts { working, awaiting }
    }

    #[test]
    fn waiting_verb_is_none_when_nothing_is_alive() {
        // Both registries empty (or count-less): the turn is genuinely done.
        assert_eq!(waiting_verb(None, agents(0, 0)), None);
        assert_eq!(waiting_verb(Some(0), agents(0, 0)), None);
    }

    #[test]
    fn waiting_verb_counts_background_tasks_alone() {
        assert_eq!(
            waiting_verb(Some(1), agents(0, 0)).as_deref(),
            Some("Waiting for 1 background task")
        );
        assert_eq!(
            waiting_verb(Some(3), agents(0, 0)).as_deref(),
            Some("Waiting for 3 background tasks")
        );
    }

    #[test]
    fn waiting_verb_counts_agents_alone() {
        assert_eq!(
            waiting_verb(Some(0), agents(2, 0)).as_deref(),
            Some("Waiting for 2 working agents")
        );
        assert_eq!(
            waiting_verb(None, agents(0, 1)).as_deref(),
            Some("Waiting for 1 agent awaiting collection")
        );
    }

    #[test]
    fn waiting_verb_folds_both_registries_with_a_plus() {
        assert_eq!(
            waiting_verb(Some(1), agents(2, 0)).as_deref(),
            Some("Waiting for 1 background task + 2 working agents")
        );
        assert_eq!(
            waiting_verb(Some(2), agents(1, 1)).as_deref(),
            Some("Waiting for 2 background tasks + 2 agents (1 working, 1 awaiting collection)")
        );
    }

    #[test]
    fn subagent_waiting_phrase_grammar_is_pinned() {
        assert_eq!(subagent_waiting_phrase(agents(1, 0)), "1 working agent");
        assert_eq!(
            subagent_waiting_phrase(agents(0, 2)),
            "2 agents awaiting collection"
        );
        assert_eq!(
            subagent_waiting_phrase(agents(2, 1)),
            "3 agents (2 working, 1 awaiting collection)"
        );
    }
}
