use crate::db::repository::SessionListOptions;
use crate::services::ServiceContext;
use crate::services::session::*;
use uuid::Uuid;

async fn create_test_service() -> SessionService {
    use crate::db::Database;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let pool = db.pool().clone();

    let context = ServiceContext::new(pool);
    SessionService::new(context)
}

#[tokio::test]
async fn test_create_session() {
    let service = create_test_service().await;
    let session = service
        .create_session(Some("Test Session".to_string()))
        .await
        .unwrap();

    assert_eq!(session.title, Some("Test Session".to_string()));
    assert_eq!(session.token_count, 0);
    assert_eq!(session.total_cost, 0.0);
    assert!(session.archived_at.is_none());
}

#[tokio::test]
async fn test_get_session() {
    let service = create_test_service().await;
    let created = service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    let found = service.get_session(created.id).await.unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, created.id);
}

#[tokio::test]
async fn test_get_session_required() {
    let service = create_test_service().await;
    let created = service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    let found = service.get_session_required(created.id).await.unwrap();
    assert_eq!(found.id, created.id);

    // Test non-existent session
    let result = service.get_session_required(Uuid::new_v4()).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_update_session_title() {
    let service = create_test_service().await;
    let session = service
        .create_session(Some("Original".to_string()))
        .await
        .unwrap();

    service
        .update_session_title(session.id, Some("Updated".to_string()))
        .await
        .unwrap();

    let updated = service.get_session_required(session.id).await.unwrap();
    assert_eq!(updated.title, Some("Updated".to_string()));
}

#[tokio::test]
async fn test_update_session_usage() {
    let service = create_test_service().await;
    let session = service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    service
        .update_session_usage(session.id, 100, 0.05, "xiaomi", "mimo-v2.5-pro", None, None)
        .await
        .unwrap();
    service
        .update_session_usage(session.id, 50, 0.025, "xiaomi", "mimo-v2.5-pro", None, None)
        .await
        .unwrap();

    let updated = service.get_session_required(session.id).await.unwrap();
    assert_eq!(updated.token_count, 150);
    assert!((updated.total_cost - 0.075).abs() < 0.0001);
}

#[tokio::test]
async fn test_archive_unarchive_session() {
    let service = create_test_service().await;
    let session = service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    // Archive
    service.archive_session(session.id).await.unwrap();
    let archived = service.get_session_required(session.id).await.unwrap();
    assert!(archived.archived_at.is_some());

    // Unarchive
    service.unarchive_session(session.id).await.unwrap();
    let unarchived = service.get_session_required(session.id).await.unwrap();
    assert!(unarchived.archived_at.is_none());
}

#[tokio::test]
async fn test_delete_session() {
    let service = create_test_service().await;
    let session = service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    service.delete_session(session.id).await.unwrap();

    // Session row preserved (soft-delete) for usage tracking, but archived
    let result = service.get_session(session.id).await.unwrap();
    assert!(result.is_some());
    assert!(result.unwrap().archived_at.is_some());
}

#[tokio::test]
async fn test_list_sessions() {
    let service = create_test_service().await;

    // Create multiple sessions
    service
        .create_session(Some("Session 1".to_string()))
        .await
        .unwrap();
    service
        .create_session(Some("Session 2".to_string()))
        .await
        .unwrap();
    service
        .create_session(Some("Session 3".to_string()))
        .await
        .unwrap();

    let options = SessionListOptions {
        include_archived: false,
        limit: None,
        offset: 0,
        query: None,
        include_subagents: false,
    };

    let sessions = service.list_sessions(options).await.unwrap();
    assert_eq!(sessions.len(), 3);
}

#[tokio::test]
async fn test_get_most_recent_session() {
    let service = create_test_service().await;

    // Create an earlier session so session2 below is the "most recent".
    // We only need the row to exist; the returned value isn't used.
    service
        .create_session(Some("Session 1".to_string()))
        .await
        .unwrap();
    // Sleep for 1 second to ensure different Unix timestamps (which have second resolution)
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    let session2 = service
        .create_session(Some("Session 2".to_string()))
        .await
        .unwrap();

    let recent = service.get_most_recent_session().await.unwrap();
    assert!(recent.is_some());
    assert_eq!(recent.unwrap().id, session2.id);
}

#[tokio::test]
async fn test_count_sessions() {
    let service = create_test_service().await;

    service
        .create_session(Some("Session 1".to_string()))
        .await
        .unwrap();
    let session2 = service
        .create_session(Some("Session 2".to_string()))
        .await
        .unwrap();
    service
        .create_session(Some("Session 3".to_string()))
        .await
        .unwrap();

    // Archive one session
    service.archive_session(session2.id).await.unwrap();

    let active_count = service.count_sessions().await.unwrap();
    let archived_count = service.count_archived_sessions().await.unwrap();

    assert_eq!(active_count, 2);
    assert_eq!(archived_count, 1);
}
