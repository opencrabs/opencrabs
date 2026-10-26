//! #728: expanding/collapsing a block via click must keep the clicked header on
//! the same screen row instead of jumping the viewport. `anchored_scroll_offset`
//! turns the anchored message's line position into the from-bottom scroll offset
//! that pins it.

use crate::tui::render::chat::{anchored_scroll_offset, format_turn_spinner_meta};

#[test]
fn header_stays_on_its_row_after_growth() {
    // Header sits on screen row 3. After expanding, its first line is at index
    // 40 in a transcript whose max scroll is 100. To keep it on row 3 the top
    // visible line must be 37, i.e. 63 lines up from the bottom.
    let (offset, auto) = anchored_scroll_offset(40, 3, 100);
    assert_eq!(offset, 63, "offset pins the header to its row");
    assert!(!auto, "not at the bottom, so auto-scroll stays off");
}

#[test]
fn anchor_at_top_scrolls_all_the_way_up() {
    // Header is the very first line and should sit on row 0 -> top of buffer,
    // which is the maximum from-bottom offset.
    let (offset, auto) = anchored_scroll_offset(0, 0, 100);
    assert_eq!(offset, 100);
    assert!(!auto);
}

#[test]
fn anchor_resolving_to_bottom_rearms_auto_scroll() {
    // Collapsing near the bottom: the desired top (line_top - row) meets or
    // exceeds max_scroll, so we sit at the bottom and re-arm auto-scroll.
    let (offset, auto) = anchored_scroll_offset(100, 0, 40);
    assert_eq!(offset, 0, "clamped to the bottom");
    assert!(
        auto,
        "at the bottom, auto-scroll re-arms so streaming sticks"
    );
}

#[test]
fn row_below_line_top_saturates_without_panic() {
    // Defensive: an anchor row larger than the line index must not underflow.
    let (offset, auto) = anchored_scroll_offset(2, 10, 50);
    assert_eq!(offset, 50, "desired top saturates to 0 -> full scroll up");
    assert!(!auto);
}

// ── format_turn_spinner_meta (#741, #744) ───────────────────────

#[test]
fn spinner_meta_labels_turn_total() {
    let m = format_turn_spinner_meta(8, 31872).unwrap();
    assert!(m.contains("8s"), "{m}");
    assert!(
        m.contains("31872 tok this turn"),
        "counter must read as a turn total, not a single thought: {m}"
    );
    assert!(m.starts_with(" (") && m.ends_with(')'), "{m}");
}

#[test]
fn spinner_meta_is_none_when_nothing_to_show() {
    assert!(format_turn_spinner_meta(0, 0).is_none());
}

#[test]
fn spinner_meta_never_shows_ctx() {
    // ctx already lives in the footer under the input — the spinner must not
    // duplicate it (#744).
    let m = format_turn_spinner_meta(3, 120).unwrap();
    assert!(m.contains("3s") && m.contains("120 tok this turn"), "{m}");
    assert!(
        !m.contains("ctx"),
        "spinner must not repeat the ctx budget: {m}"
    );
}

// ── Expanding a block is not "content appended below" (#743 follow-up) ──────
// The growth compensation existed for streaming output arriving at the bottom
// while the user is scrolled up. A click or ctrl+o that opens a block grows
// the transcript too, but where the user is looking, so absorbing it scrolled
// the view up by exactly the size of the expansion: several pages for a big
// tool group.

use crate::tui::render::chat::should_compensate_for_growth;

#[test]
fn streaming_growth_is_still_compensated() {
    assert!(should_compensate_for_growth(false, 40, 100, 130, false));
}

#[test]
fn an_expand_is_never_compensated() {
    // The case that produced the jump.
    assert!(
        !should_compensate_for_growth(false, 40, 100, 400, true),
        "a user-opened block must not shift the viewport"
    );
}

#[test]
fn an_expand_is_not_compensated_even_when_it_grows_hugely() {
    assert!(!should_compensate_for_growth(false, 5, 100, 5000, true));
}

#[test]
fn auto_scroll_never_compensates() {
    // Pinned to the bottom already, so there is nothing to hold still.
    assert!(!should_compensate_for_growth(true, 40, 100, 130, false));
}

#[test]
fn the_first_render_never_compensates() {
    // prev_lines == 0 would make the delta the whole transcript.
    assert!(!should_compensate_for_growth(false, 40, 0, 500, false));
}

#[test]
fn shrinking_never_compensates() {
    // Collapsing a block removes lines; there is no delta to absorb.
    assert!(!should_compensate_for_growth(false, 40, 300, 100, false));
    assert!(!should_compensate_for_growth(false, 40, 300, 100, true));
}

#[test]
fn at_the_bottom_there_is_nothing_to_hold_still() {
    assert!(!should_compensate_for_growth(false, 0, 100, 130, false));
}

// ── turn_anchor_row (#2241) ────────────────────────────────────
//
// The capture step used to be `line_top.saturating_sub(chat_render_scroll) as
// u16`. When the header sat ABOVE the viewport that saturated to row 0, which
// is not where the header is: it is not on screen at all. The render believed
// it, `anchored_scroll_offset` computed `desired_top = line_top - 0`, and the
// header was scrolled to the top row, throwing the transcript up by
// `scroll_top - line_top`. A folded turn still draws its final answer, so the
// header is off screen for any answer taller than the viewport: the normal
// state at the bottom of a real turn, which is exactly where the header offers
// the key.

use crate::tui::render::chat::turn_anchor_row;

#[test]
fn off_screen_header_has_no_row_to_pin() {
    // Header 200 lines above the first visible line.
    assert_eq!(turn_anchor_row(760, 960), None);
    // One line above is already off screen.
    assert_eq!(turn_anchor_row(959, 960), None);
}

#[test]
fn on_screen_header_returns_its_row() {
    assert_eq!(turn_anchor_row(960, 960), Some(0), "header on the top row");
    assert_eq!(turn_anchor_row(975, 960), Some(15));
    assert_eq!(
        turn_anchor_row(1260, 960),
        Some(300),
        "header below the fold"
    );
}

#[test]
fn a_row_beyond_u16_saturates_instead_of_wrapping() {
    // A very long transcript scrolled to the very top: the row cannot fit a
    // u16, and wrapping would pin the header to a nonsense row.
    assert_eq!(
        turn_anchor_row(usize::from(u16::MAX) + 10, 0),
        Some(u16::MAX)
    );
}

/// Replay of the turn-anchor step in `render_chat` as it stands now: capture the
/// row from the pre-toggle line map, then apply `anchored_scroll_offset`. With
/// no row to pin, both the offset and the auto-scroll flag are left alone.
fn replay_turn_pin(
    line_top: usize,
    total_lines: usize,
    visible_height: usize,
    scroll_offset: usize,
    auto_scroll: bool,
) -> (usize, bool) {
    let max_scroll = total_lines.saturating_sub(visible_height);
    let scroll_top = max_scroll.saturating_sub(scroll_offset);
    match turn_anchor_row(line_top, scroll_top) {
        Some(row) => anchored_scroll_offset(line_top, row as usize, max_scroll),
        None => (scroll_offset, auto_scroll),
    }
}

/// The same replay with the old `saturating_sub` capture, kept so the test can
/// prove the old code jumped and the new one does not.
fn replay_turn_pin_clamped(
    line_top: usize,
    total_lines: usize,
    visible_height: usize,
    scroll_offset: usize,
) -> (usize, bool) {
    let max_scroll = total_lines.saturating_sub(visible_height);
    let scroll_top = max_scroll.saturating_sub(scroll_offset);
    let row = line_top.saturating_sub(scroll_top) as u16;
    anchored_scroll_offset(line_top, row as usize, max_scroll)
}

#[test]
fn expanding_a_tall_answer_no_longer_throws_the_viewport_up() {
    // At the bottom of a 1000-line transcript in a 40-row viewport: the newest
    // turn's header sits 200 lines above the top. This is the reported case.
    let (offset, auto) = replay_turn_pin(760, 1000, 40, 0, true);
    assert_eq!(offset, 0, "still at the bottom, the turn grows in place");
    assert!(auto, "auto-scroll survives the toggle");

    let (old_offset, _) = replay_turn_pin_clamped(760, 1000, 40, 0);
    assert_eq!(old_offset, 200, "the old capture jumped up 200 lines");
    assert_ne!(old_offset, offset, "the regression is real and now fixed");
}

#[test]
fn collapsing_a_long_expanded_turn_no_longer_jumps() {
    // Scrolled up 200 lines from the bottom of a 1400-line transcript; the
    // header of the expanded turn is 400 lines above the top.
    let (offset, auto) = replay_turn_pin(760, 1400, 40, 200, false);
    assert_eq!(offset, 200, "the viewport holds still across the collapse");
    assert!(!auto, "auto-scroll was off before, and stays off");

    let (old_offset, _) = replay_turn_pin_clamped(760, 1400, 40, 200);
    assert_eq!(old_offset, 600, "the old capture jumped up 400 lines");
}

#[test]
fn a_visible_header_still_pins_to_its_row() {
    // The case #728 and #758 were built for: header on screen row 15, so the
    // pin must reproduce the current offset exactly rather than move anything.
    let (offset, auto) = replay_turn_pin(875, 1000, 40, 100, false);
    assert_eq!(offset, 100, "on-screen pin is a no-op on the offset");
    assert!(!auto);
}

#[test]
fn auto_scroll_is_not_recomputed_when_there_is_no_pin() {
    // With the header off screen nothing pins, so a user who was following the
    // bottom keeps following it instead of being silently parked mid-history.
    let (_, auto) = replay_turn_pin(760, 1000, 40, 0, true);
    assert!(auto);
}

#[test]
fn an_off_screen_toggle_still_suppresses_growth_compensation() {
    // No row to pin means the anchor is None, so the pending flag is what keeps
    // the block's own new lines from being absorbed as if they arrived at the
    // bottom. Without it the view would shift by the number of lines gained.
    assert!(
        !should_compensate_for_growth(false, 40, 100, 400, true),
        "a toggle with no pin is still user-driven growth"
    );
    assert!(
        should_compensate_for_growth(false, 40, 100, 400, false),
        "a toggle with no pin and no flag would compensate, which is the bug"
    );
}
