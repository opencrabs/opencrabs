//! Managed cloudflared quick tunnels for `opencrabs connect`.
//!
//! Lifecycle: resolve a cloudflared binary (PATH first — a user-managed
//! install always wins — then the managed pinned copy, downloading it from
//! the official GitHub release if absent), spawn a quick tunnel detached
//! with `--logfile`, wait for the public `trycloudflare.com` URL to appear
//! in the log, and record `{pid, url, port}` in `state/remote/tunnel.json`.
//! `status` checks pid liveness and self-cleans stale state; `stop` sends
//! SIGTERM, waits a grace period, escalates to SIGKILL.
//!
//! Trust model for the download: cloudflared releases publish no checksum
//! assets, so integrity = TLS to `github.com/cloudflare` (official repo) +
//! a `--version` smoke check against the pin. On macOS the binary carries a
//! Developer ID signature (Cloudflare team `68WVV388M8`), Gatekeeper-checked
//! at first run.
//!
//! Quick-tunnel URLs are ephemeral by design: a new random hostname per
//! `start`. Named tunnels (stable hostname, Cloudflare account) are out of
//! scope for Phase 1.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::now_secs;

/// Pinned cloudflared version for managed installs. Bump deliberately: the
/// filename carries the version, so a bump downloads fresh and never
/// overwrites a binary in use.
pub const CLOUDFLARED_VERSION: &str = "2026.10.6";

/// Default wait for the tunnel URL to appear in the log.
pub const URL_WAIT_DEFAULT: Duration = Duration::from_secs(60);
const URL_POLL_INTERVAL: Duration = Duration::from_millis(500);
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Everything [`start`] needs. Production builds this with
/// [`TunnelSpec::new`]; tests override `binary` (stub script) and
/// `url_timeout` (short) to exercise the lifecycle without network.
pub struct TunnelSpec {
    /// Local port the tunnel forwards to (the A2A gateway).
    pub port: u16,
    /// Host cloudflared dials for that port.
    ///
    /// Loopback is only right while the gateway listens on loopback. A gateway
    /// bound to one address (a tailnet IP, a LAN IP) refuses `127.0.0.1`, and
    /// cloudflared then answers every request with **502** while the card still
    /// shows a healthy URL. Measured on the lenovo (21 Nov):
    /// `[a2a] bind = "100.125.20.50"`, tunnel dialling `127.0.0.1:18790`,
    /// `dial tcp 127.0.0.1:18790: connect: connection refused` on every
    /// `/a2a/pair`. The tunnel must dial where the door actually is.
    pub origin_host: String,
    /// Where `tunnel.json`, `tunnel.log` and `bin/` live.
    pub state_dir: PathBuf,
    /// Explicit cloudflared binary; `None` = resolve/ensure.
    pub binary: Option<PathBuf>,
    /// How long to wait for the URL in the log.
    pub url_timeout: Duration,
}

impl TunnelSpec {
    pub fn new(port: u16) -> Self {
        Self::on(port, "127.0.0.1")
    }

    /// A tunnel that dials the address the gateway is bound to.
    ///
    /// Callers pass `[a2a] bind` straight from the config; [`origin_host`]
    /// decides what cloudflared should dial for it.
    pub fn on(port: u16, bind: &str) -> Self {
        Self {
            port,
            origin_host: origin_host(bind),
            state_dir: default_state_dir(),
            binary: None,
            url_timeout: URL_WAIT_DEFAULT,
        }
    }

    /// The origin URL cloudflared is told to forward to.
    pub fn origin(&self) -> String {
        format!("http://{}:{}", self.origin_host, self.port)
    }
}

/// The host cloudflared must dial for a gateway bound to `bind`.
///
/// A wildcard bind accepts loopback, so it dials loopback; a specific address
/// is dialled as itself. `localhost`/`::1` normalise to `127.0.0.1` because
/// cloudflared wants a literal, and any other IPv6 literal gets its brackets
/// back so the URL parses.
pub fn origin_host(bind: &str) -> String {
    match bind.trim() {
        "" | "0.0.0.0" | "::" | "[::]" | "*" | "127.0.0.1" | "localhost" | "::1" | "[::1]" => {
            "127.0.0.1".to_string()
        }
        other => {
            let bare = other.trim_start_matches('[').trim_end_matches(']');
            if bare.contains(':') {
                format!("[{bare}]")
            } else {
                bare.to_string()
            }
        }
    }
}

/// Durable record of a running tunnel: `state_dir/tunnel.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelState {
    pub pid: u32,
    pub url: String,
    pub port: u16,
    /// The host cloudflared actually dials, as derived from `[a2a] bind` when
    /// the tunnel started. It is recorded because `connect --status` used to
    /// print `127.0.0.1` from a literal: on a gateway bound to a tailnet
    /// address the status claimed loopback while the tunnel dialled something
    /// else, which is the same lie that hid the 502 for a day (2026-11-21).
    /// Defaulted so a `tunnel.json` written by an older build still loads: a
    /// tunnel that predates this field really did dial loopback.
    #[serde(default = "loopback_host")]
    pub origin_host: String,
    pub binary: String,
    pub started_at: u64,
}

/// What every tunnel started before `origin_host` was recorded dialled.
fn loopback_host() -> String {
    "127.0.0.1".to_string()
}

/// `<opencrabs_home>/state/remote`
pub fn default_state_dir() -> PathBuf {
    crate::config::opencrabs_home().join("state").join("remote")
}

pub(crate) fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("tunnel.json")
}

pub(crate) fn log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("tunnel.log")
}

/// `<state_dir>/bin/cloudflared-<PINNED VERSION>`
pub fn managed_binary_path(state_dir: &Path) -> PathBuf {
    state_dir
        .join("bin")
        .join(format!("cloudflared-{CLOUDFLARED_VERSION}"))
}

/// Release asset name for this platform.
pub fn asset_name() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("cloudflared-darwin-arm64.tgz"),
        ("macos", "x86_64") => Ok("cloudflared-darwin-amd64.tgz"),
        ("linux", "x86_64") => Ok("cloudflared-linux-amd64"),
        ("linux", "aarch64") => Ok("cloudflared-linux-arm64"),
        (os, arch) => anyhow::bail!(
            "no cloudflared asset pinned for {os}-{arch}; install cloudflared and put it on PATH"
        ),
    }
}

pub fn download_url(version: &str) -> Result<String> {
    Ok(format!(
        "https://github.com/cloudflare/cloudflared/releases/download/{version}/{}",
        asset_name()?
    ))
}

/// Find an existing cloudflared: PATH first (user-managed wins), then the
/// managed pinned copy.
pub fn resolve_binary(state_dir: &Path) -> Option<PathBuf> {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("cloudflared");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let managed = managed_binary_path(state_dir);
    managed.is_file().then_some(managed)
}

/// Ensure a usable cloudflared exists, downloading the pinned release if
/// neither PATH nor the managed copy has one.
pub fn ensure_binary(state_dir: &Path) -> Result<PathBuf> {
    if let Some(bin) = resolve_binary(state_dir) {
        return Ok(bin);
    }
    let managed = managed_binary_path(state_dir);
    let bin_dir = managed
        .parent()
        .context("managed path has no parent dir")?
        .to_path_buf();
    std::fs::create_dir_all(&bin_dir)?;

    let url = download_url(CLOUDFLARED_VERSION)?;
    let tmp = bin_dir.join(format!("dl-{}.archive", std::process::id()));
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-m", "300", "-o"])
        .arg(&tmp)
        .arg(&url)
        .status()
        .context("spawn curl for cloudflared download")?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("cloudflared download failed ({status}) from {url}");
    }

    if asset_name()?.ends_with(".tgz") {
        let status = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&tmp)
            .arg("-C")
            .arg(&bin_dir)
            .status()
            .context("spawn tar for cloudflared archive")?;
        let _ = std::fs::remove_file(&tmp);
        if !status.success() {
            anyhow::bail!("cloudflared archive extraction failed ({status})");
        }
        let extracted = bin_dir.join("cloudflared");
        std::fs::rename(&extracted, &managed)
            .with_context(|| format!("rename {} -> {}", extracted.display(), managed.display()))?;
    } else {
        std::fs::rename(&tmp, &managed)?;
    }
    set_executable(&managed)?;

    // Smoke check: the binary must report exactly the pinned version.
    let out = std::process::Command::new(&managed)
        .arg("--version")
        .output()
        .with_context(|| format!("run {} --version", managed.display()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if !text.contains(CLOUDFLARED_VERSION) {
        let _ = std::fs::remove_file(&managed);
        anyhow::bail!("downloaded cloudflared does not report {CLOUDFLARED_VERSION}: {text:?}");
    }
    tracing::info!(
        version = CLOUDFLARED_VERSION,
        "installed managed cloudflared"
    );
    Ok(managed)
}

/// Start a quick tunnel detached and wait for its public URL.
///
/// Refuses when a live tunnel is already recorded. On URL timeout the child
/// is terminated and no state file is written; the log is kept for
/// diagnosis.
pub fn start(spec: &TunnelSpec) -> Result<TunnelState> {
    // Hold the start lock across check → spawn → record: the URL wait runs
    // seconds-to-minutes, and without the lock two concurrent `connect`s
    // both observe "no tunnel" and double-spawn cloudflared.
    let start_lock = PathBuf::from(format!("{}.lock", state_path(&spec.state_dir).display()));
    let _start_guard = super::state_lock::exclusive_wait(&start_lock, super::state_lock::WAIT)?;

    if status(&spec.state_dir)?.is_some() {
        anyhow::bail!(
            "a tunnel is already running ({}); stop it first",
            state_path(&spec.state_dir).display()
        );
    }
    let binary = match &spec.binary {
        Some(b) => b.clone(),
        None => ensure_binary(&spec.state_dir)?,
    };
    std::fs::create_dir_all(&spec.state_dir)?;
    let log = log_path(&spec.state_dir);
    // `stop` removes tunnel.json but keeps the log, and cloudflared appends
    // to `--logfile`: without truncation a repeat connect in the same state
    // dir could adopt the dead tunnel's URL. Fresh log, fresh tunnel.
    std::fs::File::create(&log).context("truncate stale tunnel.log")?;

    let origin = spec.origin();
    let child = std::process::Command::new(&binary)
        .args([
            "tunnel",
            "--url",
            origin.as_str(),
            "--no-autoupdate",
            "--logfile",
        ])
        .arg(&log)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {}", binary.display()))?;
    let pid = child.id();

    match wait_for_url(&log, spec.url_timeout) {
        Some(url) => {
            let state = TunnelState {
                pid,
                url,
                port: spec.port,
                origin_host: spec.origin_host.clone(),
                binary: binary.display().to_string(),
                started_at: now_secs(),
            };
            save_state(&spec.state_dir, &state)?;
            // Fresh stamp: a leftover one from an earlier tunnel would date
            // this tunnel to a previous week and reap it on the first tick.
            write_traffic_stamp(&spec.state_dir);
            tracing::info!(url = %state.url, pid, "quick tunnel up");
            Ok(state)
        }
        None => {
            terminate(pid);
            reap(pid);
            anyhow::bail!(
                "cloudflared did not report a tunnel URL within {:?}; see {}",
                spec.url_timeout,
                log.display()
            );
        }
    }
}

/// Refuse to publish a gateway that would be useless or dangerous on the
/// internet: one that is switched off, or one with no `[a2a] api_key`.
///
/// Shared by `opencrabs connect` and the window's share card, so the two
/// doors into a public tunnel apply the same rule and cannot drift.
pub fn gateway_gate(a2a: &crate::config::A2aConfig) -> Result<()> {
    if !a2a.enabled {
        anyhow::bail!(
            "the A2A gateway is disabled ([a2a].enabled = false); a tunnel to a closed door helps nobody. Enable it first."
        );
    }
    if a2a.api_key.as_deref().unwrap_or("").trim().is_empty() {
        anyhow::bail!(
            "refusing to open a tunnel to a KEYLESS gateway: set [a2a].api_key first. \
             Without it, anyone who learns the tunnel URL gets the agent's full tool \
             surface (the #1473 open-gate posture), and route announcement stays suppressed."
        );
    }
    Ok(())
}

/// The running tunnel `open` would reuse for `port`, or `None` when nothing
/// runs. A tunnel bound to a different port is a conflict the caller must
/// resolve, not something to reuse or replace.
pub fn reusable(state_dir: &Path, port: u16) -> Result<Option<TunnelState>> {
    let existing = status(state_dir)?;
    if let Some(st) = &existing
        && st.port != port
    {
        anyhow::bail!(
            "a tunnel for port {} is already running ({}); use --port {}, or --stop first",
            st.port,
            st.url,
            st.port
        );
    }
    Ok(existing)
}

/// What [`open`] found or did.
#[derive(Debug, Clone)]
pub enum Opened {
    /// A tunnel for this port was already up; somebody else started it.
    Reused(TunnelState),
    /// This call spawned cloudflared; the caller decides who stops it.
    Started(TunnelState),
}

impl Opened {
    pub fn state(&self) -> &TunnelState {
        match self {
            Opened::Reused(st) | Opened::Started(st) => st,
        }
    }
}

/// The one way into a public tunnel: gate the gateway, reuse a tunnel already
/// up for this port, otherwise [`start`] one.
///
/// `opencrabs connect` and the window's share card both call this, so there
/// is a single place that decides whether cloudflared gets spawned. Blocking
/// (it may download cloudflared and waits for the URL): never call it on a
/// UI thread.
pub fn open(a2a: &crate::config::A2aConfig, spec: &TunnelSpec) -> Result<Opened> {
    gateway_gate(a2a)?;
    if let Some(st) = reusable(&spec.state_dir, spec.port)? {
        return Ok(Opened::Reused(st));
    }
    start(spec).map(Opened::Started)
}

/// Stop the recorded tunnel only if it is still the one with `pid`.
///
/// A caller that started a tunnel owns that tunnel and no other: if it died
/// and something else (a later `connect`) recorded a new one, stopping by
/// state file alone would kill a tunnel this caller never owned.
pub fn stop_owned(state_dir: &Path, pid: u32) -> Result<bool> {
    match status(state_dir)? {
        Some(st) if st.pid == pid => stop(state_dir),
        _ => Ok(false),
    }
}

/// A tunnel this process started and must not leave behind.
///
/// Dropping it stops cloudflared (via [`stop_owned`], so a tunnel recorded
/// by someone else since is left alone). The stop can wait the full grace
/// period, so a UI should move the guard to a worker thread before dropping
/// it; on exit, blocking briefly is the price of not orphaning the process.
#[derive(Debug)]
pub struct OwnedTunnel {
    state_dir: PathBuf,
    pid: u32,
}

impl OwnedTunnel {
    /// Take ownership of a tunnel [`open`] reported as `Started`.
    pub fn adopt(state_dir: &Path, state: &TunnelState) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
            pid: state.pid,
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for OwnedTunnel {
    fn drop(&mut self) {
        match stop_owned(&self.state_dir, self.pid) {
            Ok(true) => tracing::info!(pid = self.pid, "owned quick tunnel stopped"),
            Ok(false) => {}
            Err(e) => tracing::warn!(pid = self.pid, "owned quick tunnel not stopped: {e:#}"),
        }
    }
}

/// Read the recorded tunnel, verifying the pid still belongs to
/// cloudflared (not merely that *some* process owns the number — pids are
/// recycled). A dead or stolen tunnel's state file is removed as a side
/// effect.
pub fn status(state_dir: &Path) -> Result<Option<TunnelState>> {
    let path = state_path(state_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::Error::new(e)).context("read tunnel.json"),
    };
    let state: TunnelState =
        serde_json::from_slice(&bytes).context("parse tunnel.json (corrupt state file)")?;
    if !pid_is_cloudflared(state.pid) {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(state))
}

/// Stop a recorded tunnel: SIGTERM → grace → SIGKILL, each rung gated on
/// the pid still being cloudflared (a recycled pid is never signaled).
/// Returns whether a tunnel was running. Stale state files are removed
/// either way.
pub fn stop(state_dir: &Path) -> Result<bool> {
    let Some(state) = status(state_dir)? else {
        return Ok(false);
    };
    terminate(state.pid);
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline && pid_is_cloudflared(state.pid) {
        std::thread::sleep(Duration::from_millis(200));
    }
    if pid_is_cloudflared(state.pid) {
        kill_hard(state.pid);
    }
    reap(state.pid);
    let _ = std::fs::remove_file(state_path(state_dir));
    let _ = std::fs::remove_file(traffic_stamp_path(state_dir));
    Ok(true)
}

// ---------------------------------------------------------------- idle reaper
//
// An idle tunnel is a public hostname somebody forgot about: `connect` starts
// cloudflared and nothing ever stops it, so the URL stays reachable until the
// machine reboots. This is the missing stop condition, and it is deliberately
// NOT the `reap` function above: that one is a zombie `waitpid` for a child we
// killed, with no notion of time or traffic, and reading its name is how this
// feature came to look like it had already shipped.
//
// The signal is a stamp file touched by the gateway on every authenticated
// request. A stamp that does not exist means "no traffic since start", so the
// tunnel's own `started_at` is the baseline and a freshly connected tunnel is
// never reaped on its first check.

/// How long a quick tunnel may carry no traffic before it is stopped.
///
/// Long enough to survive a night without a disconnect, short enough that a
/// tunnel nobody remembers is gone by the next working day.
pub const IDLE_TIMEOUT_DEFAULT: Duration = Duration::from_secs(12 * 60 * 60);

/// Reaper tick. One `stat` per tick, so the cost is noise, and ten minutes is
/// two orders of magnitude below the idle window.
pub const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Floor between two stamp writes: traffic arrives per request, and the stamp
/// only needs minute resolution.
const TRAFFIC_STAMP_MIN_GAP: Duration = Duration::from_secs(60);

static LAST_STAMP_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether enough time passed since the last stamp write. Pure so the throttle
/// boundary is pinned by a test instead of by a sleep.
pub(crate) fn stamp_due(now: u64, last: u64) -> bool {
    now.saturating_sub(last) >= TRAFFIC_STAMP_MIN_GAP.as_secs()
}

/// `<state_dir>/tunnel.activity`
pub(crate) fn traffic_stamp_path(state_dir: &Path) -> PathBuf {
    state_dir.join("tunnel.activity")
}

pub(crate) fn write_traffic_stamp(state_dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(state_dir)
        .and_then(|()| std::fs::write(traffic_stamp_path(state_dir), b""))
    {
        // Traffic accounting is best effort: a read-only state dir must never
        // fail a gateway request.
        tracing::debug!("tunnel activity stamp not written: {e}");
    }
}

/// Record that the tunnel just carried traffic.
///
/// Cheap by design (the gateway calls it on every authenticated request) and
/// throttled to one write a minute. The throttle is per process, and a process
/// serves one tunnel, so a single global clock is the right scope.
pub fn note_traffic(state_dir: &Path) {
    let now = now_secs();
    let last = LAST_STAMP_SECS.load(std::sync::atomic::Ordering::Relaxed);
    if !stamp_due(now, last) {
        return;
    }
    if LAST_STAMP_SECS
        .compare_exchange(
            last,
            now,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        return;
    }
    write_traffic_stamp(state_dir);
}

/// When this tunnel last carried traffic, in unix seconds.
///
/// Falls back to the tunnel's `started_at` when no stamp exists: a tunnel that
/// has never been used is not idle merely because nobody used it yet, and a
/// fresh `connect` must not be reaped on the first tick.
pub(crate) fn last_activity_secs(state_dir: &Path, state: &TunnelState) -> u64 {
    match std::fs::metadata(traffic_stamp_path(state_dir)).and_then(|m| m.modified()) {
        Ok(modified) => modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(state.started_at)
            .max(state.started_at),
        Err(_) => state.started_at,
    }
}

/// Whether an activity reading is past the idle window. Split out so the
/// boundary is testable without a filesystem or a clock.
pub(crate) fn idle_expired(last_activity: u64, now: u64, timeout: Duration) -> bool {
    // A clock that moved backwards cannot measure idleness, and leaving a
    // public hostname up forever is the failure this reaper exists to
    // prevent, so an activity stamp from the future reads as idle (#2537).
    if now < last_activity {
        return true;
    }
    now - last_activity >= timeout.as_secs()
}

/// Stop the recorded tunnel when it has been idle for `timeout`.
///
/// Returns the tunnel that was stopped, so the caller can report the URL. `Ok(None)`
/// covers both "no tunnel running" and "tunnel has carried traffic recently",
/// which are the same answer to the caller: nothing was reaped.
pub fn reap_idle(state_dir: &Path, timeout: Duration) -> Result<Option<TunnelState>> {
    let Some(state) = status(state_dir)? else {
        return Ok(None);
    };
    let last = last_activity_secs(state_dir, &state);
    if !idle_expired(last, now_secs(), timeout) {
        return Ok(None);
    }
    tracing::info!(
        url = %state.url,
        pid = state.pid,
        idle_secs = now_secs().saturating_sub(last),
        "stopping idle quick tunnel (no traffic for the idle window)"
    );
    stop(state_dir)?;
    Ok(Some(state))
}

/// Reap on `IDLE_CHECK_INTERVAL` until the process exits (#2537).
///
/// The gateway has no request stream to piggyback on: an idle tunnel produces
/// no traffic precisely because it is idle, so the stop condition has to be a
/// timer rather than a side effect of the next request.
pub fn spawn_idle_reaper(state_dir: PathBuf) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(IDLE_CHECK_INTERVAL).await;
            if let Err(e) = reap_idle(&state_dir, IDLE_TIMEOUT_DEFAULT) {
                tracing::warn!("idle tunnel reaper: {e:#}");
            }
        }
    });
}

/// Poll the log file until a trycloudflare URL appears or the deadline hits.
fn wait_for_url(log: &Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(contents) = std::fs::read_to_string(log)
            && let Some(url) = parse_trycloudflare_url(&contents)
        {
            return Some(url);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(URL_POLL_INTERVAL);
    }
}

/// Extract the most recent `https://<id>.trycloudflare.com` URL from
/// cloudflared log text. `start` truncates the log, so normally there is
/// exactly one; when stale content survives anyway, cloudflared's append
/// semantics mean the *last* match is the live tunnel's. Deliberately
/// regex-free: split on whitespace, trim wrapping
/// punctuation (log banners box the URL in `|`s and prose wraps it in
/// parens), then require the exact scheme and domain suffix. Lookalikes
/// (`…trycloudflare.com.evil.io`, `https://evil.com/trycloudflare.com`)
/// fail the suffix/prefix checks.
pub fn parse_trycloudflare_url(text: &str) -> Option<String> {
    const MIN_LEN: usize = "https://_.trycloudflare.com".len();
    text.split_whitespace()
        .filter_map(|tok| {
            let url = tok.trim_matches(|c: char| {
                !c.is_ascii_alphanumeric() && c != '-' && c != '.' && c != ':' && c != '/'
            });
            (url.starts_with("https://")
                && url.ends_with(".trycloudflare.com")
                && url.len() >= MIN_LEN)
                .then(|| url.to_string())
        })
        .next_back()
}

pub(crate) fn save_state(state_dir: &Path, state: &TunnelState) -> Result<()> {
    let json = serde_json::to_vec_pretty(state).context("serialize tunnel state")?;
    let path = state_path(state_dir);
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, &json).context("write tunnel.json.tmp")?;
    std::fs::rename(&tmp, &path).context("rename tunnel.json.tmp")?;
    Ok(())
}

/// Identity guard: liveness only proves *some* process owns the pid. If
/// cloudflared died and the OS recycled the number, a blind `stop` would
/// signal an innocent process. The guard demands the pid's command line
/// name cloudflared before any signal or liveness claim.
///
/// Full `args`, not `comm`: the repo's test stubs run as
/// `sh <path>/cloudflared-stub …` where comm is "sh" but args carry the
/// script path; a real cloudflared's args start with its own path.
#[cfg(unix)]
fn pid_is_cloudflared(pid: u32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();
    matches!(out, Ok(o) if o.status.success()
        && String::from_utf8_lossy(&o.stdout).to_lowercase().contains("cloudflared"))
}

/// Windows: `tasklist` with a PID filter prints an `INFO:` banner when no
/// process owns the pid; anything else means one does. (Production reads
/// identity via [`pid_is_cloudflared`]; the liveness-only probe is
/// test-only on unix and unnecessary on Windows, where the identity
/// check subsumes it.)
#[cfg(windows)]
fn pid_is_cloudflared(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();
    matches!(out, Ok(o) if o.status.success()
        && String::from_utf8_lossy(&o.stdout).to_lowercase().contains("cloudflared"))
}

#[cfg(unix)]
fn terminate(pid: u32) {
    unsafe {
        libc::kill(pid.min(i32::MAX as u32) as i32, libc::SIGTERM);
    }
}

/// Windows has no signals: the polite rung is taskkill's graceful window
/// close, same call shape as the unix arm.
#[cfg(windows)]
fn terminate(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(unix)]
fn kill_hard(pid: u32) {
    unsafe {
        libc::kill(pid.min(i32::MAX as u32) as i32, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_hard(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Best-effort zombie reap. In production the CLI exits before the tunnel
/// does, so the child is reparented and this is a harmless ECHILD; in tests
/// the killed stub is our own child and gets reaped here.
#[cfg(unix)]
fn reap(pid: u32) {
    unsafe {
        libc::waitpid(
            pid.min(i32::MAX as u32) as i32,
            std::ptr::null_mut(),
            libc::WNOHANG,
        );
    }
}

/// Windows: the kernel reaps terminated children; there is no zombie to
/// collect.
#[cfg(windows)]
fn reap(_pid: u32) {}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod +x {}", path.display()))?;
    Ok(())
}

/// Windows: executability is the extension's job — the extracted
/// cloudflared binary runs as-is.
#[cfg(windows)]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}
