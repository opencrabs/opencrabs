use crate::brain::tools::session_search::SessionSearchTool;
use crate::brain::tools::{Tool, ToolExecutionContext};
use crate::db::Database;
use crate::db::models::{Message, Session};
use crate::db::repository::{MessageRepository, SessionRepository};
use crate::services::message::PLAN_COMPLETED_ROLE;
use serde_json::json;
use uuid::Uuid;

async fn setup() -> (Database, SessionSearchTool, Session) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let tool = SessionSearchTool::new(db.pool().clone());
    let session = Session::new(Some("History".to_string()), Some("m".to_string()), None);
    let sessions = SessionRepository::new(db.pool().clone());
    let messages = MessageRepository::new(db.pool().clone());
    sessions.create(&session).await.unwrap();
    let rows = [
        ("user", "needle in the first conversation"),
        ("assistant", "ordinary answer"),
        (PLAN_COMPLETED_ROLE, r#"{"needle":"raw-plan-json"}"#),
        ("user", "latest conversation"),
    ];
    for (index, (role, content)) in rows.into_iter().enumerate() {
        messages
            .create(&Message::new(
                session.id,
                role.to_string(),
                content.to_string(),
                index as i32 + 1,
            ))
            .await
            .unwrap();
    }
    (db, tool, session)
}

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext::new(Uuid::new_v4())
}

#[tokio::test]
async fn session_search_excludes_completed_plan_history_from_both_search_modes() {
    let (_db, tool, _session) = setup().await;
    let ordinary = tool
        .execute(json!({"operation": "search", "query": "needle"}), &ctx())
        .await
        .unwrap();
    assert!(ordinary.success);
    assert!(ordinary.output.contains("needle in the first conversation"));
    assert!(!ordinary.output.contains("raw-plan-json"));

    for query in ["raw-plan-json", "needle raw-plan-json"] {
        let result = tool
            .execute(json!({"operation": "search", "query": query}), &ctx())
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("No messages found"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("**History** ["));
    }
}

#[tokio::test]
async fn session_search_excludes_completed_plan_history_without_shrinking_tail() {
    let (_db, tool, session) = setup().await;
    let result = tool
        .execute(
            json!({"operation": "tail", "session": session.id.to_string(), "n": 2}),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(result.success);
    assert!(result.output.contains("Last 2 of 4 messages"));
    assert!(result.output.contains("ordinary answer"));
    assert!(result.output.contains("latest conversation"));
    assert!(!result.output.contains("first conversation"));
    assert!(!result.output.contains("raw-plan-json"));
    assert!(!result.output.contains("plan_completed"));
}
