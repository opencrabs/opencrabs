//! Regression tests for `scripts/test-stall-watchdog.sh` (#1936).
//!
//! The bug being tracked is a silent suite-wide freeze: all test-thread
//! slots block, no test completes, and the CI job burns its entire
//! `timeout-minutes` before the runner kills it, so the blocked stacks
//! are never dumped. The watchdog turns that into a marker + dumps +
//! exit 97 within its stall window. These tests pin the decision
//! behaviour with a stub dumper and synthetic logs.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/test-stall-watchdog.sh")
}

#[derive(Debug)]
struct Run {
    code: Option<i32>,
    stdout: String,
    elapsed_secs: u64,
}

fn watchdog(envs: &[(&str, &str)], shell_cmd: &str) -> Run {
    let started = Instant::now();
    let out = Command::new("bash")
        .arg(script())
        .arg("sh")
        .arg("-c")
        .arg(shell_cmd)
        .envs(envs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
        .output()
        .expect("watchdog ran");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        elapsed_secs: started.elapsed().as_secs(),
    }
}

#[test]
fn stalled_suite_is_dumped_killed_and_flagged_not_silently_waited() {
    // The #1936 shape: one completed test, then total silence. Before the
    // watchdog existed this was the full runner timeout with no verdict;
    // the stubbed run must instead return within seconds, flag the stall,
    // dump every descendant, and kill the subtree (an un-killed child
    // would hold the stdout pipe open and blow the elapsed assertion).
    let dump_on = tempfile::NamedTempFile::new().expect("dump file");
    let run = watchdog(
        &[
            ("WATCHDOG_STALL_SECS", "1"),
            ("WATCHDOG_POLL_SECS", "0.3"),
            ("WATCHDOG_DUMP_ON", dump_on.path().to_str().unwrap()),
            ("WATCHDOG_DUMPER", "/usr/bin/true"),
        ],
        r#"echo "test a ... ok"; sleep 30"#,
    );
    assert_eq!(run.code, Some(97), "stall must exit 97, got: {run:?}",);
    assert!(
        run.stdout.contains("STALL DETECTED"),
        "marker missing: {}",
        run.stdout
    );
    let dumps = std::fs::read_to_string(dump_on.path()).expect("dump record");
    assert!(
        dumps.contains("dumped:"),
        "dumper never ran for any pid: {dumps}"
    );
    assert!(
        run.elapsed_secs < 15,
        "watchdog waited on the stalled child instead of killing it ({}s)",
        run.elapsed_secs
    );
}

#[test]
fn progressing_results_reset_the_stall_clock() {
    // Four results half a second apart with a 2s stall budget: a healthy
    // suite finishing at its own pace must never be touched, even though
    // the total runtime exceeds the stall threshold.
    let run = watchdog(
        &[
            ("WATCHDOG_STALL_SECS", "2"),
            ("WATCHDOG_POLL_SECS", "0.2"),
            ("WATCHDOG_DUMPER", "/usr/bin/true"),
        ],
        r#"for i in 1 2 3 4; do echo "test $i ... ok"; sleep 0.5; done"#,
    );
    assert_eq!(run.code, Some(0), "healthy suite was interfered with");
    assert!(!run.stdout.contains("STALL DETECTED"));
}

#[test]
fn compile_phase_never_counts_as_a_stall() {
    // The same command spends 20+ minutes in "Compiling" with zero test
    // lines. The stall clock must not start before the first result,
    // or the watchdog would murder every cold-cache build. The command
    // sleeps past the stall budget and must still run to its own exit.
    let run = watchdog(
        &[
            ("WATCHDOG_STALL_SECS", "1"),
            ("WATCHDOG_POLL_SECS", "0.2"),
            ("WATCHDOG_DUMPER", "/usr/bin/true"),
        ],
        r#"echo Compiling; sleep 1.5; exit 3"#,
    );
    assert_eq!(run.code, Some(3), "exit code must pass through");
    assert!(run.stdout.contains("Compiling"), "log not cat'd");
    assert!(!run.stdout.contains("STALL DETECTED"));
}

#[test]
fn external_sigterm_dumps_the_buffer_instead_of_shredding_it() {
    // A reaped step (SIGTERM from the runner) used to fire the EXIT trap,
    // delete the buffer, and die from the signal before the final `cat`
    // could print it: the job log kept the start line, an empty middle and
    // exit 143, which is exactly what Test (Linux) showed three times
    // (2026-10-25 9m19s, 2026-10-27 8m08s, 2026-10-28 7m37s, no test output
    // at all). The buffered output must survive the signal, and the killed
    // subtree must not be orphaned holding the stdout pipe open.
    let child = Command::new("bash")
        .arg(script())
        .arg("sh")
        .arg("-c")
        .arg(r#"echo "buffered evidence line"; echo "test a ... ok"; sleep 20"#)
        .env("WATCHDOG_STALL_SECS", "300")
        .env("WATCHDOG_POLL_SECS", "0.2")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("watchdog spawned");

    // Let the wrapper start the command and buffer its first lines.
    std::thread::sleep(Duration::from_millis(1500));

    let killed = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -TERM {}", child.id()))
        .status()
        .expect("kill ran");
    assert!(killed.success(), "could not SIGTERM the watchdog");

    let out = child.wait_with_output().expect("watchdog output");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        out.status.code(),
        Some(143),
        "an externally signalled run must report 143, not a bare signal death: {all}"
    );
    assert!(
        all.contains("buffered evidence line"),
        "buffered command output was shredded by the EXIT trap on SIGTERM: {all}"
    );
    assert!(
        all.contains("SIGTERM"),
        "the dump must name the signal that killed the run: {all}"
    );
}

#[test]
fn result_output_and_exit_code_pass_through_when_green() {
    // CI reads the suite's own lines from the watchdog's stdout; a
    // wrapper that swallows them makes the whole leg undiagnosable.
    let run = watchdog(
        &[("WATCHDOG_STALL_SECS", "5"), ("WATCHDOG_POLL_SECS", "0.2")],
        r#"echo "test a ... ok"; echo "test b ... FAILED"; exit 0"#,
    );
    assert_eq!(run.code, Some(0));
    assert!(run.stdout.contains("test a ... ok"), "{}", run.stdout);
    assert!(run.stdout.contains("test b ... FAILED"));
}
