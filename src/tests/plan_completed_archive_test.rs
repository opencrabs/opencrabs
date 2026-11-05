use crate::brain::agent::service::messaging::persist_completed_plan_before_archive;
use crate::config::profile::{home_for_profile, with_profile_home_async};
use crate::db::Database;
use crate::services::message::{MessageService, PLAN_COMPLETED_ROLE};
use crate::services::{ServiceContext, SessionService};
use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use crate::utils::plan_files::{archive_dir, plan_json_path, save_plan};
use uuid::Uuid;

async fn in_temp_home<F>(f: F)
where
    F: std::future::Future<Output = ()>,
{
    let profile = format!("plan-completed-archive-test-{}", Uuid::new_v4());
    with_profile_home_async(Some(&profile), f).await;
    let _ = std::fs::remove_dir_all(home_for_profile(Some(&profile)));
}

async fn services() -> (MessageService, SessionService) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    (
        MessageService::new(context.clone()),
        SessionService::new(context),
    )
}

fn plan(session_id: Uuid, statuses: &[TaskStatus]) -> PlanDocument {
    let mut plan = PlanDocument::new(session_id, "Archive after history".to_string());
    plan.status = PlanStatus::Active;
    for (index, status) in statuses.iter().enumerate() {
        let mut task = PlanTask::new(
            index + 1,
            format!("Task {}", index + 1),
            "description".to_string(),
            TaskType::Test,
        );
        task.status = status.clone();
        plan.add_task(task);
    }
    plan
}

fn archived_json_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count()
}

#[tokio::test]
async fn completed_plan_persisted_before_archive_complete() {
    in_temp_home(async {
        let (messages, sessions) = services().await;
        let session = sessions
            .create_session(Some("Test".to_string()))
            .await
            .unwrap();
        let plan = plan(session.id, &[TaskStatus::Completed, TaskStatus::Skipped]);
        save_plan(&plan).await.unwrap();

        let record = persist_completed_plan_before_archive(&messages, &plan)
            .await
            .expect("complete plan returns its durable history row");
        assert_eq!(record.role, PLAN_COMPLETED_ROLE);
        assert_eq!(record.session_id, session.id);
        assert!(!plan_json_path(session.id).await.exists());
        assert_eq!(archived_json_count(&archive_dir(session.id).await), 1);

        let stored = messages
            .list_messages_for_session(session.id)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, record.id);
    })
    .await;
}

#[tokio::test]
async fn completed_plan_persisted_before_archive_rejects_partial() {
    in_temp_home(async {
        let (messages, sessions) = services().await;
        let session = sessions
            .create_session(Some("Test".to_string()))
            .await
            .unwrap();
        let plan = plan(session.id, &[TaskStatus::Completed, TaskStatus::Pending]);
        save_plan(&plan).await.unwrap();

        assert!(
            persist_completed_plan_before_archive(&messages, &plan)
                .await
                .is_none()
        );
        assert!(plan_json_path(session.id).await.exists());
        assert_eq!(archived_json_count(&archive_dir(session.id).await), 0);
        assert_eq!(
            messages
                .count_messages_in_session(session.id)
                .await
                .unwrap(),
            0
        );
    })
    .await;
}

#[tokio::test]
async fn completed_plan_persisted_before_archive_skips_archive_on_db_failure() {
    in_temp_home(async {
        let (messages, _sessions) = services().await;
        // No matching session row makes the message insert fail its foreign key.
        let session_id = Uuid::new_v4();
        let plan = plan(session_id, &[TaskStatus::Completed]);
        save_plan(&plan).await.unwrap();

        assert!(
            persist_completed_plan_before_archive(&messages, &plan)
                .await
                .is_none()
        );
        assert!(plan_json_path(session_id).await.exists());
        assert_eq!(archived_json_count(&archive_dir(session_id).await), 0);
    })
    .await;
}

#[test]
fn completed_plan_persisted_before_archive_source_order() {
    const SOURCE: &str = include_str!("../brain/agent/service/messaging.rs");
    let write = SOURCE.find("record_completed_plan(plan).await").unwrap();
    let archive = SOURCE.find("archive_plan(plan.session_id).await").unwrap();
    assert!(write < archive);
}
