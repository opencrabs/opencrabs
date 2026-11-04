//! Tests for session_search discovery (#1203): the `updated_since` parser
//! and per-row turn-state reporting. House rule: no inline test modules in
//! source files — everything lives here under src/tests/.

use crate::brain::tools::session_search::{SessionSearchTool, parse_updated_since};
use crate::channels::telegram::TelegramState;
use crate::db::Database;
use std::sync::Arc;
use uuid::Uuid;

#[test]
fn parses_rfc3339() {
    let dt = parse_updated_since("2026-08-25T00:00:00Z").expect("valid rfc3339");
    assert_eq!(dt.to_rfc3339(), "2026-08-25T00:00:00+00:00");
}

#[test]
fn parses_shorthand() {
    let before = chrono::Utc::now();
    let dt = parse_updated_since("24h").expect("24h valid");
    assert!(dt > before - chrono::Duration::hours(25));
    assert!(dt <= chrono::Utc::now());

    let dt = parse_updated_since("7d").expect("7d valid");
    assert!(dt < before - chrono::Duration::days(6));
}

#[test]
fn rejects_garbage() {
    assert!(parse_updated_since("yesterday").is_err());
    assert!(parse_updated_since("").is_err());
    assert!(parse_updated_since("12x").is_err());
    assert!(parse_updated_since("-5d").is_err());
}

/// The unit is the final CHARACTER, not the final byte. Slicing at `len - 1`
/// landed inside a multi-byte unit and panicked (#2124).
#[test]
fn rejects_a_multibyte_unit_without_panicking() {
    // "°" is 2 bytes, so `len - 1` sat inside it.
    assert!(parse_updated_since("24°").is_err());
    // A 4-byte emoji: three of the four bytes are non-boundaries.
    assert!(parse_updated_since("7🎯").is_err());
}

#[tokio::test]
async fn stateless_tool_reports_no_turn_state() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let tool = SessionSearchTool::new(db.pool().clone());
    // Core (daemon/cron) registration wires no channel state, so rows omit
    // turn info instead of guessing idle.
    assert_eq!(tool.turn_state(Uuid::new_v4()), None);
}

#[tokio::test]
async fn turn_state_tracks_the_active_turn_guard() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let tg = Arc::new(TelegramState::new());
    let tool = SessionSearchTool::with_telegram(db.pool().clone(), tg.clone());
    let sid = Uuid::new_v4();

    // No turn in flight: idle, not unknown.
    assert_eq!(tool.turn_state(sid), Some("idle"));

    // RAII guard held by a running turn flips the report to running...
    let guard = tg.mark_turn_active(sid);
    assert_eq!(tool.turn_state(sid), Some("running"));

    // ...and dropping it (normal return, early return, panic or cancel)
    // clears the flag so the session reads idle again (#302 Stage 2).
    drop(guard);
    assert_eq!(tool.turn_state(sid), Some("idle"));
}
