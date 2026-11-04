//! The chat's `...` menu and the topic icons (#2480, #2304).
//!
//! The window has no headless harness, so the wiring is read from source. The
//! data half is real: the icons the tabs paint are the rows
//! `SessionRepository::topic_icons` returns, which is the call boot now makes.

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const SESSIONS: &str = include_str!("../gui/sessions.rs");

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

    // The query behind that call: a child session with an icon is returned, a
    // root session and an icon-less topic are not.
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
    assert_eq!(
        rows,
        vec![(topic, "\u{1f4a1}".to_string())],
        "only topics carry a tab icon"
    );
}

/// A menu that grows the strip is not a menu. Both menus draw the same card,
/// and the strip no longer has one to grow.
#[test]
fn the_menu_floats_as_a_card_and_the_strip_nothing_grows_from() {
    assert!(
        SESSIONS.contains("pub fn menu_card("),
        "one card builder serves the rail menu and the chat menu"
    );
    assert!(
        SESSIONS.contains("menu_card(\"rail-context-menu\""),
        "the rail menu keeps its card"
    );
    assert!(
        GUI_MAIN.contains("sessions::menu_card(\"chat-menu\""),
        "the chat menu draws the same card"
    );
    assert!(
        !GUI_MAIN.contains("fn topic_bar("),
        "the strip used to hold the button and its items; it holds neither"
    );
    assert!(
        GUI_MAIN.contains("let strip = topic_strip(self.topic_edge, toggle_edge, tabs);"),
        "the strip takes the tabs and the edge button, nothing else"
    );

    // Painted last, so the conversation cannot cover the open card: children
    // paint in order.
    let footer_arm = GUI_MAIN
        .find("Screen::MissionControl => right.child(footer),")
        .expect("the chat column ends with its footer");
    let menu = GUI_MAIN
        .find("if let Some(menu) = self.chat_menu(cx) {")
        .expect("the card is attached to the chat column");
    assert!(
        menu > footer_arm,
        "the card is the column's last child, so it paints over the transcript"
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
        GUI_MAIN.contains(".child(SharedString::from(\"...\"))"),
        "the button that opens the card lives at the conversation's top right"
    );
}

/// The card around the name read as a button no matter how snug it was, so
/// the name is plain text across the row.
#[test]
fn the_title_is_plain_text_and_not_a_chip() {
    assert!(!GUI_MAIN.contains("fn session_title_chip"));
    let (_, title_fn) = GUI_MAIN
        .split_once("fn session_title(name: &str)")
        .expect("the title prints");
    let (body, _) = title_fn.split_once("\n}\n").expect("the title fn ends");
    assert!(
        body.contains(".flex_1()"),
        "the title takes the row's width instead of hugging its text"
    );
    for retired in [".border_1()", ".bg(", ".px_2()", ".py_1()"] {
        assert!(
            !body.contains(retired),
            "the title draws no card: {retired} came back"
        );
    }
}
