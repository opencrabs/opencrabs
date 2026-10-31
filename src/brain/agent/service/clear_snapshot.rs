//! Environment snapshot and turn instructions for the reply that follows
//! `/clear`.
//!
//! The snapshot only points at state that already persists: the session plan
//! files and the session working directory. Nothing is copied or summarised,
//! so the snapshot costs no provider call.

use crate::utils::plan_files::PlanModeState;

/// Plan state as the clear snapshot describes it. Both Editing sub-states
/// collapse into `Editing`, since the agent acts the same way on either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanState {
    None,
    Editing,
    Active,
}

impl From<PlanModeState> for PlanState {
    fn from(state: PlanModeState) -> Self {
        match state {
            PlanModeState::NoPlan => Self::None,
            PlanModeState::PreInitEditing | PlanModeState::PostInitEditing => Self::Editing,
            PlanModeState::Active => Self::Active,
        }
    }
}

/// Where the session's state lives, read at the moment of the clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvironmentSnapshot {
    pub working_directory: Option<String>,
    pub plan_state: PlanState,
    /// Absolute path to the plan `.md`, when one exists.
    pub plan_md: Option<String>,
    /// Absolute path to the plan `.json` state file, when one exists.
    pub plan_json: Option<String>,
}

/// The block appended to the clear marker. It names where things are and
/// never repeats their contents.
pub(crate) fn snapshot_block(snapshot: &EnvironmentSnapshot) -> String {
    let directory = snapshot
        .working_directory
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or("not set");
    let mut lines = vec![
        "Environment at the clear:".to_string(),
        format!("- Working directory: {directory}"),
    ];
    match snapshot.plan_state {
        PlanState::None => lines.push("- Plan: none in this session".to_string()),
        PlanState::Editing => lines.push("- Plan: editing, not started yet".to_string()),
        PlanState::Active => lines.push("- Plan: active, checklist running".to_string()),
    }
    if snapshot.plan_state != PlanState::None {
        if let Some(md) = &snapshot.plan_md {
            lines.push(format!("- Plan document: {md}"));
        }
        if let Some(json) = &snapshot.plan_json {
            lines.push(format!("- Plan state file: {json}"));
        }
    }
    lines.join("\n")
}

/// Instructions for the one turn that answers `/clear`. They reach the model
/// but are never written to history, so they do not repeat in later context.
pub(crate) fn reply_instructions(title: &str, snapshot: &EnvironmentSnapshot) -> String {
    let next = match snapshot.plan_state {
        PlanState::Active => {
            "the plan is active, so resume its next incomplete task now without asking"
        }
        PlanState::Editing => "the plan is still in editing, so say so and ask what to do next",
        PlanState::None => "there is no plan in flight, so ask what the user wants next",
    };
    format!(
        "[Turn instructions, not stored in the history: the user just ran /clear and \
         wants to keep going. Open your reply with these four steps, in order, then \
         continue.\n\
         1. Say plainly that your context window was wiped and that nothing is lost: \
         the full history is still in this session (use session_search with session \
         '{title}'), and the environment above points to the rest.\n\
         2. State how confident you are that you can pick the task back up, and give \
         the reason in one line.\n\
         3. Add ONE short ROAST: yourself, the context you just lost, or the situation. \
         Be creative and surprise them; cursing allowed. One line, then straight on.\n\
         4. Continue: {next}.]"
    )
}
