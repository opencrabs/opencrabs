use crate::db::models::Message;
use crate::tui::app::state::*;
use uuid::Uuid;

#[test]
fn test_display_message_from_db_message() {
    let msg = Message {
        forward: None,
        id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        role: "user".to_string(),
        content: "Hello".to_string(),
        sequence: 1,
        created_at: chrono::Utc::now(),
        token_count: Some(10),
        cost: Some(0.001),
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    };

    let display_msg: DisplayMessage = msg.into();
    assert_eq!(display_msg.role, "user");
    assert_eq!(display_msg.content, "Hello");
    assert!(display_msg.details.is_none());
}

#[test]
fn test_display_message_thinking_from_db() {
    let msg = Message {
        forward: None,
        id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        role: "assistant".to_string(),
        content: "Here is the answer.".to_string(),
        sequence: 2,
        created_at: chrono::Utc::now(),
        token_count: Some(50),
        cost: Some(0.005),
        input_tokens: Some(200),
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: Some("I need to analyze this carefully...".to_string()),
        duration_secs: None,
    };

    let display_msg: DisplayMessage = msg.into();
    assert_eq!(display_msg.role, "assistant");
    assert_eq!(display_msg.content, "Here is the answer.");
    assert_eq!(
        display_msg.details,
        Some("I need to analyze this carefully...".to_string())
    );
}
