//! A slash command typed into a split pane must act on that pane's session,
//! never on the chat the window happens to be showing (#2672).
//!
//! The pane's id was already resolved in `send_from_pane` and then dropped one
//! layer later, so `/discard` typed in pane B destroyed the live plan of the
//! conversation drawn in pane A. The reply was moved back into pane B
//! afterwards, which is what hid it: the answer appeared in the leaf that was
//! typed in while the effect landed somewhere else.
//!
//! The window has no headless harness, so these read source, the same way the
//! other `gui_*_test.rs` files do. Each assertion names one link of the chain
//! that has to stay joined: the value the door hands down, the parameter that
//! receives it, the body that must stop re-deriving it, and the seeded turn
//! that carries it back out.

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const AGENT_BRIDGE: &str = include_str!("../gui/agent_bridge.rs");

/// The file up to its own test module. The prose down there quotes this bug,
/// and a scan that reads its own explanation proves nothing.
fn production(src: &str) -> &str {
    src.split("\n#[cfg(test)]\nmod tests")
        .next()
        .expect("the file always has content before its test module")
}

/// Slice from a byte offset with the char-boundary check made explicit.
fn tail(src: &str, start: usize) -> &str {
    src.get(start..)
        .unwrap_or_else(|| panic!("slice from {start} is not a char boundary"))
}

/// Slice a byte range with the char-boundary check made explicit.
fn window(src: &str, start: usize, end: usize) -> &str {
    src.get(start..end)
        .unwrap_or_else(|| panic!("slice {start}..{end} is not a char boundary"))
}

/// The body of the function whose signature starts with `signature`, braces
/// matched, so a scan of one function cannot run on into the next one.
fn function_body<'a>(src: &'a str, signature: &str) -> Option<&'a str> {
    let start = src.find(signature)?;
    let open = start + tail(src, start).find('{')?;
    let mut depth = 0usize;
    for (offset, ch) in tail(src, open).char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(window(src, open, open + offset + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// The body of one match arm: from its marker up to the next arm in the same
/// `match`. Slicing to the first `}` instead stops inside the arm's own
/// pattern (`TurnEvent::CommandReply { session, text }`), and a boundary that
/// short turns every assertion written against it into a vacuous pass.
fn arm_body<'a>(src: &'a str, marker: &str) -> &'a str {
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("the window folds `{marker}`"));
    let rest = tail(src, start + marker.len());
    let end = rest.find("TurnEvent::").unwrap_or(rest.len());
    window(rest, 0, end)
}

/// The slice between two match-arm markers of the same `match`. Arms are
/// bounded by the next arm, so an assertion about one arm can never read the
/// code of the arm below it.
fn between<'a>(src: &'a str, from: &str, to: &str) -> &'a str {
    let start = src
        .find(from)
        .unwrap_or_else(|| panic!("the window folds `{from}`"));
    let rest = tail(src, start + from.len());
    let end = rest
        .find(to)
        .unwrap_or_else(|| panic!("`{to}` follows `{from}` in the same match"));
    window(rest, 0, end)
}

#[test]
fn the_door_hands_the_pane_session_to_the_text_runner() {
    let src = production(GUI_MAIN);
    assert!(
        src.contains("self.run_text_command(command, arg, session, cx)"),
        "`GuiAction::Text` must hand the session it was given to the text runner (#2672)"
    );
}

#[test]
fn the_text_runner_takes_the_session_it_was_handed() {
    let src = production(GUI_MAIN);
    let signature = src
        .find("fn run_text_command(")
        .expect("run_text_command exists");
    let open = signature + tail(src, signature).find('{').expect("its body opens");
    assert!(
        window(src, signature, open).contains("session: Option<Uuid>"),
        "run_text_command must take the session of the door it answers (#2672)"
    );
    let body = function_body(src, "fn run_text_command(").expect("its body is closed");
    assert!(
        !body.contains("self.transcript.session"),
        "run_text_command must act on the session it was handed, not re-derive the \
         window's chat (#2672)"
    );
}

#[test]
fn the_seeded_turn_carries_the_session_it_belongs_to() {
    let src = production(AGENT_BRIDGE);
    assert!(
        src.contains("CommandTurn {"),
        "the seeded turn must carry the session it belongs to (#2672)"
    );
    assert!(
        !src.contains("CommandTurn("),
        "no constructor may keep the session-less shape (#2672)"
    );
    let main = production(GUI_MAIN);
    let body = arm_body(main, "TurnEvent::CommandTurn {");
    assert!(
        body.contains("self.send_turn_to(session, text, display, cx)"),
        "the seeded turn must be sent to the session it carries (#2672), and \
         the display it carries keeps the prompt out of history (#2756)"
    );
    assert!(
        !body.contains("self.send_message("),
        "the seeded turn must go to its own session, not to the window's chat (#2672)"
    );
}

#[test]
fn every_reply_names_the_side_it_answers_for() {
    let src = production(AGENT_BRIDGE);
    for signature in ["pub fn spawn_text_command(", "async fn run_text_command("] {
        let body = function_body(src, signature).expect("its body is closed");
        assert!(
            !body.contains("session: None"),
            "a command run for a session must answer for it, not for the window (#2672)"
        );
        assert!(
            body.contains("session: Some(session_id)"),
            "the reply a session's command produces must carry that session (#2672)"
        );
    }
}

#[test]
fn the_reply_lands_on_the_strip_of_the_session_that_asked() {
    let src = production(GUI_MAIN);
    let body = arm_body(src, "TurnEvent::CommandReply {");
    assert!(
        body.contains("self.push_session_notice(id, text)"),
        "an answered command belongs in the strip of the session that asked (#2672)"
    );
    assert!(
        body.contains("self.live.notices.push(text)"),
        "a reply with no session keeps answering on the window strip (#2672)"
    );
}

#[test]
fn the_compaction_and_goal_turns_carry_the_pane_session() {
    let src = production(GUI_MAIN);
    for (from, to) in [
        ("C::Compact =>", "C::Cowork =>"),
        ("C::Cowork =>", "C::Goal =>"),
        ("C::Goal =>", "C::Cd =>"),
    ] {
        let body = between(src, from, to);
        let compact: String = body.split_whitespace().collect();
        assert!(
            compact.contains("send_command_turn(session,"),
            "`{from}` must send its turn to the session that asked (#2672)"
        );
        assert!(
            !body.contains("self.send_message("),
            "`{from}` must not fall through to the window's chat (#2672)"
        );
    }

    let cd = between(src, "C::Cd =>", "other =>");
    let compact: String = cd.split_whitespace().collect();
    assert!(
        compact.contains("this.push_session_notice(session,"),
        "the asynchronous /cd reply must land on the session it changed (#2672)"
    );
}
