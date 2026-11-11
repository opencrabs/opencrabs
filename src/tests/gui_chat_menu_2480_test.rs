//! The chat's `...` menu and the topic icons (#2480, #2304).
//!
//! The window has no headless harness, so the wiring is read from source. The
//! data half is real: the icons the tabs paint are the rows
//! `SessionRepository::topic_icons` returns, which is the call boot now makes.

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const SESSIONS: &str = include_str!("../gui/sessions.rs");
// The card, its item and the popover moved out of the session rail so the
// transcript can draw its own message menu without depending on it (#2613).
// One builder still serves every menu; only its address changed.
const MENU: &str = include_str!("../gui/menu.rs");

/// The icons vanished because the map started empty and only filled after a
/// close or a create. The rows were in the database the whole time; nothing
/// read them on the way in.
#[tokio::test]
async fn boot_reads_the_icon_rows_instead_of_starting_with_an_empty_map() {
    assert!(
        !GUI_MAIN.contains("topic_icons: HashMap::new()"),
        "boot fills topic_icons from the database, never with an empty map"
    );
    assert!(
        GUI_MAIN.contains("sessions::load_topic_icons("),
        "boot loads the icons it paints"
    );

    // The query behind that call: every session with an icon is returned,
    // root or topic, because the rename dialog sets one on any conversation
    // (#2790). An icon-less topic is still left out.
    let db = crate::db::database::Database::connect_in_memory()
        .await
        .unwrap();
    db.run_migrations().await.unwrap();
    let sessions = crate::db::repository::session::SessionRepository::new(db.pool().clone());

    let root = crate::db::models::Session::new(Some("root".into()), None, None);
    sessions.create(&root).await.unwrap();
    let topic = sessions.create_topic(root.id, "topic").await.unwrap();
    let _bare = sessions.create_topic(root.id, "bare").await.unwrap();
    sessions
        .set_topic_icon(topic, Some("\u{1f4a1}"))
        .await
        .unwrap();
    sessions
        .set_topic_icon(root.id, Some("\u{1f980}"))
        .await
        .unwrap();

    let rows = sessions.topic_icons().await.unwrap();
    let by_id: std::collections::HashMap<_, _> = rows.into_iter().collect();
    assert_eq!(
        by_id.get(&topic).map(String::as_str),
        Some("\u{1f4a1}"),
        "a topic keeps its icon"
    );
    assert_eq!(
        by_id.get(&root.id).map(String::as_str),
        Some("\u{1f980}"),
        "a root conversation carries its icon too, so its card paints the pick (#2790)"
    );
    assert_eq!(by_id.len(), 2, "the icon-less topic is still left out");
}

/// A menu that grows the strip is not a menu. Both menus draw the same card,
/// and the strip no longer has one to grow.
#[test]
fn the_menu_floats_as_a_card_and_the_strip_nothing_grows_from() {
    assert_eq!(
        MENU.matches("pub fn menu_card(").count(),
        1,
        "one card builder serves the rail menu and the chat menu"
    );
    assert!(
        MENU.contains("pub fn menu_popover("),
        "one popover places that card at the pointer that opened it (#2690)"
    );
    // The move is mechanical: the rail still imports the primitives under the
    // same names, so nothing in that module learned a new address (#2613).
    assert!(
        SESSIONS.contains("pub use crate::menu::{MenuEntry, MenuKind, menu_item, menu_popover};"),
        "the rail re-exports the primitives it still draws with"
    );
    assert!(
        GUI_MAIN.contains("sessions::menu_popover(\"chat-menu\", items, at)"),
        "the chat menu draws the same card, at the press that asked for it"
    );
    assert!(
        !GUI_MAIN.contains("fn topic_bar("),
        "the strip used to hold the button and its items; it holds neither"
    );
    assert!(
        GUI_MAIN
            .contains("let strip = topic_strip(\"window\", self.topic_edge, toggle_edge, tabs);"),
        "the strip takes a key, the tabs and the edge button, nothing else"
    );

    // Painted last, so the conversation cannot cover the open card. It is
    // attached at the window root now, deferred, above everything (#2690).
    let footer_arm = GUI_MAIN
        .find("Screen::MissionControl => right.child(footer),")
        .expect("the chat column ends with its footer");
    let menu = GUI_MAIN
        .find("if let Some(menu) = self.chat_menu(cx) {")
        .expect("the card is attached to the chat column");
    assert!(
        menu > footer_arm,
        "the card is attached after the column, so it paints over the transcript"
    );
}

/// Right pressing a topic did nothing, and the only way to edit one was to
/// open it first and hunt the `...`.
#[test]
fn a_right_press_on_a_tab_and_on_the_chat_opens_the_same_card() {
    assert!(
        GUI_MAIN.contains(".on_mouse_down(MouseButton::Right, on_right)"),
        "a topic tab answers a right press"
    );
    assert!(
        GUI_MAIN.contains(".on_mouse_down(MouseButton::Right, ask_menu)"),
        "the conversation answers a right press"
    );
    assert!(
        GUI_MAIN.contains("this.topic_menu = Some(id);"),
        "a tab's press opens the card for that tab's session, not for whatever is on screen"
    );
    assert!(
        !GUI_MAIN.contains("chat-menu-button"),
        "the row that held the `...` is gone, so the card opens from a right press (#2596)"
    );
}

/// The chat screen prints the conversation's name in its footer, so the row
/// that repeated the same name across the top was removed (#2596).
#[test]
fn the_chat_screen_draws_no_title_row() {
    assert!(
        !GUI_MAIN.contains("let mut header = div()"),
        "no title row above the transcript"
    );
    assert!(
        !GUI_MAIN.contains("fn session_title(name: &str)"),
        "the row's title helper went with it"
    );
    assert!(
        !GUI_MAIN.contains("fn header_name("),
        "the shortening helper existed only for that row"
    );
    assert!(
        GUI_MAIN.contains("self.session_title().unwrap_or_else(|| \"chat\".to_string()),"),
        "the footer still leads with the conversation's name"
    );
}
