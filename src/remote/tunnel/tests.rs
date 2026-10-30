use super::*;
use std::os::unix::fs::PermissionsExt;

/// A stand-in for cloudflared: honours `--logfile`, optionally writes a
/// real-shaped URL line, then `exec sleep` so SIGTERM hits it directly.
fn stub_script(dir: &Path, writes_url: bool) -> PathBuf {
    let path = dir.join("cloudflared-stub");
    let mut body = String::from(
        "#!/bin/sh\nLOG=/dev/null\nwhile [ $# -gt 0 ]; do\ncase \"$1\" in\n--logfile) shift; LOG=\"$1\";;\nesac\nshift\ndone\n",
    );
    // Real cloudflared creates the logfile immediately (before any URL
    // line); model that so the "log kept for diagnosis" contract is testable.
    body.push_str("touch \"$LOG\"\n");
    if writes_url {
        body.push_str(
                "echo \"2026-10-30T00:00:00Z INF Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):\" >> \"$LOG\"\necho \"2026-10-30T00:00:00Z INF https://stub-abc-123.trycloudflare.com\" >> \"$LOG\"\n",
            );
    }
    body.push_str("exec sleep 30\n");
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    let p = managed_binary_path(Path::new("/tmp/x"));
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
