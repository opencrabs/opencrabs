//! Telegram-like session rail presentation (#2540).
//!
//! GPUI has no headless pixel harness here, so these checks pin the exact
//! layout wiring. The ordering and keyboard behaviour are exercised by the
//! real `SessionsState` tests in `gui/sessions.rs`.

const SESSIONS: &str = include_str!("../gui/sessions.rs");
const GUI_MAIN: &str = include_str!("../gui/main.rs");

fn block<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = source.split_once(start).expect("source block starts");
    let (body, _) = tail.split_once(end).expect("source block ends");
    body
}

#[test]
fn session_rows_paint_edge_to_edge_without_card_chrome() {
    let row = block(SESSIONS, "pub fn row(", "pub fn on_click(");
    assert!(row.contains(".w_full()"), "each row fills the rail width");
    assert!(
        !row.contains(".rounded_sm()"),
        "a session row is flat, not an inset rounded card"
    );
    assert!(row.contains(".p_2()"), "the title keeps its internal inset");

    let rail_root = block(SESSIONS, "pub fn rail(", "    if state.filter_active");
    assert!(
        !rail_root.contains(".p_2()") && !rail_root.contains(".gap_1()"),
        "the rail adds no outer gutter or gap around its rows"
    );
    assert!(
        SESSIONS.contains(".when(!selected, |d| d.hover(")
            && SESSIONS.contains(".when(selected, |d| d.bg("),
        "hover and selection still paint on the full-width row"
    );
}

