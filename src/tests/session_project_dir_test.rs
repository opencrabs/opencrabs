//! A `/cd` moves the whole project, not one conversation (#3036).
//!
//! A topic is a session inside a session (#2301), and a parent with its topics
//! is one project: the same tree, the same repo, the same rules. Moving only
//! the session the operator typed in left the group reading two directories
//! nobody chose, which is what the report showed after a picker landed in a
//! split window. These tests pin the family resolution and the move, on the
//! handle the next turn reads and on the row the next process restores.

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::db::repository::SessionRepository;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use std::sync::Arc;
use uuid::Uuid;

/// An in-memory service plus the pool its rows live in. No global cwd is
/// touched by these tests: they call `set_project_directory`, not
/// `apply_operator_cd`, so one test cannot move what another one reads.
async fn service() -> (AgentService, SessionRepository) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let pool = db.pool().clone();
    let repo = SessionRepository::new(pool.clone());
    let context = ServiceContext::new(pool);
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    (AgentService::new_for_test(provider, context).await, repo)
}

async fn session(repo: &SessionRepository, title: &str) -> Uuid {
    let row = crate::db::models::Session::new(Some(title.to_string()), None, None);
    let id = row.id;
    repo.create(&row).await.unwrap();
    id
}

/// The parent plus every topic under it: one project, one directory.
#[tokio::test]
async fn a_cd_on_the_parent_moves_every_topic() {
    let (svc, repo) = service().await;
    let parent = session(&repo, "project").await;
    let first = repo.create_topic(parent, "topic one").await.unwrap();
    let second = repo.create_topic(parent, "topic two").await.unwrap();

    let dir = std::path::PathBuf::from("/tmp/project-root");
    svc.set_project_directory(parent, dir.clone()).await;

    for id in [parent, first, second] {
        assert_eq!(
            svc.get_working_directory_for_session(id),
            dir,
            "session {id} reads the project's directory"
        );
        let row = crate::services::SessionService::new(svc.context().clone())
            .get_session_required(id)
            .await
            .unwrap();
        assert_eq!(
            row.working_directory,
            Some(dir.display().to_string()),
            "and the row a resume reads carries it too"
        );
    }
}

/// A topic asked for directly moves the parent and its siblings: the project
/// is the group, not the row the cursor happened to be on.
#[tokio::test]
async fn a_cd_on_a_topic_moves_the_parent_and_its_siblings() {
    let (svc, repo) = service().await;
    let parent = session(&repo, "project").await;
    let first = repo.create_topic(parent, "topic one").await.unwrap();
    let second = repo.create_topic(parent, "topic two").await.unwrap();

    let dir = std::path::PathBuf::from("/tmp/from-a-topic");
    svc.set_project_directory(second, dir.clone()).await;

    for id in [parent, first, second] {
        assert_eq!(
            svc.get_working_directory_for_session(id),
            dir,
            "the whole project moved"
        );
    }
}

/// A session with no topics is a project of one: nothing else moves, and the
/// session still moves.
#[tokio::test]
async fn a_lone_session_moves_alone() {
    let (svc, repo) = service().await;
    let lonely = session(&repo, "on its own").await;
    let other = session(&repo, "somebody else").await;

    let dir = std::path::PathBuf::from("/tmp/lonely");
    svc.set_project_directory(lonely, dir.clone()).await;

    assert_eq!(svc.get_working_directory_for_session(lonely), dir);
    assert_ne!(
        svc.get_working_directory_for_session(other),
        dir,
        "an unrelated session keeps its own directory"
    );
}

/// The family always names the session the operator asked about, so a read
/// that fails can never leave that one behind.
#[tokio::test]
async fn the_family_always_contains_the_session_asked_for() {
    let (svc, repo) = service().await;
    let parent = session(&repo, "project").await;
    let topic = repo.create_topic(parent, "topic").await.unwrap();

    for asked in [parent, topic] {
        let family = svc.project_family(asked).await;
        assert!(
            family.contains(&asked),
            "a topic asked for directly is part of its own project"
        );
        assert!(family.contains(&parent), "and so is the parent");
        assert_eq!(family.len(), 2, "one parent and one topic, no duplicates");
    }
}
