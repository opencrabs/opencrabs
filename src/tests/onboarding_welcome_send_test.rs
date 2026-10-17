//! The onboarding welcome is sent after every saved wizard completion, not
//! only on first run. A failed save sends nothing (#2022).

use std::sync::Arc;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::onboarding::WizardMode;

async fn app_with_session() -> App {
    let mut app = app().await;
    let session = app
        .session_service
        .create_session(Some("onboarding".into()))
        .await
        .unwrap();
    app.current_session = Some(session);
    app
}

async fn app() -> App {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    {
        App::new(
            service,
            context,
            Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
        )
    }
    #[cfg(not(feature = "whatsapp"))]
    {
        App::new(service, context)
    }
}

#[tokio::test]
async fn failed_save_sends_no_welcome() {
    let mut app = app_with_session().await;
    let messages_before = app.messages.len();

    app.send_onboarding_welcome(false, WizardMode::QuickStart)
        .await;

    assert!(!app.is_processing, "a failed save must not start a turn");
    assert_eq!(app.messages.len(), messages_before);
}

#[tokio::test]
async fn saved_completion_sends_welcome_without_first_run_flag() {
    let mut app = app_with_session().await;

    app.send_onboarding_welcome(true, WizardMode::QuickStart)
        .await;

    assert!(
        app.is_processing,
        "a saved /onboard completion must start the welcome turn"
    );
}

#[tokio::test]
async fn welcome_reads_provider_and_model_from_the_session_row() {
    let mut app = app().await;
    let session = app
        .session_service
        .create_session_with_provider(
            Some("onboarding".into()),
            Some("opencrabs".into()),
            Some("cb/deepseek-v4.1-flash".into()),
            None,
        )
        .await
        .unwrap();
    app.current_session = Some(session);

    let (provider, model) = app.welcome_provider_and_model().await;

    assert_eq!(provider, "opencrabs");
    assert_eq!(model, "cb/deepseek-v4.1-flash");
}
