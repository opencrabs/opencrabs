use crate::db::Database;
use crate::db::models::Session;
use crate::db::repository::session::*;
use tokio;

#[tokio::test]
async fn test_session_crud() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = SessionRepository::new(db.pool().clone());

    // Create
    let session = Session::new(
        Some("Test Session".to_string()),
        Some("claude-sonnet-4-5".to_string()),
        Some("anthropic".to_string()),
    );
    repo.create(&session)
        .await
        .expect("Failed to create session");

    // Read
    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find session");
    assert!(found.is_some());
    assert_eq!(
        found.as_ref().unwrap().title,
        Some("Test Session".to_string())
    );

    // Update
    let mut updated_session = session.clone();
    updated_session.title = Some("Updated Title".to_string());
    repo.update(&updated_session)
        .await
        .expect("Failed to update session");

    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find session");
    assert_eq!(found.unwrap().title, Some("Updated Title".to_string()));

    // Delete (soft-delete: row preserved with archived_at set)
    repo.delete(session.id)
        .await
        .expect("Failed to delete session");
    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find session");
    let found = found.expect("Soft-deleted session should still be findable by ID");
    assert!(
        found.archived_at.is_some(),
        "Soft-deleted session should have archived_at set"
    );
}

#[tokio::test]
async fn test_session_archive() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = SessionRepository::new(db.pool().clone());

    let session = Session::new(Some("Test".to_string()), Some("model".to_string()), None);
    repo.create(&session)
        .await
        .expect("Failed to create session");

    // Archive
    repo.archive(session.id).await.expect("Failed to archive");
    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find")
        .unwrap();
    assert!(found.is_archived());

    // Unarchive
    repo.unarchive(session.id)
        .await
        .expect("Failed to unarchive");
    let found = repo
        .find_by_id(session.id)
        .await
        .expect("Failed to find")
        .unwrap();
    assert!(!found.is_archived());
}

/// #2881: the rail's staleness signal. One aggregate, and it has to move when a
/// turn lands on any channel, because the rail cannot see those events.
#[tokio::test]
async fn ledger_signature_counts_rows_and_follows_the_newest_update() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = SessionRepository::new(db.pool().clone());

    assert_eq!(
        repo.ledger_signature().await.expect("empty signature"),
        (0, 0),
        "an empty ledger reads as no updates and no rows"
    );

    let first = Session::new(
        Some("First".to_string()),
        Some(String::new()),
        Some("anthropic".to_string()),
    );
    repo.create(&first).await.expect("Failed to create first");
    assert_eq!(
        repo.ledger_signature().await.expect("signature").1,
        1,
        "the row count follows creation"
    );

    let second = Session::new(
        Some("Second".to_string()),
        Some(String::new()),
        Some("anthropic".to_string()),
    );
    repo.create(&second).await.expect("Failed to create second");

    let (newest, count) = repo.ledger_signature().await.expect("signature");
    assert_eq!(count, 2);
    assert_eq!(
        newest,
        first
            .updated_at
            .timestamp()
            .max(second.updated_at.timestamp()),
        "the newest update is the signal the probe compares"
    );
}
