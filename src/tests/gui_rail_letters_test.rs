//! The window answers the session letters the terminal answers (#2670).
//!
//! The window has no headless key harness, so these checks read source the
//! same way `gui_split_render_test` does. Each rule below was broken once: the
//! rail answered arrows and Enter but nothing else, so `r`, `t` and `c` did
//! nothing in the window while the terminal answered them, and the window's
//! own help refused to list them because it only lists what it can answer.
//! `z` rides the same gate in the terminal, but the window lost its topic fold
//! with #2701, so the window binds the three that still have a handler here.

const GUI_MAIN: &str = include_str!("../gui/main.rs");

/// The key handler, from its opening to the next method on the window.
fn on_key() -> &'static str {
    GUI_MAIN
        .split("fn on_key(")
        .nth(1)
        .expect("on_key is defined")
        .split("\n    fn ")
        .next()
        .expect("the handler ends at the next method")
}

/// One arm of the rail's key match: its body up to the `return` that ends it.
fn arm_body(header: &str) -> &'static str {
    on_key()
        .split(header)
        .nth(1)
        .unwrap_or_else(|| panic!("the rail answers {header}"))
        .split("return;")
        .next()
        .expect("the arm returns once it has acted")
}

#[test]
fn the_rail_answers_the_session_letters() {
    let keys = on_key();
    // Each letter is spelled with the rail_focused gate the arrows use, so a
    // letter typed with the composer focused stays text (#2670).
    for letter in ["r", "t", "c"] {
        let header = format!("\"{letter}\" if self.rail_focused => {{");
        assert!(
            keys.contains(&header),
            "the rail answers {letter}, gated on rail focus"
        );
    }
    // The three letters reach the handlers their row menu items already call,
    // so the keyboard and the menu cannot drift apart.
    assert!(
        arm_body("\"r\" if self.rail_focused => {").contains("self.start_rename("),
        "r renames the row the keyboard stands on"
    );
    let topic = arm_body("\"t\" if self.rail_focused => {");
    assert!(
        topic.contains("self.open_session(") && topic.contains("self.start_topic_draft("),
        "t opens the row first, then asks for the topic, as the menu does"
    );
    assert!(
        arm_body("\"c\" if self.rail_focused => {").contains("self.toggle_closed_topic("),
        "c closes or reopens the topic"
    );
    // #2701 replaced the fold tree with chips that always paint, so the window
    // answers no z. Retiring it honestly beats leaving a key that folds
    // nothing while the help still advertises it.
    assert!(
        !keys.contains("\"z\" if self.rail_focused => {"),
        "z is retired with the fold tree, not silently dead"
    );
    assert!(
        !GUI_MAIN.contains("fn toggle_fold_topic("),
        "the handler left with the key that called it"
    );
}

#[test]
fn p_stays_unbound_until_the_window_has_a_projects_screen() {
    // The terminal's p walks to Projects. The window has no projects screen at
    // all (#2669), so binding p here would be a key that does nothing. The arm
    // arrives with the screen, and the help row below already spells it None
    // so the window's help stays honest in the meantime.
    assert!(
        !on_key().contains("\"p\" if self.rail_focused => {"),
        "p is not claimed until the window can act on it"
    );
    assert!(
        on_key().contains("_ => {}"),
        "the rail still falls through to the rest of the handler"
    );
}

#[test]
fn the_prompt_heading_stops_calling_every_conversation_a_topic() {
    // A top-level session is a conversation; only a session with a parent is a
    // topic. The heading was hardcoded to "Rename topic" for both (#2668).
    // The guard, not the label, is what tells the two apart.
    assert!(
        GUI_MAIN.contains("parents.contains_key(&id) => \"Rename topic\""),
        "only a session with a parent is renamed as a topic"
    );
    let heading = GUI_MAIN
        .split("=> \"Rename topic\"")
        .nth(1)
        .expect("the heading tells a topic from a conversation")
        .split(";")
        .next()
        .expect("the heading match closes");
    assert!(
        heading.contains("\"Rename conversation\""),
        "a session with no parent is renamed as a conversation"
    );
    assert!(
        heading.contains("\"New topic\""),
        "with nothing being renamed the prompt still asks for a new topic"
    );
}

#[test]
fn the_window_help_admits_the_session_letters() {
    use crate::ui_core::help_keys::{HelpSection, gui_rows, section_rows};
    // Every Sessions row the window answers is listed in its help, and the
    // three letters it still answers are among them now that the rail answers
    // them. z left with the fold tree (#2701).
    let gui: Vec<&str> = gui_rows().map(|r| r.neutral).collect();
    for chord in ["r", "t", "c"] {
        assert!(
            gui.contains(&chord),
            "the window's help lists {chord} now that the rail answers it"
        );
    }
    assert!(
        !gui.contains(&"z"),
        "the window's help lists no z: the fold tree is gone (#2701)"
    );
    assert!(
        !gui.contains(&"p"),
        "p is not offered by the window's help while the key is unbound"
    );
    // The section still describes the same set the terminal documents.
    let sessions: Vec<&str> = section_rows(HelpSection::Sessions)
        .map(|r| r.neutral)
        .collect();
    assert!(
        sessions.contains(&"p"),
        "the terminal still documents p for its Projects screen"
    );
}
