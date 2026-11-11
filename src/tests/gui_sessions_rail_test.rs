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
fn rail_card_media_is_large_dual_and_the_title_is_bold() {
    assert!(
        SESSIONS.contains("pub const RAIL_MARK_REMS: f32 = 1.75;"),
        "full-card media is 28px at the default 16px root"
    );
    let wiring = block(
        GUI_MAIN,
        "icon: self.sessions.topic_icons.get(&id)",
        "};",
    );
    for resolver in [
        "topic_icons::tab_icon(stored, sessions::RAIL_MARK_REMS)",
        "self.custom_icon_picture(stored, sessions::RAIL_MARK_REMS)",
        "topic_icons::tab_glyph(stored, sessions::RAIL_MARK_REMS)",
    ] {
        assert!(
            wiring.contains(resolver),
            "stored media resolver uses the shared full-card size: {resolver}"
        );
    }

    let row = block(SESSIONS, "pub fn row(", "pub fn on_click(");
    let media = block(row, "let platform = rail::platform_of", "let title_text =");
    assert_eq!(
        media.matches(".child(icon)").count(),
        1,
        "the stored picture has one mount, not a second icon beside it"
    );
    assert!(
        media.contains(".relative()") && media.contains(".size(rems(RAIL_MARK_REMS))"),
        "the picture owns one 28px positioning box"
    );
    let badge = block(media, "let badge =", "            .relative()");
    for needle in [
        "platform_icons::platform_icon_with_colour(",
        "RAIL_CHANNEL_GLYPH_REMS",
        "palette::current().accent",
        ".absolute()",
        ".top_0()",
        ".right_0()",
        ".rounded_full()",
        ".size(rems(RAIL_CHANNEL_BADGE_REMS))",
        ".border_color(rgb(palette::current().accent))",
    ] {
        assert!(
            badge.contains(needle),
            "the rounded top-right channel badge uses the theme accent: {needle}"
        );
    }
    assert!(
        media.contains("let marks = picture.or_else")
            && media.contains("platform_icons::platform_icon(platform, RAIL_MARK_REMS)"),
        "only a missing picture falls back to the standalone 28px channel mark"
    );
    assert!(
        !media.contains(".when_some(platform_mark,") && !media.contains(".gap_2()"),
        "the side-by-side double-icon group is gone"
    );

    let title = block(row, "let title = div()", "let body = div()");
    assert!(
        title.contains(".gap_4()") && title.contains(".when_some(marks, |d, marks| d.child(marks))"),
        "the single media slot has 16px separation from the title"
    );
    let title_weight = block(row, "let title_text = if indented", "let title = div()");
    assert!(title_weight.contains("title_text.text_xs()"));
    assert!(title_weight.contains(".text_sm()"));
    assert!(
        title_weight.contains(".font_weight(gpui::FontWeight::BOLD)"),
        "only the full session title gains bold weight"
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
        row.contains(".when_some(pin_toggle, |d, pin| d.child(pin))"),
        "full and grouped rows share one pin control in the trailing cluster"
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
        GUI_MAIN.contains("sessions::row(s, flags, chips)"),
        "topic pins keep their click action and context menu"
    );
}

#[test]
fn rail_header_has_no_kebab_and_shares_one_line() {
    assert!(
        !SESSIONS.contains("dropbar") && !GUI_MAIN.contains("dropbar"),
        "the empty menu row and its renderer are gone"
    );
    assert!(
        !SESSIONS.contains('\u{22EE}'),
        "no vertical-dots glyph remains on the rail"
    );

    let row = block(SESSIONS, "pub fn row(", "pub fn on_click(");
    // The stamp owns a full line above the name (#2802): sharing the title
    // line is exactly what let the date paint over the title on a wide rail.
    let stamp = block(row, "let stamp = (!indented)", "let title = div()");
    assert!(
        stamp.contains("rail_stamp(s.updated_at") && stamp.contains("rail_cost(s.total_cost)"),
        "the stamp is the shared day-or-clock plus compact cost"
    );
    assert!(
        stamp.contains(".w_full()") && stamp.contains(".justify_end()"),
        "the stamp fills its own line and hugs the card's right edge"
    );
    let title = block(row, "let title = div()", "let body = div()");
    let trailing = title
        .split(".ml_auto()")
        .nth(1)
        .expect("right-aligned mark cluster");
    assert!(
        trailing.contains(".when_some(pin_toggle,") && trailing.contains(".when_some(badge,"),
        "the pin and the activity glyph keep the right edge of the name line"
    );
    assert!(
        !trailing.contains("s.total_cost") && !trailing.contains("%m-%d %H:%M"),
        "the stamp no longer shares a line with the title it used to overlap"
    );
    assert!(
        trailing.contains(".flex_row()") && trailing.contains(".gap_2()"),
        "the pin and the unread badge form one horizontal cluster"
    );
    assert!(
        !trailing.contains(".flex_col()") && !trailing.contains(".items_end()"),
        "no second stacked metadata line remains"
    );
    assert!(
        !row.contains("row.child(action)"),
        "the removed slide-out strip does not return"
    );
}

#[test]
fn rail_pin_toggles_without_opening_the_card() {
    let control = block(
        SESSIONS,
        "pub fn pin_toggle(",
        "/// The menu's contents",
    );
    for affordance in [
        "session-pin-toggle-{id}",
        ".size(rems(1.4))",
        ".rounded_sm()",
        ".cursor_pointer()",
        ".child(pin_icon(favorited, 0.9))",
    ] {
        assert!(
            control.contains(affordance),
            "the pin control keeps its target and artwork: {affordance}"
        );
    }
    assert!(
        control.contains(".hover(|d| d.bg(rgb(palette::current().panel_hi)))"),
        "the pin hover uses the rail's panel_hi"
    );

    let row = block(SESSIONS, "pub fn row(", "pub fn on_click(");
    assert_eq!(
        row.matches(".when_some(pin_toggle,").count(),
        1,
        "one wired control serves both full and compact rows"
    );

    let handler = block(
        GUI_MAIN,
        "let toggle_pin = cx.listener",
        "let flags = sessions::RowFlags",
    );
    assert!(handler.contains("cx.stop_propagation();"));
    assert!(handler.contains("this.toggle_favorite(id, cx);"));
    assert!(handler.contains("sessions::pin_toggle(id, favorite)"));
    assert!(handler.contains(".on_mouse_down(MouseButton::Left, toggle_pin)"));
    assert!(
        !handler.contains("pick_or_open") && !handler.contains("rail_swipe"),
        "the pin never opens the row or starts a slide"
    );

    let flags = block(
        GUI_MAIN,
        "let flags = sessions::RowFlags",
        "// A parent card carries",
    );
    assert!(flags.contains("pin_toggle: Some(pin_toggle)"));
}

#[test]
fn selected_topic_owns_the_parent_activity_line() {
    let resolution = block(
        GUI_MAIN,
        "let progress_id = children_by_parent",
        "let toggle_pin = cx.listener",
    );
    assert!(resolution.contains("children_by_parent.get(&id)"));
    assert!(resolution.contains("self.sessions"));
    assert!(resolution
        .contains("progress_id(id, kids.iter().map(|(kid_id, _)| *kid_id))"));
    assert!(
        !resolution.contains(".cursor"),
        "keyboard cursor is not an input to the progress owner"
    );

    let flags = block(
        GUI_MAIN,
        "let flags = sessions::RowFlags",
        "// A parent card carries",
    );
    assert_eq!(
        flags.matches(".get(&progress_id)").count(),
        2,
        "both content match and persisted preview belong to the selected child"
    );
    assert!(flags.contains("activity: self.rail_activity_for(progress_id)"));
    assert!(flags.contains("plan: plan_progress.get(&id).copied()"));
    assert!(flags.contains("running: running_tasks.get(&id).cloned()"));

    let body = block(SESSIONS, "let body = div()", "let mut row = div()");
    let chips = body.find(".when_some(chips,").expect("chip strip");
    let fallback = body
        .find("let preview = activity.or(preview);")
        .expect("live activity wins over the selected topic's preview");
    let activity = body
        .find(".when_some(preview,")
        .expect("the existing activity/preview line");
    assert!(
        chips < fallback && fallback < activity,
        "the selected topic's line stays below the chips"
    );
    assert_eq!(
        body.matches(".when_some(preview,").count(),
        1,
        "one existing renderer serves parents and selected topics"
    );
}
