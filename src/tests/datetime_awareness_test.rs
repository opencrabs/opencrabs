//! Integration tests for #153 datetime awareness:
//! 1. Sub-agent tool loop inheritance and temporal grounding
//! 2. Post-compaction context temporal continuation

use crate::brain::agent::service::AgentService;
use crate::brain::agent::service::tool_loop::check_intra_turn_time_marker;
use std::fs;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn subagent_or_worker_turn_ingress_carries_time_marker() {
    let dir = TempDir::new().unwrap();
    let user_md = dir.path().join("USER.md");
    fs::write(&user_md, "Timezone: Europe/Paris\n").unwrap();

    let session_id = Uuid::new_v4();
    let prompt = "Examine repo health and run diagnostics";
    let augmented = AgentService::augment_user_message(session_id, prompt, Some(dir.path())).await;

    // Must prepend the temporal marker with dual or UTC time
    assert!(augmented.starts_with("[Current time: "));
    assert!(augmented.contains("UTC"));
    assert!(augmented.contains("user:"));
    // The marker is prepended; the plan reminder and MEMORY.md recall ride
    // after the message (#799), so what must survive is the message itself,
    // not the tail position.
    assert!(augmented.contains(prompt));
}

#[test]
fn intra_turn_evaluator_handles_subagent_and_compaction_cadence() {
    // When a long task runs across multiple tool iterations or post-compaction:
    let start = chrono::Utc::now();
    let interval = 900u64; // 15 minutes

    // Just after compaction or early in turn (< 15 min): no marker injected
    assert!(
        check_intra_turn_time_marker(
            start,
            start + chrono::Duration::seconds(300),
            interval,
            None
        )
        .is_none()
    );

    // Crossing the 15-minute boundary: marker is generated with fresh wall-clock
    let after_15m = start + chrono::Duration::seconds(901);
    let marker = check_intra_turn_time_marker(start, after_15m, interval, None);
    assert!(marker.is_some());
    let (notice, updated) = marker.unwrap();
    assert_eq!(updated, after_15m);
    assert!(notice.starts_with("[System: Current time: "));
    assert!(notice.contains("UTC"));

    // Subsequent iteration immediately after marker (< 15 min from updated): no marker
    assert!(
        check_intra_turn_time_marker(
            updated,
            updated + chrono::Duration::seconds(60),
            interval,
            None
        )
        .is_none()
    );

    // Next 15m cycle: fires again
    let next_cycle = updated + chrono::Duration::seconds(905);
    let marker2 = check_intra_turn_time_marker(updated, next_cycle, interval, None);
    assert!(marker2.is_some());
    let (notice2, updated2) = marker2.unwrap();
    assert_eq!(updated2, next_cycle);
    assert!(notice2.starts_with("[System: Current time: "));
}
