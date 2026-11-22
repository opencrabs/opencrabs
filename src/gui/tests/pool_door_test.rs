//! Guard: the window never reaches the pool without a reactor (#2973).
//!
//! The report was two panics 45 ms apart on Windows, both from deadpool:
//! `deadpool-runtime-0.3.1/src/lib.rs:92` in `spawn_blocking`, and `:125` in
//! `spawn_blocking_background`, which runs from `impl Drop for SyncWrapper`.
//! The second is the one that makes this a class rather than a call site:
//! releasing a pooled SQLite connection on a thread with no Tokio reactor
//! panics, and a panic inside `Drop` cannot name its caller. The window
//! survived because the hook at `gui/logging.rs` logs instead of aborting,
//! which is what kept it silent.
//!
//! The call site was never found, and the report could not have found it:
//! `strip = true` in `[profile.release]` leaves a Windows backtrace as PE
//! export-table noise (`onig_*` and `tree_sitter_rust` for a deadpool frame).
//! So the fix is structural. Three invariants hold the structure:
//!
//! 1. No file under `gui/` names a pool entry point in code. Comments may,
//!    and do: that is where the trap is written down. Code asks the service,
//!    and the service owns the pool.
//! 2. No synchronous function under `gui/` opens the database without a
//!    reactor either in its own body or in the lane it hands the work to.
//!    Async functions are exempt because they run on whatever reactor polls
//!    them, and every spawn that polls one builds it in the same function.
//! 3. Every thread `agent_bridge.rs` spawns builds a reactor in the function
//!    that spawns it: `new_current_thread` for a one-shot call, and
//!    `new_multi_thread` for the surface that outlives the boot. Since #3033
//!    that is one thread builder in `spawn_worker` and one in
//!    `spawn_bootstrap`; a `spawn_*` that opens the database hands the job to
//!    the lane, which builds the runtime on the caller before it sends.
//!
//! What this cannot prove: a path that reaches the pool through an async
//! function nobody polls on a reactor. The panic is the only witness to
//! that, and it needs the symbols half of #2973 to say anything.

use std::fs;
use std::path::{Path, PathBuf};

/// How a gpui task ends up asking deadpool for a reactor it does not have.
/// Naming one of these in window code is the regression.
const POOL_ENTRY_POINTS: &[&str] = &["interact(", "deadpool_sqlite::", "Pool<"];

/// The two ways this crate builds a reactor. Anything that blocks on the pool
/// has to be inside one of them.
const REACTOR_BUILDS: &[&str] = &["new_current_thread()", "new_multi_thread()"];

/// The one handoff that carries a reactor without naming one (#3033). A job
/// sent to `spawn_worker` runs inside the runtime that helper builds, so a
/// function whose database work is all inside a lane job is as covered as one
/// that blocks on its own runtime.
const LANE_HANDOFF: &str = "spawn_worker(";

/// The window's only DB door. It holds the service handle and every thread it
/// spawns is expected to carry its own reactor.
const DOOR: &str = "agent_bridge.rs";

fn gui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui")
}

/// Every module file under `gui/`, skipping `tests/`: those files are pins
/// that quote source as string literals, so a comment there is not a call
/// site and a quote is not code.
fn window_sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![gui_dir()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {:?}: {e}", dir));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|e| panic!("cannot read an entry in {:?}: {e}", dir))
                .path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "tests") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The line without its trailing comment. A `//` inside a string literal is
/// not handled, which only ever hides text: the checks below look for tokens,
/// and truncating can never invent one.
fn code_of(line: &str) -> &str {
    // `split_once` rather than slicing at `find`'s byte index: the index is
    // always on a boundary because the needle is ASCII, but a byte slice is a
    // panic waiting for the next edit and the ratchet in
    // scripts/check-byte-slice-ratchet.py counts every one of them (#2973).
    line.split_once("//").map_or(line, |(code, _)| code)
}

/// The file up to the first `#[cfg(test)]` marker. Test code is exempt: the
/// inline test modules that predate the guard build their runtime in a helper
/// (`mission_control.rs`), and a test that never runs in the window cannot
/// panic it.
fn production_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in text.lines() {
        if code_of(line).trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        out.push(line);
    }
    out
}

/// A function signature and the line range of its body.
struct FnSpan {
    name: String,
    is_async: bool,
    start: usize,
    end: usize,
}

fn fn_signature(line: &str) -> Option<(String, bool)> {
    let mut rest = code_of(line).trim_start();
    for prefix in ["pub(crate) ", "pub ", "unsafe "] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
        }
    }
    let (is_async, rest) = match rest.strip_prefix("async ") {
        Some(stripped) => (true, stripped),
        None => (false, rest),
    };
    let rest = rest.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    Some((name, is_async))
}

fn fn_spans(lines: &[&str]) -> Vec<FnSpan> {
    let mut spans: Vec<FnSpan> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some((name, is_async)) = fn_signature(line) {
            spans.push(FnSpan {
                name,
                is_async,
                start: i,
                end: lines.len(),
            });
        }
    }
    for i in 0..spans.len().saturating_sub(1) {
        spans[i].end = spans[i + 1].start;
    }
    spans
}

fn carries_a_reactor(lines: &[&str]) -> bool {
    lines.iter().any(|line| {
        REACTOR_BUILDS
            .iter()
            .any(|build| code_of(line).contains(build))
    })
}

/// The job runs on the lane's thread, inside the runtime `spawn_worker` built
/// before it sent. Exempting this is only honest because the test below pins
/// that the helper really does build one.
fn hands_work_to_a_lane(lines: &[&str]) -> bool {
    lines
        .iter()
        .any(|line| code_of(line).contains(LANE_HANDOFF))
}

#[test]
fn the_window_never_names_the_pool() {
    let mut hits = Vec::new();
    for path in window_sources() {
        let text =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {:?}: {e}", path));
        for (n, line) in text.lines().enumerate() {
            let code = code_of(line);
            for needle in POOL_ENTRY_POINTS {
                if code.contains(needle) {
                    hits.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "window code must not name the pool: a gpui task has no Tokio reactor, and \
         deadpool panics instead of returning (#2973). Route the call through a \
         `spawn_*` in {DOOR}, which gives its thread a runtime.\n{}",
        hits.join("\n")
    );
}

#[test]
fn no_window_thread_opens_the_database_without_a_reactor() {
    let mut offenders = Vec::new();
    let mut checked = 0;
    for path in window_sources() {
        let text =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {:?}: {e}", path));
        let lines = production_lines(&text);
        for span in fn_spans(&lines) {
            // An async fn borrows whatever reactor polls it; every caller that
            // polls one builds it in its own body, which rule 3 pins.
            if span.is_async {
                continue;
            }
            let lines_of_fn = &lines[span.start..span.end];
            let opens_db = lines_of_fn
                .iter()
                .any(|line| code_of(line).contains("Database::connect"));
            if !opens_db {
                continue;
            }
            checked += 1;
            if !carries_a_reactor(lines_of_fn) && !hands_work_to_a_lane(lines_of_fn) {
                offenders.push(format!(
                    "{}:{} fn {}",
                    path.display(),
                    span.start + 1,
                    span.name
                ));
            }
        }
    }
    assert!(
        checked > 0,
        "no synchronous function opens the database, so this guard checked nothing"
    );
    assert!(
        offenders.is_empty(),
        "a synchronous function opens the database without building a reactor, so \
         the first pool call in it panics with \"there is no reactor running\" \
         (#2973). Hand the work to `spawn_worker`, the shape every `spawn_*` in \
         {DOOR} uses, or block on a `current_thread` runtime you built first:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_bridge_spawns_every_thread_with_a_reactor() {
    let path = gui_dir().join(DOOR);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {:?}: {e}", path));
    let lines = production_lines(&text);
    let spans = fn_spans(&lines);
    let mut spawns = 0;
    let mut offenders = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !code_of(line).contains("std::thread::Builder::new()") {
            continue;
        }
        spawns += 1;
        let owner = spans.iter().rev().find(|span| span.start <= i);
        let (name, fn_body): (String, &[&str]) = match owner {
            Some(span) => (span.name.clone(), &lines[span.start..span.end]),
            None => ("<file>".to_string(), &lines[..]),
        };
        if !carries_a_reactor(fn_body) {
            offenders.push(format!("line {}: {}", i + 1, name));
        }
    }
    assert!(
        spawns > 0,
        "no thread spawn was found in {DOOR}, so this guard checked nothing"
    );
    assert!(
        offenders.is_empty(),
        "every thread {DOOR} spawns must build a Tokio reactor, or a pool call on \
         it panics with \"there is no reactor running\" (#2973). The two shapes in \
         use are `new_current_thread` for a one-shot call and `new_multi_thread` \
         for the surface that outlives the boot:\n{}",
        offenders.join("\n")
    );
}

/// The lane exemption above is only safe if the helper it names really builds
/// the runtime (#3033). This pins the source of that promise, so a helper that
/// stops building one fails here instead of quietly widening rule 2.
#[test]
fn the_lane_handoff_builds_the_reactor_it_promises() {
    let path = gui_dir().join(DOOR);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {:?}: {e}", path));
    let lines = production_lines(&text);
    let spans = fn_spans(&lines);
    let helper = spans
        .iter()
        .find(|span| span.name == "spawn_worker")
        .unwrap_or_else(|| panic!("{DOOR} has no spawn_worker, so rule 2 exempts nothing"));
    let body = &lines[helper.start..helper.end];
    assert!(
        carries_a_reactor(body),
        "`spawn_worker` must build the reactor its jobs run on, or every \
         synchronous function that hands it database work panics on the first \
         pool call (#2973, #3033)"
    );
    assert!(
        body.iter()
            .any(|line| code_of(line).contains("std::thread::Builder::new()")),
        "`spawn_worker` is also the thread it hands the job to"
    );
}
