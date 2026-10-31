//! One source of truth for the input box geometry.
//!
//! Click to place the caret, drag to copy, the selection highlight and
//! Alt+Up/Down all have to wrap text at the same width and map a screen cell
//! to the same byte. They used to be three copies that disagreed: the drag
//! copy repeated the first character of every soft wrapped row, and Alt+Up/Down
//! wrapped two columns narrower than the renderer, so the cursor drifted right
//! on every wrapped line. The behaviour lives in `tui::input_layout`; these
//! checks pin the wiring so a later edit cannot quietly fork it again.

fn body_of<'a>(source: &'a str, marker: &str) -> &'a str {
    let rest = source
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("{marker} not found"));
    rest.split("\n    }\n").next().unwrap_or("")
}

#[test]
fn alt_up_down_wraps_at_the_render_width() {
    let input = include_str!("../tui/app/input.rs");
    let body = body_of(input, "fn input_visual_line_width(");
    assert!(
        body.contains("input_layout::content_width(area_width)"),
        "Alt+Up/Down must wrap where the renderer wraps"
    );
    assert!(
        !body.contains("saturating_sub(4)"),
        "the 2 column narrower width was the drift bug"
    );
}

#[test]
fn renderer_wraps_at_the_shared_content_width() {
    let render = include_str!("../tui/render/input.rs");
    assert!(
        render.contains("input_layout::content_width(area.width)"),
        "the renderer must wrap at the shared width"
    );
}

#[test]
fn drag_copy_uses_the_shared_layout() {
    let input = include_str!("../tui/app/input.rs");
    let drag = body_of(input, "fn extract_input_drag_selection(");
    assert!(
        drag.contains("input_layout::selection_text("),
        "drag to copy must go through the shared mapping"
    );
}

#[test]
fn click_to_caret_uses_the_shared_layout() {
    let input = include_str!("../tui/app/input.rs");
    let click = body_of(input, "fn handle_input_mouse_click(");
    assert!(click.contains("input_layout::screen_to_visual("));
    assert!(click.contains("input_layout::byte_at_visual_pos("));
}

#[test]
fn a_click_in_the_input_is_handled_before_the_chat_gesture() {
    let state = include_str!("../tui/app/state.rs");
    assert!(
        state.contains("self.handle_input_mouse_click(col, row)"),
        "the mouse-down arm must place the caret before the chat click logic"
    );
}

// ---------------------------------------------------------------------------
// Behaviour at the App level: the helpers above are only useful if the click
// and drag handlers actually call them with the box geometry and the scroll
// the last frame applied. These two tests press and drag for real.
// ---------------------------------------------------------------------------

use std::sync::Arc;

use ratatui::{Terminal, backend::TestBackend};

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;

/// A chat-mode app with a 40 column input box whose top border sits on row 10.
async fn app_with_input_box() -> App {
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
    app.input_area_x = 0;
    app.input_area_y = 10;
    app.input_area_width = 40;
    app.input_area_height = 5;
    app
}

#[tokio::test]
async fn click_places_the_caret_under_the_pointer() {
    let mut app = app_with_input_box().await;
    app.input_buffer = "hello\nworld".to_string();
    app.cursor_position = 0;

    // First content row (11), first text column (2).
    assert!(app.handle_input_mouse_click(2, 11));
    assert_eq!(app.cursor_position, 0);

    // Second content row, three cells in: byte 6 is "world", plus 3.
    assert!(app.handle_input_mouse_click(5, 12));
    assert_eq!(app.cursor_position, 9);

    // A click below the text parks the caret at the end, never past it.
    assert!(app.handle_input_mouse_click(4, 13));
    assert_eq!(app.cursor_position, app.input_buffer.len());

    // The borders and anything outside the box are not the input's.
    assert!(!app.handle_input_mouse_click(3, 10), "top border");
    assert!(!app.handle_input_mouse_click(3, 14), "bottom border");
    assert!(!app.handle_input_mouse_click(3, 4), "chat area above");
}

#[tokio::test]
async fn click_ignores_the_row_prefix_and_keeps_the_caret() {
    let mut app = app_with_input_box().await;
    app.input_buffer = "hello".to_string();
    app.cursor_position = 3;
    // Column 1 is inside the two column prefix, so the caret must not jump.
    assert!(app.handle_input_mouse_click(1, 11));
    assert_eq!(app.cursor_position, 3);
}

#[tokio::test]
async fn drag_copy_reads_the_row_the_user_sees() {
    let mut app = app_with_input_box().await;
    app.input_buffer = "l0\nl1\nl2\nl3".to_string();
    let a = (2, 11);
    let b = (4, 11);

    // Unscrolled, screen row 11 is the first buffer row.
    assert_eq!(app.extract_input_drag_selection(a, b), "l0");

    // Two rows scrolled away, the same cells hold the third row.
    app.input_scroll_y = 2;
    assert_eq!(app.extract_input_drag_selection(a, b), "l2");
}

/// The text the input box actually paints, row by row, prefixes included.
fn painted_input_rows(app: &App, terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (app.input_area_y..app.input_area_y + app.input_area_height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

#[tokio::test]
async fn a_wrapped_row_does_not_repeat_the_boundary_character() {
    // The row splitting used to begin every soft wrapped row one character
    // early, so the character on the wrap boundary was painted twice and the
    // row held one column more text than the box is wide. Every character of
    // the buffer has to appear exactly once, in order.
    let mut app = app_with_input_box().await;
    app.input_buffer = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ".to_string();
    app.cursor_position = 0;

    let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
    terminal
        .draw(|f| crate::tui::render::render(f, &mut app))
        .unwrap();

    let rows = painted_input_rows(&app, &terminal);
    let painted: String = rows
        .iter()
        .skip(1)
        .take(rows.len().saturating_sub(2))
        .map(|row| row.chars().skip(2).collect::<String>())
        .collect::<String>()
        .trim_end()
        .to_string();

    assert_eq!(
        painted, app.input_buffer,
        "the painted rows must spell the buffer once, with no repeated character at a wrap"
    );
}
