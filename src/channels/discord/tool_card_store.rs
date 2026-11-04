//! Durable snapshot of a tool card (#1880). The card repository stores this as
//! JSON, so a card can be paged, logged and expanded after a restart.
//!
//! Clocks are stored as elapsed milliseconds. An `Instant` does not survive a
//! process restart, so restore rebuilds the anchor from the stored elapsed.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::tool_group::{GroupEntry, GroupState, SettledStatus};
use crate::channels::background_work::FlowOutcome;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StoredCard {
    pub entries: Vec<StoredEntry>,
    pub notes: Vec<String>,
    pub expanded: bool,
    pub page: usize,
    pub elapsed_ms: u64,
    pub settled: Option<StoredSettled>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StoredEntry {
    pub name: String,
    pub context: String,
    pub call_id: String,
    pub status: Option<bool>,
    /// Detached-task id (#2148). `default` so a snapshot written before the
    /// field existed still restores.
    #[serde(default)]
    pub task_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StoredSettled {
    pub elapsed_ms: u64,
    pub ctx: Option<String>,
    pub waiting: Option<String>,
    pub outcome: Option<String>,
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn outcome_key(outcome: FlowOutcome) -> &'static str {
    match outcome {
        FlowOutcome::Finished => "finished",
        FlowOutcome::Failed => "failed",
        FlowOutcome::TimedOut => "timed_out",
        FlowOutcome::Cancelled => "cancelled",
    }
}

fn outcome_from_key(key: &str) -> Option<FlowOutcome> {
    match key {
        "finished" => Some(FlowOutcome::Finished),
        "failed" => Some(FlowOutcome::Failed),
        "timed_out" => Some(FlowOutcome::TimedOut),
        "cancelled" => Some(FlowOutcome::Cancelled),
        _ => None,
    }
}

/// The stored form of a live or settled group.
pub(crate) fn snapshot(group: &GroupState) -> StoredCard {
    StoredCard {
        entries: group
            .entries
            .iter()
            .map(|e| StoredEntry {
                name: e.name.clone(),
                context: e.context.clone(),
                call_id: e.call_id.clone(),
                status: e.status,
                task_id: e.task_id.clone(),
            })
            .collect(),
        notes: group.notes.clone(),
        expanded: group.expanded,
        page: group.page,
        elapsed_ms: millis(group.started_at.elapsed()),
        settled: group.settled.as_ref().map(|s| StoredSettled {
            elapsed_ms: millis(s.elapsed),
            ctx: s.ctx.clone(),
            waiting: s.waiting.clone(),
            outcome: s.outcome.map(|o| outcome_key(o).to_string()),
        }),
    }
}

/// Rebuild a group from its stored form, anchoring the clock so it reads the
/// stored elapsed time at restore.
pub(crate) fn restore(stored: StoredCard) -> GroupState {
    let elapsed = Duration::from_millis(stored.elapsed_ms);
    GroupState {
        entries: stored
            .entries
            .into_iter()
            .map(|e| GroupEntry {
                name: e.name,
                context: e.context,
                call_id: e.call_id,
                status: e.status,
                task_id: e.task_id,
            })
            .collect(),
        notes: stored.notes,
        expanded: stored.expanded,
        page: stored.page,
        started_at: Instant::now()
            .checked_sub(elapsed)
            .unwrap_or_else(Instant::now),
        settled: stored.settled.map(|s| SettledStatus {
            elapsed: Duration::from_millis(s.elapsed_ms),
            ctx: s.ctx,
            waiting: s.waiting,
            outcome: s.outcome.as_deref().and_then(outcome_from_key),
        }),
    }
}
