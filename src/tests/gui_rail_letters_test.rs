//! The window's rail spends its bare characters on the session filter (#2812).
//!
//! #2670 gave the window the terminal's session letters (r, t, c). #2812 took
//! them back: a printable character with the rail focused now opens the find
//! field and types itself, so the first keystroke of "rust" cannot also be the
//! rename command. The letters stay on the terminal and on the row menu, and
//! the help table marks them terminal-only. `z` left with the fold tree
//! (#2701). The window has no headless key harness, so these checks read source
//! the same way `gui_split_render_test` does.

use crate::tests::gui_dashboard_source::DASHBOARD;

const GUI_MAIN: &str = include_str!("../gui/main.rs");
/// The key handler moved into a module of its own (#2822).
const DASHBOARD_KEYS: &str = include_str!("../gui/dashboard_keys.rs");
const SCREEN_KEYS: &str = include_str!("../gui/screen_keys.rs");

/// The key handler, from its opening to the next method in its module (#2822).
fn on_key() -> &'static str {
    DASHBOARD_KEYS
        .split("fn on_key(")
        .nth(1)
        .expect("on_key is defined")
        .split("\n    pub(crate) fn ")
        .next()
        .expect("the handler ends at the next method")
}

#[test]
fn the_rail_spends_its_bare_letters_on_the_find_field() {
    let keys = on_key();
    // A printable character with the rail focused arms the field ctrl+f arms
    // and types itself into it (#2812), so a name can be searched by typing.
    assert!(
        keys.contains("_ if self.rail_focused =>"),
        "a typed character reaches the rail's find field"
    );
    assert!(
        keys.contains("self.arm_rail_filter(cx);"),
        "typing arms the same field ctrl+f arms"
    );
    assert!(
        keys.contains("self.sessions.filter.push(ch);"),
        "the character that opened the field also lands in it"
    );
    assert!(
        keys.contains("ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt"),
        "a chord keeps its own meaning; only bare typing opens the field"
    );
    // The three letters cannot answer bare at the same time: each would swallow
    // the first character of every search (#2812). They stay on the row menu.
    for letter in ["r", "t", "c"] {
        assert!(
            !keys.contains(&format!("\"{letter}\" if self.rail_focused => {{")),
            "{letter} must not answer bare on the window's rail"
        );
    }
    // #2701 replaced the fold tree with chips that always paint, so the window
    // answers no z. Retiring it honestly beats leaving a key that folds
    // nothing while the help still advertises it.
    assert!(
        !keys.contains("\"z\" if self.rail_focused => {"),
        "z is retired with the fold tree, not silently dead"
    );
    assert!(
        !DASHBOARD.contains("fn toggle_fold_topic("),
        "the handler left with the key that called it"
    );
}

#[test]
fn alt_p_opens_projects_while_bare_p_still_searches() {
    assert!(
        !on_key().contains("\"p\" if self.rail_focused => {"),
        "printable p still belongs to rail search (#2812)"
    );
    assert!(
        SCREEN_KEYS.contains(
            "if mods.alt && !mods.control && !mods.shift && !mods.platform && key == \"p\""
        ),
        "alt+p is a distinct screen chord"
    );
    assert!(SCREEN_KEYS.contains("Screen::Projects => ScreenAction::LeaveProjects"));
    assert!(SCREEN_KEYS.contains("_ => ScreenAction::EnterProjects"));
    assert!(DASHBOARD.contains("ScreenAction::EnterProjects => self.open_projects(cx)"));
    assert!(DASHBOARD.contains("ScreenAction::LeaveProjects => self.close_projects(cx)"));
    assert!(
        on_key().contains(
            "ev.keystroke.modifiers.control && ev.keystroke.modifiers.shift && ev.keystroke.key == \"p\""
        ),
        "ctrl+shift+p remains the pin chord"
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
fn the_window_help_marks_the_session_letters_terminal_only() {
    use crate::ui_core::help_keys::{HelpSection, gui_rows, section_rows};
    // The table keeps one row per letter so the terminal still documents them,
    // but none advertises a window chord now that the rail spends its bare
    // characters on the find field (#2812). Rename, New topic and Close topic
    // stay on the row menu.
    let gui: Vec<&str> = gui_rows().map(|r| r.neutral).collect();
    for chord in ["r", "t", "c"] {
        assert!(
            !gui.contains(&chord),
            "the window's help offers no {chord}: the rail spends it on typing"
        );
    }
    let sessions: Vec<_> = section_rows(HelpSection::Sessions).collect();
    for (chord, description) in [
        ("r", "Rename the selected session"),
        ("t", "Topics on, then a new topic"),
        ("c", "Close or reopen the selected topic"),
    ] {
        let hits: Vec<_> = sessions.iter().filter(|row| row.neutral == chord).collect();
        assert_eq!(hits.len(), 1, "{chord} is still documented once");
        assert_eq!(
            hits[0].description, description,
            "{chord} changed its words"
        );
        assert_eq!(
            hits[0].gui, None,
            "{chord} must not advertise a window chord"
        );
    }
    assert!(
        !gui.contains(&"z"),
        "the window's help lists no z: the fold tree is gone (#2701)"
    );
    let projects = sessions
        .iter()
        .find(|row| row.neutral == "p")
        .expect("the shared Help table keeps the Projects row");
    assert_eq!(projects.tui, Some("P"));
    assert_eq!(projects.gui, Some("alt-p"));
    assert!(
        gui.contains(&"p"),
        "the window's Help lists Projects through alt+p"
    );
}
