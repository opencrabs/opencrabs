//! Client-side route store: race, learn, fail over.
//!
//! One JSON file (`state/remote/routes.json`, 0600, flock + atomic rename —
//! same mechanics as `pairing.rs`) keyed by the host_id servers announce.
//! Per route: kind, source (configured routes are never pruned by
//! learning), latency EWMA, and a windowed failure count.
//!
//! Race discipline (t3code, adapted):
//! - Probe, never POST: racing real mutations risks double-send. `race_best`
//!   GETs `/a2a/health` on every route concurrently; the first healthy
//!   response wins and the rest are aborted (aborted ≠ failed: no outcome
//!   recorded for them).
//! - Stale routes get a short leash: a route with recent failures is probed
//!   with [`STALE_TIMEOUT`] instead of [`FRESH_TIMEOUT`], so a dead LAN
//!   address (Wi-Fi changed) can't stall resolution by seconds.
//! - Measurement beats memory: EWMA (α = 0.3) from real probes orders the
//!   next race; there is no background poller and no move-back timer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::routes::{AnnouncedRoute, RouteKind};

/// Probe timeout for routes with recent failures.
pub const STALE_TIMEOUT: Duration = Duration::from_secs(1);
/// Probe timeout for routes in good standing.
pub const FRESH_TIMEOUT: Duration = Duration::from_secs(3);
/// EWMA smoothing factor for probe latency.
pub const EWMA_ALPHA: f64 = 0.3;
/// Failures older than this window don't count toward staleness/ordering.
pub const FAILURE_WINDOW_SECS: u64 = 300;
/// Learned routes unseen in announcements for this long are pruned.
pub const LEARNED_PRUNE_SECS: u64 = 24 * 3600;
/// Ordering latency assumed for never-measured routes (mid-pack, not last).
const UNMEASURED_MS: f64 = 1000.0;

/// Where a route came from: user/profile config, or learned from the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouteSource {
    Configured,
    Learned,
}

/// One known way to reach one host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteEntry {
    pub url: String,
    pub kind: RouteKind,
    pub source: RouteSource,
    /// Last announcement/sighting (drives learned pruning).
    pub last_seen: u64,
    /// Unix seconds of the last successful probe/call.
    pub last_ok: u64,
    /// Unix seconds of the last failed probe/call.
    pub last_fail: u64,
    /// Smoothed probe latency; 0.0 = never measured.
    pub ewma_ms: f64,
    /// Consecutive failures inside [`FAILURE_WINDOW_SECS`].
    pub failures: u32,
}

impl RouteEntry {
    fn new(url: String, kind: RouteKind, source: RouteSource, now: u64) -> Self {
        Self {
            url,
            kind,
            source,
            last_seen: now,
            last_ok: 0,
            last_fail: 0,
            ewma_ms: 0.0,
            failures: 0,
        }
    }

    /// Is this route in bad standing right now (recent failures)?
    pub fn stale(&self, now: u64) -> bool {
        self.failures > 0 && now.saturating_sub(self.last_fail) < FAILURE_WINDOW_SECS
    }

    /// Probe timeout: short leash for stale routes.
    pub fn probe_timeout(&self, now: u64) -> Duration {
        if self.stale(now) {
            STALE_TIMEOUT
        } else {
            FRESH_TIMEOUT
        }
    }
}

/// Everything known about one remote host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostRoutes {
    pub label: String,
    pub seen_at: u64,
    pub routes: Vec<RouteEntry>,
}

/// On-disk shape of `routes.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RouteStoreFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub hosts: BTreeMap<String, HostRoutes>,
}

fn default_version() -> u32 {
    1
}

/// What one route did in a race. Aborted (cancelled) routes are absent —
/// absence is neutrality, not failure.
#[derive(Debug, Clone)]
pub struct RaceOutcome {
    pub url: String,
    pub ok: bool,
    pub latency_ms: f64,
}

/// The route store. Path-injected for tests; production uses
/// [`RouteStore::default_path`].
#[derive(Clone)]
pub struct RouteStore {
    path: PathBuf,
}

impl RouteStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `<opencrabs_home>/state/remote/routes.json`
    pub fn default_path() -> PathBuf {
        crate::config::opencrabs_home()
            .join("state")
            .join("remote")
            .join("routes.json")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<RouteStoreFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse route store {}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RouteStoreFile::default()),
            Err(e) => {
                Err(anyhow::Error::new(e)).with_context(|| format!("read {}", self.path.display()))
            }
        }
    }

    /// Atomic write: temp → 0600 → fsync → rename (same contract as pairing).
    pub fn save(&self, file: &RouteStoreFile) -> Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(file).context("serialize route store")?;
        let tmp = PathBuf::from(format!("{}.tmp", self.path.display()));
        {
            let mut f =
                std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Read-modify-write under the shared remote flock (same lock file the
    /// pairing store uses: one writer at a time across ALL remote state).
    fn modify(&self, f: impl FnOnce(&mut RouteStoreFile) -> Result<()>) -> Result<()> {
        use std::os::unix::io::AsRawFd;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = self.path.with_extension("lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        match crate::config::flock::exclusive(lock_file.as_raw_fd(), false) {
            crate::config::flock::FlockOutcome::Acquired => {}
            crate::config::flock::FlockOutcome::Held => {
                anyhow::bail!("remote lock unexpectedly held in non-blocking mode")
            }
            crate::config::flock::FlockOutcome::Failed(e) => {
                return Err(anyhow::Error::new(e)).context("flock routes.lock");
            }
        }
        let mut file = self.load()?;
        f(&mut file)?;
        self.save(&file)
    }

    /// Register a user/profile route (e.g. a profile's advertise_url).
    /// Existing entries at the same URL are promoted to Configured —
    /// configured routes are never pruned by learning.
    pub fn upsert_configured(
        &self,
        host_id: &str,
        label: &str,
        url: &str,
        kind: RouteKind,
        now: u64,
    ) -> Result<()> {
        let url = normalize_url(url);
        self.modify(|file| {
            let host = file
                .hosts
                .entry(host_id.to_string())
                .or_insert_with(|| HostRoutes {
                    label: label.to_string(),
                    seen_at: now,
                    routes: Vec::new(),
                });
            host.seen_at = now;
            match host.routes.iter_mut().find(|r| r.url == url) {
                Some(r) => {
                    r.source = RouteSource::Configured;
                    r.last_seen = now;
                }
                None => host
                    .routes
                    .push(RouteEntry::new(url, kind, RouteSource::Configured, now)),
            }
            Ok(())
        })
    }

    /// Merge a host's announcement (union by URL). Learned routes unseen
    /// for [`LEARNED_PRUNE_SECS`] are pruned; configured routes never are.
    pub fn learn(
        &self,
        host_id: &str,
        label: &str,
        announced: &[AnnouncedRoute],
        now: u64,
    ) -> Result<()> {
        self.modify(|file| {
            let host = file
                .hosts
                .entry(host_id.to_string())
                .or_insert_with(|| HostRoutes {
                    label: label.to_string(),
                    seen_at: now,
                    routes: Vec::new(),
                });
            host.seen_at = now;
            for a in announced {
                let url = normalize_url(&a.url);
                match host.routes.iter_mut().find(|r| r.url == url) {
                    Some(r) => {
                        r.last_seen = now;
                        r.kind = a.kind;
                    }
                    None => {
                        host.routes
                            .push(RouteEntry::new(url, a.kind, RouteSource::Learned, now))
                    }
                }
            }
            host.routes.retain(|r| {
                r.source == RouteSource::Configured
                    || now.saturating_sub(r.last_seen) <= LEARNED_PRUNE_SECS
            });
            Ok(())
        })
    }

    /// Record a successful probe/call: EWMA update, failure streak cleared.
    pub fn record_success(&self, url: &str, latency_ms: f64, now: u64) -> Result<()> {
        let url = normalize_url(url);
        self.modify(|file| {
            for host in file.hosts.values_mut() {
                if let Some(r) = host.routes.iter_mut().find(|r| r.url == url) {
                    r.ewma_ms = if r.ewma_ms <= 0.0 {
                        latency_ms
                    } else {
                        EWMA_ALPHA * latency_ms + (1.0 - EWMA_ALPHA) * r.ewma_ms
                    };
                    r.last_ok = now;
                    r.failures = 0;
                }
            }
            Ok(())
        })
    }

    /// Record a connect-level failure: windowed streak bump.
    pub fn record_failure(&self, url: &str, now: u64) -> Result<()> {
        let url = normalize_url(url);
        self.modify(|file| {
            for host in file.hosts.values_mut() {
                if let Some(r) = host.routes.iter_mut().find(|r| r.url == url) {
                    r.failures = if now.saturating_sub(r.last_fail) < FAILURE_WINDOW_SECS {
                        r.failures + 1
                    } else {
                        1
                    };
                    r.last_fail = now;
                }
            }
            Ok(())
        })
    }

    pub fn host(&self, host_id: &str) -> Result<Option<HostRoutes>> {
        Ok(self.load()?.hosts.get(host_id).cloned())
    }

    /// Find the host that owns a URL (exact match after normalization).
    pub fn find_host_by_url(&self, url: &str) -> Result<Option<(String, HostRoutes)>> {
        let url = normalize_url(url);
        let file = self.load()?;
        for (id, host) in file.hosts {
            if host.routes.iter().any(|r| r.url == url) {
                return Ok(Some((id, host)));
            }
        }
        Ok(None)
    }
}

/// Race ordering: good standing first, then fastest measured latency;
/// never-measured routes sort as [`UNMEASURED_MS`] (mid-pack, not last —
/// a fresh route deserves a chance).
pub fn order(routes: &[RouteEntry], now: u64) -> Vec<RouteEntry> {
    let mut v = routes.to_vec();
    v.sort_by(|a, b| {
        let ka = (
            a.stale(now) as u8,
            if a.ewma_ms > 0.0 {
                a.ewma_ms
            } else {
                UNMEASURED_MS
            },
        );
        let kb = (
            b.stale(now) as u8,
            if b.ewma_ms > 0.0 {
                b.ewma_ms
            } else {
                UNMEASURED_MS
            },
        );
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });
    v
}

fn normalize_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

/// Concurrent health probes; first healthy response wins, the rest are
/// aborted (cancellation records nothing). Returns the winner plus every
/// outcome that completed before the win — callers record success/failure
/// from those and ignore absent (aborted) routes.
pub async fn race_best(routes: &[RouteEntry], now: u64) -> (Option<String>, Vec<RaceOutcome>) {
    let client = reqwest::Client::new();
    let mut set = tokio::task::JoinSet::new();
    for r in routes {
        let url = r.url.clone();
        let timeout = r.probe_timeout(now);
        let c = client.clone();
        set.spawn(async move {
            let started = Instant::now();
            let probe = c.get(format!("{}/a2a/health", url)).timeout(timeout).send();
            let ok = match tokio::time::timeout(timeout + Duration::from_millis(250), probe).await {
                Ok(Ok(resp)) => resp.status().is_success(),
                _ => false,
            };
            (url, ok, started.elapsed().as_secs_f64() * 1000.0)
        });
    }

    let mut outcomes = Vec::new();
    let mut winner = None;
    while let Some(joined) = set.join_next().await {
        if let Ok((url, ok, latency_ms)) = joined {
            outcomes.push(RaceOutcome {
                url: url.clone(),
                ok,
                latency_ms,
            });
            if ok {
                winner = Some(url);
                set.abort_all();
                break;
            }
        }
    }
    (winner, outcomes)
}

#[cfg(test)]
mod tests;
