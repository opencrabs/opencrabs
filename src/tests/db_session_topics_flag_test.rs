//! The per-session topics flag (#2302): turning it on persists, reads back,
//! and does not move the session in the recency order.

use crate::db::Database;
use crate::db::models::{Message, Session};
use crate::db::repository::message::MessageRepository;
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
async fn creating_a_topic_switches_topics_on_for_the_parent() {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    let repo = SessionRepository::new(db.pool().clone());

    let parent = Session::new(Some("p".to_string()), None, None);
    repo.create(&parent).await.expect("create parent");
    assert!(
        !repo
            .topic_enabled_ids()
            .await
            .expect("list")
            .contains(&parent.id),
        "topics start off"
    );

    repo.create_topic(parent.id, "Design")
        .await
        .expect("create topic");

    assert!(
        repo.topic_enabled_ids()
            .await
            .expect("list")
            .contains(&parent.id),
        "the parent reads back with topics on, so the chat topic bar and the \
         rail menu agree with the tree"
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

/// A parent holding two rows, with two topics under it that hold two rows
/// each. The fold tests below all start from this shape.
async fn seeded_family() -> (
    Database,
    SessionRepository,
    MessageRepository,
    uuid::Uuid,
    Vec<uuid::Uuid>,
) {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    let sessions = SessionRepository::new(db.pool().clone());
    let messages = MessageRepository::new(db.pool().clone());

    let parent = Session::new(Some("parent".to_string()), None, None);
    sessions.create(&parent).await.expect("create parent");
    messages
        .create(&Message::new(parent.id, "user".into(), "p0".into(), 0))
        .await
        .expect("parent row 0");
    messages
        .create(&Message::new(parent.id, "assistant".into(), "p1".into(), 1))
        .await
        .expect("parent row 1");

    let mut topics = Vec::new();
    for (index, name) in ["Design", "Build"].into_iter().enumerate() {
        let topic = sessions
            .create_topic(parent.id, name)
            .await
            .expect("create topic");
        for row in 0..2 {
            messages
                .create(&Message::new(
                    topic,
                    "user".into(),
                    format!("t{index}-{row}"),
                    row,
                ))
                .await
                .expect("topic row");
        }
        topics.push(topic);
    }
    (db, sessions, messages, parent.id, topics)
}

#[tokio::test]
async fn folding_topics_moves_every_row_into_the_parent_in_topic_order() {
    let (_db, sessions, messages, parent, topics) = seeded_family().await;

    let folded = sessions
        .merge_topics_into_parent(parent)
        .await
        .expect("fold the family");
    assert_eq!(folded, 2, "both topics are folded in");

    let landed = messages
        .find_recent_by_session(parent, 50)
        .await
        .expect("read the parent");
    let bodies: Vec<&str> = landed.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        bodies,
        vec!["p0", "p1", "t0-0", "t0-1", "t1-0", "t1-1"],
        "the parent keeps its own rows, then each topic's rows in topic order"
    );

    for topic in &topics {
        let left = messages
            .find_recent_by_session(*topic, 50)
            .await
            .expect("read the topic");
        assert!(left.is_empty(), "a folded topic holds no rows of its own");
    }
}

#[tokio::test]
async fn folding_topics_archives_them_and_switches_the_flag_off() {
    let (_db, sessions, _messages, parent, topics) = seeded_family().await;

    sessions
        .merge_topics_into_parent(parent)
        .await
        .expect("fold the family");

    assert!(
        !sessions
            .topic_enabled_ids()
            .await
            .expect("list")
            .contains(&parent),
        "the parent reads back with topics off, so the strip stops drawing"
    );
    let archived = sessions.list_archived().await.expect("list archived");
    for topic in &topics {
        assert!(
            archived.iter().any(|s| s.id == *topic),
            "the emptied topic is archived, not deleted, so usage_ledger keeps \
             its owner"
        );
    }
    assert!(
        sessions
            .topic_parents()
            .await
            .expect("list parents")
            .iter()
            .all(|(child, _)| !topics.contains(child)),
        "an archived topic is no longer a child of the family"
    );
}

#[tokio::test]
async fn folding_keeps_a_tool_call_next_to_its_result() {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    let sessions = SessionRepository::new(db.pool().clone());
    let messages = MessageRepository::new(db.pool().clone());

    let parent = Session::new(Some("parent".to_string()), None, None);
    sessions.create(&parent).await.expect("create parent");
    let topic = sessions
        .create_topic(parent.id, "Tools")
        .await
        .expect("create topic");
    for (sequence, role, body) in [
        (0, "user", "run it"),
        (1, "assistant", "calling the tool"),
        (2, "tool", "the result"),
        (3, "assistant", "done"),
    ] {
        messages
            .create(&Message::new(topic, role.into(), body.into(), sequence))
            .await
            .expect("row");
    }

    sessions
        .merge_topics_into_parent(parent)
        .await
        .expect("fold");

    let landed = messages
        .find_recent_by_session(parent, 50)
        .await
        .expect("read the parent");
    let bodies: Vec<&str> = landed.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        bodies,
        vec!["run it", "calling the tool", "the result", "done"],
        "the call and its result stay adjacent instead of being reordered by \
         timestamp"
    );
}

#[tokio::test]
async fn deleting_the_last_live_topic_switches_topics_off() {
    let (_db, sessions, _messages, parent, topics) = seeded_family().await;

    sessions.delete(topics[0]).await.expect("delete first");
    assert!(
        sessions
            .topic_enabled_ids()
            .await
            .expect("list")
            .contains(&parent),
        "one topic is still live, so the family keeps its topics"
    );

    sessions.delete(topics[1]).await.expect("delete last");
    assert!(
        !sessions
            .topic_enabled_ids()
            .await
            .expect("list")
            .contains(&parent),
        "a family with no live topic is not a family (#2863)"
    );
}
