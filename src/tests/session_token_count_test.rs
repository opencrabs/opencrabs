//! `sessions.token_count` is a lifetime accumulator with two writers (#2180).
//!
//! Two facts made it dangerous. It was written by a read-modify-write: load the
//! row, add in Rust, write the whole row back, so two turns finishing on one
//! session at the same time lost one increment with no error anywhere. And it
//! was rendered as a bare "Tokens", which reads as the size of the context
//! window, on a value that had passed `i32::MAX` on a long-lived install.
//!
//! These pin the three parts of the fix: the increment is atomic under
//! concurrency, the delta is wide enough not to truncate, and the two display
//! sites that render the accumulator say "Lifetime tokens" instead of "Tokens".

use crate::db::Database;
use crate::db::Pool;
use crate::db::models::Session;
use crate::db::repository::SessionRepository;
use crate::services::{ServiceContext, SessionService};

async fn test_pool() -> Pool {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    db.pool().clone()
}

async fn seeded_session(pool: &Pool) -> Session {
    let repo = SessionRepository::new(pool.clone());
    let session = Session::new(Some("usage".to_string()), None, None);
    repo.create(&session).await.expect("seed the session");
    session
}

/// Concurrent increments must all land. The pre-fix writer loaded the row,
/// added in Rust and wrote it back, so two turns overlapping in that window
/// meant the second write overwrote the first: one increment vanished, the
/// counter disagreed with the ledger, and nothing reported a problem.
///
/// A burst rather than a pair: a lost update is a race, so one round of two
/// writers can get lucky. Twelve rounds make the loss certain under the old
/// read-modify-write and impossible under the atomic increment.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_usage_updates_all_land() {
    const WRITERS: i64 = 12;
    const EACH: i64 = 1_000;

    let pool = test_pool().await;
    let session = seeded_session(&pool).await;
    let svc = SessionService::new(ServiceContext::new(pool.clone()));

    // Twelve turns finishing at once on one session: the shape that produced
    // the loss, i.e. overlapping inbound messages on one chat.
    let mut handles = Vec::new();
    for _ in 0..WRITERS {
        let svc = svc.clone();
        let id = session.id;
        handles.push(tokio::spawn(async move {
            svc.update_session_usage(id, EACH, 0.01, "zhipu", "glm-5.1", None, None)
                .await
        }));
    }
    for h in handles {
        h.await
            .expect("writer task did not panic")
            .expect("update lands");
    }

    let stored = SessionRepository::new(pool.clone())
        .find_by_id(session.id)
        .await
        .expect("read back")
        .expect("session exists");
    assert_eq!(
        stored.token_count,
        WRITERS * EACH,
        "every increment survives; a lost update shows up here"
    );
    assert!(
        (stored.total_cost - (WRITERS as f64 * 0.01)).abs() < 1e-9,
        "cost is summed the same way, got {}",
        stored.total_cost
    );
}

/// The delta is `i64`, matching the column. A session on a long-lived install
/// passed `i32::MAX`, so a narrower delta truncates or overflows on the way in.
#[tokio::test]
async fn a_delta_wider_than_i32_is_not_truncated() {
    let pool = test_pool().await;
    let session = seeded_session(&pool).await;
    let svc = SessionService::new(ServiceContext::new(pool.clone()));

    let huge: i64 = i32::MAX as i64 + 1;
    svc.update_session_usage(session.id, huge, 0.0, "zhipu", "glm-5.1", None, None)
        .await
        .expect("a delta past i32::MAX is a normal value here");

    let stored = SessionRepository::new(pool.clone())
        .find_by_id(session.id)
        .await
        .expect("read back")
        .expect("session exists");
    assert_eq!(
        stored.token_count, huge,
        "the counter is i64 end to end, so nothing wrapped"
    );
}

/// The accumulator is not a context size, and two displays rendered it as one.
/// A reader who sees `Tokens: 1713578933` beside `Cost:` reasonably concludes
/// the context is about to blow up. Both sites now name what it is.
#[test]
fn the_lifetime_accumulator_is_not_labelled_as_context() {
    let usage = include_str!("../brain/tools/slash_command.rs");
    let cli = include_str!("../cli/commands.rs");

    assert!(
        usage.contains("Lifetime tokens: {}"),
        "/usage must name the accumulator for what it is (#2180)"
    );
    assert!(
        cli.contains("Lifetime tokens: {}"),
        "`session show` must name it too (#2180)"
    );
    assert!(
        !usage.contains("\"  Tokens: {}\""),
        "the bare Tokens label next to Cost is the lie the issue reports"
    );
}

/// The read-modify-write is gone, not merely unused. Its shape was
/// `session.token_count += ...` followed by `repo.update(&session)`, which is
/// how a concurrent turn's increment got overwritten.
#[test]
fn the_hot_path_no_longer_writes_the_whole_row() {
    let svc = include_str!("../services/session.rs");
    let start = svc
        .find("pub async fn update_session_usage")
        .expect("the hot path exists");
    let body = &svc[start..start + 1_800.min(svc.len() - start)];

    assert!(
        body.contains("update_stats(id, token_count, cost)"),
        "the hot path must go through the atomic increment (#2180)"
    );
    assert!(
        !body.contains("session.token_count +="),
        "a read-modify-write on a shared counter is the bug"
    );
}
