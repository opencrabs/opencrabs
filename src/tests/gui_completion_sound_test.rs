//! #2691: the plan chime was TUI-only, so the GUI was silent no matter what
//! the operator set. The player had two callers and both lived in terminal
//! code; `nm` on the GUI binary showed `play_completion_sound` was not even
//! linked in. These checks read source because the window has no headless
//! harness, and each one pins a call site whose absence IS the bug: drop the
//! event arm, drop the render hook, or give each strip its own seen set, and
//! the notification goes back to silence or starts ringing twice.
//!
//! The chime decision itself is behavioural and lives in
//! `gui::plan_strip::tests`, over a real plan file. No audio is asserted here:
//! CI has no output device.

const GUI_MAIN: &str = include_str!("../gui/main.rs");

/// The body of one function, from its signature to the next top-level item.
fn fn_body(signature: &str) -> &'static str {
    GUI_MAIN
        .split(signature)
        .nth(1)
        .unwrap_or_else(|| panic!("{signature} exists in the GUI"))
        .split("\n    /// ")
        .next()
        .unwrap_or_else(|| panic!("{signature} has a body"))
}

/// The settle emits `PlanCompleted` before it archives the plan file, so the
/// event is the one signal a file poll cannot miss. The GUI has to arm it,
/// and the arm has to sit BEFORE the catch-all or it is unreachable.
#[test]
fn the_gui_arms_plan_completed_before_the_catch_all() {
    let arm = GUI_MAIN
        .find("ProgressEvent::PlanCompleted")
        .expect("the GUI matches PlanCompleted");
    let catch_all = GUI_MAIN
        .find("TurnEvent::ProgressFor(_, _, _) => {}")
        .expect("the progress catch-all exists");
    assert!(
        arm < catch_all,
        "the PlanCompleted arm must precede the catch-all, or the event is swallowed again"
    );
    // Split, never index: a byte slice of a str is a ratchet site, and the
    // two needles are all this needs to carve the arm body out.
    let body = GUI_MAIN
        .split("ProgressEvent::PlanCompleted")
        .nth(1)
        .and_then(|tail| tail.split("TurnEvent::ProgressFor(_, _, _) => {}").next())
        .expect("the arm body sits between the event and the catch-all");
    assert!(
        body.contains("Self::play_task_chime()"),
        "the arm has to reach the player, not just name the event: {body}"
    );
}

/// A gpui window has no tick, so the render path is where the terminal's
/// per-task chime belongs. It has to run AFTER the strip re-reads its file,
/// otherwise it decides against the previous frame's plan.
#[test]
fn the_render_path_chimes_after_the_strip_refresh() {
    let render = GUI_MAIN
        .split("impl Render for Dashboard {")
        .nth(1)
        .expect("the Dashboard renders");
    let refresh = render
        .find("self.plan.refresh();")
        .expect("render re-reads the window plan");
    let chime = render
        .find("self.chime_settled_plan_tasks();")
        .expect("render runs the chime pass");
    assert!(
        refresh < chime,
        "the chime pass reads the plan the refresh just loaded, so it runs second"
    );
}

/// One seen set for the whole window. A plan can be drawn by the window strip
/// and by a pane strip at once, so a per-strip set would ring twice for one
/// task, and a count instead of a set would ring on every re-read.
#[test]
fn the_chime_pass_walks_every_strip_through_one_seen_set() {
    let body = fn_body("fn chime_settled_plan_tasks(&mut self) {");
    assert!(
        body.contains("&mut self.announced_task_completions"),
        "the pass uses the window's single seen set: {body}"
    );
    assert!(
        body.contains("self.plan.document()"),
        "the window strip is walked: {body}"
    );
    // The pass now walks the map with its keys, not `.values()`: a pane plan
    // written before the hook stamped its session carries the nil id, and the
    // pane's own key is the only honest fallback for the flash deadline.
    assert!(
        body.contains("in &self.pane_states") && body.contains("pane.plan.document()"),
        "every pane strip is walked too: {body}"
    );
    // The settle count and the OS banner share one dedupe: the helper reports
    // how many of these plans newly resolved into the one window seen set, and
    // its count is what arms the flash and the chime. A local count would drift
    // from the banner and ring twice for one task.
    assert!(
        body.contains("announce_settled_tasks(seen, &plans)"),
        "the dedupe is the shared helper over one seen set, not a local count: {body}"
    );
    assert!(
        body.contains("Self::play_task_chime()"),
        "a fresh settle reaches the player: {body}"
    );
}

/// The opt-in and the build feature both gate the player. Reading the flag at
/// the moment of the chime is what lets a live config flip take effect without
/// a restart, which is how the terminal's arm behaves.
#[test]
fn the_player_stays_behind_the_opt_in_and_the_feature() {
    let gated = GUI_MAIN
        .split("#[cfg(feature = \"notification-sound\")]\n    fn play_task_chime() {")
        .nth(1)
        .expect("play_task_chime is feature-gated");
    let body = gated.split("\n    }").next().expect("the player body");
    assert!(
        body.contains("completion_notification_enabled"),
        "the operator's opt-in is honoured: {body}"
    );
    assert!(
        body.contains("play_completion_sound()"),
        "the enabled path reaches the embedded WAV: {body}"
    );
    assert!(
        GUI_MAIN
            .contains("#[cfg(not(feature = \"notification-sound\"))]\n    fn play_task_chime() {}"),
        "a build without the feature still compiles, with a silent no-op"
    );
}
