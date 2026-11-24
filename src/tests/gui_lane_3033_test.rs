//! One lane per operation instead of one thread per call (#3033).
//!
//! `src/gui/agent_bridge.rs` used to carry the same twenty lines at nineteen
//! call sites: build a 4 MB OS thread, build a tokio runtime inside it, run one
//! operation, exit. A thread per call is why the GUI process held 51 to 56
//! threads at rest, and why a single session create showed up as nine threads
//! born in one minute. The work now goes through `spawn_worker`, which reuses a
//! thread per operation name and grows the lane instead of queueing a burst
//! behind work that is already running.
//!
//! The window has no headless harness, so these are source pins: the shape of
//! the helper, the two threads that are still allowed to exist, and the call
//! sites that must not grow a third. Behaviour is pinned where it can be run
//! (`src/gui/tests/pool_door_test.rs` already fails a bridge thread that has no
//! reactor).

const AGENT_BRIDGE: &str = include_str!("../gui/agent_bridge.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const BACKEND_GLUE: &str = include_str!("../gui/backend_glue.rs");

/// One function's byte range, from its signature to the closing brace in column
/// zero. Every function in these files ends there, and no nested block starts at
/// column zero.
fn fn_range(source: &'static str, signature: &str) -> std::ops::Range<usize> {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("{signature} is missing"));
    let len = source[start..]
        .find("\n}\n")
        .expect("the function closes in column zero");
    start..start + len
}

/// The text of one function, by the same rule.
fn fn_body(source: &'static str, signature: &str) -> &'static str {
    &source[fn_range(source, signature)]
}

/// Two threads are allowed to exist: the lane worker and the boot thread. Any
/// third means an operation went back to paying for its own.
#[test]
fn the_bridge_builds_threads_in_two_places_only() {
    let helper = fn_range(AGENT_BRIDGE, "fn spawn_worker<F>(");
    let boot = fn_range(AGENT_BRIDGE, "pub fn spawn_bootstrap(");

    let mut seen = 0;
    let mut from = 0;
    while let Some(found) = AGENT_BRIDGE[from..].find("std::thread::Builder::new()") {
        let at = from + found;
        assert!(
            helper.contains(&at) || boot.contains(&at),
            "a thread is built outside spawn_worker and spawn_bootstrap, at byte {at}"
        );
        seen += 1;
        from = at + 1;
    }
    assert_eq!(
        seen, 2,
        "the bridge should build exactly the lane worker and the boot thread"
    );
}

/// The transcript reads and writes moved onto the same lane (#3033), so the
/// per-turn fetch and the edit resend no longer open a thread each.
#[test]
fn the_transcript_has_no_thread_of_its_own() {
    assert!(
        !TRANSCRIPT.contains("std::thread::Builder::new()"),
        "a transcript path still builds its own thread"
    );
    // The reads and writes go through the backend now, and the backend's one
    // door onto the window's threads is the lane: `Backend::spawn`, which
    // `Backend::call` sends through, is a `spawn_worker` and nothing else.
    let door = BACKEND_GLUE
        .split("pub fn spawn<O: Op>(")
        .nth(1)
        .expect("the backend has one door onto the lane")
        .split("\n    }\n")
        .next()
        .unwrap_or_default();
    assert!(
        door.contains("agent_bridge::spawn_worker(") && !door.contains("std::thread"),
        "the transcript should reach the lane for its reads and writes"
    );
    assert!(
        TRANSCRIPT
            .matches(".call(opencrabs::backend::transcript::")
            .count()
            >= 3,
        "the transcript should reach the lane for its reads and writes"
    );
}

/// The runtime is built on the caller, not inside the thread. That is what lets
/// a failed build come back as `Err` through the arm each operation already had
/// for a failed spawn, instead of dying on a thread nobody is watching.
#[test]
fn the_helper_builds_the_runtime_before_the_thread() {
    let helper = fn_body(AGENT_BRIDGE, "fn spawn_worker<F>(");
    let signature = helper
        .split('{')
        .next()
        .expect("the signature ends at the body");
    assert!(
        signature.contains("-> Result<(), String>"),
        "a failed runtime or spawn has to be reportable to the caller"
    );

    let runtime = helper
        .find("tokio::runtime::Builder::new_current_thread()")
        .expect("the helper owns the runtime");
    let thread = helper
        .find("std::thread::Builder::new()")
        .expect("the helper owns the thread");
    assert!(
        runtime < thread,
        "the runtime has to be built before the thread, so its failure is a return value"
    );
    assert!(
        helper.contains("map_err(|e| format!(\"{name} runtime: {e}\"))?"),
        "a failed runtime build returns the same Err the spawn arm reports"
    );
}

/// Nothing waits behind another job. A lane grows by one thread per concurrent
/// operation, so ten panes of a split run ten turns. A cap here would stall a
/// pane for the whole length of somebody else's turn, which is the one thing
/// this runtime is not allowed to do.
#[test]
fn a_busy_lane_grows_instead_of_queueing() {
    let helper = fn_body(AGENT_BRIDGE, "fn spawn_worker<F>(");
    assert!(
        !AGENT_BRIDGE.contains("LANE_THREADS"),
        "a lane cap makes a pane wait behind another pane's work"
    );
    assert!(
        !helper.contains("lane.first()"),
        "a job must never be handed to a thread that is already running one"
    );
    assert!(
        !helper.contains("lane.len() <"),
        "the helper has to spawn whenever no thread is parked"
    );
    // The only two arms: a parked thread, or a new one. There is no third
    // branch, so there is no path that queues.
    assert_eq!(
        helper.matches("        None => {").count(),
        1,
        "the spawn arm has to be the sole fallback"
    );
}

/// A thread marks itself free only after a job finishes. Born free, a fresh
/// thread would look parked and every job of a burst would stack behind it,
/// which is a lane of one thread pretending to be four.
#[test]
fn a_new_thread_is_born_busy() {
    let helper = fn_body(AGENT_BRIDGE, "fn spawn_worker<F>(");
    assert!(
        helper.contains("AtomicBool::new(false)"),
        "a new lane thread is about to take the job, so it starts busy"
    );
    let free = helper
        .find("thread_free.store(true")
        .expect("the thread frees itself after a job");
    let recv = helper
        .find("receiver.blocking_recv()")
        .expect("the thread waits for the next job");
    assert!(
        recv < free,
        "the free flag is set after the job, before the next wait"
    );
}

/// The voice reply reports from its own clone, so the arm where the job never
/// reached a worker can still clear the card. Sending from the moved `sink`
/// there does not compile, and the bug it hides is a card stuck on "speaking".
#[test]
fn the_voice_reply_clones_the_sink_before_the_spawn() {
    let body = fn_body(AGENT_BRIDGE, "pub fn spawn_speak_reply(");
    let clone = body
        .find("let thread_sink = sink.clone();")
        .expect("the voice reply keeps a sink for the spawn-failure arm");
    let spawn = body
        .find("spawn_worker(\"gui-voice-reply\"")
        .expect("the voice reply runs on the lane");
    assert!(clone < spawn, "the clone has to exist before the closure");
    assert!(
        body.contains("TurnEvent::VoiceReplyDone"),
        "a job that never reached a worker still has to clear the card"
    );
}
