//! Regression tests for the input-border notice (#1369).
//!
//! The "Copied to clipboard" toast and the error toast used to live inside
//! the chat line buffer, so showing one grew the scrollable content by three
//! rows and the history jumped. They now render as a title on the input box's
//! bottom border, and the expiry rule moved out of the renderer into a pure
//! helper driven by the tick.

use std::time::{Duration, Instant};

use crate::tui::render::notice::{
    CLIPBOARD_HINT_TTL, ERROR_TTL, NOTIFICATION_TTL, NoticeKind, notice_expired, notice_label,
    pick_notice,
};

const TTL: Duration = Duration::from_secs(2);

#[test]
fn untimestamped_notice_never_expires() {
    // Runner errors carry no `shown_at` and must stay until the user acts.
    assert!(!notice_expired(None, Instant::now(), TTL));
}

#[test]
fn fresh_notice_is_not_expired() {
    let shown = Instant::now();
    assert!(!notice_expired(
        Some(shown),
        shown + Duration::from_millis(1999),
        TTL
    ));
}

#[test]
fn notice_expires_exactly_at_ttl() {
    let shown = Instant::now();
    assert!(notice_expired(Some(shown), shown + TTL, TTL));
    assert!(notice_expired(
        Some(shown),
        shown + Duration::from_secs(30),
        TTL
    ));
}

#[test]
fn clock_before_shown_at_is_not_expired() {
    // A monotonic clock cannot run backwards, but the helper must not panic
    // or expire if `now` is somehow earlier than `shown_at`.
    let shown = Instant::now() + Duration::from_secs(5);
    assert!(!notice_expired(Some(shown), Instant::now(), TTL));
}

#[test]
fn error_ttl_is_longer_than_notification_ttl() {
    // The 2.5s error toast and the 2s info toast kept their pre-#1369 timings.
    assert_eq!(NOTIFICATION_TTL, Duration::from_secs(2));
    assert_eq!(ERROR_TTL, Duration::from_millis(2500));
    assert_eq!(CLIPBOARD_HINT_TTL, Duration::from_secs(6));
    assert!(ERROR_TTL > NOTIFICATION_TTL);
    assert!(CLIPBOARD_HINT_TTL > ERROR_TTL);
}

#[test]
fn error_outranks_info_in_the_single_slot() {
    assert_eq!(
        pick_notice(Some("boom"), Some("Copied to clipboard"), None),
        Some((NoticeKind::Error, "boom"))
    );
}

#[test]
fn info_alone_is_shown_as_info() {
    assert_eq!(
        pick_notice(None, Some("Copied to clipboard"), None),
        Some((NoticeKind::Info, "Copied to clipboard"))
    );
}

#[test]
fn the_clipboard_hint_is_the_lowest_priority_candidate() {
    assert_eq!(
        pick_notice(Some("boom"), Some("Copied to clipboard"), Some("hint")),
        Some((NoticeKind::Error, "boom"))
    );
    assert_eq!(
        pick_notice(None, Some("Copied to clipboard"), Some("hint")),
        Some((NoticeKind::Info, "Copied to clipboard"))
    );
    assert_eq!(
        pick_notice(None, None, Some("hint")),
        Some((NoticeKind::Info, "hint"))
    );
    assert_eq!(pick_notice(None, None, None), None);
}

#[test]
fn short_label_is_padded_with_one_space_each_side() {
    assert_eq!(
        notice_label("Copied to clipboard", 40).as_deref(),
        Some(" Copied to clipboard ")
    );
}

#[test]
fn label_that_exactly_fits_is_not_cut() {
    // 5 chars of text + 2 padding = 7 cells.
    assert_eq!(notice_label("abcde", 7).as_deref(), Some(" abcde "));
}

#[test]
fn long_label_is_cut_with_an_ellipsis_inside_the_slot() {
    let label = notice_label("Press Esc again to clear input", 12).unwrap();
    assert_eq!(label, " Press Esc… ");
    assert_eq!(label.chars().count(), 12);
}

#[test]
fn multi_line_error_collapses_to_one_row() {
    let error = "render panic at x.rs:1\n\n   caller: y\tz   ";
    assert_eq!(
        notice_label(error, 80).as_deref(),
        Some(" render panic at x.rs:1 caller: y z ")
    );
}

#[test]
fn label_counts_characters_not_bytes() {
    // 5 multi-byte chars + padding must fit a 7-cell slot without a cut.
    assert_eq!(notice_label("éàçñü", 7).as_deref(), Some(" éàçñü "));
}

#[test]
fn slot_too_narrow_for_text_plus_ellipsis_yields_nothing() {
    assert_eq!(notice_label("Copied to clipboard", 3), None);
    assert_eq!(notice_label("Copied to clipboard", 0), None);
}

#[test]
fn whitespace_only_text_yields_nothing() {
    assert_eq!(notice_label("  \n\t ", 40), None);
}

#[test]
fn chat_renderer_no_longer_reads_transient_notices() {
    // The whole point of #1369: the chat line buffer must not depend on the
    // notice, so its line count is identical with and without one live. The
    // only way that can regress is a read of these fields creeping back in.
    let chat = include_str!("../tui/render/chat.rs");
    assert!(
        !chat.contains("app.notification"),
        "chat.rs reads app.notification again; the notice belongs on the input border"
    );
    assert!(
        !chat.contains("app.error_message"),
        "chat.rs reads app.error_message again; the notice belongs on the input border"
    );
}

#[test]
fn input_renderer_owns_the_notice() {
    let input = include_str!("../tui/render/input.rs");
    assert!(input.contains("notice::pick_notice("));
    assert!(input.contains("app.notification.as_deref()"));
    assert!(input.contains("app.error_message.as_deref()"));
}

#[test]
fn notice_is_appended_to_the_cursor_row_not_a_border_title() {
    // The ask was "inside the input": trailing ghost text on the cursor's
    // row, never a title hung on the bottom border. Exactly one thing is
    // allowed to be a border title: the ctx budget. The #2237 in-flight
    // "Enter queues" hint used to make it two and was removed in #2243, so a
    // second title fails here again.
    let input = include_str!("../tui/render/input.rs");
    let body = input
        .split("pub(super) fn render_input(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("render_input body");
    assert!(
        body.contains("input_lines.get_mut(cursor_row)"),
        "the notice must be appended to the cursor row of the input"
    );
    assert_eq!(
        body.matches(".title_bottom(").count(),
        1,
        "only the ctx budget may sit on the bottom border"
    );
    assert!(
        body.contains(".title_bottom(context_title)"),
        "the ctx budget must still be the right-hand bottom title"
    );
    assert!(
        !body.contains("title_bottom(notice"),
        "the notice must not be a border title"
    );
}

#[test]
fn notice_label_fits_the_room_left_on_the_cursor_row() {
    // Empty input renders "❯ " plus the cursor cell (3 cells); a 40-cell
    // content row leaves 37 for the notice.
    let room = 40usize.saturating_sub(3);
    assert_eq!(
        notice_label("Copied to clipboard", room).as_deref(),
        Some(" Copied to clipboard ")
    );
    // A row already full of typed text has no room and shows nothing rather
    // than pushing the text or wrapping onto a row the layout never allotted.
    assert_eq!(notice_label("Copied to clipboard", 0), None);
}
