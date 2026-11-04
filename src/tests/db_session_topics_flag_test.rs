//! The per-session topics flag (#2302): turning it on persists, reads back,
//! and does not move the session in the recency order.

use crate::db::Database;
use crate::db::models::Session;
use crate::db::repository::session::SessionRepository;

#[tokio::test]
async fn topics_flag_round_trips_and_keeps_recency() {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    let repo = SessionRepository::new(db.pool().clone());

    let session = Session::new(Some("t".to_string()), Some("m".to_string()), None);
    repo.create(&session).await.expect("create session");
    let before = repo
        .find_by_id(session.id)
        .await
        .expect("read")
        .expect("exists")
        .updated_at;

    assert!(
        repo.topic_enabled_ids()
            .await
            .expect("list")
            .iter()
            .all(|id| *id != session.id),
        "topics start off"
    );

    repo.set_topics_enabled(session.id, true)
        .await
        .expect("turn topics on");
    assert!(
        repo.topic_enabled_ids()
            .await
            .expect("list")
            .contains(&session.id),
        "the flag is read back from the database"
    );

    let after = repo
        .find_by_id(session.id)
        .await
        .expect("read")
        .expect("exists")
        .updated_at;
    assert_eq!(before, after, "turning topics on must not move the session");

    repo.set_topics_enabled(session.id, false)
        .await
        .expect("turn topics off");
    assert!(
        repo.topic_enabled_ids()
            .await
            .expect("list")
            .iter()
            .all(|id| *id != session.id)
    );
}

#[tokio::test]
async fn a_topic_is_created_under_a_session_and_listed_with_its_parent() {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    let repo = SessionRepository::new(db.pool().clone());

    let parent = Session::new(Some("p".to_string()), None, None);
    repo.create(&parent).await.expect("create parent");
    let topic = repo
        .create_topic(parent.id, "Design")
        .await
        .expect("a topic under a plain session");

    let pairs = repo.topic_parents().await.expect("list parents");
    assert!(pairs.contains(&(topic, parent.id)));

    assert!(
        repo.create_topic(topic, "Nested").await.is_err(),
        "depth is one: a topic cannot take a topic"
    );
}

#[tokio::test]
async fn a_topic_can_be_renamed_and_its_info_is_read() {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    let repo = SessionRepository::new(db.pool().clone());

    let parent = Session::new(Some("p".to_string()), None, None);
    repo.create(&parent).await.expect("create parent");
    let topic = repo
        .create_topic(parent.id, "Draft")
        .await
        .expect("create topic");

    repo.rename(topic, "Final").await.expect("rename");
    let renamed = repo.find_by_id(topic).await.expect("read").expect("exists");
    assert_eq!(renamed.title.as_deref(), Some("Final"));

    let (parent_id, _created, messages) = repo
        .topic_info(topic)
        .await
        .expect("info")
        .expect("topic exists");
    assert_eq!(parent_id, Some(parent.id));
    assert_eq!(messages, 0, "a new topic holds no messages");
}

#[tokio::test]
async fn a_topic_closes_and_reopens() {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    let repo = SessionRepository::new(db.pool().clone());

    let parent = Session::new(Some("p".to_string()), None, None);
    repo.create(&parent).await.expect("create parent");
    let topic = repo
        .create_topic(parent.id, "Archive me")
        .await
        .expect("create topic");

    repo.set_closed(topic, true).await.expect("close");
    assert!(repo.closed_ids().await.expect("list").contains(&topic));

    repo.set_closed(topic, false).await.expect("reopen");
    assert!(!repo.closed_ids().await.expect("list").contains(&topic));
}
