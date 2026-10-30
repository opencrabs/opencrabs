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
    /// Where `tunnel.json`, `tunnel.log` and `bin/` live.
    pub state_dir: PathBuf,
    /// Explicit cloudflared binary; `None` = resolve/ensure.
    pub binary: Option<PathBuf>,
    /// How long to wait for the URL in the log.
    pub url_timeout: Duration,
}

impl TunnelSpec {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            state_dir: default_state_dir(),
            binary: None,
            url_timeout: URL_WAIT_DEFAULT,
        }
    }
}

/// Durable record of a running tunnel: `state_dir/tunnel.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelState {
    pub pid: u32,
    pub url: String,
    pub port: u16,
    pub binary: String,
    pub started_at: u64,
}

/// `<opencrabs_home>/state/remote`
pub fn default_state_dir() -> PathBuf {
    crate::config::opencrabs_home().join("state").join("remote")
}

fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("tunnel.json")
}

fn log_path(state_dir: &Path) -> PathBuf {
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

    let child = std::process::Command::new(&binary)
        .args([
            "tunnel",
            "--url",
            &format!("http://127.0.0.1:{}", spec.port),
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
                binary: binary.display().to_string(),
                started_at: now_secs(),
            };
            save_state(&spec.state_dir, &state)?;
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

/// Read the recorded tunnel, verifying the pid is still alive. A dead
/// tunnel's state file is removed as a side effect.
pub fn status(state_dir: &Path) -> Result<Option<TunnelState>> {
    let path = state_path(state_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::Error::new(e)).context("read tunnel.json"),
    };
    let state: TunnelState =
        serde_json::from_slice(&bytes).context("parse tunnel.json (corrupt state file)")?;
    if !pid_alive(state.pid) {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(state))
}

/// Stop a recorded tunnel: SIGTERM → grace → SIGKILL. Returns whether a
/// tunnel was running. Stale state files are removed either way.
pub fn stop(state_dir: &Path) -> Result<bool> {
    let Some(state) = status(state_dir)? else {
        return Ok(false);
    };
    terminate(state.pid);
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline && pid_alive(state.pid) {
        std::thread::sleep(Duration::from_millis(200));
    }
    if pid_alive(state.pid) {
        kill_hard(state.pid);
    }
    reap(state.pid);
    let _ = std::fs::remove_file(state_path(state_dir));
    Ok(true)
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

/// Extract the first `https://<id>.trycloudflare.com` URL from cloudflared
/// log text. Deliberately regex-free: split on whitespace, trim wrapping
/// punctuation (log banners box the URL in `|`s and prose wraps it in
/// parens), then require the exact scheme and domain suffix. Lookalikes
/// (`…trycloudflare.com.evil.io`, `https://evil.com/trycloudflare.com`)
/// fail the suffix/prefix checks.
pub fn parse_trycloudflare_url(text: &str) -> Option<String> {
    const MIN_LEN: usize = "https://_.trycloudflare.com".len();
    text.split_whitespace().find_map(|tok| {
        let url = tok.trim_matches(|c: char| {
            !c.is_ascii_alphanumeric() && c != '-' && c != '.' && c != ':' && c != '/'
        });
        (url.starts_with("https://") && url.ends_with(".trycloudflare.com") && url.len() >= MIN_LEN)
            .then(|| url.to_string())
    })
}

fn save_state(state_dir: &Path, state: &TunnelState) -> Result<()> {
    let json = serde_json::to_vec_pretty(state).context("serialize tunnel state")?;
    let path = state_path(state_dir);
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, &json).context("write tunnel.json.tmp")?;
    std::fs::rename(&tmp, &path).context("rename tunnel.json.tmp")?;
    Ok(())
}

fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) probes existence: ESRCH = gone, EPERM = alive (not ours).
    // Clamp to i32::MAX: an out-of-range pid cast negative would signal -1.
    let rc = unsafe { libc::kill(pid.min(i32::MAX as u32) as i32, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn terminate(pid: u32) {
    unsafe {
        libc::kill(pid.min(i32::MAX as u32) as i32, libc::SIGTERM);
    }
}

fn kill_hard(pid: u32) {
    unsafe {
        libc::kill(pid.min(i32::MAX as u32) as i32, libc::SIGKILL);
    }
}

/// Best-effort zombie reap. In production the CLI exits before the tunnel
/// does, so the child is reparented and this is a harmless ECHILD; in tests
/// the killed stub is our own child and gets reaped here.
fn reap(pid: u32) {
    unsafe {
        libc::waitpid(
            pid.min(i32::MAX as u32) as i32,
            std::ptr::null_mut(),
            libc::WNOHANG,
        );
    }
}

fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod +x {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests;
