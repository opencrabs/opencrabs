use crate::db::Database;
use crate::db::models::Message;
use crate::db::models::Session;
use crate::db::repository::SessionRepository;
use crate::db::repository::message::*;
use tokio;

#[tokio::test]
async fn test_message_crud() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let session_repo = SessionRepository::new(db.pool().clone());
    let message_repo = MessageRepository::new(db.pool().clone());

    // Create session first
    let session = Session::new(Some("Test".to_string()), Some("model".to_string()), None);
    session_repo
        .create(&session)
        .await
        .expect("Failed to create session");

    // Create message
    let message = Message::new(session.id, "user".to_string(), "Hello!".to_string(), 1);
    message_repo
        .create(&message)
        .await
        .expect("Failed to create message");

    // Read
    let found = message_repo
        .find_by_id(message.id)
        .await
        .expect("Failed to find");
    assert!(found.is_some());
    assert_eq!(found.unwrap().content, "Hello!");

    // Update
    let mut updated = message.clone();
    updated.content = "Updated content".to_string();
    message_repo
        .update(&updated)
        .await
        .expect("Failed to update");

    let found = message_repo
        .find_by_id(message.id)
        .await
        .expect("Failed to find");
    assert_eq!(found.unwrap().content, "Updated content");

    // Delete
    message_repo
        .delete(message.id)
        .await
        .expect("Failed to delete");
    let found = message_repo
        .find_by_id(message.id)
        .await
        .expect("Failed to find");
    assert!(found.is_none());
}

#[tokio::test]
async fn test_message_list_by_session() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let session_repo = SessionRepository::new(db.pool().clone());
    let message_repo = MessageRepository::new(db.pool().clone());

    let session = Session::new(Some("Test".to_string()), Some("model".to_string()), None);
    session_repo
        .create(&session)
        .await
        .expect("Failed to create session");

    // Create multiple messages
    for i in 0..3 {
        let msg = Message::new(
            session.id,
            "user".to_string(),
            format!("Message {}", i),
            i + 1,
        );
        message_repo
            .create(&msg)
            .await
            .expect("Failed to create message");
    }

    let messages = message_repo
        .list_by_session(session.id)
        .await
        .expect("Failed to list");
    assert_eq!(messages.len(), 3);

    let count = message_repo
        .count_by_session(session.id)
        .await
        .expect("Failed to count");
    assert_eq!(count, 3);
}

/// #730: a turn cancelled before any reply leaves a user query plus an empty
/// assistant placeholder. The pair must be removed together so the query does
/// not linger in the next turn's context and duplicate on resend.
#[tokio::test]
async fn delete_trailing_unanswered_pair_removes_query_and_empty_placeholder() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let session_repo = SessionRepository::new(db.pool().clone());
    let repo = MessageRepository::new(db.pool().clone());

    let session = Session::new(Some("t".to_string()), Some("m".to_string()), None);
    session_repo.create(&session).await.unwrap();

    // A prior answered pair that must survive untouched.
    repo.create(&Message::new(session.id, "user".into(), "first".into(), 1))
        .await
        .unwrap();
    repo.create(&Message::new(
        session.id,
        "assistant".into(),
        "a reply".into(),
        2,
    ))
    .await
    .unwrap();
    // The cancelled turn: a query and an empty assistant placeholder.
    repo.create(&Message::new(session.id, "user".into(), "go".into(), 3))
        .await
        .unwrap();
    repo.create(&Message::new(
        session.id,
        "assistant".into(),
        String::new(),
        4,
    ))
    .await
    .unwrap();

    let deleted = repo
        .delete_trailing_unanswered_pair(session.id)
        .await
        .unwrap();
    assert_eq!(
        deleted, 2,
        "the query and its empty placeholder are removed"
    );

    let remaining = repo.list_by_session(session.id).await.unwrap();
    assert_eq!(remaining.len(), 2, "only the answered pair survives");
    assert_eq!(remaining[0].content, "first");
    assert_eq!(remaining[1].content, "a reply");
}

/// The guard: a trailing assistant row that actually holds a reply is a real
/// answered turn and must never be disturbed.
#[tokio::test]
async fn delete_trailing_unanswered_pair_leaves_answered_turn_alone() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let session_repo = SessionRepository::new(db.pool().clone());
    let repo = MessageRepository::new(db.pool().clone());

    let session = Session::new(Some("t".to_string()), Some("m".to_string()), None);
    session_repo.create(&session).await.unwrap();

    repo.create(&Message::new(session.id, "user".into(), "go".into(), 1))
        .await
        .unwrap();
    repo.create(&Message::new(
        session.id,
        "assistant".into(),
        "done".into(),
        2,
    ))
    .await
    .unwrap();

    let deleted = repo
        .delete_trailing_unanswered_pair(session.id)
        .await
        .unwrap();
    assert_eq!(deleted, 0, "an answered turn is untouched");
    assert_eq!(repo.list_by_session(session.id).await.unwrap().len(), 2);
}

/// #1166: find_recent_by_session returns the LAST n messages in
/// chronological order, pushing the LIMIT into SQL instead of loading the
/// whole history.
#[tokio::test]
async fn find_recent_by_session_returns_chronological_tail() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let session_repo = SessionRepository::new(db.pool().clone());
    let repo = MessageRepository::new(db.pool().clone());

    let session = Session::new(Some("t".to_string()), Some("m".to_string()), None);
    session_repo.create(&session).await.unwrap();

    for i in 1..=5 {
        repo.create(&Message::new(
            session.id,
            "user".into(),
            format!("msg{}", i),
            i,
        ))
        .await
        .unwrap();
    }

    // Tail of 2 = [msg4, msg5] — oldest-first within the window.
    let tail = repo.find_recent_by_session(session.id, 2).await.unwrap();
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].content, "msg4");
    assert_eq!(tail[1].content, "msg5");

    // A limit larger than the history returns everything, still ordered.
    let all = repo.find_recent_by_session(session.id, 100).await.unwrap();
    assert_eq!(all.len(), 5);
    assert_eq!(all[0].content, "msg1");

    // Empty session yields an empty vec rather than an error.
    let empty_session = Session::new(Some("e".to_string()), Some("m".to_string()), None);
    session_repo.create(&empty_session).await.unwrap();
    let none = repo
        .find_recent_by_session(empty_session.id, 10)
        .await
        .unwrap();
    assert!(none.is_empty());
}

/// #2281: find_recent_before_sequence pages strictly older rows, newest of
/// those first, returned oldest-first, so PageUp never re-reads the tail.
#[tokio::test]
async fn find_recent_before_sequence_pages_older_rows() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let session_repo = SessionRepository::new(db.pool().clone());
    let repo = MessageRepository::new(db.pool().clone());

    let session = Session::new(Some("t".to_string()), Some("m".to_string()), None);
    session_repo.create(&session).await.unwrap();

    for i in 1..=5 {
        repo.create(&Message::new(
            session.id,
            "user".into(),
            format!("msg{}", i),
            i,
        ))
        .await
        .unwrap();
    }

    // Older than sequence 4, up to 2 rows: [msg2, msg3].
    let page = repo
        .find_recent_before_sequence(session.id, 4, 2)
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].content, "msg2");
    assert_eq!(page[1].content, "msg3");

    // Fewer rows exist than the limit: returns just what is left.
    let rest = repo
        .find_recent_before_sequence(session.id, 2, 10)
        .await
        .unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].content, "msg1");

    // Nothing older than the first row.
    let none = repo
        .find_recent_before_sequence(session.id, 1, 10)
        .await
        .unwrap();
    assert!(none.is_empty());
}

/// An edited message replaces the row it came from and drops everything the
/// conversation said after it (#2826). The row at the edited sequence has to
/// survive, or the resend has nothing to replace, and another conversation's
/// rows are never part of the question.
#[tokio::test]
async fn delete_after_keeps_the_rows_at_or_before_the_sequence() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let session_repo = SessionRepository::new(db.pool().clone());
    let message_repo = MessageRepository::new(db.pool().clone());

    let session = Session::new(Some("Edit".to_string()), Some("model".to_string()), None);
    session_repo
        .create(&session)
        .await
        .expect("Failed to create session");
    let other = Session::new(Some("Other".to_string()), Some("model".to_string()), None);
    session_repo
        .create(&other)
        .await
        .expect("Failed to create session");

    for (i, (role, text)) in [
        ("user", "first question"),
        ("assistant", "first answer"),
        ("user", "second question"),
        ("assistant", "second answer"),
    ]
    .iter()
    .enumerate()
    {
        let message = Message::new(session.id, role.to_string(), text.to_string(), i as i32 + 1);
        message_repo
            .create(&message)
            .await
            .expect("Failed to create message");
    }
    let elsewhere = Message::new(
        other.id,
        "user".to_string(),
        "another conversation".to_string(),
        1,
    );
    message_repo
        .create(&elsewhere)
        .await
        .expect("Failed to create message");

    // The edited row is sequence 3: it stays, and only the answer after it goes.
    let deleted = message_repo
        .delete_after(session.id, 3)
        .await
        .expect("Failed to delete after a sequence");
    assert_eq!(deleted, 1, "only the row after the edited one is dropped");

    let left = message_repo
        .find_by_session(session.id)
        .await
        .expect("Failed to read the rows back");
    assert_eq!(
        left.iter().map(|m| m.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the edited row and everything before it survive"
    );
    assert_eq!(
        left.last().map(|m| m.content.as_str()),
        Some("second question"),
        "the row the edit replaces is still the tail"
    );

    let untouched = message_repo
        .find_by_session(other.id)
        .await
        .expect("Failed to read the other session");
    assert_eq!(untouched.len(), 1, "another session is never truncated");
}
