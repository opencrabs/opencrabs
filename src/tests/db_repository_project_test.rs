use crate::db::Database;
use crate::db::models::Project;
use crate::db::models::Session;
use crate::db::repository::SessionRepository;
use crate::db::repository::project::*;
use tokio;

#[tokio::test]
async fn test_project_crud() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = ProjectRepository::new(db.pool().clone());

    // Create
    let project = Project::new("Test Project".to_string(), Some("A test".to_string()));
    repo.create(&project).await.expect("Failed to create");

    // Read by ID
    let found = repo.find_by_id(project.id).await.expect("find_by_id");
    assert!(found.is_some());
    assert_eq!(found.as_ref().unwrap().name, "Test Project");
    assert_eq!(
        found.as_ref().unwrap().description,
        Some("A test".to_string())
    );

    // Read by name
    let found = repo
        .find_by_name("Test Project")
        .await
        .expect("find_by_name");
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, project.id);

    // Update
    let mut updated = project.clone();
    updated.name = "Updated Project".to_string();
    repo.update(&updated).await.expect("Failed to update");

    let found = repo.find_by_id(project.id).await.expect("find_by_id");
    assert_eq!(found.unwrap().name, "Updated Project");

    // Delete
    repo.delete(project.id).await.expect("Failed to delete");
    let found = repo.find_by_id(project.id).await.expect("find_by_id");
    assert!(found.is_none());
}

#[tokio::test]
async fn test_project_list_all() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = ProjectRepository::new(db.pool().clone());

    for i in 0..3 {
        let p = Project::new(format!("Project {}", i), None);
        repo.create(&p).await.expect("Failed to create");
    }

    let projects = repo.list_all().await.expect("Failed to list");
    assert_eq!(projects.len(), 3);
}

#[tokio::test]
async fn test_assign_unassign_session() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let project_repo = ProjectRepository::new(db.pool().clone());
    let session_repo = SessionRepository::new(db.pool().clone());

    let project = Project::new("Test".to_string(), None);
    project_repo.create(&project).await.expect("create project");

    let session = Session::new(
        Some("Test Session".to_string()),
        Some("model".to_string()),
        None,
    );
    session_repo.create(&session).await.expect("create session");

    // Assign
    project_repo
        .assign_session(session.id, project.id)
        .await
        .expect("assign");

    // Check session is in project
    let sessions = project_repo
        .find_sessions_by_project(project.id)
        .await
        .expect("find_sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session.id);

    // Unassigned should be empty
    let unassigned = project_repo
        .find_unassigned_sessions()
        .await
        .expect("find_unassigned");
    assert!(unassigned.is_empty());

    // Unassign
    project_repo
        .unassign_session(session.id)
        .await
        .expect("unassign");

    let sessions = project_repo
        .find_sessions_by_project(project.id)
        .await
        .expect("find_sessions");
    assert!(sessions.is_empty());

    let unassigned = project_repo
        .find_unassigned_sessions()
        .await
        .expect("find_unassigned");
    assert_eq!(unassigned.len(), 1);
}

#[tokio::test]
async fn assignable_sessions_include_other_projects_but_not_topics_or_archives() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let projects = ProjectRepository::new(db.pool().clone());
    let sessions = SessionRepository::new(db.pool().clone());
    let project = Project::new("Existing project".to_string(), None);
    projects.create(&project).await.expect("create project");

    let plain = Session::new(Some("Plain".to_string()), Some("m".to_string()), None);
    let assigned = Session::new(Some("Assigned".to_string()), Some("m".to_string()), None);
    let archived = Session::new(Some("Archived".to_string()), Some("m".to_string()), None);
    for session in [&plain, &assigned, &archived] {
        sessions.create(session).await.expect("create session");
    }
    projects
        .assign_session(assigned.id, project.id)
        .await
        .expect("assign session");
    sessions.archive(archived.id).await.expect("archive session");
    sessions
        .create_topic(plain.id, "Topic".to_string())
        .await
        .expect("create topic");

    let rows = projects.find_assignable_sessions().await.expect("list");
    let ids: Vec<_> = rows.iter().map(|session| session.id).collect();
    assert!(ids.contains(&plain.id));
    assert!(ids.contains(&assigned.id));
    assert!(!ids.contains(&archived.id));
    assert_eq!(rows.len(), 2, "the child topic is not an independent pick");
}

#[tokio::test]
async fn test_count_sessions_and_files() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let project_repo = ProjectRepository::new(db.pool().clone());
    let session_repo = SessionRepository::new(db.pool().clone());
    let file_repo = crate::db::repository::FileRepository::new(db.pool().clone());

    let project = Project::new("Test".to_string(), None);
    project_repo.create(&project).await.expect("create project");

    let s1 = Session::new(Some("S1".to_string()), Some("m".to_string()), None);
    let s2 = Session::new(Some("S2".to_string()), Some("m".to_string()), None);
    session_repo.create(&s1).await.expect("create s1");
    session_repo.create(&s2).await.expect("create s2");

    project_repo
        .assign_session(s1.id, project.id)
        .await
        .expect("assign s1");
    project_repo
        .assign_session(s2.id, project.id)
        .await
        .expect("assign s2");

    assert_eq!(
        project_repo
            .count_sessions(project.id)
            .await
            .expect("count"),
        2
    );

    // Add a file to s1
    let file = crate::db::models::File::new(s1.id, std::path::PathBuf::from("/test/file.rs"), None);
    file_repo.create(&file).await.expect("create file");

    assert_eq!(
        project_repo
            .count_files(project.id)
            .await
            .expect("count files"),
        1
    );
}

#[tokio::test]
async fn test_delete_project_unassigns_sessions() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let project_repo = ProjectRepository::new(db.pool().clone());
    let session_repo = SessionRepository::new(db.pool().clone());

    let project = Project::new("Test".to_string(), None);
    project_repo.create(&project).await.expect("create project");

    let session = Session::new(Some("S".to_string()), Some("m".to_string()), None);
    session_repo.create(&session).await.expect("create session");

    project_repo
        .assign_session(session.id, project.id)
        .await
        .expect("assign");

    // Delete project
    project_repo
        .delete(project.id)
        .await
        .expect("delete project");

    // Session should still exist but with NULL project_id
    let found = session_repo
        .find_by_id(session.id)
        .await
        .expect("find session")
        .expect("session should exist");
    assert!(found.project_id.is_none());
}

#[tokio::test]
async fn unassigned_sessions_exclude_topics() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let project_repo = ProjectRepository::new(db.pool().clone());
    let session_repo = SessionRepository::new(db.pool().clone());

    let project = Project::new("Parent sessions".to_string(), None);
    project_repo.create(&project).await.expect("create project");
    let parent = Session::new(Some("Parent".to_string()), Some("m".to_string()), None);
    session_repo.create(&parent).await.expect("create parent");
    let topic_id = session_repo
        .create_topic(parent.id, "Topic inside parent")
        .await
        .expect("create topic");

    let unassigned = project_repo
        .find_unassigned_sessions()
        .await
        .expect("find unassigned parents");
    assert_eq!(unassigned.len(), 1);
    assert_eq!(unassigned[0].id, parent.id);
    assert!(unassigned.iter().all(|session| session.id != topic_id));

    project_repo
        .assign_session(parent.id, project.id)
        .await
        .expect("assign parent");

    let assigned = project_repo
        .find_sessions_by_project(project.id)
        .await
        .expect("find assigned parents");
    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].id, parent.id);
    assert!(
        project_repo
            .find_unassigned_sessions()
            .await
            .expect("find remaining parents")
            .is_empty(),
        "a topic never remains as an independent unassigned pick"
    );
}
