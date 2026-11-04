//! Full-bleed Sessions and Help (#2162).
//!
//! Both screens used to spend three rows on chrome (the app title row and a
//! top plus bottom border) and both laid their content out with borders that
//! stole two more columns. These are rendered-buffer assertions, not source
//! scans: they prove the border is gone, the title row is gone, the footer
//! owns the last row, and a session row is always exactly one row.

use std::sync::Arc;

use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

use crate::brain::agent::service::AgentService;
use crate::db::Database;
use crate::db::models::Session;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::app::events::AppMode;
use crate::tui::render::render;
use crate::tui::render::theme::{self, Role};

async fn app_in(mode: AppMode) -> App {
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
    app.mode = mode;
    // A real Sessions visit always arrives through a loaded session, and
    // `load_session` binds the focused pane. An unbound pane renders the
    // "select a session for the new pane" prompt on row 0, which would push
    // every row down by one and quietly invalidate the row assertions.
    if mode == AppMode::Sessions
        && let Some(pane) = app.pane_manager.focused_pane_mut()
    {
        pane.session_id = Some(uuid::Uuid::new_v4());
    }
    app
}

fn session(title: &str) -> Session {
    Session {
        id: uuid::Uuid::new_v4(),
        title: Some(title.to_string()),
        model: None,
        provider_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        archived_at: None,
        token_count: 0,
        total_cost: 0.0,
        working_directory: None,
        auto_title_attempted: false,
        project_id: None,
        channel_chat_key: None,
    }
}

fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    // A real Sessions visit always arrives with the focused pane bound to a
    // session (`load_session` does the binding). An unbound pane correctly
    // draws the "select a session for the new pane" prompt, which would push
    // every row down by one and make the row assertions meaningless, so the
    // fixture binds it here instead of pretending the prompt is not there.
    if app.mode == AppMode::Sessions
        && let Some(first) = app.sessions.first().map(|s| s.id)
        && let Some(pane) = app.pane_manager.focused_pane_mut()
    {
        pane.session_id = Some(first);
    }
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn row_text(buffer: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

fn all_rows(buffer: &Buffer, width: u16, height: u16) -> Vec<String> {
    (0..height).map(|y| row_text(buffer, y, width)).collect()
}

/// No border cell anywhere on screen: not a side, not a corner, not the
/// rounded variant.
fn assert_no_border(buffer: &Buffer, width: u16, height: u16) {
    for y in 0..height {
        for x in 0..width {
            let sym = buffer[(x, y)].symbol();
            assert!(
                !matches!(
                    sym,
                    "│" | "┃" | "┌" | "┐" | "└" | "┘" | "╭" | "╮" | "╰" | "╯"
                ),
                "border cell {sym:?} at ({x},{y}) in a full-bleed screen"
            );
        }
    }
}

/// The app title row (`🦀 OpenCrabs AI Agent vX.Y.Z`) is gone.
fn assert_no_app_title(rows: &[String]) {
    for row in rows {
        assert!(
            !row.contains("OpenCrabs AI Agent"),
            "app title row still rendered: {row:?}"
        );
    }
}

// ── Sessions ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sessions_has_no_border_and_no_app_title() {
    let mut app = app_in(AppMode::Sessions).await;
    app.sessions = vec![session("CrabDev"), session("Crabsland")];

    let (w, h) = (100u16, 30u16);
    let buf = draw(&mut app, w, h);

    assert_no_border(&buf, w, h);
    let rows = all_rows(&buf, w, h);
    assert_no_app_title(&rows);

    // The very first row is content, not chrome.
    assert!(
        rows[0].contains("CrabDev"),
        "first row must be the first session, got {:?}",
        rows[0]
    );
}

#[tokio::test]
async fn sessions_row_never_wraps() {
    let mut app = app_in(AppMode::Sessions).await;
    // Far longer than the 60 columns it will be drawn in: if the row wrapped,
    // the second session would land two or three rows below the first.
    app.sessions = vec![
        session(
            &"Telegram: Open Crabs Official / OC Dev / a very long channel title that keeps going"
                .repeat(2),
        ),
        session("Crabsland"),
    ];

    let (w, h) = (60u16, 20u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    let first = rows
        .iter()
        .position(|r| r.contains("Telegram"))
        .unwrap_or_else(|| panic!("first session missing: {rows:?}"));
    let second = rows
        .iter()
        .position(|r| r.contains("Crabsland"))
        .unwrap_or_else(|| panic!("second session missing: {rows:?}"));
    assert_eq!(
        second,
        first + 1,
        "a long title wrapped instead of truncating: {rows:?}"
    );
    // Truncation is visible, not silent.
    assert!(
        rows[first].contains('…'),
        "long name should be ellipsized: {:?}",
        rows[first]
    );
}

#[tokio::test]
async fn sessions_selection_is_a_full_width_bar() {
    let mut app = app_in(AppMode::Sessions).await;
    app.sessions = vec![session("CrabDev"), session("Crabsland")];
    app.selected_session_index = 1;

    let (w, h) = (100u16, 20u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    let selected = rows
        .iter()
        .position(|r| r.contains("Crabsland"))
        .unwrap_or_else(|| panic!("selected session missing: {rows:?}"));

    let bar = theme::role(Role::SelectionBg);
    for x in 0..w {
        assert_eq!(
            buf[(x, selected as u16)].bg,
            bar,
            "selection bar stops at column {x} of {w}"
        );
    }
    // The row above is NOT selected, so the bar is the selection and not
    // something every row carries.
    let other = buf[(0, selected as u16 - 1)].bg;
    assert_ne!(
        other, bar,
        "unselected row must not carry the selection bar"
    );
}

#[tokio::test]
async fn sessions_footer_is_the_last_row() {
    let mut app = app_in(AppMode::Sessions).await;
    app.sessions = vec![session("CrabDev")];

    let (w, h) = (100u16, 20u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    let last = &rows[h as usize - 1];
    assert!(
        last.contains("↑↓: navigate"),
        "footer missing from the last row: {last:?}"
    );
    assert!(
        last.contains("Esc: back"),
        "Esc must survive at 100 cols: {last:?}"
    );
    assert!(
        !last.contains('['),
        "bracket hint style leaked into the footer: {last:?}"
    );
    // Nothing below it: the footer really is the last row.
    assert_eq!(rows.len(), h as usize);
}

#[tokio::test]
async fn sessions_footer_drops_whole_bindings_when_narrow() {
    let mut app = app_in(AppMode::Sessions).await;
    app.sessions = vec![session("CrabDev")];

    let wide = draw(&mut app, 200, 20);
    let narrow = draw(&mut app, 44, 20);
    let wide_footer = row_text(&wide, 19, 200);
    let narrow_footer = row_text(&narrow, 19, 44).trim_end().to_string();

    assert!(
        wide_footer.trim_end().starts_with(&narrow_footer),
        "narrow footer must be a prefix of the wide one\nwide:   {wide_footer:?}\nnarrow: {narrow_footer:?}"
    );
    // The last visible binding is whole: every verb in the narrow footer
    // appears in the wide footer as a complete word followed by a separator
    // or the end of the line.
    for part in narrow_footer.split("  ") {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        assert!(
            wide_footer.contains(part),
            "binding {part:?} is cut mid-verb in the narrow footer"
        );
    }
}

#[tokio::test]
async fn sessions_row_badges_follow_width_tiers() {
    let mut app = app_in(AppMode::Sessions).await;
    let project_id = uuid::Uuid::new_v4();
    let mut s = session("CrabDev");
    s.provider_name = Some("opencrabs".to_string());
    s.model = Some("flash".to_string());
    s.token_count = 4_657_300;
    s.project_id = Some(project_id);
    app.sessions = vec![s];
    app.project_name_cache
        .insert(project_id, "crabsland".to_string());

    // <100: name, clock time, status. No full date, no tokens, no provider.
    let narrow = row_text(&draw(&mut app, 90, 20), 0, 90);
    assert!(narrow.contains("CrabDev"), "{narrow:?}");
    assert!(
        !narrow.contains("2026-") && !narrow.contains("total"),
        "90 cols must stay at clock-only: {narrow:?}"
    );
    assert!(
        !narrow.contains("[opencrabs/flash]"),
        "provider badge must not render at 90 cols: {narrow:?}"
    );
    assert!(
        !narrow.contains("{.crabsland}"),
        "project badge must not render at 90 cols: {narrow:?}"
    );

    // 100..119: full timestamp and cumulative tokens, still no provider.
    let medium = row_text(&draw(&mut app, 110, 20), 0, 110);
    assert!(
        medium.contains("2026-") && medium.contains("total"),
        "110 cols must show the full timestamp and total tokens: {medium:?}"
    );
    assert!(
        !medium.contains("[opencrabs/flash]"),
        "provider badge must not render at 110 cols: {medium:?}"
    );

    // 120..149: plus the provider/model pair, still no project badge.
    let wide = row_text(&draw(&mut app, 130, 20), 0, 130);
    assert!(
        wide.contains("[opencrabs/flash]"),
        "provider badge must render at 130 cols: {wide:?}"
    );
    assert!(
        !wide.contains("{.crabsland}"),
        "project badge must not render at 130 cols: {wide:?}"
    );

    // 150 and up: plus the project badge.
    let extra = row_text(&draw(&mut app, 160, 20), 0, 160);
    assert!(
        extra.contains("[opencrabs/flash]"),
        "provider badge must render at 160 cols: {extra:?}"
    );
    assert!(
        extra.contains("{.crabsland}"),
        "project badge must render at 160 cols: {extra:?}"
    );

    // Every tier keeps the name, whatever else it drops.
    for row in [&narrow, &medium, &wide, &extra] {
        assert!(row.contains("CrabDev"), "name lost: {row:?}");
    }
}

// ── Help ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn help_has_no_border_and_no_app_title() {
    let mut app = app_in(AppMode::Help).await;

    let (w, h) = (100u16, 30u16);
    let buf = draw(&mut app, w, h);

    assert_no_border(&buf, w, h);
    let rows = all_rows(&buf, w, h);
    assert_no_app_title(&rows);
    assert!(
        rows[0].contains("ABOUT"),
        "idle help must start with content, got {:?}",
        rows[0]
    );
    assert!(
        !rows[0].contains('🔍'),
        "idle help must not render a search row: {:?}",
        rows[0]
    );
}

#[tokio::test]
async fn help_footer_is_the_last_row_and_content_fills_the_rest() {
    let mut app = app_in(AppMode::Help).await;

    let (w, h) = (100u16, 30u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    let last = &rows[h as usize - 1];
    assert!(
        last.contains("Esc: back"),
        "help footer missing from the last row: {last:?}"
    );
    // The rule sits between the catalog and the footer, and it spans the
    // whole width: that is what marks the footer as a footer.
    assert_rule_row(&rows[h as usize - 2], w);
    // Content reaches the bottom: the last non-blank row of the catalog sits
    // within two rows of the rule. The catalog puts a single blank separator
    // between sections, so demanding text on the exact row above the rule
    // fails whenever a gap lands there; what must never happen is a blank
    // band, which is what the old border plus its padding left behind.
    let last_content = rows[..h as usize - 2]
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .unwrap_or_else(|| panic!("help content is empty: {rows:?}"));
    assert!(
        last_content + 2 >= h as usize - 2,
        "blank band above the footer: last content row is {last_content}, rule is at {}",
        h as usize - 2
    );
}

/// A separator row: nothing but `─` across the full width.
fn assert_rule_row(row: &str, width: u16) {
    let trimmed = row.trim_end();
    assert!(!trimmed.is_empty(), "expected a rule row, got a blank line");
    assert!(
        trimmed.chars().all(|c| c == '─'),
        "expected a rule of `─` only, got {row:?}"
    );
    assert_eq!(
        trimmed.chars().count(),
        width as usize,
        "the rule must span the full width: {row:?}"
    );
}

#[tokio::test]
async fn sessions_has_a_rule_between_list_and_footer() {
    let mut app = app_in(AppMode::Sessions).await;
    app.sessions = vec![session("CrabDev"), session("Crabsland")];

    let (w, h) = (100u16, 20u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    assert_rule_row(&rows[h as usize - 2], w);
    // The list stops above the rule: the last session is not on the rule row
    // and the footer is not on the last content row.
    assert!(
        !rows[h as usize - 2].contains("CrabDev"),
        "session row leaked onto the rule row"
    );
    assert!(
        rows[h as usize - 1].contains("↑↓: navigate"),
        "footer must still own the last row: {:?}",
        rows[h as usize - 1]
    );
}

#[tokio::test]
async fn sessions_row_never_clips_a_badge() {
    let mut app = app_in(AppMode::Sessions).await;
    // A provider/model pair plus a working directory at a width whose tier
    // would normally show them: the row has to drop whole badges rather than
    // let the terminal cut one at the edge.
    // The title is longer than the room the badges leave at this width, so the
    // ellipsis path is genuinely exercised: a short title would fit and prove
    // nothing about truncation.
    let mut s = session(
        "A session with a very long title that takes the room and then keeps going well past the edge of any sane terminal",
    );
    s.provider_name = Some("openrouter-with-a-long-name".to_string());
    s.model = Some("anthropic/claude-sonnet-4-very-long-model-name".to_string());
    s.token_count = 1_234_567;
    s.working_directory =
        Some("/Users/someone/srv/rs/opencrabs-a-really-deep-directory".to_string());
    app.sessions = vec![s];

    let (w, h) = (100u16, 20u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    let row = &rows[0];
    // The row fits the terminal: no cell of it was pushed off the edge.
    assert_eq!(row.chars().count(), w as usize, "row width changed");
    // A badge that survived is whole: a provider badge opens and closes.
    let opens = row.matches('[').count();
    let closes = row.matches(']').count();
    assert_eq!(
        opens, closes,
        "a badge was cut mid-token: {row:?} (the `[oper` bug)"
    );
    // And the name is still truncated with an ellipsis rather than dropped.
    assert!(
        row.contains('…'),
        "the long name should be ellipsised, got {row:?}"
    );
}

#[tokio::test]
async fn help_two_columns_only_when_wide() {
    let mut app = app_in(AppMode::Help).await;

    let wide = draw(&mut app, 120, 30);
    // Single column stacks every right-hand section under the left ones, so
    // SESSIONS only comes into view on a terminal tall enough to hold the
    // whole stack.
    let narrow = draw(&mut app, 80, 140);

    let wide_rows = all_rows(&wide, 120, 30);
    let sessions_wide = wide_rows
        .iter()
        .position(|r| r.contains("SESSIONS"))
        .unwrap_or_else(|| panic!("SESSIONS missing at 120 cols: {wide_rows:?}"));
    let col = wide_rows[sessions_wide].find("SESSIONS").unwrap();
    assert!(
        col > 60,
        "at 120 cols SESSIONS must sit in the right column (col {col})"
    );
    // The separator that replaced the border is there.
    assert!(
        wide_rows[sessions_wide].contains('│'),
        "two-column help must draw the rule: {:?}",
        wide_rows[sessions_wide]
    );

    let narrow_rows = all_rows(&narrow, 80, 140);
    let sessions_narrow = narrow_rows
        .iter()
        .position(|r| r.contains("SESSIONS"))
        .unwrap_or_else(|| panic!("SESSIONS missing at 80 cols: {narrow_rows:?}"));
    let col = narrow_rows[sessions_narrow].find("SESSIONS").unwrap();
    assert!(
        col < 40,
        "at 80 cols the right column is stacked, so SESSIONS starts at the left (col {col})"
    );
    assert!(
        !narrow_rows.iter().any(|r| r.contains('│')),
        "single column must not draw a rule"
    );
}

#[tokio::test]
async fn help_scroll_clamps_each_column_independently() {
    let mut app = app_in(AppMode::Help).await;

    // Two columns on a terminal shorter than either one, so each column has
    // its own maximum scroll and the two maxima differ. That difference is
    // the whole point: one shared offset must not drag the short column past
    // its end into blank space.
    let (w, h) = (120u16, 24u16);
    let _ = draw(&mut app, w, h);

    let viewport = app.help_viewport_rows;
    let left_rows = app.help_content_rows;
    assert!(
        viewport > 0 && left_rows > viewport,
        "the left column must be taller than the viewport \
         (left={left_rows}, viewport={viewport})"
    );

    // Scroll far past the end of both columns.
    app.help_scroll_offset = 10_000;
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    assert!(
        !rows.iter().any(|r| r.contains("ABOUT")),
        "left column should have scrolled past its first section: {rows:?}"
    );
    // The left column stops at ITS last line instead of continuing into
    // blank rows, which is what an unclamped shared offset would do.
    assert!(
        rows.iter().any(|r| r.contains("docs.opencrabs.com")),
        "left column ran past its own end into blank space: {rows:?}"
    );
    // The right column follows the offset to its own (different) end.
    assert!(
        rows.iter().any(|r| r.contains("FEATURES")),
        "right column did not reach its own end: {rows:?}"
    );
}

// ---------------------------------------------------------------------------
// Footer drift: every binding the Sessions footer advertises must actually do
// something. The footer is generated from `DialogScope::Sessions`, so a key
// that stops being handled in `handle_sessions_key` would otherwise keep
// rendering as a promise the screen no longer keeps (#2162).
// ---------------------------------------------------------------------------

use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};
use crossterm::event::KeyEvent;

/// Everything a Sessions binding is allowed to move. A binding that changes
/// none of these did nothing when pressed.
#[derive(Debug, PartialEq, Eq)]
struct SessionsProbe {
    mode: AppMode,
    selected: usize,
    sessions: usize,
    renaming: bool,
    searching: bool,
    panes: usize,
}

fn probe(app: &App) -> SessionsProbe {
    SessionsProbe {
        mode: app.mode,
        selected: app.selected_session_index,
        sessions: app.sessions.len(),
        renaming: app.session_renaming,
        searching: app.session_search_active,
        panes: app.pane_manager.panes.len(),
    }
}

/// Three real sessions in the DB and in the list, cursor in the middle so
/// both Up and Down have somewhere to go.
async fn sessions_app() -> App {
    let mut app = app_in(AppMode::Sessions).await;
    for i in 0..3 {
        app.session_service
            .create_session(Some(format!("drift session {i}")))
            .await
            .unwrap();
    }
    app.load_sessions().await.unwrap();
    assert_eq!(app.sessions.len(), 3, "fixture did not load sessions");
    app.selected_session_index = 1;
    app
}

#[tokio::test]
async fn every_sessions_footer_binding_is_handled() {
    let table = dialog_keys(DialogScope::Sessions);
    assert!(
        table.len() >= 10,
        "Sessions keymap shrank unexpectedly: {} bindings",
        table.len()
    );

    for dialog_key in table {
        for (code, mods) in dialog_key.events {
            let mut app = sessions_app().await;
            let before = probe(&app);
            app.handle_sessions_key(KeyEvent::new(*code, *mods))
                .await
                .unwrap_or_else(|e| {
                    panic!(
                        "{}:{} ({code}) errored: {e}",
                        dialog_key.label, dialog_key.verb
                    )
                });
            let after = probe(&app);
            assert_ne!(
                before, after,
                "{}:{} ({code}) is advertised in the footer but did nothing to the app state",
                dialog_key.label, dialog_key.verb
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Gutters and columns (#2175). The border removal in #2162 took away the right
// padding a border used to supply for free, and only the left gutter was put
// back, so every row ran flush to the last column. The right half was also
// built row by row and drawn as one right-aligned blob, so a row carrying one
// more badge than its neighbour slid every field to its left.
// ---------------------------------------------------------------------------

use crate::tui::render::responsive::EDGE_INSET;

/// The pane edge keeps the same breathing space on both sides.
#[tokio::test]
async fn sessions_rows_are_inset_on_both_sides() {
    let mut app = app_in(AppMode::Sessions).await;
    app.sessions = vec![session("CrabDev"), session("Crabsland")];

    let (w, h) = (100u16, 20u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    // Row 0 is selected and carries a full-width selection bar on purpose:
    // a bar that stopped short of the edge would read as a highlight of the
    // text rather than of the row. Every other row is the one to measure.
    let plain: Vec<&String> = rows.iter().filter(|r| r.contains("Crabsland")).collect();
    assert_eq!(plain.len(), 1, "expected exactly one plain row: {rows:?}");

    let row = plain[0];
    let inset = EDGE_INSET as usize;
    assert_eq!(
        row.chars().count(),
        w as usize,
        "row width changed: {row:?}"
    );
    assert!(
        row.starts_with(&" ".repeat(inset)),
        "left gutter missing: {row:?}"
    );
    assert!(
        row.ends_with(&" ".repeat(inset)),
        "right gutter missing, the row still ends flush at the pane edge: {row:?}"
    );
}

/// The right half is a table: a badge that only one row has must not move the
/// columns its neighbours share.
#[tokio::test]
async fn sessions_columns_do_not_shift_between_rows() {
    let mut app = app_in(AppMode::Sessions).await;

    // Same date on both, one carries a provider pair the other does not. That
    // single difference is what used to slide the date by the width of the
    // badge, which is how two adjacent rows could disagree by 101 columns.
    let mut bare = session("Bare");
    bare.updated_at = chrono::Utc::now();
    let mut rich = session("Rich");
    rich.updated_at = bare.updated_at;
    rich.provider_name = Some("opencrabs".to_string());
    rich.model = Some("flash".to_string());
    rich.token_count = 4_657_300;
    app.sessions = vec![bare, rich];

    let (w, h) = (160u16, 20u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    let bare_row = rows
        .iter()
        .find(|r| r.contains("Bare"))
        .unwrap_or_else(|| panic!("bare row missing: {rows:?}"));
    let rich_row = rows
        .iter()
        .find(|r| r.contains("Rich"))
        .unwrap_or_else(|| panic!("rich row missing: {rows:?}"));

    let date_of = |row: &str| {
        row.find("2026-")
            .unwrap_or_else(|| panic!("no date on row: {row:?}"))
    };
    assert_eq!(
        date_of(bare_row),
        date_of(rich_row),
        "the date column slid between rows:\nbare: {bare_row:?}\nrich: {rich_row:?}"
    );
    assert_eq!(
        bare_row.chars().count(),
        rich_row.chars().count(),
        "rows disagree on width:\nbare: {bare_row:?}\nrich: {rich_row:?}"
    );
    assert!(
        rich_row.contains("[opencrabs/flash]"),
        "provider badge must still render at 160 cols: {rich_row:?}"
    );
}

/// The footer's right label is inset by the same amount as the rows, so the
/// bottom of the screen lines up with the list above it.
#[tokio::test]
async fn sessions_footer_label_is_inset() {
    let mut app = app_in(AppMode::Sessions).await;
    app.sessions = vec![session("CrabDev")];

    // Wide enough that every binding fits beside the label, so this measures
    // the inset rather than whether the label was dropped for room.
    let (w, h) = (200u16, 20u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    let last = &rows[h as usize - 1];
    assert!(
        last.contains("sessions"),
        "footer label missing from the last row: {last:?}"
    );
    assert_eq!(
        last.trim_end().chars().count(),
        w as usize - EDGE_INSET as usize,
        "the footer label must stop {EDGE_INSET} columns short of the edge: {last:?}"
    );
    assert!(
        last.starts_with(&" ".repeat(EDGE_INSET as usize)),
        "the footer's left inset is missing: {last:?}"
    );
}

/// A row without a badge reserves that badge's columns instead of sliding its
/// neighbours into them.
#[tokio::test]
async fn sessions_reserve_columns_for_absent_badges() {
    let mut app = app_in(AppMode::Sessions).await;
    let mut bare = session("Bare");
    bare.updated_at = chrono::Utc::now();
    let mut rich = session("Rich");
    rich.updated_at = bare.updated_at;
    rich.provider_name = Some("opencrabs".to_string());
    rich.model = Some("flash".to_string());
    app.sessions = vec![bare, rich];

    let (w, h) = (160u16, 20u16);
    let buf = draw(&mut app, w, h);
    let rows = all_rows(&buf, w, h);

    let bare_row = rows.iter().find(|r| r.contains("Bare")).unwrap();
    let rich_row = rows.iter().find(|r| r.contains("Rich")).unwrap();

    let badge = "[opencrabs/flash]";
    let start = rich_row
        .find(badge)
        .unwrap_or_else(|| panic!("provider badge missing: {rich_row:?}"));
    let end = start + badge.chars().count();
    let reserved: String = bare_row.chars().skip(start).take(end - start).collect();
    assert_eq!(
        reserved,
        " ".repeat(end - start),
        "the bare row did not reserve the provider badge's columns:\nbare: {bare_row:?}\nrich: {rich_row:?}"
    );
}

/// Narrow terminals still draw a usable list instead of panicking or writing
/// outside the pane.
#[tokio::test]
async fn sessions_render_at_narrow_widths() {
    for width in [40u16, 60u16] {
        let mut app = app_in(AppMode::Sessions).await;
        // Long enough that the name column must ellipsise at both widths,
        // which is what proves the column still has room rather than being
        // squeezed to nothing by the badges.
        app.sessions = vec![
            session("CrabDev with a title far longer than a narrow terminal"),
            session("Crabsland"),
        ];

        let height = 20u16;
        let buf = draw(&mut app, width, height);
        let rows = all_rows(&buf, width, height);

        // A draw that wrote outside the buffer would have panicked in
        // TestBackend, so reaching here already proves containment.
        for row in &rows {
            assert_eq!(
                row.chars().count(),
                width as usize,
                "row width changed at {width} cols: {row:?}"
            );
        }
        let name_row = rows
            .iter()
            .find(|r| r.contains("Crab"))
            .unwrap_or_else(|| panic!("name column vanished at {width} cols: {rows:?}"));
        assert!(
            name_row.contains('\u{2026}'),
            "the long name should be ellipsised at {width} cols, not dropped: {name_row:?}"
        );
    }
}
