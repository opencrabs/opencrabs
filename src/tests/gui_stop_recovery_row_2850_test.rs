//! A turn the user stopped is not resumed after a restart (#2850).
//!
//! A stop drops the turn's future (`agent_bridge::race_turn`), so the code
//! inside the turn never runs again, and the line that deletes its
//! `pending_requests` row is inside the turn (`tool_loop.rs`). Every stop
//! therefore left its recovery ticket behind, and the next boot resumed a turn
//! the user had abandoned: the window painted their last message again as if it
//! had just been sent.
//!
//! The log shows it plainly: stops at 19:00 and 19:18, restart at 21:14,
//! `[boot] interrupted=1 resumed=[019ddc16-...]`. A stop is not a crash, so the
//! stop site deletes the ticket itself. Crash recovery is untouched.

const AGENT_BRIDGE: &str = include_str!("../gui/agent_bridge.rs");

use crate::db::Database;
use crate::db::repository::PendingRequestRepository;

/// The stop's own thread body, cut where the next function starts.
fn stop_body() -> &'static str {
    AGENT_BRIDGE
        .split_once("pub fn spawn_record_turn_stopped(")
        .expect("the stop records its verdict in one place")
        .1
        .split_once("/// Generate the personalized brain files")
        .expect("the brain generator follows the stop")
        .0
}

#[test]
fn the_stop_drops_its_recovery_ticket() {
    let body = stop_body();
    assert!(
        body.contains("pending.delete_session(session_id)"),
        "a stopped turn must delete the row that would resume it"
    );
    assert!(
        body.contains("record_turn_stopped(session_id)"),
        "and it must still write the verdict row (#2811)"
    );
}

/// The deletion is by session, not by one guessed id: a stop ends the session's
/// in-flight work, and a leftover row from an earlier turn of the same session
/// would resume just as wrongly.
#[tokio::test]
async fn deleting_a_session_leaves_every_other_session_alone() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = PendingRequestRepository::new(db.pool().clone());

    let stopped = uuid::Uuid::new_v4();
    let running = uuid::Uuid::new_v4();
    repo.insert(
        uuid::Uuid::new_v4(),
        stopped,
        "the question the user stopped",
        "tui",
        None,
        None,
        "user",
    )
    .await
    .unwrap();
    repo.insert(
        uuid::Uuid::new_v4(),
        running,
        "a turn in another session",
        "tui",
        None,
        None,
        "user",
    )
    .await
    .unwrap();

    repo.delete_session(stopped).await.unwrap();

    let left = repo.get_interrupted().await.unwrap();
    assert_eq!(left.len(), 1, "only the stopped session's row is deleted");
    assert_eq!(
        left[0].session_id,
        running.to_string(),
        "the other session still resumes: a stop is not a global wipe"
    );
    assert!(
        repo.find_latest_for_session(stopped)
            .await
            .unwrap()
            .is_none(),
        "nothing is left to replay for the stopped session"
    );
}
