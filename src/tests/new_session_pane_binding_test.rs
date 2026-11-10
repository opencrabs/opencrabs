//! Pins the two fixes for #165 ("In split TUI, strange new session
//! behaviour") at the source-level:
//!
//! 1. `create_new_session` (messaging.rs) MUST update the focused
//!    pane's `session_id` to the new session id. Without it, Tab-away
//!    plus Tab-back loads the previous (pre-Ctrl+N) session because
//!    the `is_focus_next_pane` handler in state.rs reads
//!    `pane.session_id`.
//!
//! 2. The auto-title spawn in `tool_loop.rs` MUST send
//!    `ChannelSessionEvent::TitleUpdated` after a successful
//!    `update_session_title` so the TUI footer refreshes immediately.
//!    Without it, the footer kept showing "New Chat" until the user
//!    manually switched sessions and triggered a `load_session`
//!    re-read.
//!
//! Both pinned source-level because the real call sites are async and
//! depend on a live SessionService + provider chain — a behavioural
//! unit test would be a small integration test, overkill for guarding
//! against a future refactor that drops the two-line pane sync or the
//! event send.

const MESSAGING_SRC: &str = include_str!("../tui/app/messaging.rs");
const TOOL_LOOP_SRC: &str = include_str!("../brain/agent/service/tool_loop.rs");
const STATE_SRC: &str = include_str!("../tui/app/state.rs");

/// Strip `//` line comments so source-level invariant scans don't
/// false-match against doc-comments describing the bug they guard.
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| {
            if let Some(idx) = line.find("//") {
                let before = &line[..idx];
                if before.matches('"').count() % 2 == 0 {
                    return before.trim_end().to_string();
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn create_new_session_binds_focused_pane_to_new_session_id() {
    // Find the `create_new_session` function body and assert it
    // contains `pane.session_id = Some(session.id)` — the line that
    // syncs the pane binding. Without this sync, Tab-back after Ctrl+N
    // resurrects the previous session (the #165 repro).
    let src = strip_line_comments(MESSAGING_SRC);
    let fn_marker = "pub(crate) async fn create_new_session(&mut self) -> Result<()>";
    let start = src
        .find(fn_marker)
        .expect("create_new_session function signature must exist in messaging.rs");
    // Bound the search at the next `pub(crate) async fn` so a similar
    // sync in load_session below doesn't false-positive this test.
    let rest = &src[start + fn_marker.len()..];
    let end_marker = "\n    pub(crate) async fn";
    let body_end = rest.find(end_marker).unwrap_or(rest.len());
    let body = &rest[..body_end];

    assert!(
        body.contains("self.bind_focused_pane_session(session.id);"),
        "create_new_session must bind the focused pane to the new session id. \
         Without this, the pane keeps pointing at the previous session and \
         Tab-away + Tab-back loads the pre-Ctrl+N session, making the new \
         session appear to vanish (issue #165)."
    );
    // The sync itself moved into one helper when drafts started travelling
    // with the pane, so the assignment is pinned where it now lives (#2780).
    // Pinning both halves still fails if either the call or the assignment
    // goes away.
    let helper_marker = "fn bind_focused_pane_session(&mut self, session_id: Uuid) {";
    let helper_start = src
        .find(helper_marker)
        .expect("bind_focused_pane_session must carry the pane's session sync");
    let helper_body = &src[helper_start + helper_marker.len()..];
    let helper_end = helper_body
        .find("\n    pub(crate) async fn")
        .unwrap_or(helper_body.len());
    let helper = &helper_body[..helper_end];
    assert!(
        helper.contains("focused_pane_mut()"),
        "bind_focused_pane_session must reach for `focused_pane_mut()` to \
         perform the pane.session_id sync, pinning the access path so a future \
         refactor that drops the mutable access also breaks this test."
    );
    assert!(
        helper.contains("pane.session_id = Some(session_id)"),
        "bind_focused_pane_session must assign the pane's session_id. Without \
         it the pane keeps pointing at the conversation it left (#165)."
    );
}

#[test]
fn auto_title_spawn_sends_title_updated_event() {
    // The auto-title path writes the new title to DB and must also fan
    // out a TitleUpdated event so the TUI footer can refresh in-memory.
    // Without the send, the footer keeps showing "New Chat" until the
    // user switches sessions.
    let src = strip_line_comments(TOOL_LOOP_SRC);

    assert!(
        src.contains("ChannelSessionEvent::TitleUpdated"),
        "tool_loop.rs must emit ChannelSessionEvent::TitleUpdated after a \
         successful update_session_title — the TUI footer relies on this event \
         to refresh `current_session.title` without a full DB reload."
    );
    assert!(
        src.contains("let title_update_tx = self.session_updated_tx.clone()"),
        "tool_loop.rs must capture session_updated_tx before the auto-title \
         spawn so the spawned task can fan out the TitleUpdated event. Without \
         the capture, the spawn has no handle to notify the TUI."
    );
}

#[test]
fn tui_handles_session_title_updated_in_memory_only() {
    // The TUI handler for SessionTitleUpdated should mutate current_session
    // and the cached sessions list directly — NOT trigger a full reload
    // via load_session or schedule pending_session_refresh. A full reload
    // would defeat the purpose of having a lightweight event for what's
    // effectively a string update.
    let src = strip_line_comments(STATE_SRC);
    let marker = "TuiEvent::SessionTitleUpdated";
    let start = src
        .find(marker)
        .expect("state.rs must handle TuiEvent::SessionTitleUpdated");

    // Bound the arm at the next `TuiEvent::` so adjacent handlers don't
    // pollute the scan.
    let rest = &src[start + marker.len()..];
    let arm_end = rest
        .find("\n            TuiEvent::")
        .unwrap_or_else(|| rest.len().min(2000));
    let arm = &rest[..arm_end];

    assert!(
        !arm.contains("load_session"),
        "SessionTitleUpdated arm must not call load_session — that's the heavy \
         path. The point of this event is a cheap in-memory string swap."
    );
    assert!(
        !arm.contains("pending_session_refresh"),
        "SessionTitleUpdated arm must not schedule pending_session_refresh — \
         that path also ends in a full load_session reload."
    );
    assert!(
        arm.contains("s.title = Some(title"),
        "SessionTitleUpdated arm must update current_session.title in-memory."
    );
}
