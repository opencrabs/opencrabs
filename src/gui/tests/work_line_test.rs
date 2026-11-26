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
///
/// The anchor is the line that adds the conversation to the session surface.
/// It used to be `right = right.child(middle);`; the remote panel turned the
/// column into an `AnyElement` and the conversation moved into the chat arm, so
/// the old anchor matched nothing and this test failed on a tree that was
/// correct (measured 21 Nov 2026, run 44927789600).
#[test]
fn the_bar_is_added_before_the_transcript() {
    let source = include_str!("../dashboard_render.rs");
    let bar = source
        .find("self.work_line()")
        .expect("the render asks for the work line");
    let middle = source
        .find("session = session.child(middle);")
        .expect("the chat column is built");
    assert!(bar < middle, "the bar is added before the transcript");
}

/// It is a chat-screen fact: the other screens keep their own chrome.
///
/// Read line by line instead of from a character window: the bar sits deep
/// inside the `Screen::Chat` arm, far more than the 400 characters the old
/// window covered, and a window that short would pass for the wrong reason the
/// moment the arm grew. The arm the line sits in is what the assertion names.
#[test]
fn the_bar_is_gated_on_the_chat_screen() {
    let source = include_str!("../dashboard_render.rs");
    let mut arm: Option<String> = None;
    let mut found: Option<String> = None;
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Screen::")
            && let Some(name) = rest.split_whitespace().next()
        {
            arm = Some(name.trim_end_matches(['=', ',', '{', ' ']).to_string());
        }
        if trimmed.contains("self.work_line()") {
            found = arm.clone();
            break;
        }
    }
    assert_eq!(
        found.as_deref(),
        Some("Chat"),
        "the bar belongs to the chat screen, and it was found under {found:?}"
    );
}
