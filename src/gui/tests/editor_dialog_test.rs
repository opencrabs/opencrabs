//! The bang operator's built-in editor, read as text (#3163).
//!
//! `!vi` used to hand the file to Terminal.app on macOS, which took the edit
//! out of the window the user asked for it in. The rules here pin the
//! replacement: the editor opens in the window, keyed by the conversation
//! that asked, and never spawns a terminal again.

use crate::dashboard_source::{DASHBOARD, production_half};

/// The runner's own text, because it is not one of the window's dashboard
/// modules and the concat cannot carry it.
const BANG: &str = include_str!("../bang.rs");

#[test]
fn the_bang_runner_never_spawns_a_terminal_editor() {
    assert!(
        !BANG.contains("osascript"),
        "the editor path still shells out to Terminal.app"
    );
    assert!(
        !BANG.contains("open_in_terminal"),
        "the Terminal.app helper is still in the runner"
    );
    // The parse has to branch ahead of the shell thread, or `!vi` would both
    // open the dialog and run `vi` as a captured command whose bytes come
    // back as gibberish when the editor exits.
    let editor = BANG
        .find("editor_command(&cmd)")
        .expect("the runner no longer recognises editor commands");
    let shell = BANG
        .find("run_shell(&cmd)")
        .expect("the runner lost its ordinary shell path, `!ls` and friends");
    assert!(
        editor < shell,
        "the shell thread must not run before the editor branch answers"
    );
    let request = BANG
        .find("TurnEvent::EditorRequest")
        .expect("the editor branch must end in the event the window answers");
    assert!(request > editor);
}

#[test]
fn the_editor_is_keyed_by_the_session_that_asked() {
    let src = production_half();
    assert!(
        src.contains("editor_dialogs: HashMap<Uuid"),
        "the window no longer keeps one editor per conversation, so a split \
         would share buffers between panes"
    );
    assert!(
        src.contains("get_working_directory_for_session"),
        "the session cwd lookup is gone, so a relative `!vi notes.md` would \
         read the process launch directory instead of the conversation's own"
    );
    // The dialog's own module must never reach for the process launch
    // directory: each session owns its own `/cd` path, and the launch
    // directory has nothing to do with the conversation the user moved
    // around in. The slice starts at the module's unique doc comment, so
    // this test's own needles cannot match the test.
    let module = DASHBOARD
        .find("The built-in editor the bang operator opens")
        .expect("the editor dialog module left the window's source concat");
    let body = &DASHBOARD[module..];
    assert!(
        body.contains("let cwd = agent_bridge::bridge()"),
        "the editor resolves paths without the session bridge"
    );
    assert!(
        !body.contains(["launch", "_cwd()"].concat().as_str()),
        "the editor resolves a path against the process launch cwd instead of \
         the session's own"
    );
}

#[test]
fn the_editor_dialog_owns_the_keyboard_while_it_is_up() {
    let branch = DASHBOARD
        .find("self.editor_focus.is_some()")
        .expect("the window never checks whether an editor is focused");
    let settings = DASHBOARD
        .find("if self.settings_open")
        .expect("the settings modal's key branch is gone");
    // In the key dispatcher the editor is answered ahead of the settings
    // modal, so a keystroke typed into the buffer cannot fall through to a
    // modal underneath.
    assert!(
        branch < settings,
        "the editor's key branch lost its place ahead of the settings modal"
    );
    let owner = DASHBOARD
        .find("OverlayId::Editor")
        .expect("the editor has no overlay id, so nothing hands it the keys");
    let render = DASHBOARD
        .find("editor_dialog::overlay(")
        .expect("the editor dialog is never drawn");
    assert!(owner > 0 && render > owner);
}

#[test]
fn a_file_that_is_not_text_opens_read_only() {
    let src = production_half();
    assert!(
        src.contains("opened read-only"),
        "a non-UTF-8 file must open read-only, never silently corrupted"
    );
    assert!(
        src.contains("esc again to discard"),
        "one Esc must never throw away typed text on its own"
    );
    assert!(
        src.contains("name the file to save into"),
        "an empty buffer needs a save-as prompt before it can save"
    );
}
