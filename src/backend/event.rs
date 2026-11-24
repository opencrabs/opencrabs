//! What a crab tells a surface without being asked.
//!
//! Operations answer; events arrive. A local backend forwards them from the
//! agent service, a remote one reads them off the wire, and a surface reacts
//! to both the same way.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Something changed on the crab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        ] {
            let json = serde_json::to_string(&event).expect("encode");
            let back: Event = serde_json::from_str(&json).expect("decode");
            assert_eq!(back, event, "{json}");
        }
    }
}
