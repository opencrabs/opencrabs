//! #2237: the queue row has to say what the queue does and which key edits it.
//!
//! Two layers. The pure helpers decide the wording and the truncation, so the
//! maths is checked at any terminal width without a frame. The rendered test
//! then proves the wording actually reaches the frame: the helpers being right
//! means nothing if the row is never drawn.
//!
//! The in-flight "turn running · Enter queues" border title this module used
//! to pin was removed in #2243. A guard below keeps it from creeping back.

use std::sync::Arc;

use ratatui::{Terminal, backend::TestBackend};

use crate::brain::agent::QueuedUserMessage;
use crate::brain::agent::service::AgentService;
use crate::db::Database;
use crate::db::models::Session;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::render::queue_line_parts;
use crate::tui::render::render;
use unicode_width::UnicodeWidthStr;

const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;

async fn app_with_session() -> (App, uuid::Uuid) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn crate::brain::provider::Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    let mut app = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let mut app = App::new(service, context);

    let session = Session::new(Some("queue indicator".to_string()), None, None);
    let sid = session.id;
    app.current_session = Some(session);
    (app, sid)
}

fn queue(app: &App, sid: uuid::Uuid, texts: &[&str]) {
    let mut map = app.queued_messages.lock().unwrap();
    map.insert(
        sid,
        texts
            .iter()
            .map(|t| QueuedUserMessage::plain((*t).to_string()))
            .collect(),
    );
}

fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

fn input_rows(buffer: &ratatui::buffer::Buffer, app: &App) -> String {
    (app.input_area_y..app.input_area_y + app.input_area_height)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n")
}

// --- the queue row wording -------------------------------------------------

#[test]
fn a_wide_row_says_what_the_queue_does_and_which_key_edits_it() {
    let parts = queue_line_parts(2, "hello bre", 118);
    assert_eq!(parts.label, "⏳ queued (2): ");
    assert_eq!(parts.preview, "hello bre");
    assert!(
        parts.hint.contains("next tool call"),
        "the row has to say when the queued text goes out: {:?}",
        parts.hint
    );
    assert!(
        parts.hint.contains("back to this box"),
        "and that it is not lost if no tool call comes: {:?}",
        parts.hint
    );
    assert!(
        parts.hint.contains("↑ edit"),
        "the key has to be named: {:?}",
        parts.hint
    );
}

#[test]
fn a_normal_terminal_keeps_the_timing_and_drops_the_fallback_sentence() {
    // An 80 column terminal has no room for the whole explanation beside the
    // count and a readable preview. The timing and the key survive; the "or
    // back to this box" clause is the first thing to go.
    let parts = queue_line_parts(2, "hello bre", 78);
    assert!(
        parts.hint.contains("next tool call"),
        "when it goes out is the point of the row: {:?}",
        parts.hint
    );
    assert!(
        !parts.hint.contains("back to this box"),
        "the fallback sentence yields first: {:?}",
        parts.hint
    );
    assert!(parts.hint.contains("↑ edit"));
}

#[test]
fn a_narrow_row_keeps_the_count_and_the_key_before_anything_else() {
    let parts = queue_line_parts(1, "ping bre", 40);
    assert_eq!(parts.label, "⏳ queued: ");
    assert_eq!(parts.preview, "ping bre");
    assert_eq!(
        parts.hint, " · ↑ edit",
        "when there is no room for the explanation the key survives, not the other way round"
    );
}

#[test]
fn the_row_never_exceeds_the_space_it_is_given() {
    let long = "x".repeat(400);
    for width in [24usize, 40, 60, 120] {
        let parts = queue_line_parts(3, &long, width);
        let used = UnicodeWidthStr::width(parts.label.as_str())
            + UnicodeWidthStr::width(parts.preview.as_str())
            + UnicodeWidthStr::width(parts.hint);
        assert!(
            used <= width,
            "row of {used} columns does not fit in {width}: {:?}",
            (parts.label.clone(), parts.preview.clone(), parts.hint)
        );
        assert!(
            parts.preview.ends_with("...") || parts.preview.is_empty(),
            "an over-long preview is truncated or dropped, never left whole: {:?}",
            parts.preview
        );
    }
}

#[test]
fn a_queue_with_no_room_for_a_preview_still_shows_the_count() {
    let parts = queue_line_parts(12, "ping bre", 20);
    assert_eq!(parts.label, "⏳ queued (12)");
    assert!(parts.preview.is_empty());
    assert!(parts.hint.contains("↑ edit"));
}

#[test]
fn a_multiline_queued_message_is_flattened_to_one_row() {
    let parts = queue_line_parts(1, "hello bre\nping bre", 78);
    assert_eq!(parts.preview, "hello bre ping bre");
}

// --- the removed in-flight border title (#2243) ----------------------------

#[tokio::test]
async fn the_input_box_never_claims_enter_queues() {
    // #2243: the border title that read "turn running · Enter queues" sat
    // next to a thinking line, a step counter and a background-task title
    // that already said the same thing, and it made the box louder than the
    // text being typed into it. It must not come back, in either state.
    let (mut app, sid) = app_with_session().await;
    queue(&app, sid, &["ping bre"]);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();

    terminal.draw(|f| render(f, &mut app)).unwrap();
    let idle = input_rows(terminal.backend().buffer(), &app);
    assert!(
        !idle.contains("Enter queues") && !idle.contains("turn running"),
        "an idle box must carry no queue hint:\n{idle}"
    );

    app.is_processing = true;
    terminal.draw(|f| render(f, &mut app)).unwrap();
    let in_flight = input_rows(terminal.backend().buffer(), &app);
    assert!(
        !in_flight.contains("Enter queues") && !in_flight.contains("turn running"),
        "the in-flight border title was removed in #2243:\n{in_flight}"
    );

    // The queue row is the feedback that earns its space, so dropping the
    // title must not have taken the row with it.
    let buffer = terminal.backend().buffer();
    let frame: String = (0..HEIGHT)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n");
    // One message renders the bare label ("queued", no count); the count only
    // appears from two up, so pin the label and the preview, not a count.
    assert!(
        frame.contains("queued") && frame.contains("ping bre"),
        "the queued row must survive the title removal:\n{frame}"
    );
}

#[tokio::test]
async fn a_queued_message_names_the_count_and_the_key_in_the_frame() {
    let (mut app, sid) = app_with_session().await;
    app.is_processing = true;
    queue(&app, sid, &["hello bre", "ping bre"]);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal.draw(|f| render(f, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let frame: String = (0..HEIGHT)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        frame.contains("queued (2)"),
        "the count has to be on screen:\n{frame}"
    );
    assert!(
        frame.contains("next tool call"),
        "the row has to say when it goes out:\n{frame}"
    );
    assert!(
        frame.contains("↑ edit"),
        "the key that pulls it back for editing has to be named:\n{frame}"
    );
}
