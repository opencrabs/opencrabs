//! A stopped turn's clock holds its second instead of counting on (#2849).
//!
//! #2811 made a stopped turn settle on a stop glyph and the word "Stopped",
//! but the footer clock kept reading `Instant::elapsed()` to now, so a card
//! nobody was running went `◷ 3:11`, `3:12`, `3:13`. The rate was already
//! frozen by `finalize`; the clock was not. These checks pin the freeze where
//! it is decided, and pin that every stop path takes it: a single assignment
//! left anywhere would keep counting again on that one path.
//!
//! The window has no headless harness, so the contract is read off the source.

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const DASHBOARD_COMMANDS: &str = include_str!("../gui/dashboard_commands.rs");
const DASHBOARD_KEYS: &str = include_str!("../gui/dashboard_keys.rs");
const DASHBOARD_TURNS: &str = include_str!("../gui/dashboard_turns.rs");

/// The card's own methods, cut where the draw code starts.
fn live_turn_impl() -> &'static str {
    TRANSCRIPT
        .split_once("impl LiveTurn {")
        .expect("LiveTurn has an impl block")
        .1
        .split_once("/// One block, drawn.")
        .expect("the draw code follows the impl")
        .0
}

#[test]
fn the_stop_takes_the_instant_once() {
    let body = live_turn_impl();
    let stop = body
        .split_once("pub fn stop(&mut self) {")
        .expect("a stop is recorded in one place")
        .1
        .split_once("\n    }")
        .expect("the stop method ends")
        .0;
    assert!(
        stop.contains("self.stopped = true;"),
        "the stop still has to say the turn was stopped (#2811)"
    );
    assert!(
        stop.contains("self.stopped_at = Some(std::time::Instant::now());"),
        "the stop must take the instant it landed"
    );
    assert!(
        stop.contains("if self.stopped_at.is_none()"),
        "the instant is taken once: a second stop repaints, it does not re-stamp"
    );
}

#[test]
fn the_clock_reads_to_the_stop_never_to_now() {
    let body = live_turn_impl();
    let clock = body
        .split_once("pub fn elapsed_secs(&self) -> u64 {")
        .expect("the clock is one method")
        .1
        .split_once("\n    }")
        .expect("the clock method ends")
        .0;
    assert!(
        clock.contains("self.stopped_at.unwrap_or_else(std::time::Instant::now)"),
        "the clock reads to the stop when there is one, or the number keeps climbing"
    );
    assert!(
        !clock.contains("start.elapsed()"),
        "measuring to now is the bug: the stop has to end the measurement"
    );
}

#[test]
fn a_new_turn_forgets_the_old_stop() {
    let body = live_turn_impl();
    let clear = body
        .split_once("pub fn clear(&mut self) {")
        .expect("clear drops the live copy")
        .1
        .split_once("\n    }")
        .expect("clear ends")
        .0;
    assert!(
        clear.contains("self.stopped_at = None;"),
        "a fresh turn must not inherit the previous stop's instant"
    );
}

#[test]
fn every_stop_path_takes_the_freeze() {
    for (name, src) in [
        ("dashboard_commands.rs", DASHBOARD_COMMANDS),
        ("dashboard_keys.rs", DASHBOARD_KEYS),
        ("dashboard_turns.rs", DASHBOARD_TURNS),
    ] {
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        assert!(
            !production.contains("live.stopped = true;"),
            "{name} stops a turn by setting the flag alone, which leaves the clock running"
        );
    }
    let stops = DASHBOARD_COMMANDS.matches("live.stop();").count()
        + DASHBOARD_KEYS.matches("live.stop();").count()
        + DASHBOARD_TURNS.matches("live.stop();").count();
    assert_eq!(
        stops, 7,
        "seven stop sites: `/stop` for the window and for a pane, Esc, the two \
         edit stops, and the two settle arms"
    );
}
