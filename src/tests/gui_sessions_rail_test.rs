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

    let rail_root = block(SESSIONS, "pub fn rail(", ".child(brand_header())");
    assert!(
        !rail_root.contains(".p_2()") && !rail_root.contains(".gap_1()"),
        "the rail adds no outer gutter or gap around its rows"
    );
    assert!(
        SESSIONS.contains(".when(!selected, |d| d.hover(")
            && SESSIONS.contains(".bg(rgb(palette::current().panel))"),
        "hover still paints on the full-width row, and every card paints the same panel (#2792)"
    );
}

#[test]
fn the_rail_header_is_compact_opencrabs_branding() {
    let header = block(SESSIONS, "fn brand_header()", "#[cfg(test)]");
    assert!(header.contains("icon::banner_logo()"));
    assert!(header.contains(".child(\"OpenCrabs\")"));
    assert!(header.contains(".text_sm()"));
    assert!(
        !SESSIONS.contains("SESSIONS · {}"),
        "the brand replaces the redundant session counter"
    );
}

#[test]
fn grouped_topics_are_compact_visible_pins_not_a_dropdown() {
    let ordered = block(SESSIONS, "pub fn ordered(", "/// Whether a row is a topic");
    // #2701 replaced the fold tree with chips that always paint, so the order
    // keeps every topic directly beneath its parent and hides none of them.
    // The toggle is a key, not rail chrome: the row still paints no fold
    // button or dropdown.
    assert!(
        ordered.contains("self.parents.get(&s.id) == Some(&top.id) && grouped(s)"),
        "a topic always follows its parent in the order"
    );
    assert!(
        !ordered.contains("is_collapsed"),
        "the order never hides a topic behind a fold"
    );

    let row = block(SESSIONS, "pub fn row(", "pub fn on_click(");
    assert!(
        !row.contains("topic_pin_mark"),
        "the pre-title branch mark is gone: one pin, in the trailing cluster (#2788)"
    );
    assert!(
        row.contains(".when(indented, |d| d.child(pin_icon(favorited, 0.9)))"),
        "a grouped topic keeps its single pin in the trailing cluster"
    );
    assert!(row.contains(".when(indented, |d| d.pl_4().pr_2().py_1())"));
    assert!(row.contains("title_text.text_xs()"));
    assert!(
        row.contains(".when(!indented, |d|"),
        "compact topic pins omit repeated date and cost metadata"
    );
    assert!(
        !SESSIONS.contains("pub fn fold_button(") && !GUI_MAIN.contains("sessions::fold_button("),
        "no fold button joins the row chrome"
    );
    assert!(
        GUI_MAIN.contains("sessions::row(s, flags, action, chips)"),
        "topic pins keep their click action, slide actions, and context menu"
    );
}
