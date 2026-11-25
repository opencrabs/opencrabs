//! The wires between the window and a remote crab that only the window can
//! plug in.
//!
//! `Backend::watch` shipped with no caller: a remote turn streamed nothing,
//! so the card showed no reasoning and no answer until the reply, and the
//! facts (model, directory, branch) were never read, so the footer drew
//! dashes. The class is a door of the `Backend` seam that exists and is
//! wired by nobody; the source is pinned, the way the other window guards
//! pin theirs.

const GLUE: &str = include_str!("../backend_glue.rs");
const RENDER: &str = include_str!("../dashboard_render.rs");
const WIRING: &str = include_str!("../dashboard_instances.rs");

/// The body of the first item whose header contains `header`.
fn item_body<'a>(src: &'a str, header: &str) -> &'a str {
    let start = src
        .find(header)
        .unwrap_or_else(|| panic!("{header} not found"));
    let open = start + src[start..].find('{').expect("an opening brace");
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("{header} never closes")
}

#[test]
fn no_door_of_the_backend_seam_is_left_unwired() {
    // An `allow(dead_code)` on the seam is a door nobody calls in the
    // shipped window: exactly how the event stream went missing.
    let seam = item_body(GLUE, "impl Backend");
    assert!(
        !seam.contains("allow(dead_code)"),
        "a Backend method is marked dead_code; wire it or remove it"
    );
}

#[test]
fn every_frame_keeps_the_remote_conversation_streamed() {
    let render = item_body(RENDER, "fn render(");
    assert!(
        render.contains("self.sync_remote_watch()"),
        "the window no longer opens a remote conversation's event stream"
    );
    let sync = item_body(WIRING, "fn sync_remote_watch(");
    assert!(
        sync.contains("self.backend.watch("),
        "sync_remote_watch no longer opens the stream through the Backend seam"
    );
    assert!(
        sync.contains("self.remote_watch = None;"),
        "the old stream must close before the next one opens"
    );
}
