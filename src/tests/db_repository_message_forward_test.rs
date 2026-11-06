//! Forwarding a run of transcript rows into another session (#2608).
//!
//! The storage half of the feature, proven without a window: a forward writes
//! COPIES into the destination, they land after whatever the destination
//! already had, and every row of one forward carries the same batch id so the
//! transcript can draw one header over them.

use crate::db::Database;
use crate::db::models::{Message, Session};
use crate::db::repository::SessionRepository;
use crate::db::repository::message::MessageRepository;
use tokio;

async fn seeded() -> (Database, MessageRepository, Session, Session) {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let sessions = SessionRepository::new(db.pool().clone());
    let source = Session::new(Some("Source".to_string()), None, None);
    let target = Session::new(Some("Target".to_string()), None, None);
    sessions.create(&source).await.expect("source session");
    sessions.create(&target).await.expect("target session");
    let repo = MessageRepository::new(db.pool().clone());
    (db, repo, source, target)
}

#[tokio::test]
async fn forward_store_copies_rows_into_the_target_under_one_batch() {
    let (_db, repo, source, target) = seeded().await;
    let mut a = Message::new(source.id, "user".to_string(), "primeira".into(), 0);
    let mut b = Message::new(source.id, "assistant".to_string(), "segunda".into(), 1);
    repo.create(&a).await.expect("create a");
    repo.create(&b).await.expect("create b");
    // Reload so the ids and provenance match what the window holds.
    a = repo.find_by_id(a.id).await.unwrap().expect("a persisted");
    b = repo.find_by_id(b.id).await.unwrap().expect("b persisted");

    let ids = repo
        .forward(&[a.clone(), b.clone()], target.id, "Source")
        .await
        .expect("forward");
    assert_eq!(ids.len(), 2, "one row per picked message");

    let landed = repo
        .find_recent_by_session(target.id, 20)
        .await
        .expect("read target");
    assert_eq!(landed.len(), 2);

    for (row, original) in landed.iter().zip([&a, &b]) {
        // A forward is a copy the destination's own turn will read as an
        // instruction, never as another session's answer.
        assert_eq!(row.role, "user");
        assert_eq!(row.content, original.content);
        let mark = row.forward.as_ref().expect("provenance written");
        assert_eq!(mark.from_session, source.id);
        assert_eq!(mark.from_message, original.id);
        assert_eq!(mark.label, "Source");
    }
    assert_eq!(
        landed[0].forward.as_ref().map(|f| f.batch),
        landed[1].forward.as_ref().map(|f| f.batch),
        "one batch id per forward, so one header draws over both rows"
    );
    // The source keeps its own rows: a forward never moves anything.
    let kept = repo
        .find_recent_by_session(source.id, 20)
        .await
        .expect("read source");
    assert_eq!(kept.len(), 2);
    assert!(kept.iter().all(|m| m.forward.is_none()));
}

#[tokio::test]
async fn forward_store_lands_after_the_destination_tail() {
    let (_db, repo, source, target) = seeded().await;
    let existing = Message::new(target.id, "user".to_string(), "já cá estava".into(), 0);
    repo.create(&existing).await.expect("seed target");
    let m = Message::new(source.id, "user".to_string(), "forwarded".into(), 0);
    repo.create(&m).await.expect("create m");

    repo.forward(&[m], target.id, "Source")
        .await
        .expect("forward");

    let landed = repo
        .find_recent_by_session(target.id, 20)
        .await
        .expect("read target");
    assert_eq!(landed.len(), 2);
    // Newest last: the block reads under whatever the reader already had.
    assert_eq!(landed[1].content, "forwarded");
    assert!(landed[1].sequence > landed[0].sequence);
}

#[tokio::test]
async fn forward_store_refuses_the_source_session_and_mixed_selections() {
    let (_db, repo, source, target) = seeded().await;
    let a = Message::new(source.id, "user".to_string(), "a".into(), 0);
    let b = Message::new(target.id, "user".to_string(), "b".into(), 0);
    repo.create(&a).await.expect("create a");
    repo.create(&b).await.expect("create b");

    let err = repo
        .forward(std::slice::from_ref(&a), source.id, "Source")
        .await
        .expect_err("a forward into its own session is not a forward");
    assert!(format!("{err}").contains("destination is the source"));

    let err = repo
        .forward(&[a, b], source.id, "Source")
        .await
        .expect_err("a selection spanning two sessions is refused");
    assert!(format!("{err}").contains("more than one session"));

    let err = repo
        .forward(&[], target.id, "Source")
        .await
        .expect_err("nothing picked is nothing to forward");
    assert!(format!("{err}").contains("nothing selected"));
}
