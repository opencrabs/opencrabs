//! #1634: a theme switch must drop the pre-styled render cache.
//!
//! `render_cache` stores `Line`s whose spans already carry resolved colours,
//! keyed only by (message id, width). Nothing in `theme::set` could reach them,
//! so a live `/theme` repainted the widgets that call `theme::role` every frame
//! (title, borders, the picker itself) and left every message already on screen
//! wearing the previous palette until an unrelated resize happened to clear the
//! cache. That is why switching themes looked like it changed "almost nothing":
//! the transcript, which is most of the screen, was frozen.
//!
//! `theme::set` now bumps a generation counter and `render_chat` compares it
//! against the one the cache was filled under. These tests drive that counter
//! directly rather than calling `theme::set`, because the active theme is
//! process-global and the parallel harness runs one sanctioned mutator test
//! (`tui_theme_presets_test::set_and_reset_switch_active_theme`); a second
//! mutator would race it.
//!
//! Reading it races that mutator too: the counter is process-wide, so its
//! eleven bumps can land between an `App` capturing the live generation and
//! this module asserting the two still agree. Every test here therefore takes
//! `theme_global_lock` before touching the counter, and takes it *after* its
//! async setup so the guard never spans an `.await`.

use std::sync::Arc;

use ratatui::{Terminal, backend::TestBackend};
use uuid::Uuid;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::{App, DisplayMessage};
use crate::tui::render::{render, theme};

const WIDTH: u16 = 60;
const HEIGHT: u16 = 24;

fn message(role: &str, content: &str) -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: role.to_string(),
        content: content.to_string(),
        timestamp: chrono::Utc::now(),
        token_count: None,
        cost: None,
        approval: None,
        approve_menu: None,
        details: None,
        expanded: false,
        expanded_full: false,
        tool_group: None,
        duration_secs: None,
    }
}

/// The async half of the fixture. Split from [`app_with_chat`] so a test can
/// finish every `.await` before it takes the theme lock: `App::new` captures
/// the live generation, which is exactly the read that must sit inside the
/// critical section.
async fn service_and_context() -> (Arc<AgentService>, ServiceContext) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    (service, context)
}

/// The synchronous half: builds the `App` (stamping it with the live
/// generation) and seeds a two-message transcript for the cache to fill from.
fn app_with_chat(service: Arc<AgentService>, context: ServiceContext) -> App {
    #[cfg(feature = "whatsapp")]
    let mut app = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let mut app = App::new(service, context);
    // Hermetic layout. `App::new` calls `PaneManager::load_layout()`, which reads
    // the LIVE `~/.opencrabs/layout.json`. A persisted multi-pane split makes
    // `render_split_panes` skip every pane that falls under its 3x3 floor, so on
    // a 60x24 test terminal `render_chat` never runs and neither the cache fill
    // nor the generation sweep below can happen. CI has no layout file, so the
    // suite was green there and red on any machine with panes open.
    app.pane_manager = crate::tui::pane::PaneManager::new();
    app.messages.push(message("user", "a question with `code`"));
    app.messages
        .push(message("assistant", "an answer with **bold** prose"));
    app
}

fn draw(terminal: &mut Terminal<TestBackend>, app: &mut App) {
    terminal.draw(|f| render(f, app)).unwrap();
}

/// A fresh App starts in sync with the live generation, so the first frame
/// does not throw away a cache it just built.
#[tokio::test]
async fn a_fresh_app_starts_in_sync_with_the_live_generation() {
    let (service, context) = service_and_context().await;
    let _guard = crate::tests::theme_global_lock::lock();
    let app = app_with_chat(service, context);
    assert_eq!(app.render_cache_theme_gen, theme::generation());
}

/// The frame renders, fills the cache, and leaves the stamp matching.
#[tokio::test]
async fn rendering_fills_the_cache_and_stamps_the_generation() {
    let (service, context) = service_and_context().await;
    let _guard = crate::tests::theme_global_lock::lock();
    let mut app = app_with_chat(service, context);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    draw(&mut terminal, &mut app);
    assert!(
        !app.render_cache.is_empty(),
        "nothing cached, so the invalidation test below would prove nothing"
    );
    assert_eq!(app.render_cache_theme_gen, theme::generation());
}

/// The regression: entries styled under an older generation are dropped.
///
/// A sentinel line is planted under a key no message owns. A plain re-render
/// keeps it (the cache is not cleared every frame, which is the whole point of
/// having one); moving the stamp backwards, exactly what a `theme::set` does to
/// it, must sweep it away.
#[tokio::test]
async fn a_stale_generation_drops_the_cache() {
    let (service, context) = service_and_context().await;
    let _guard = crate::tests::theme_global_lock::lock();
    let mut app = app_with_chat(service, context);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    draw(&mut terminal, &mut app);

    let sentinel = (Uuid::new_v4(), WIDTH);
    app.render_cache
        .insert(sentinel, vec![ratatui::text::Line::from("stale")]);

    draw(&mut terminal, &mut app);
    assert!(
        app.render_cache.contains_key(&sentinel),
        "the cache is being cleared every frame, which defeats its purpose"
    );

    // What a theme switch looks like from the cache's side.
    app.render_cache_theme_gen = theme::generation().wrapping_sub(1);
    draw(&mut terminal, &mut app);

    assert!(
        !app.render_cache.contains_key(&sentinel),
        "lines styled under the previous theme survived the switch"
    );
    assert_eq!(
        app.render_cache_theme_gen,
        theme::generation(),
        "the stamp was not re-synced, so every later frame would clear again"
    );
}

/// The streaming cache holds pre-styled lines for the in-flight answer and is
/// keyed by length alone, so it needs the same sweep.
#[tokio::test]
async fn a_stale_generation_drops_the_streaming_cache() {
    let (service, context) = service_and_context().await;
    let _guard = crate::tests::theme_global_lock::lock();
    let mut app = app_with_chat(service, context);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    draw(&mut terminal, &mut app);

    app.streaming_render_cache = Some((7, vec![ratatui::text::Line::from("stale stream")]));
    app.render_cache_theme_gen = theme::generation().wrapping_sub(1);
    draw(&mut terminal, &mut app);

    assert!(
        app.streaming_render_cache.is_none(),
        "the streaming answer kept the previous theme's colours"
    );
}
