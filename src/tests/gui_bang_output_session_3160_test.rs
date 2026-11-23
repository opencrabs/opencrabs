//! The output of a `!cmd` must land in the conversation that ran it (#3160).
//!
//! The event carried the output alone, so the window painted it on whatever
//! conversation happened to be on screen when the command came back. The
//! command runs on its own thread and takes as long as it takes, so the
//! reader may have switched in the meantime, and in a split the pane that ran
//! the command is not the chat the window is painting. The output landed in a
//! conversation that never asked for it.
//!
//! The window has no headless harness, so these read source, the same way the
//! other `gui_*_test.rs` files do. Each assertion names one link of the chain
//! that has to stay joined: the event has to carry the session, the runner
//! has to hand it over, both dispatch sites have to name the session they
//! ran in, and the arrival has to be routed to that owner.

const AGENT_BRIDGE: &str = include_str!("../gui/agent_bridge.rs");
const BANG: &str = include_str!("../gui/bang.rs");
const DASHBOARD_TURNS: &str = include_str!("../gui/dashboard_turns.rs");
const DASHBOARD_MODELS: &str = include_str!("../gui/dashboard_models.rs");

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
/// pattern, and a boundary that short turns every assertion written against
/// it into a vacuous pass.
fn arm_body<'a>(src: &'a str, marker: &str) -> &'a str {
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("the window folds `{marker}`"));
    let rest = tail(src, start + marker.len());
    let end = rest.find("TurnEvent::").unwrap_or(rest.len());
    window(rest, 0, end)
}

#[test]
fn the_output_event_carries_the_conversation_it_belongs_to() {
    let src = production(AGENT_BRIDGE);
    assert!(
        src.contains("BangOutput {"),
        "the bang output event must carry the session it belongs to (#3160)"
    );
    assert!(
        !src.contains("BangOutput(String)"),
        "the session-less shape must not come back (#3160)"
    );
    let start = src.find("BangOutput {").expect("the variant is shaped");
    let rest = tail(src, start);
    let end = rest.find("},").expect("the variant closes");
    let variant = window(src, start, start + end);
    assert!(
        variant.contains("session: Option<Uuid>"),
        "the event must name the session it ran in (#3160)"
    );
    assert!(
        variant.contains("text: String"),
        "the event must still carry the output (#3160)"
    );
}

#[test]
fn the_runner_hands_the_session_to_the_event() {
    let src = production(BANG);
    // The session is a parameter, so it lives in the signature, not in the
    // body the brace matcher returns.
    let signature = src.find("pub fn spawn(").expect("the runner is there");
    let params = tail(src, signature)
        .find(')')
        .map(|end| window(src, signature, signature + end))
        .expect("the signature closes");
    assert!(
        params.contains("session: Option<Uuid>"),
        "the runner must take the session the command runs in (#3160)"
    );
    let body = function_body(src, "pub fn spawn(").expect("its body is closed");
    assert!(
        body.contains("TurnEvent::BangOutput {"),
        "the runner must send the shaped event (#3160)"
    );
    let sent = body
        .find("TurnEvent::BangOutput {")
        .expect("the event is sent");
    let rest = tail(body, sent);
    assert!(
        window(
            body,
            sent,
            sent + rest.find('}').expect("the pattern closes")
        )
        .contains("session"),
        "the sent event must carry the session it was handed (#3160)"
    );
}

#[test]
fn the_pane_runs_the_command_in_its_own_session() {
    let src = production(DASHBOARD_TURNS);
    let body = function_body(src, "pub(crate) fn send_from_pane(").expect("its body is closed");
    assert!(
        body.contains("bang::spawn(cmd.to_string(), Some(session),"),
        "a pane's command must run in that pane's session (#3160)"
    );
    assert!(
        body.contains("self.push_session_notice(session, format!(\"! {cmd}\"), cx)"),
        "the echo of a pane's command must stay in that pane (#3160)"
    );
}

#[test]
fn the_window_names_the_conversation_it_ran_the_command_in() {
    let src = production(DASHBOARD_TURNS);
    let body =
        function_body(src, "pub(crate) fn send_message_showing(").expect("its body is closed");
    assert!(
        body.contains("bang::spawn(cmd.to_string(), session,"),
        "the window's command must carry the session it ran in (#3160)"
    );
    assert!(
        body.contains("let session = self.chat_state.session();"),
        "the window must take the session now, because the reader may switch \
         before the command answers (#3160)"
    );
}

#[test]
fn the_output_lands_on_the_strip_of_the_conversation_that_asked() {
    let src = production(DASHBOARD_TURNS);
    let body = arm_body(src, "TurnEvent::BangOutput {");
    assert!(
        body.contains("self.push_session_notice(id, text, cx)"),
        "the output belongs in the conversation that ran the command (#3160)"
    );
    // The window's own strip is only for output that has no conversation to
    // own it, and that branch is the `None` arm of the match on the session
    // the event carries. A bare push to the window's strip would put the
    // output on whichever conversation happens to be on screen (#3160).
    let window_strip = body.matches("self.turn.live.notices.push(text)").count();
    assert!(
        window_strip == 1,
        "only the session-less fallback may paint on the window's strip (#3160)"
    );
    let none_arm = body.find("None =>").expect("the fallback arm is there");
    assert!(
        none_arm
            < body
                .find("self.turn.live.notices.push(text)")
                .expect("the push is there"),
        "the window's strip must be the fallback, not the default (#3160)"
    );
}

#[test]
fn the_door_routes_to_the_pane_and_parks_the_rest() {
    let src = production(DASHBOARD_MODELS);
    let body =
        function_body(src, "pub(crate) fn push_session_notice(").expect("its body is closed");
    assert!(
        body.contains("self.chat_session == Some(session)"),
        "the conversation on screen takes its own line (#3160)"
    );
    assert!(
        body.contains("self.pane_states.get(&session)"),
        "a pane takes its own line (#3160)"
    );
    assert!(
        body.contains("self.stashed"),
        "a conversation that is neither on screen nor in a pane parks the line, \
         so it is waiting when that conversation comes back (#3160)"
    );
}
