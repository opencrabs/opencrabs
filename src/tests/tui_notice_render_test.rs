//! Frame-level regression for #1369: the "Copied to clipboard" notice after a
//! drag-select must not move the chat history.
//!
//! The source-scan tests in `tui_notice_test` pin WHERE the notice is coded.
//! They cannot say whether the history moves, and the first fix shipped on
//! their word alone. These tests render real frames through the top-level
//! `render` with a live `App`, then compare the chat rows and the scroll
//! state cell for cell with and without a notice live.
//!
//! The clipboard write itself is not exercised: `copy_to_clipboard` talks to
//! the OS and returns false on a headless CI box, which would leave
//! `notification` unset and the test proving nothing. The state the mouse-up
//! handler produces after a successful copy is set directly instead.

use std::sync::Arc;
use std::time::Instant;

use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use uuid::Uuid;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::{App, DisplayMessage};
use crate::tui::pane::PaneManager;
use crate::tui::render::render;

const WIDTH: u16 = 60;
const HEIGHT: u16 = 24;
const NOTICE: &str = "Copied to clipboard";

fn message(role: &str, content: String) -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: role.to_string(),
        content,
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

/// A live `App` with enough chat to overflow the viewport, so scrolling up
/// is possible and a three-row growth would be visible.
async fn app_with_chat(messages: usize) -> App {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    let mut app = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let mut app = App::new(service, context);
    // Hermetic: `App::new` restores the host's pane layout from disk, and a
    // split layout draws the notice inside a pane composer instead of the
    // window input box these tests measure. Start from one single-mode pane.
    app.pane_manager = PaneManager::new();
    for i in 0..messages {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        app.messages
            .push(message(role, format!("message number {i} in the history")));
    }
    app
}

fn draw(terminal: &mut Terminal<TestBackend>, app: &mut App) -> Buffer {
    terminal.draw(|f| render(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

/// Every cell above the input box, row by row.
fn chat_rows(buffer: &Buffer, input_top: u16) -> Vec<String> {
    (0..input_top).map(|y| row_text(buffer, y)).collect()
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    (0..WIDTH)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

/// The rows the input box owns (top border to bottom border).
fn input_rows(buffer: &Buffer, app: &App) -> String {
    (app.input_area_y..app.input_area_y + app.input_area_height)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn show_copied(app: &mut App) {
    // What `handle_mouse_up` leaves behind after a successful clipboard write.
    app.notification = Some(NOTICE.to_string());
    app.notification_shown_at = Some(Instant::now());
}

fn expire(app: &mut App) {
    // What the tick does once `NOTIFICATION_TTL` has passed.
    app.notification = None;
    app.notification_shown_at = None;
}

#[tokio::test]
async fn copied_notice_shows_inside_the_input_box() {
    let mut app = app_with_chat(6).await;
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    draw(&mut terminal, &mut app);

    show_copied(&mut app);
    let frame = draw(&mut terminal, &mut app);

    let input = input_rows(&frame, &app);
    assert!(
        input.contains(NOTICE),
        "the notice must render inside the input box rows:\n{input}"
    );
    let content_row = row_text(&frame, app.input_area_y + 1);
    assert!(
        content_row.contains('\u{276F}') && content_row.contains(NOTICE),
        "the notice sits on the prompt row, after the cursor: {content_row:?}"
    );
    for y in 0..app.input_area_y {
        assert!(
            !row_text(&frame, y).contains(NOTICE),
            "row {y} above the input box must not carry the notice"
        );
    }
}

#[tokio::test]
async fn copied_notice_leaves_the_chat_untouched_when_pinned_to_bottom() {
    let mut app = app_with_chat(40).await;
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    let baseline = draw(&mut terminal, &mut app);
    let input_top = app.input_area_y;
    let lines_before = app.prev_rendered_lines;
    assert!(app.auto_scroll, "fresh app is pinned to the bottom");

    show_copied(&mut app);
    let shown = draw(&mut terminal, &mut app);
    assert_eq!(
        app.prev_rendered_lines, lines_before,
        "notice grew the chat"
    );
    assert_eq!(
        chat_rows(&shown, input_top),
        chat_rows(&baseline, input_top),
        "chat rows moved while the notice was showing"
    );
    assert!(input_rows(&shown, &app).contains(NOTICE));

    expire(&mut app);
    let after = draw(&mut terminal, &mut app);
    assert_eq!(
        app.prev_rendered_lines, lines_before,
        "expiry shrank the chat"
    );
    assert_eq!(
        chat_rows(&after, input_top),
        chat_rows(&baseline, input_top),
        "chat rows moved when the notice expired"
    );
    assert!(!input_rows(&after, &app).contains(NOTICE));
}

#[tokio::test]
async fn copied_notice_leaves_the_chat_untouched_when_scrolled_up() {
    let mut app = app_with_chat(40).await;
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    draw(&mut terminal, &mut app);

    // Scroll up as the mouse wheel would, then settle on a baseline frame.
    app.auto_scroll = false;
    app.scroll_offset = 7;
    let baseline = draw(&mut terminal, &mut app);
    let input_top = app.input_area_y;
    let offset = app.scroll_offset;
    let lines_before = app.prev_rendered_lines;
    assert!(offset > 0, "the fixture must overflow the viewport");

    show_copied(&mut app);
    let shown = draw(&mut terminal, &mut app);
    assert_eq!(
        app.scroll_offset, offset,
        "compensation fired on the notice"
    );
    assert_eq!(app.prev_rendered_lines, lines_before);
    assert_eq!(
        chat_rows(&shown, input_top),
        chat_rows(&baseline, input_top)
    );

    expire(&mut app);
    let after = draw(&mut terminal, &mut app);
    assert_eq!(app.scroll_offset, offset, "expiry moved the scroll offset");
    assert_eq!(
        chat_rows(&after, input_top),
        chat_rows(&baseline, input_top)
    );
}

#[tokio::test]
async fn error_toast_takes_the_same_slot_and_leaves_the_chat_untouched() {
    let mut app = app_with_chat(40).await;
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    let baseline = draw(&mut terminal, &mut app);
    let input_top = app.input_area_y;

    app.error_message = Some("clipboard unavailable".to_string());
    app.error_message_shown_at = Some(Instant::now());
    let shown = draw(&mut terminal, &mut app);
    assert_eq!(
        chat_rows(&shown, input_top),
        chat_rows(&baseline, input_top)
    );
    assert!(input_rows(&shown, &app).contains("clipboard unavailable"));
}
