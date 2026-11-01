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
