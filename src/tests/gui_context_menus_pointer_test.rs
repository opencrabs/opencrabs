//! Every right press opens its card at the pointer that asked for it
//! (#2690, #2749).
//!
//! The window has no headless harness, so the wiring is read from source. The
//! rule below is the one that kept breaking: a card placed from the geometry
//! of the row that owns it (absolute at `right_1` / `top_1`, then beside the
//! rail at the press height) answers "where is the row", not "where did the
//! user click". A long message pushed its menu far from the press, and in a
//! split the pane's own frame moved it again.

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const EDITOR: &str = include_str!("../gui/editor.rs");
const MENU: &str = include_str!("../gui/menu.rs");

/// The body of one item, from its own definition to the brace that closes it.
/// Only a real definition counts: the scans below quote the names they look
/// for inside their own strings.
fn item_body(src: &'static str, needle: &str) -> &'static str {
    let at = src
        .match_indices(needle)
        .map(|(at, _)| at)
        .find(|&at| {
            let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
            let prefix = src[line..at].trim();
            prefix.is_empty() || prefix.starts_with("pub")
        })
        .unwrap_or_else(|| panic!("{needle} not found"));
    let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let indent = src[line..at]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    let close = format!("\n{}}}", " ".repeat(indent));
    let rest = &src[at..];
    rest.split(&close).next().unwrap_or(rest)
}

fn fn_body(src: &'static str, name: &str) -> &'static str {
    item_body(src, &format!("fn {name}("))
}

/// A file without its own test module. The scans below count occurrences, and
/// a test that quotes the string it looks for would count itself.
fn code(src: &'static str) -> &'static str {
    src.split("\n#[cfg(test)]\nmod tests").next().unwrap_or(src)
}

/// A menu is only at the pointer if the press recorded one. Every surface that
/// answers a right press keeps the point it landed on, in window coordinates,
/// and hands that to the card.
#[test]
fn every_right_press_records_the_point_it_happened_at() {
    // Two entry points on the rail: a row and a topic chip.
    assert_eq!(
        code(GUI_MAIN)
            .matches("this.rail_menu_at = Some(ev.position);")
            .count(),
        2,
        "a rail row and a topic chip each record their press"
    );
    assert!(
        code(GUI_MAIN).contains("this.topic_menu_at = Some(ev.position);"),
        "a topic tab records its press too"
    );
    assert!(
        code(GUI_MAIN).contains("this.msg_menu_open(id, ev.position, cx);"),
        "a stored message row opens its menu at the press"
    );
    assert!(
        code(EDITOR).contains("self.ctx_menu_at = Some(event.position);"),
        "the composer keeps the point a right press landed on (#2749)"
    );
    // One right press on the composer: the field is one target, so a second
    // handler would mean two menus for one click.
    assert_eq!(
        code(EDITOR)
            .matches("on_mouse_down(MouseButton::Right")
            .count(),
        1,
        "the composer answers a right press exactly once"
    );
    // The press is window coordinates, never field-local: the card is placed
    // from it, and the field's own origin would move it (#2690).
    let press = fn_body(EDITOR, "on_right_press");
    assert!(
        press.contains("cx.stop_propagation()"),
        "the press that opens the card is not also a press on the surface below"
    );
}

/// One popover places every card. Four menus, one placement rule: the rail,
/// the chat's topic menu, a message row's menu and the composer's.
#[test]
fn every_menu_goes_through_the_one_pointer_popover() {
    assert_eq!(
        MENU.matches("pub fn menu_popover(").count(),
        1,
        "one popover serves every menu"
    );
    let popover = fn_body(MENU, "menu_popover");
    assert!(
        popover.contains("anchored()"),
        "the card is anchored in window coordinates, not absolute in the row"
    );
    assert!(
        popover.contains(".position(at)"),
        "the press point is the anchor it opens from"
    );
    assert!(
        popover.contains(".snap_to_window_with_margin("),
        "a card near the window edge is pulled back inside it"
    );
    assert!(
        popover.contains("menu_card(id, items)"),
        "the one card builder is what the popover places"
    );
    for call in [
        "sessions::menu_popover(\"rail-context-menu\", items, at)",
        "sessions::menu_popover(\"chat-menu\", items, at)",
    ] {
        assert!(GUI_MAIN.contains(call), "the window draws {call}");
    }
    assert!(
        TRANSCRIPT.contains("crate::menu::menu_popover(\"message-context-menu\", rows, at)"),
        "the message menu is a window-level card, not a child of its row"
    );
    assert!(
        EDITOR.contains("crate::menu::menu_popover(\"composer-context-menu\", rows, at)"),
        "the composer menu is drawn by the same popover (#2749)"
    );
    // Nothing is placed from the geometry of the surface that owns it any
    // more: no card built straight from a row, and no absolute corner.
    assert!(
        !GUI_MAIN.contains("sessions::menu_card("),
        "the window never builds a card it has not anchored at a press"
    );
    assert!(
        !TRANSCRIPT.contains("message_menu_card"),
        "the message row holds no card of its own"
    );
    let card = fn_body(MENU, "menu_card");
    for corner in ["right_1", "right_2", "top_1", "top_3"] {
        assert!(
            !card.contains(corner),
            "the card carries no absolute {corner} placement"
        );
    }
}

/// The card swallows its own presses. Its items pick on mouse-up and every
/// surface behind it dismisses on mouse-down, so a press that bubbled through
/// would remove the card before the item it landed on could fire.
#[test]
fn the_card_stops_its_own_presses() {
    let card = fn_body(MENU, "menu_card");
    assert!(
        card.contains(".on_mouse_down(MouseButton::Left"),
        "the card answers its own left press"
    );
    assert!(
        card.contains("cx.stop_propagation()"),
        "and stops it there instead of letting the host close the card"
    );
    assert!(
        card.contains(".occlude()"),
        "it also occludes, so nothing beneath it takes the click"
    );
}

/// The composer's card is the four edits a text field is expected to offer,
/// and the two that need a selection are drawn disabled rather than dropped,
/// so the card keeps one shape under the pointer (#2749).
#[test]
fn the_composer_menu_offers_the_four_edits_and_disables_what_it_cannot_do() {
    let menu = fn_body(EDITOR, "context_menu");
    for label in ["Cut", "Copy", "Paste", "Select all"] {
        assert!(
            menu.contains(&format!("menu_item(\"{label}\")"))
                || menu.contains(&format!("menu_item_disabled(\"{label}\")")),
            "{label} is on the card"
        );
    }
    assert!(
        menu.contains("menu_item_disabled(\"Cut\")")
            && menu.contains("menu_item_disabled(\"Copy\")"),
        "with nothing selected, Cut and Copy are drawn disabled"
    );
    assert!(
        !menu.contains("menu_item_disabled(\"Paste\")")
            && !menu.contains("menu_item_disabled(\"Select all\")"),
        "Paste and Select all always have something to do"
    );
    assert!(
        menu.contains("if has_selection"),
        "the enablement is decided by the field's own selection"
    );
    // Paste goes through the action's own path, so a screenshot on the
    // clipboard still attaches as an image instead of pasting nothing (#2479).
    assert!(
        menu.contains("this.paste(&Paste, window, cx)"),
        "Paste is the existing action, not a second paste path"
    );
}

/// A right press does not move the caret: Cut and Copy act on what is
/// selected, and a press that collapsed the selection first would leave both
/// with nothing to act on (#2749).
#[test]
fn a_right_press_keeps_the_selection_it_will_act_on() {
    let press = fn_body(EDITOR, "on_right_press");
    assert!(
        !press.contains("move_to(") && !press.contains("select_to("),
        "the press opens the card without touching the selection"
    );
    assert!(
        press.contains("window.focus(&self.focus_handle)"),
        "and it still gives the field the keyboard, so Paste lands here"
    );
}

/// Esc closes the card, and a left press anywhere in the window closes it: the
/// card stops its own presses, so a press that reaches the window is a press
/// outside it.
#[test]
fn the_composer_menu_closes_on_escape_and_on_a_press_outside_it() {
    let keys = fn_body(EDITOR, "intercept_raw_key");
    assert!(
        keys.contains("self.ctx_menu_at.take()"),
        "Esc closes the composer's card"
    );
    assert!(
        EDITOR.contains("pub fn close_context_menu("),
        "the window can close it without reaching into the field"
    );
    let close = fn_body(GUI_MAIN, "close_composer_menus");
    assert!(
        close.contains("editor.close_context_menu(cx)"),
        "the window closes the composer's card through its own door"
    );
    assert!(
        close.contains("self.pane_states.values()"),
        "a split pane has its own composer, and only one of them opened the card"
    );
    assert!(
        GUI_MAIN.contains("this.close_composer_menus(cx);"),
        "the window's left press is what closes it"
    );
}

/// One press closes every card. A card is placed at a window point now, so no
/// surface owns its lifetime any more: the window's own press is what takes
/// down whichever one is open (#2690, #2749).
#[test]
fn one_left_press_closes_whichever_card_is_open() {
    // The window root's left press, bounded by the arm that ends it.
    let at = GUI_MAIN
        .find(".on_mouse_down(\n                MouseButton::Left,")
        .expect("the window root answers a left press");
    let rest = &GUI_MAIN[at..];
    let end = rest
        .find("this.close_composer_menus(cx);")
        .expect("the press closes the composers' cards");
    let press = &rest[..end];
    for closer in [
        "this.sessions.rail_menu.take()",
        "this.topic_menu.take()",
        "this.msg_menu.take()",
    ] {
        assert!(
            press.contains(closer),
            "the window's press closes it too: {closer}"
        );
    }
}
