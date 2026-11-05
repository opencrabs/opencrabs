use crate::services::ServiceContext;
use crate::services::SessionService;
use crate::services::message::*;
use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};

async fn create_test_service() -> (MessageService, SessionService) {
    use crate::db::Database;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let pool = db.pool().clone();

    let context = ServiceContext::new(pool);
    (
        MessageService::new(context.clone()),
        SessionService::new(context),
    )
}

#[tokio::test]
async fn test_create_message() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    let message = message_service
        .create_message(session.id, "user".to_string(), "Hello".to_string())
        .await
        .unwrap();

    assert_eq!(message.session_id, session.id);
    assert_eq!(message.role, "user");
    assert_eq!(message.content, "Hello");
    assert_eq!(message.sequence, 1);
}

#[tokio::test]
async fn test_get_message() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    let created = message_service
        .create_message(session.id, "user".to_string(), "Test".to_string())
        .await
        .unwrap();

    let found = message_service.get_message(created.id).await.unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, created.id);
}

#[tokio::test]
async fn test_list_messages_for_session() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    message_service
        .create_message(session.id, "user".to_string(), "Message 1".to_string())
        .await
        .unwrap();
    message_service
        .create_message(session.id, "assistant".to_string(), "Message 2".to_string())
        .await
        .unwrap();

    let messages = message_service
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].sequence, 1);
    assert_eq!(messages[1].sequence, 2);
}

#[tokio::test]
async fn test_update_message_usage() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    let message = message_service
        .create_message(session.id, "user".to_string(), "Test".to_string())
        .await
        .unwrap();

    message_service
        .update_message_usage(
            message.id,
            crate::services::message::MessageUsage {
                token_count: 100,
                cost: 0.05,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let updated = message_service
        .get_message_required(message.id)
        .await
        .unwrap();
    assert_eq!(updated.token_count, Some(100));
    assert_eq!(updated.cost, Some(0.05));
}

#[tokio::test]
async fn test_delete_message() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    let message = message_service
        .create_message(session.id, "user".to_string(), "Test".to_string())
        .await
        .unwrap();

    message_service.delete_message(message.id).await.unwrap();

    let result = message_service.get_message(message.id).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_delete_messages_for_session() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    message_service
        .create_message(session.id, "user".to_string(), "Message 1".to_string())
        .await
        .unwrap();
    message_service
        .create_message(session.id, "assistant".to_string(), "Message 2".to_string())
        .await
        .unwrap();

    message_service
        .delete_messages_for_session(session.id)
        .await
        .unwrap();

    let messages = message_service
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    assert_eq!(messages.len(), 0);
}

#[tokio::test]
async fn test_count_messages_in_session() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    message_service
        .create_message(session.id, "user".to_string(), "Message 1".to_string())
        .await
        .unwrap();
    message_service
        .create_message(session.id, "assistant".to_string(), "Message 2".to_string())
        .await
        .unwrap();

    let count = message_service
        .count_messages_in_session(session.id)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn test_get_last_message() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    message_service
        .create_message(session.id, "user".to_string(), "First".to_string())
        .await
        .unwrap();
    let last = message_service
        .create_message(session.id, "assistant".to_string(), "Last".to_string())
        .await
        .unwrap();

    let result = message_service.get_last_message(session.id).await.unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().id, last.id);
}

#[tokio::test]
async fn test_get_messages_by_role() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    message_service
        .create_message(session.id, "user".to_string(), "User 1".to_string())
        .await
        .unwrap();
    message_service
        .create_message(
            session.id,
            "assistant".to_string(),
            "Assistant 1".to_string(),
        )
        .await
        .unwrap();
    message_service
        .create_message(session.id, "user".to_string(), "User 2".to_string())
        .await
        .unwrap();

    let user_messages = message_service
        .get_messages_by_role(session.id, "user")
        .await
        .unwrap();
    assert_eq!(user_messages.len(), 2);

    let assistant_messages = message_service
        .get_messages_by_role(session.id, "assistant")
        .await
        .unwrap();
    assert_eq!(assistant_messages.len(), 1);
}

#[tokio::test]
async fn test_calculate_totals() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    let msg1 = message_service
        .create_message(session.id, "user".to_string(), "Message 1".to_string())
        .await
        .unwrap();
    message_service
        .update_message_usage(
            msg1.id,
            crate::services::message::MessageUsage {
                token_count: 100,
                cost: 0.05,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let msg2 = message_service
        .create_message(session.id, "assistant".to_string(), "Message 2".to_string())
        .await
        .unwrap();
    message_service
        .update_message_usage(
            msg2.id,
            crate::services::message::MessageUsage {
                token_count: 200,
                cost: 0.10,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let total_tokens = message_service
        .calculate_total_tokens(session.id)
        .await
        .unwrap();
    let total_cost = message_service
        .calculate_total_cost(session.id)
        .await
        .unwrap();

    assert_eq!(total_tokens, 300);
    assert!((total_cost - 0.15).abs() < 0.0001);
}

fn plan_with_tasks(session_id: uuid::Uuid, statuses: &[TaskStatus]) -> PlanDocument {
    let mut plan = PlanDocument::new(session_id, "Completed history".to_string());
    plan.status = PlanStatus::Active;
    for (index, status) in statuses.iter().enumerate() {
        let mut task = PlanTask::new(
            index + 1,
            format!("Task {}", index + 1),
            format!("Description {}", index + 1),
            TaskType::Test,
        );
        task.status = status.clone();
        plan.add_task(task);
    }
    plan
}

#[tokio::test]
async fn plan_completed_record_roundtrip() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();
    let plan = plan_with_tasks(session.id, &[TaskStatus::Completed, TaskStatus::Skipped]);

    let first = message_service.record_completed_plan(&plan).await.unwrap();
    let second = message_service.record_completed_plan(&plan).await.unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(first.sequence, second.sequence);
    assert_eq!(first.created_at, second.created_at);
    assert_eq!(first.role, PLAN_COMPLETED_ROLE);
    assert_eq!(first.sequence, 1);
    assert!(first.token_count.is_none());
    assert!(first.cost.is_none());

    let decoded = CompletedPlanPayload::from_message(&first).unwrap();
    assert_eq!(decoded.version, 1);
    assert_eq!(decoded.plan.id, plan.id);
    assert_eq!(decoded.plan.session_id, session.id);
    assert_eq!(decoded.plan.title, plan.title);
    assert_eq!(decoded.plan.tasks.len(), 2);
    assert_eq!(decoded.plan.tasks[0].id, plan.tasks[0].id);
    assert_eq!(decoded.plan.tasks[0].order, 1);
    assert_eq!(decoded.plan.tasks[0].status, TaskStatus::Completed);
    assert_eq!(decoded.plan.tasks[1].order, 2);
    assert_eq!(decoded.plan.tasks[1].status, TaskStatus::Skipped);

    let messages = message_service
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    assert_eq!(messages.len(), 1, "retry must not insert a second row");
}

#[tokio::test]
async fn plan_completed_record_rejects_unfinished_plans() {
    let (message_service, session_service) = create_test_service().await;
    let session = session_service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    let mut editing = plan_with_tasks(session.id, &[TaskStatus::Completed]);
    editing.status = PlanStatus::Editing;
    let cases = vec![
        plan_with_tasks(session.id, &[]),
        editing,
        plan_with_tasks(session.id, &[TaskStatus::Pending]),
        plan_with_tasks(session.id, &[TaskStatus::InProgress]),
        plan_with_tasks(session.id, &[TaskStatus::Failed]),
        plan_with_tasks(session.id, &[TaskStatus::Blocked("waiting".to_string())]),
    ];

    for plan in cases {
        assert!(
            message_service.record_completed_plan(&plan).await.is_err(),
            "unfinished plan must not become history: {}",
            plan.title
        );
    }
    assert_eq!(
        message_service
            .count_messages_in_session(session.id)
            .await
            .unwrap(),
        0
    );
}
