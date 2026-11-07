//! #2526 regression: Tab/Space select-here in the /cd DirectoryPicker
//! must work in split layouts and must return to Chat immediately.
//!
//! Two defects pinned here:
//!
//! 1. The split-mode pane-focus interceptor (`is_focus_next_pane` in
//!    state.rs) consumed bare Tab before mode dispatch, so the picker
//!    never received the footer's advertised "Space/Tab: select here"
//!    key. The interceptor must only fire for the chat surface.
//! 2. The select-here branch awaited the persistence write
//!    (`apply_working_directory_change`) BEFORE `switch_mode(Chat)`, so
//!    a slow write left the picker stuck open after the select. The
//!    historical behaviour was an instant return to chat with a
//!    vanishing confirmation, so the mode switch now happens first.

use std::sync::Arc;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::AppMode;
use crate::tui::app::App;
use crate::tui::pane::{PaneManager, SplitDirection};

async fn app() -> App {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    let app_value = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let app_value = App::new(service, context);
    // Hermetic: App::new restores the host's persisted pane layout, so a
    // fresh App can already carry a real split. Reset to one clean pane;
    // each test splits exactly what it needs.
    let mut app = app_value;
    app.pane_manager = PaneManager::new();
    app
}

/// Split into two panes (focus lands on the new pane) and open the
/// /cd picker pointed at `dir`.
async fn split_and_open_picker(app: &mut App, dir: &std::path::Path) {
    app.pane_manager.split(SplitDirection::Horizontal);
    assert!(app.pane_manager.is_split());
    app.mode = AppMode::DirectoryPicker;
    app.file_picker_current_dir = dir.to_path_buf();
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}

#[tokio::test]
async fn tab_selects_here_from_directory_picker_in_split_mode() {
    let mut app = app().await;
    let dir = std::env::temp_dir().canonicalize().unwrap();
    split_and_open_picker(&mut app, &dir).await;

    app.handle_key_event(key(crossterm::event::KeyCode::Tab))
        .await
        .unwrap();

    assert_eq!(
        app.mode,
        AppMode::Chat,
        "Tab select-here must return to chat"
    );
    assert_eq!(
        app.working_directory, dir,
        "Tab must select the picker's current directory as working dir"
    );
}

#[tokio::test]
async fn space_selects_here_from_directory_picker_in_split_mode() {
    let mut app = app().await;
    let dir = std::env::temp_dir().canonicalize().unwrap();
    split_and_open_picker(&mut app, &dir).await;

    app.handle_key_event(key(crossterm::event::KeyCode::Char(' ')))
        .await
        .unwrap();

    assert_eq!(
        app.mode,
        AppMode::Chat,
        "Space select-here must return to chat"
    );
    assert_eq!(
        app.working_directory, dir,
        "Space must select the picker's current directory as working dir"
    );
}

#[tokio::test]
async fn tab_select_returns_to_chat_before_persistence_completes_ordering() {
    // Pins the reorder: switch_mode(Chat) must appear textually BEFORE
    // the apply await in the select-here branch, so a slow write can
    // never hold the picker open.
    let src = include_str!("../tui/app/dialogs.rs");
    let marker = "Tab/Space selects the current directory as working dir.";
    let start = src.find(marker).expect("select-here branch must exist");
    let body = src
        .get(start..start + 800)
        .expect("select-here branch window");
    let switch = body
        .find("self.switch_mode(AppMode::Chat)")
        .expect("select-here must switch to chat");
    let apply = body
        .find("self.apply_working_directory_change")
        .expect("select-here must apply the directory change");
    assert!(
        switch < apply,
        "the chat mode switch must run before the persistence await so the \
         picker closes instantly even when the write is slow (#2526)"
    );
}

#[tokio::test]
async fn alt_tab_cycles_panes_when_chat_is_split() {
    // The #2494 pane-cycling behaviour must survive the #2526 guard and the
    // #2580 chord move: in Chat mode on a split layout Alt+Tab moves pane
    // focus, while bare Tab keeps its own action.
    let mut app = app().await;
    app.pane_manager.split(SplitDirection::Horizontal);
    let ids = app.pane_manager.pane_ids_in_order();
    assert_eq!(ids.len(), 2);
    let before = app.pane_manager.focused_pane().map(|p| p.id);

    app.handle_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::ALT,
    ))
    .await
    .unwrap();

    assert_eq!(app.mode, AppMode::Chat);
    let after_alt = app.pane_manager.focused_pane().map(|p| p.id);
    assert_ne!(
        after_alt, before,
        "Alt+Tab in split Chat mode must still cycle pane focus (#2494, #2580)"
    );

    // Bare Tab is the follow-up accept and picker key now, never a pane move.
    app.handle_key_event(key(crossterm::event::KeyCode::Tab))
        .await
        .unwrap();
    assert_eq!(
        app.pane_manager.focused_pane().map(|p| p.id),
        after_alt,
        "bare Tab must not rotate panes any more (#2580)"
    );
}
