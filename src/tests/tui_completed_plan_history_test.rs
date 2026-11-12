//! Durable completed-plan cards in immediate, reloaded, and folded TUI history.

use std::sync::Arc;

use crate::brain::agent::service::{AgentResponse, AgentService};
use crate::brain::provider::{Provider, TokenUsage};
use crate::db::Database;
use crate::db::models::Message;
use crate::services::message::{CompletedPlanPayload, MessageService, PLAN_COMPLETED_ROLE};
use crate::services::{ServiceContext, SessionService};
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::{App, DisplayMessage};
use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use crate::tui::render::chat::render_chat;
use crate::tui::render::panes::render_simple_message;
use crate::ui_core::plan_history::COMPLETED_PLAN_FOOTER;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use uuid::Uuid;

async fn test_app() -> (App, MessageService, SessionService) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let messages = MessageService::new(context.clone());
    let sessions = SessionService::new(context.clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    let app = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let app = App::new(service, context);
    (app, messages, sessions)
}

fn completed_plan(session_id: Uuid) -> PlanDocument {
    let mut plan = PlanDocument::new(session_id, "Ship durable history".to_string());
    plan.status = PlanStatus::Active;
    for (order, title, status) in [
        (1, "Persist the resolved checklist", TaskStatus::Completed),
        (2, "Render every completed task", TaskStatus::Skipped),
    ] {
        let mut task = PlanTask::new(
            order,
            title.to_string(),
            "description".to_string(),
            TaskType::Test,
        );
        task.status = status;
        plan.add_task(task);
    }
    plan
}

fn dm(role: &str, content: &str) -> DisplayMessage {
    DisplayMessage::from(Message::new(
        Uuid::new_v4(),
        role.to_string(),
        content.to_string(),
        0,
    ))
}

fn settled_response(completed_plan: Message) -> AgentResponse {
    AgentResponse {
        message_id: Uuid::new_v4(),
        content: "Final answer".to_string(),
        stop_reason: None,
        usage: TokenUsage {
            input_tokens: 1,
            output_tokens: 1,
            ..TokenUsage::default()
        },
        context_tokens: 2,
        tokens_per_second: None,
        cost: 0.0,
        model: "test-model".to_string(),
        provider_name: "test-provider".to_string(),
        started_on_session_provider: true,
        completed_plan: Some(completed_plan),
    }
}

fn rendered_app(app: &mut App) -> String {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render_chat(frame, app, Rect::new(0, 0, 120, 30)))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "current_thread")]
async fn tui_completed_plan_history_settles_after_answer_once() {
    let (mut app, messages, sessions) = test_app().await;
    let session = sessions
        .create_session(Some("completed plan immediate".to_string()))
        .await
        .unwrap();
    let answer_text = "Final answer\n\n**Sources**\n- [Rust](https://doc.rust-lang.org)";
    // The agent persists its assistant row before the completion seam writes
    // the plan card. Preserve that chronology in this settlement fixture.
    let answer = messages
        .create_message(session.id, "assistant".to_string(), answer_text.to_string())
        .await
        .unwrap();
    let completed = messages
        .record_completed_plan(&completed_plan(session.id))
        .await
        .unwrap();
    let session_id = session.id;
    app.current_session = Some(session);
    app.messages = vec![dm("user", "ship it")];

    for _ in 0..2 {
        let mut response = settled_response(completed.clone());
        response.message_id = answer.id;
        response.content = answer_text.to_string();
        app.complete_response(response).await.unwrap();
    }

    assert_eq!(app.messages.last().unwrap().id, completed.id);
    assert_eq!(app.messages[app.messages.len() - 2].id, answer.id);
    assert_eq!(app.messages[app.messages.len() - 2].content, answer_text);
    assert_eq!(
        app.messages
            .iter()
            .filter(|message| message.id == completed.id)
            .count(),
        1
    );
    assert_eq!(
        app.messages
            .iter()
            .filter(|message| message.id == answer.id)
            .count(),
        1
    );

    let immediate = rendered_app(&mut app);
    assert!(immediate.find("Sources").unwrap() < immediate.find("Ship durable history").unwrap());
    assert!(immediate.contains(COMPLETED_PLAN_FOOTER));

    // A fresh reload must reconstruct the same answer -> Sources -> card
    // ordering from SQLite, not from the in-memory response carriage.
    app.load_session(session_id).await.unwrap();
    assert_eq!(
        app.messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        vec![answer.id, completed.id]
    );
    let reloaded = rendered_app(&mut app);
    assert!(reloaded.find("Sources").unwrap() < reloaded.find("Ship durable history").unwrap());
    assert_eq!(reloaded.matches("Ship durable history").count(), 1);
    assert!(reloaded.contains(COMPLETED_PLAN_FOOTER));
}

/// A session short enough to fit one read opens complete (#2818). Opening
/// used to stop at the display window whatever the session held, so the first
/// thing a reader saw of a 300-message conversation was its last 200 rows.
#[tokio::test(flavor = "current_thread")]
async fn a_short_session_opens_whole_with_no_history_marker() {
    let (mut app, messages, sessions) = test_app().await;
    let session = sessions
        .create_session(Some("short session".to_string()))
        .await
        .unwrap();
    for index in 0..50 {
        messages
            .create_message(session.id, "user".to_string(), format!("row {index}"))
            .await
            .unwrap();
    }

    app.load_session(session.id).await.unwrap();
    assert_eq!(
        app.hidden_older_messages, 0,
        "nothing sits behind the window"
    );
    assert_eq!(
        app.messages.len(),
        50,
        "every row the session holds is here"
    );
    assert!(
        !app.messages
            .iter()
            .any(|message| message.role == "history_marker"),
        "a complete transcript shows no paging marker"
    );
    assert_eq!(app.messages.first().unwrap().content, "row 0");
}

#[tokio::test(flavor = "current_thread")]
async fn tui_completed_plan_history_reloads_pages_and_renders() {
    let (mut app, messages, sessions) = test_app().await;
    let session = sessions
        .create_session(Some("completed plan reload".to_string()))
        .await
        .unwrap();
    let completed = messages
        .record_completed_plan(&completed_plan(session.id))
        .await
        .unwrap();
    // Long enough that opening windows it: a session under the full-load
    // limit now opens whole, and paging is what this test is about (#2818).
    for index in 0..App::FULL_LOAD_LIMIT + 1 {
        messages
            .create_message(session.id, "user".to_string(), format!("older {index}"))
            .await
            .unwrap();
    }
    messages
        .create_message(session.id, "user".to_string(), "question".to_string())
        .await
        .unwrap();

    app.load_session(session.id).await.unwrap();
    assert!(app.hidden_older_messages > 0);
    // The window, plus the dim marker row it puts on top (#2818).
    assert_eq!(app.messages.len(), App::DISPLAY_MESSAGE_WINDOW + 1);
    assert!(
        !app.messages
            .iter()
            .any(|message| message.id == completed.id)
    );

    // Page until the session is whole. Each press asks for a longer span than
    // the last, so a session this long takes a handful of them, never one per
    // window (#2818).
    let mut presses = 0;
    while app.hidden_older_messages > 0 {
        app.load_more_history().await.unwrap();
        presses += 1;
        assert!(
            presses < 16,
            "paging never reached the start of the session"
        );
    }
    assert!(presses > 1, "a windowed session takes more than one press");
    assert_eq!(app.messages.first().unwrap().id, completed.id);
    assert!(
        app.messages
            .iter()
            .any(|message| message.content == "question")
    );

    // Keep paging and rendering assertions independent: the paged card is at
    // the top of 201 rows, outside a 30-line viewport anchored at the bottom.
    let card = app.messages.first().unwrap().clone();
    app.messages = vec![dm("user", "question"), card];
    let rendered = rendered_app(&mut app);
    assert!(rendered.contains("question"));
    assert!(rendered.contains("Ship durable history"));
    assert!(rendered.contains("2 tasks"));
    assert!(rendered.contains("100%"));
    assert!(rendered.contains("Persist the resolved checklist"));
    assert!(rendered.contains("Render every completed task"));
    assert!(rendered.contains(COMPLETED_PLAN_FOOTER));
    assert_eq!(rendered.matches("Ship durable history").count(), 1);
    assert!(!rendered.contains("PLAN-JSON-SENTINEL"));
}

#[tokio::test(flavor = "current_thread")]
async fn tui_completed_plan_history_malformed_rows_never_render_raw_json() {
    let mut valid = dm(PLAN_COMPLETED_ROLE, "unused");
    valid.content = serde_json::to_string(&CompletedPlanPayload {
        version: 1,
        plan: completed_plan(Uuid::new_v4()),
    })
    .unwrap();
    let mut malformed = valid.clone();
    malformed.content = "{\"secret\":\"PLAN-JSON-SENTINEL\"}".to_string();

    let (mut app, _messages, _sessions) = test_app().await;
    app.messages = vec![valid, malformed.clone()];
    let rendered = rendered_app(&mut app);
    assert!(rendered.contains("Ship durable history"));
    assert!(!rendered.contains("PLAN-JSON-SENTINEL"));
    assert!(!rendered.contains("\"secret\""));

    let mut pane_lines = Vec::new();
    render_simple_message(&mut pane_lines, &malformed, 80);
    assert!(pane_lines.is_empty());
    assert!(malformed.copy_text().is_empty());
}

#[test]
fn tui_completed_plan_history_stays_visible_when_turn_folds() {
    let mut card = dm(PLAN_COMPLETED_ROLE, "unused");
    card.content = serde_json::to_string(&CompletedPlanPayload {
        version: 1,
        plan: completed_plan(Uuid::new_v4()),
    })
    .unwrap();
    let mut messages = vec![dm("user", "question"), dm("assistant", "old answer"), card];
    messages.push(dm("user", "next question"));
    messages.push(dm("assistant", "new answer"));

    let folded = crate::tui::render::chat::folded_pane_lines(&messages, 80);
    let text = folded
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("Ship durable history"));
    assert!(text.contains("Render every completed task"));
    assert!(text.contains(COMPLETED_PLAN_FOOTER));
}

#[test]
fn tui_completed_plan_history_copy_is_human_readable() {
    let plan = completed_plan(Uuid::new_v4());
    let payload = serde_json::to_string(&CompletedPlanPayload { version: 1, plan }).unwrap();
    let card = dm(PLAN_COMPLETED_ROLE, &payload);
    let copied = card.copy_text();
    assert!(copied.contains("☑ Plan · Ship durable history · 2 tasks · 100%"));
    assert!(copied.contains("☑ 1. Persist the resolved checklist"));
    assert!(copied.contains("☑ 2. Render every completed task"));
    assert!(copied.ends_with(COMPLETED_PLAN_FOOTER));
    assert!(!copied.contains("\"tasks\""));
    assert!(!copied.contains("session_id"));
}
