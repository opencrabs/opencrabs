//! What the transcript draws per frame, and how much of it (#2805).
//!
//! The window has no headless harness, so these checks read source, the way the
//! other gui tests do. Each rule below was paid for once: the pane built an
//! element per line of every row in the window (64,827 lines for the largest
//! local session, rebuilt on every keystroke), the merged family loaded one
//! window per member (3,627 rows into a pane built for 200), and every row's
//! copy text was flattened again on every paint.

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const MESSAGE_REPO: &str = include_str!("../db/repository/message.rs");

/// The renderer between two of its neighbours, so an assertion cannot be
/// satisfied by an unrelated function elsewhere in the file.
fn region_in(source: &'static str, start: &str, end: &str) -> &'static str {
    source
        .split(start)
        .nth(1)
        .unwrap_or_else(|| panic!("{start} is defined"))
        .split(end)
        .next()
        .unwrap_or_else(|| panic!("{end} follows {start}"))
}

fn region(start: &str, end: &str) -> &'static str {
    region_in(TRANSCRIPT, start, end)
}

/// A folded turn draws an excerpt of a non-answer body and says how much it
/// hid. The cut lands on the raw source, so the marker strip and markdown parse
/// only ever see the excerpt. Visible answers take the full-body path (#2855).
#[test]
fn a_folded_turn_excerpts_non_answer_bodies_and_announces_what_it_hid() {
    let excerpt = region("fn excerpt_lines(", "/// Char-safe truncation");
    assert!(
        excerpt.contains("content[i + 1..].lines().count()"),
        "the hidden count is measured from the lines past the cut"
    );
    assert!(
        excerpt.contains("Cow::Borrowed(&content[..i])"),
        "the cut is a borrow of the source, not a rebuilt string"
    );

    for (start, end) in [
        ("fn bubble(", "/// A forwarded copy"),
        (
            "fn forward_block(",
            "/// The files a stored message carries",
        ),
    ] {
        let body = region(start, end);
        assert!(
            body.contains("full: bool"),
            "{start} takes the expanded state of its turn"
        );
        assert!(
            body.contains("excerpt_lines(content, MSG_EXCERPT_LINES)"),
            "{start} cuts the source before anything walks it"
        );
        assert!(
            body.contains("more lines hidden (click the turn header to read them)"),
            "{start} tells the reader how much it hid instead of cutting silently"
        );
    }

    // The rule is bounded on both ends: a ceiling worth reading, and a marker
    // that always names the count.
    // The region starts after the name, so the bound is read from its tail.
    let cap = region("const MSG_EXCERPT_LINES", "/// Cut a body at");
    assert!(
        cap.starts_with(": usize = 20"),
        "the folded body is bounded to one screenful"
    );
}

/// The transcript is redrawn on every keystroke, so what a turn costs to draw
/// is the lag the owner feels. Working-out stays bounded, but an answer the
/// fold keeps visible must reach the renderer whole (#2855).
#[test]
fn a_folded_turn_bounds_working_out_without_cutting_the_answer() {
    let view = region("pub fn view(", "/// The owner's test feedback");
    assert!(
        view.contains("let full_body = !folded"),
        "expanded turns still draw every body in full"
    );
    assert!(
        view.contains("role == \"assistant\""),
        "the folded full-body exception is limited to assistant answers"
    );
    assert!(
        view.contains("visible_when_folded(block, turn.final_idx, i)"),
        "the final answer and deliverable reports use the same visibility decision as the fold"
    );
    assert!(
        view.contains("Some(&selection),\n                            full_body,"),
        "the selectable answer path receives the answer-aware fullness"
    );
    assert!(
        view.contains("block_view(block, content_width, &attachment_handlers, None, full_body)"),
        "the plain answer path receives the answer-aware fullness"
    );
    assert!(
        !view.contains("attachment_handlers, None)"),
        "no persisted row is drawn without its fold state"
    );
}

/// One conversation, one window: a session with topics reads as its whole
/// family, and reading each member's own window put 3,627 rows on screen.
#[test]
fn the_merged_family_reads_one_window_not_one_per_member() {
    let loader = region(
        "pub async fn load_recent(",
        "/// What a stored session title",
    );
    assert!(
        loader.contains("repo.find_recent_by_sessions(&ids, WINDOW)"),
        "the family is read as one window"
    );
    assert!(
        !loader.contains("rows.extend(window)"),
        "no window is appended per member"
    );
    assert!(
        loader.contains("total += repo.count_by_session(*id).await?"),
        "the hidden count still comes from the whole family"
    );

    let repo = region_in(
        MESSAGE_REPO,
        "fn find_recent_by_sessions(",
        "/// Recent conversation rows for tools",
    );
    assert!(
        repo.contains("ORDER BY created_at DESC, id DESC LIMIT ?"),
        "the newest rows across the family are the ones read"
    );
    assert!(
        repo.contains("messages.reverse()"),
        "the pane reads its rows oldest first, as it always has"
    );
}

/// Copy text for a stored row is built once and reused: the pane asks for it on
/// every paint, and flattening the window again meant 65,000 throwaway strings
/// per keystroke.
#[test]
fn a_stored_rows_copy_text_is_built_once() {
    let memo = region("fn stored_copy_text(", "/// The plain text a row copies");
    assert!(
        memo.contains("static CACHE: RefCell<HashMap<uuid::Uuid, Rc<str>>>"),
        "the cache is keyed by the row, which never changes once written"
    );
    assert!(memo.contains("CACHE_ENTRIES"), "the cache has a ceiling");

    let text = region("fn block_text(", "/// What the transcript view listens to");
    assert!(
        text.contains("message_id: Some(id),"),
        "a stored row is routed through the cache"
    );
    assert!(
        text.contains("stored_copy_text(*id, role, content)"),
        "the stored row's text comes from the memo"
    );
    assert!(
        !memo.contains("hit.to_string()"),
        "a cache hit hands back the shared text, not a copy of the body"
    );
}

/// The source this file pins must still be the file the window builds, so a
/// rename cannot leave these checks reading a copy nothing draws from.
#[test]
fn the_renderer_under_test_is_the_one_the_window_builds() {
    assert!(
        TRANSCRIPT.contains("pub fn view("),
        "the transcript renderer is present"
    );
    assert!(
        MESSAGE_REPO.contains("fn find_recent_by_sessions("),
        "the family read is on the message repository"
    );
}

/// The transcript draws a window of turns, not every row it loaded. A
/// keystroke re-renders the view, so the drawn window is the per-character
/// cost: the terminal draws what its viewport holds and leaves the rest behind
/// a history marker, and the window does the same (#2805).
#[test]
fn the_transcript_draws_a_window_of_turns() {
    assert!(
        TRANSCRIPT.contains("const TURN_WINDOW: usize = 24;"),
        "the drawn window is a named constant, not a literal in the loop"
    );
    let body = region_in(TRANSCRIPT, "pub fn view(", "\n#[cfg(test)]\n");
    assert!(
        body.contains("if ti < first {"),
        "the loop skips the turns older than the drawn window"
    );
    assert!(
        body.contains("older_turns_marker(first)"),
        "the skipped turns are announced, never dropped in silence"
    );
    let window = region("pub fn hidden_turns(", "pub fn newest_anchor(");
    let drawn = region("pub fn drawn_turns(", "pub fn message_ids(");
    assert!(
        drawn.contains("&self.turns[self.turns.len() - drawn..]"),
        "the drawn window is one slice the frame walks"
    );
    for (start, end) in [
        ("pub fn anchors(", "pub fn attachment_paths("),
        (
            "pub fn attachment_paths(",
            "/// Every path whose tile must carry",
        ),
    ] {
        assert!(
            region(start, end).contains("self.drawn_turns()"),
            "{start} walks the drawn window, not every loaded turn"
        );
    }
    assert!(
        window.contains("TURN_WINDOW.saturating_add(self.older_loaded)"),
        "ctrl+o widens the window instead of replacing it"
    );
}

/// A closed rail builds no rows: it is an overlay, and every row carries its
/// own listeners plus four maps under it, all of it thrown away at the layout
/// pass when the rail is not drawn (#2805).
#[test]
fn a_closed_rail_builds_no_rows() {
    let window = include_str!("../gui/main.rs");
    let (_, rail) = window
        .split_once("let rail_drawn = self.rail_open")
        .expect("the rail decides whether it is drawn before it is built");
    for gated in [
        "let activity = if rail_drawn {",
        "let plan_progress = if rail_drawn {",
        "let running_tasks = if rail_drawn {",
        "let rail_order: Vec<&opencrabs::db::models::Session> = if rail_drawn {",
    ] {
        assert!(
            rail.contains(gated),
            "{gated} is gated on the rail being drawn"
        );
    }
    assert!(
        rail.contains("if rail_drawn {\n            self.snapshot_rail_plan_progress();"),
        "the plan index is only warmed for a rail that draws"
    );
}

/// ctrl+o is what the marker tells the user to press, so the chord's first job
/// is the history the window is not drawing (#2805). Before it, the chord only
/// knew how to open or fold the newest turn.
#[test]
fn the_chord_loads_the_next_window_before_anything_else() {
    let nav = include_str!("../gui/dashboard_nav.rs");
    let chord = region_in(
        nav,
        "pub(crate) fn toggle_newest_turn(",
        "if !self.turn.live.is_empty()",
    );
    assert!(
        chord.contains("if self.chat_state.transcript.load_older() {"),
        "the chord loads older turns first"
    );
    assert!(
        chord.contains("return;"),
        "loading a window ends the chord instead of folding the card too"
    );
    let door = region_in(
        nav,
        "pub(crate) fn set_chat_session(",
        "pub(crate) fn begin_conversation(",
    );
    assert!(
        door.contains("self.chat_state.transcript.older_loaded = 0;"),
        "another conversation opens at its newest window"
    );
    let marker = region("fn older_turns_marker(", "/// The TUI's spinner frames");
    assert!(
        marker.contains("older turns hidden (ctrl+o to load)"),
        "the marker names the chord that reveals it"
    );
}
