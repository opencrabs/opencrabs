use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::remote::tunnel::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Test-only liveness probe: kill(pid, 0) — ESRCH = gone, EPERM = alive
/// (not ours). Clamped to i32::MAX because an out-of-range pid cast
/// negative would signal -1. Production never needs bare liveness: it
/// asks [`pid_is_cloudflared`] instead, which subsumes existence.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid.min(i32::MAX as u32) as i32, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// A stand-in for cloudflared: honours `--logfile`, optionally writes a
/// real-shaped URL line, then stays alive as the script process (NOT
/// `exec sleep` — an exec would swap the pid's identity to `sleep`, and
/// status()/stop() verify the pid still looks like cloudflared before
/// acting on it, exactly as they would against the real binary).
fn stub_script(dir: &Path, writes_url: bool) -> PathBuf {
    stub_script_with_url(
        dir,
        writes_url.then_some("https://stub-abc-123.trycloudflare.com"),
    )
}

/// `url`: the host the stub announces (`None` = never prints a URL).
fn stub_script_with_url(dir: &Path, url: Option<&str>) -> PathBuf {
    let path = dir.join("cloudflared-stub");
    let mut body = String::from(
        "#!/bin/sh\nLOG=/dev/null\nwhile [ $# -gt 0 ]; do\ncase \"$1\" in\n--logfile) shift; LOG=\"$1\";;\nesac\nshift\ndone\n",
    );
    // Real cloudflared creates the logfile immediately (before any URL
    // line); model that so the "log kept for diagnosis" contract is testable.
    body.push_str("touch \"$LOG\"\n");
    if let Some(host) = url {
        body.push_str(&format!(
            "echo \"2026-10-30T00:00:00Z INF Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):\" >> \"$LOG\"\necho \"2026-10-30T00:00:00Z INF {host}\" >> \"$LOG\"\n"
        ));
    }
    body.push_str("sleep 30\n");
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[test]
fn parse_url_from_real_log_fixture() {
    let log = concat!(
        "2026-10-30T02:00:00Z INF Thank you for trying Cloudflare Tunnel.\n",
        "2026-10-30T02:00:01Z INF Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):\n",
        "2026-10-30T02:00:01Z INF https://random-words-1234.trycloudflare.com\n",
        "2026-10-30T02:00:02Z INF +----------------------------+\n",
        "2026-10-30T02:00:02Z INF |  Your quick Tunnel has been created!  |\n",
    );
    assert_eq!(
        parse_trycloudflare_url(log).as_deref(),
        Some("https://random-words-1234.trycloudflare.com")
    );
}

#[test]
fn parse_url_banner_box_variant() {
    let log = "INF |  https://boxed-host.trycloudflare.com  |\n";
    assert_eq!(
        parse_trycloudflare_url(log).as_deref(),
        Some("https://boxed-host.trycloudflare.com")
    );
}

#[test]
fn parse_url_rejects_noise_and_lookalikes() {
    assert_eq!(parse_trycloudflare_url(""), None);
    assert_eq!(parse_trycloudflare_url("no urls here at all"), None);
    assert_eq!(parse_trycloudflare_url("https://example.com"), None);
    // suffix must be exactly .trycloudflare.com at end of the URL
    assert_eq!(
        parse_trycloudflare_url("https://x.trycloudflare.com.evil.io"),
        None
    );
    assert_eq!(
        parse_trycloudflare_url("https://evil.com/trycloudflare.com"),
        None
    );
    // http (not https) refused
    assert_eq!(
        parse_trycloudflare_url("http://insecure.trycloudflare.com"),
        None
    );
    // too short to contain a host id
    assert_eq!(parse_trycloudflare_url("https://.trycloudflare.com"), None);
}

#[test]
fn managed_path_carries_pinned_version() {
    let p = managed_binary_path(Path::new("state"));
    assert!(
        p.to_string_lossy()
            .ends_with(&format!("bin/cloudflared-{CLOUDFLARED_VERSION}"))
    );
}

#[test]
fn download_url_points_at_official_release() {
    let url = download_url("2026.10.6").unwrap();
    assert!(url.starts_with(
        "https://github.com/cloudflare/cloudflared/releases/download/2026.10.6/cloudflared-"
    ));
}

#[test]
fn start_status_stop_lifecycle_with_stub() {
    let dir = tempfile::tempdir().unwrap();
    let stub = stub_script(dir.path(), true);
    let spec = TunnelSpec {
        port: 18999,
        state_dir: dir.path().to_path_buf(),
        binary: Some(stub),
        url_timeout: Duration::from_secs(10),
    };
    let state = start(&spec).expect("stub start");
    assert_eq!(state.url, "https://stub-abc-123.trycloudflare.com");
    assert_eq!(state.port, 18999);
    assert!(pid_alive(state.pid));
    assert!(state.started_at > 0);

    // a second start while live must be refused
    let err = start(&spec).expect_err("double start");
    assert!(err.to_string().contains("already running"));

    let st = status(dir.path()).unwrap().expect("state file present");
    assert_eq!(st.pid, state.pid);
    assert_eq!(st.url, state.url);

    assert!(stop(dir.path()).unwrap());
    assert!(status(dir.path()).unwrap().is_none());
    assert!(!stop(dir.path()).unwrap(), "second stop is a no-op");
}

#[test]
fn parse_url_takes_latest_when_log_has_stale_entries() {
    // cloudflared appends to --logfile; leftover content from a previous
    // tunnel must never win over the live tunnel's URL.
    let log =
        "INF https://old-dead-1.trycloudflare.com\nINF https://new-live-2.trycloudflare.com\n";
    assert_eq!(
        parse_trycloudflare_url(log).as_deref(),
        Some("https://new-live-2.trycloudflare.com")
    );
}

#[test]
fn second_start_after_stop_reports_new_tunnel_url() {
    let dir = tempfile::tempdir().unwrap();
    let first = stub_script_with_url(dir.path(), Some("https://first-tunnel-1.trycloudflare.com"));
    let spec = TunnelSpec {
        port: 18997,
        state_dir: dir.path().to_path_buf(),
        binary: Some(first),
        url_timeout: Duration::from_secs(10),
    };
    let state = start(&spec).expect("first start");
    assert_eq!(state.url, "https://first-tunnel-1.trycloudflare.com");
    assert!(stop(dir.path()).unwrap());
    // stop removes tunnel.json but keeps the log: the dead tunnel's URL is
    // still on disk, exactly like a repeat `connect` in the same state dir.
    assert!(log_path(dir.path()).exists());

    let second = stub_script_with_url(
        dir.path(),
        Some("https://second-tunnel-2.trycloudflare.com"),
    );
    let spec = TunnelSpec {
        port: 18997,
        state_dir: dir.path().to_path_buf(),
        binary: Some(second),
        url_timeout: Duration::from_secs(10),
    };
    let state = start(&spec).expect("second start must not adopt the dead tunnel's URL");
    assert_eq!(state.url, "https://second-tunnel-2.trycloudflare.com");
    assert!(stop(dir.path()).unwrap());
}

#[test]
fn concurrent_second_start_fails_instead_of_double_spawning() {
    let dir = tempfile::tempdir().unwrap();
    // Silent stub: the first start sits in its URL wait for the full
    // timeout, holding the start lock through the whole window.
    let silent = stub_script_with_url(dir.path(), None);
    let first_spec = TunnelSpec {
        port: 18995,
        state_dir: dir.path().to_path_buf(),
        binary: Some(silent.clone()),
        url_timeout: Duration::from_millis(1500),
    };
    let first = std::thread::spawn(move || start(&first_spec).map(|_| ()));
    // Let the first start get inside its URL wait (spawn + log poll).
    std::thread::sleep(Duration::from_millis(400));
    let second_spec = TunnelSpec {
        port: 18995,
        state_dir: dir.path().to_path_buf(),
        binary: Some(silent),
        url_timeout: Duration::from_millis(1500),
    };
    let err = start(&second_spec).expect_err("second start must not double-spawn");
    assert!(
        err.to_string().contains("held by another writer"),
        "expected the start-lock error, got: {err}"
    );
    // The first start still ends in its clean timeout, with no state.
    first
        .join()
        .unwrap()
        .expect_err("first start must time out");
    assert!(status(dir.path()).unwrap().is_none());
}

#[test]
fn status_and_stop_refuse_a_recycled_pid_and_clean_state() {
    let dir = tempfile::tempdir().unwrap();
    // An innocent process that is very much alive but is not cloudflared:
    // exactly what a recycled pid looks like after cloudflared dies.
    let mut innocent = std::process::Command::new("sleep")
        .arg("30")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let state = TunnelState {
        pid: innocent.id(),
        url: "https://stolen.trycloudflare.com".into(),
        port: 1,
        binary: "/nonexistent".into(),
        started_at: 1,
    };
    save_state(dir.path(), &state).unwrap();

    // Alive, yet the identity guard must not be fooled.
    assert!(pid_alive(innocent.id()), "test rig: innocent process alive");
    assert!(status(dir.path()).unwrap().is_none());
    assert!(!state_path(dir.path()).exists(), "stale state removed");

    // stop() finds no tunnel and never signals the innocent process.
    assert!(!stop(dir.path()).unwrap());
    assert!(
        pid_alive(innocent.id()),
        "the innocent process must survive"
    );

    // Cleanup through the Child handle: properly killed and reaped.
    let _ = innocent.kill();
    let _ = innocent.wait();
}

#[test]
fn start_times_out_and_kills_silent_stub() {
    let dir = tempfile::tempdir().unwrap();
    let stub = stub_script(dir.path(), false);
    let spec = TunnelSpec {
        port: 18998,
        state_dir: dir.path().to_path_buf(),
        binary: Some(stub),
        url_timeout: Duration::from_millis(1200),
    };
    let err = start(&spec).expect_err("must time out");
    assert!(err.to_string().contains("did not report"));
    assert!(
        status(dir.path()).unwrap().is_none(),
        "no state after failed start"
    );
    assert!(log_path(dir.path()).exists(), "log kept for diagnosis");
}

#[test]
fn status_clears_dead_pid() {
    let dir = tempfile::tempdir().unwrap();
    let state = TunnelState {
        pid: (i32::MAX as u32) - 7, // guaranteed ESRCH, never -1 semantics
        url: "https://dead-one.trycloudflare.com".into(),
        port: 1,
        binary: "/nonexistent".into(),
        started_at: 1,
    };
    save_state(dir.path(), &state).unwrap();
    assert!(status(dir.path()).unwrap().is_none());
    assert!(!state_path(dir.path()).exists(), "stale state removed");
}

#[test]
fn status_absent_is_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(status(dir.path()).unwrap().is_none());
}

#[test]
fn pid_alive_clamps_out_of_range_pids() {
    // u32::MAX must NOT become kill(-1): clamp to i32::MAX → ESRCH.
    assert!(!pid_alive(u32::MAX));
    assert!(!pid_alive(i32::MAX as u32));
}
