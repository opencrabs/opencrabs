//! #2691: the plan chime was TUI-only, so the GUI was silent no matter what
//! the operator set. The player had two callers and both lived in terminal
//! code; `nm` on the GUI binary showed `play_completion_sound` was not even
//! linked in. These checks read source because the window has no headless
//! harness, and each one pins a call site whose absence IS the bug: drop the
//! event arm, drop the render hook, or give each strip its own record, and the
//! notification goes back to silence or starts ringing twice.
//!
//! #3118 adds the other half: one chime per plan, never one per task. The
//! settle pass still walks every strip, but all it does is flash the rail.
//!
//! #3004 moves what counts as already-announced off the window and into a
//! durable per-session ledger, so a relaunch does not replay every settle the
//! last process flashed. The pass must still read every strip, and must do it
//! through that one ledger.
//!
//! The chime decision itself is behavioural and lives in
//! `gui::plan_strip::tests`, over a real plan file. No audio is asserted here:
//! CI has no output device.

const GUI_MAIN: &str = concat!(
    include_str!("../gui/main.rs"),
    include_str!("../gui/app.rs"),
    include_str!("../gui/dashboard_render.rs"),
    include_str!("../gui/dashboard_state.rs"),
);
/// The event arms moved into a module of their own (#2822).
const DASHBOARD_TURNS: &str = include_str!("../gui/dashboard_turns.rs");
/// So did the settle pass and the player it calls.
const DASHBOARD_KEYS: &str = include_str!("../gui/dashboard_keys.rs");

/// The body of one function, from its signature to the next top-level item.
fn fn_body(source: &'static str, signature: &str) -> &'static str {
    source
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
    let arm = DASHBOARD_TURNS
        .find("ProgressEvent::PlanCompleted")
        .expect("the GUI matches PlanCompleted");
    let catch_all = DASHBOARD_TURNS
        .find("TurnEvent::ProgressFor(_, _, _) => {}")
        .expect("the progress catch-all exists");
    assert!(
        arm < catch_all,
        "the PlanCompleted arm must precede the catch-all, or the event is swallowed again"
    );
    // Split, never index: a byte slice of a str is a ratchet site, and the
    // two needles are all this needs to carve the arm body out.
    let body = DASHBOARD_TURNS
        .split("ProgressEvent::PlanCompleted")
        .nth(1)
        .and_then(|tail| tail.split("TurnEvent::ProgressFor(_, _, _) => {}").next())
        .expect("the arm body sits between the event and the catch-all");
    assert!(
        body.contains("Self::play_plan_chime()"),
        "the arm has to reach the player, not just name the event: {body}"
    );
}

/// A gpui window has no tick, so the render path is where the terminal's
/// settle pass belongs. It has to run AFTER the strip re-reads its file,
/// otherwise it decides against the previous frame's plan.
#[test]
fn the_render_path_settles_after_the_strip_refresh() {
    let render = GUI_MAIN
        .split("impl Render for Dashboard {")
        .nth(1)
        .expect("the Dashboard renders");
    let refresh = render
        .find("self.plan.refresh();")
        .expect("render re-reads the window plan");
    let settle = render
        .find("self.settle_plan_tasks(cx);")
        .expect("render runs the settle pass");
    assert!(
        refresh < settle,
        "the settle pass reads the plan the refresh just loaded, so it runs second"
    );
}

/// One ledger for the whole window, durable per session (#3004). A plan can be
/// drawn by the window strip and by a pane strip at once, so a per-strip record
/// would flash twice for one task, and a count instead of a set would flash on
/// every re-read.
#[test]
fn the_settle_pass_walks_every_strip_through_one_ledger() {
    let body = fn_body(
        DASHBOARD_KEYS,
        "pub(crate) fn settle_plan_tasks(&mut self, cx: &mut Context<Self>) {",
    );
    assert!(
        body.contains("self.settle_ledger.record("),
        "the pass records through the window's one ledger: {body}"
    );
    assert!(
        body.contains("self.plan.document()"),
        "the window strip is walked: {body}"
    );
    // The pass now walks the map with its keys, not `.values()`: a pane plan
    // written before the hook stamped its session carries the nil id, and the
    // pane's own key is the only honest fallback for the flash deadline.
    assert!(
        body.contains("in &self.pane_states") && body.contains("pane.read(cx).plan.document()"),
        "every pane strip is walked too, read through its own handle: {body}"
    );
    // The freshness answer comes from the ledger, which folds in what a
    // previous process already flashed, so a plan drawn twice flashes once and
    // a relaunch on an unchanged file flashes not at all. A local count would
    // drift and blink on every frame.
    assert!(
        body.contains("self.settle_ledger.record(*session, Some(doc))"),
        "the dedupe is the durable ledger per session, not a local count: {body}"
    );
    // #3118: the pass is bookkeeping only. The per-task chime is gone, so a
    // plan that settles six tasks rings once, at the finish.
    assert!(
        !body.contains("Self::play_") && !body.contains("play_completion_sound"),
        "the settle pass must not ring, only flash: {body}"
    );
}

/// The opt-in and the build feature both gate the player. Reading the flag at
/// the moment of the chime is what lets a live config flip take effect without
/// a restart, which is how the terminal's arm behaves.
#[test]
fn the_player_stays_behind_the_opt_in_and_the_feature() {
    let gated = DASHBOARD_KEYS
        .split("#[cfg(feature = \"notification-sound\")]\n    pub(crate) fn play_plan_chime() {")
        .nth(1)
        .expect("play_plan_chime is feature-gated");
    let body = gated.split("\n    }").next().expect("the player body");
    assert!(
        body.contains("notify_on_plan_completed"),
        "the operator's opt-in is honoured: {body}"
    );
    assert!(
        body.contains("play_completion_sound()"),
        "the enabled path reaches the embedded WAV: {body}"
    );
    assert!(
        DASHBOARD_KEYS.contains(
            "#[cfg(not(feature = \"notification-sound\"))]\n    pub(crate) fn play_plan_chime() {}"
        ),
        "a build without the feature still compiles, with a silent no-op"
    );
}
