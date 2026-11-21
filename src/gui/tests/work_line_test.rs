//! The work bar: the one line at the top of the chat that answers "is it still
//! going?".
//!
//! A window needs a GPU and a session, so the wiring is proved where it is
//! written, the same way the plan card and the rail row prove theirs. The two
//! facts that matter: the bar is built from the same `PlanStripState` the strip
//! above the composer reads, and it is added to the column before the
//! transcript, so it stays at the top.

/// The bar has to read the strip's own state, or the two can disagree about
/// one plan.
#[test]
fn the_bar_reads_the_plan_strips_state() {
    let source = include_str!("../dashboard_render.rs");
    let body = source
        .split("fn work_line(&self)")
        .nth(1)
        .expect("the render has a work_line");
    assert!(
        body.contains("self.plan.progress_label()"),
        "the bar must read the same PlanStripState the strip reads"
    );
}

/// It goes above the transcript: a bar under it would scroll away, which is
/// exactly the problem it exists to fix.
#[test]
fn the_bar_is_added_before_the_transcript() {
    let source = include_str!("../dashboard_render.rs");
    let bar = source
        .find("self.work_line()")
        .expect("the render asks for the work line");
    let middle = source
        .find("right = right.child(middle);")
        .expect("the chat column is built");
    assert!(bar < middle, "the bar is added before the transcript");
}

/// It is a chat-screen fact: the other screens keep their own chrome.
#[test]
fn the_bar_is_gated_on_the_chat_screen() {
    let source = include_str!("../dashboard_render.rs");
    let call = source
        .split("self.work_line()")
        .next()
        .expect("the render asks for the work line");
    // Windowed by characters, not bytes: a byte slice can land mid-character
    // and the byte-slice ratchet gate rejects it. This only needs the 400
    // characters before the call site.
    let skip = call.chars().count().saturating_sub(400);
    let window: String = call.chars().skip(skip).collect();
    assert!(
        window.contains("self.screen == Screen::Chat"),
        "the bar belongs to the chat screen"
    );
}
