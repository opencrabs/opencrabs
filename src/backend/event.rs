//! What a crab tells a surface without being asked.
//!
//! Operations answer; events arrive. A local backend forwards them from the
//! agent service, a remote one reads them off the wire, and a surface reacts
//! to both the same way.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::brain::agent::ProgressEvent;

/// Something changed on the crab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A running turn made progress: a tool started, a chunk streamed.
    Turn {
        /// The session the turn runs in.
        session_id: Uuid,
        /// What happened, exactly as the agent service reported it.
        progress: ProgressEvent,
    },
    /// A turn finished with an answer.
    TurnDone {
        /// The session the turn ran in.
        session_id: Uuid,
    },
    /// A turn ended in an error; carries the text the surface shows.
    TurnFailed {
        /// The session the turn ran in.
        session_id: Uuid,
        /// Why it failed.
        message: String,
    },
    /// A turn was stopped before it finished.
    TurnCancelled {
        /// The session the turn ran in.
        session_id: Uuid,
    },
    /// The session rail changed: a surface reloads it rather than patching.
    RailChanged,
    /// A session's transcript gained or lost messages.
    TranscriptChanged {
        /// The session whose messages moved.
        session_id: Uuid,
    },
    /// The background tasks changed: one started, finished or was stopped.
    TasksChanged,
    /// A remote turn waits for a yes or a no before it runs a tool. The
    /// answer comes back as `turn.approval_answer` with the same
    /// `request_id`; without one the tool is denied once the wait runs out.
    ApprovalRequested {
        /// The session the turn runs in.
        session_id: Uuid,
        /// What the answer names to reach this request.
        request_id: Uuid,
        /// The tool that wants to run.
        tool_name: String,
        /// What the tool does, as the tool describes itself.
        tool_description: String,
        /// The input the model gave the tool.
        tool_input: serde_json::Value,
        /// What the tool may touch.
        capabilities: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::brain::tools::suggest_options::{SuggestionItem, SuggestionStyle};

    /// One of every `ProgressEvent`. The match below has no wildcard, so a
    /// variant added to the enum fails to compile here until it is listed.
    fn every_progress_event() -> Vec<ProgressEvent> {
        let all = vec![
            ProgressEvent::Thinking,
            ProgressEvent::ToolStarted {
                tool_name: "bash".into(),
                tool_input: serde_json::json!({"command": "ls"}),
                tool_call_id: "call_1".into(),
            },
            ProgressEvent::ToolCompleted {
                tool_name: "bash".into(),
                tool_input: serde_json::json!({"command": "ls"}),
                success: true,
                summary: "3 files".into(),
                tool_call_id: "call_1".into(),
            },
            ProgressEvent::DetachedTaskStarted {
                task_id: "t1".into(),
                label: "cargo test".into(),
            },
            ProgressEvent::DetachedTaskFinished {
                task_id: "t1".into(),
                label: "cargo test".into(),
                success: false,
                elapsed_secs: 12.5,
            },
            ProgressEvent::IntermediateText {
                text: "looking".into(),
                reasoning: Some("why".into()),
            },
            ProgressEvent::QueuedUserMessage {
                text: "also this".into(),
            },
            ProgressEvent::StreamingChunk { text: "wor".into() },
            ProgressEvent::Compacting {
                usage_pct: 81.25,
                predicted: Some(Duration::from_millis(29_500)),
            },
            ProgressEvent::Compacting {
                usage_pct: 70.0,
                predicted: None,
            },
            ProgressEvent::CompactionSummary {
                summary: "shorter".into(),
                before_pct: 81.25,
                after_pct: 12.5,
                elapsed: Duration::new(3, 250_000_000),
            },
            ProgressEvent::BuildLine("Compiling foo v1.0".into()),
            ProgressEvent::RestartReady {
                status: "built".into(),
                binary_path: Some(PathBuf::from("/tmp/opencrabs")),
            },
            ProgressEvent::RestartReady {
                status: "evolved".into(),
                binary_path: None,
            },
            ProgressEvent::TokenCount(4096),
            ProgressEvent::ReasoningChunk { text: "hmm".into() },
            ProgressEvent::SelfHealingAlert {
                message: "config recovered".into(),
            },
            ProgressEvent::StripStreamedContent {
                bytes: 42,
                reason: "refusal preamble".into(),
            },
            ProgressEvent::ProviderSwitched {
                from_name: "a".into(),
                from_model: "a-1".into(),
                to_name: "b".into(),
                to_model: "b-1".into(),
                reason: "rate limited".into(),
            },
            ProgressEvent::RetryAttempt {
                attempt: 2,
                max: 5,
                reason: "stream dropped".into(),
            },
            ProgressEvent::SuggestedOptions(vec![SuggestionItem::new(
                "run the tests",
                SuggestionStyle::default(),
            )]),
            ProgressEvent::PlanCompleted {
                title: "ship it".into(),
                tasks: 4,
            },
        ];
        for event in &all {
            match event {
                ProgressEvent::Thinking
                | ProgressEvent::ToolStarted { .. }
                | ProgressEvent::ToolCompleted { .. }
                | ProgressEvent::DetachedTaskStarted { .. }
                | ProgressEvent::DetachedTaskFinished { .. }
                | ProgressEvent::IntermediateText { .. }
                | ProgressEvent::QueuedUserMessage { .. }
                | ProgressEvent::StreamingChunk { .. }
                | ProgressEvent::Compacting { .. }
                | ProgressEvent::CompactionSummary { .. }
                | ProgressEvent::BuildLine(_)
                | ProgressEvent::RestartReady { .. }
                | ProgressEvent::TokenCount(_)
                | ProgressEvent::ReasoningChunk { .. }
                | ProgressEvent::SelfHealingAlert { .. }
                | ProgressEvent::StripStreamedContent { .. }
                | ProgressEvent::ProviderSwitched { .. }
                | ProgressEvent::RetryAttempt { .. }
                | ProgressEvent::SuggestedOptions(_)
                | ProgressEvent::PlanCompleted { .. } => {}
            }
        }
        all
    }

    #[test]
    fn every_progress_event_survives_the_wire() {
        for progress in every_progress_event() {
            let json = serde_json::to_string(&progress).expect("encode");
            let back: ProgressEvent = serde_json::from_str(&json).expect("decode");
            assert_eq!(back, progress, "{json}");
        }
    }

    #[test]
    fn every_turn_event_survives_the_wire() {
        let session_id = Uuid::new_v4();
        for progress in every_progress_event() {
            let event = Event::Turn {
                session_id,
                progress,
            };
            let json = serde_json::to_string(&event).expect("encode");
            let back: Event = serde_json::from_str(&json).expect("decode");
            assert_eq!(back, event, "{json}");
        }
    }

    #[test]
    fn a_duration_travels_as_secs_and_nanos() {
        let json = serde_json::to_value(ProgressEvent::Compacting {
            usage_pct: 50.0,
            predicted: Some(Duration::new(2, 5)),
        })
        .expect("encode");
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "compacting",
                "data": {"usage_pct": 50.0, "predicted": {"secs": 2, "nanos": 5}}
            })
        );
    }

    #[test]
    fn events_survive_the_wire() {
        let id = Uuid::new_v4();
        for event in [
            Event::TurnDone { session_id: id },
            Event::TurnFailed {
                session_id: id,
                message: "provider: timeout".into(),
            },
            Event::TurnCancelled { session_id: id },
            Event::RailChanged,
            Event::TranscriptChanged { session_id: id },
            Event::TasksChanged,
            Event::ApprovalRequested {
                session_id: id,
                request_id: Uuid::new_v4(),
                tool_name: "bash".into(),
                tool_description: "run a command".into(),
                tool_input: serde_json::json!({"command": "ls"}),
                capabilities: vec!["ExecuteShell".into()],
            },
        ] {
            let json = serde_json::to_string(&event).expect("encode");
            let back: Event = serde_json::from_str(&json).expect("decode");
            assert_eq!(back, event, "{json}");
        }
    }
}
