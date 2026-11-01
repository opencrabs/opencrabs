//! The topics schema (#2301): a topic is a session with `parent_id` set. Depth
//! is one, a parent's delete removes its topics, and existing rows keep their
//! defaults.

use crate::db::Database;
use uuid::Uuid;

async fn fresh_db() -> Database {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    db
}

async fn run<T, F>(db: &Database, f: F) -> rusqlite::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
{
    db.pool()
        .get()
        .await
        .expect("pool connection")
        .interact(f)
        .await
        .expect("interact")
}

async fn insert_session(db: &Database, id: Uuid, parent: Option<Uuid>) -> rusqlite::Result<()> {
    run(db, move |conn| {
        conn.execute(
            "INSERT INTO sessions (id, created_at, updated_at, parent_id) VALUES (?1, 0, 0, ?2)",
            rusqlite::params![id.to_string(), parent.map(|p| p.to_string())],
        )
        .map(|_| ())
    })
    .await
}

async fn count_children(db: &Database, parent: Uuid) -> i64 {
    run(db, move |conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent_id = ?1",
            rusqlite::params![parent.to_string()],
            |row| row.get(0),
        )
    })
    .await
    .expect("count query")
}

#[tokio::test]
async fn an_existing_session_is_not_a_topic_and_has_no_closed_time() {
    let db = fresh_db().await;
    let id = Uuid::new_v4();
    insert_session(&db, id, None).await.expect("insert");

    let (topics, parent, closed): (i64, Option<String>, Option<i64>) = run(&db, move |conn| {
        conn.query_row(
            "SELECT topics_enabled, parent_id, closed_at FROM sessions WHERE id = ?1",
            rusqlite::params![id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    })
    .await
    .expect("defaults");

    assert_eq!(topics, 0, "topics stay off for every existing session");
    assert!(parent.is_none());
    assert!(closed.is_none());
}

#[tokio::test]
async fn a_topic_under_a_session_is_accepted() {
    let db = fresh_db().await;
    let parent = Uuid::new_v4();
    let topic = Uuid::new_v4();
    insert_session(&db, parent, None).await.expect("parent");
    insert_session(&db, topic, Some(parent))
        .await
        .expect("a topic under a plain session is allowed");
    assert_eq!(count_children(&db, parent).await, 1);
}

#[tokio::test]
async fn a_topic_under_a_topic_is_refused() {
    let db = fresh_db().await;
    let parent = Uuid::new_v4();
    let topic = Uuid::new_v4();
    let grandchild = Uuid::new_v4();
    insert_session(&db, parent, None).await.expect("parent");
    insert_session(&db, topic, Some(parent))
        .await
        .expect("topic");
    assert!(
        insert_session(&db, grandchild, Some(topic)).await.is_err(),
        "depth is one: a topic cannot have a topic"
    );
}

#[tokio::test]
async fn re_parenting_under_a_topic_is_refused() {
    let db = fresh_db().await;
    let parent = Uuid::new_v4();
    let topic = Uuid::new_v4();
    let other = Uuid::new_v4();
    insert_session(&db, parent, None).await.expect("parent");
    insert_session(&db, topic, Some(parent))
        .await
        .expect("topic");
    insert_session(&db, other, None).await.expect("other");

    let result = run(&db, move |conn| {
        conn.execute(
            "UPDATE sessions SET parent_id = ?1 WHERE id = ?2",
            rusqlite::params![topic.to_string(), other.to_string()],
        )
        .map(|_| ())
    })
    .await;
    assert!(result.is_err(), "a session cannot move under a topic");
}

#[tokio::test]
async fn deleting_a_parent_removes_its_topics() {
    let db = fresh_db().await;
    let parent = Uuid::new_v4();
    let topic = Uuid::new_v4();
    insert_session(&db, parent, None).await.expect("parent");
    insert_session(&db, topic, Some(parent))
        .await
        .expect("topic");

    run(&db, move |conn| {
        conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![parent.to_string()],
        )
        .map(|_| ())
    })
    .await
    .expect("delete parent");

    let remaining: i64 = run(&db, move |conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = ?1",
            rusqlite::params![topic.to_string()],
            |row| row.get(0),
        )
    })
    .await
    .expect("count");
    assert_eq!(remaining, 0, "the topic goes with its parent");
}
