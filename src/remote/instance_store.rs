//! Client-side instance credentials: the crabs this window can reach.
//!
//! `state/remote/instances.json`, 0600, flock + atomic rename — the same
//! mechanics as [`super::pairing`] and [`super::route_store`]. One entry per
//! remote host: the stable host id, the label its owner gave it, the URL we
//! last reached it at, and the `ocpair_` session token the pairing exchange
//! issued.
//!
//! Two files, two questions, deliberately apart:
//!
//! - `routes.json` says WHERE a host is (lan, tailnet, tunnel, and which one
//!   answers fastest right now).
//! - `instances.json` says WHO we are to that host.
//!
//! Keeping them apart means a route can be re-learned or pruned without ever
//! dropping the credential, and a credential can be revoked without forgetting
//! how to reach the host.
//!
//! The pairing exchange itself ([`pair`]) is the client half of the public
//! `POST /a2a/pair` route: the one-time code is the credential, and the token
//! it returns is stored here. No protocol change is needed for that direction,
//! which is why level 1 of the instance rail works against a stock gateway.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::now_secs;

/// On-disk schema version. Bumped only when a reader would misread the file.
pub const INSTANCES_VERSION: u32 = 1;

/// How long the pairing exchange waits on the remote before giving up.
/// Pairing happens over whatever route the owner typed; a dead URL must fail
/// in seconds, not hang the modal.
pub const PAIR_TIMEOUT: Duration = Duration::from_secs(8);

fn default_version() -> u32 {
    INSTANCES_VERSION
}

/// One remote crab we hold a credential for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownInstance {
    /// Stable id the remote announced on `/a2a/routes`.
    pub host_id: String,
    /// The name the owner gave it ("server i7"), not the remote's own hostname.
    pub label: String,
    /// The URL the pairing exchange reached it at.
    pub url: String,
    /// The `ocpair_` session token. Plaintext at rest in a 0600 file, exactly
    /// like the pairing store's own sessions are hashed because THEY are the
    /// server side; a client must be able to present the token, so it cannot
    /// be hashed here.
    pub token: String,
    pub added_at: u64,
    /// Unix seconds of the last successful call, for the strip's "last seen".
    #[serde(default)]
    pub last_ok: u64,
}

/// On-disk shape of `state/remote/instances.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InstanceFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub instances: Vec<KnownInstance>,
}

/// The client credential store. Production uses [`InstanceStore::default_path`];
/// tests inject a temp file so a test never touches the live home.
#[derive(Clone)]
pub struct InstanceStore {
    path: PathBuf,
}

impl InstanceStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `<opencrabs_home>/state/remote/instances.json`
    pub fn default_path() -> PathBuf {
        crate::config::opencrabs_home()
            .join("state")
            .join("remote")
            .join("instances.json")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the store. A missing file is an empty store, never an error: a
    /// fresh install has paired with nobody yet.
    pub fn load(&self) -> Result<InstanceFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse instance store {}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(InstanceFile {
                version: INSTANCES_VERSION,
                instances: Vec::new(),
            }),
            Err(e) => {
                Err(anyhow::Error::new(e)).with_context(|| format!("read {}", self.path.display()))
            }
        }
    }

    /// Insert or replace one entry, keyed by `host_id`. Pairing twice with the
    /// same host is a token rotation, not a second row.
    pub fn upsert(&self, instance: KnownInstance) -> Result<()> {
        self.with_locked_store(|st| {
            match st
                .instances
                .iter_mut()
                .find(|i| i.host_id == instance.host_id)
            {
                Some(existing) => *existing = instance,
                None => st.instances.push(instance),
            }
            Ok(())
        })
    }

    /// Forget one host. Returns whether a row was actually removed, so the
    /// caller can say "nothing to remove" instead of pretending.
    pub fn remove(&self, host_id: &str) -> Result<bool> {
        self.with_locked_store(|st| {
            let before = st.instances.len();
            st.instances.retain(|i| i.host_id != host_id);
            Ok(st.instances.len() != before)
        })
    }

    /// One entry, if we hold a credential for that host.
    pub fn find(&self, host_id: &str) -> Result<Option<KnownInstance>> {
        Ok(self
            .load()?
            .instances
            .into_iter()
            .find(|i| i.host_id == host_id))
    }

    /// Stamp a successful call, which is what "last seen 2h ago" reads.
    pub fn record_ok(&self, host_id: &str, now: u64) -> Result<()> {
        self.with_locked_store(|st| {
            if let Some(i) = st.instances.iter_mut().find(|i| i.host_id == host_id) {
                i.last_ok = now;
            }
            Ok(())
        })
    }

    /// Load → mutate → atomic-save under the exclusive state lock. The lock is
    /// the sibling `.lock` file, same contract as the pairing store: bounded
    /// wait, then a hard error, never a silent unlocked write.
    fn with_locked_store<T>(&self, f: impl FnOnce(&mut InstanceFile) -> Result<T>) -> Result<T> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let _lock =
            super::state_lock::exclusive_wait(&self.lock_path(), super::state_lock::WAIT)?;
        let mut store = self.load()?;
        if store.version == 0 {
            store.version = INSTANCES_VERSION;
        }
        let out = f(&mut store)?;
        self.save(&store)?;
        Ok(out)
    }

    /// Atomic write: temp file → 0600 → fsync → rename over the target. A
    /// reader never observes a partial file.
    fn save(&self, store: &InstanceFile) -> Result<()> {
        let json = serde_json::to_vec_pretty(store).context("serialize instance store")?;
        let tmp = PathBuf::from(format!("{}.tmp", self.path.display()));
        {
            let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .context("chmod 0600 instance store")?;
        }
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), self.path.display()))?;
        Ok(())
    }

    fn lock_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.lock", self.path.display()))
    }
}

/// What a successful pairing leaves us with.
#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub host_id: String,
    pub label: String,
    pub url: String,
    pub token: String,
    pub expires_at: u64,
}

/// Exchange a one-time code for a scoped session token, then learn the host id
/// from the route announcement.
///
/// Two calls, in this order, because the pair response deliberately carries no
/// host id: `/a2a/pair` is unauthenticated and must not leak topology, while
/// `/a2a/routes` is auth-gated and is exactly where the id lives. A token
/// minted with `read` scope can call both.
pub async fn pair(base_url: &str, code: &str, label: &str) -> Result<Paired> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        anyhow::bail!("the remote URL is empty");
    }
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/a2a/pair"))
        .timeout(PAIR_TIMEOUT)
        .json(&serde_json::json!({ "code": code.trim(), "label": label.trim() }))
        .send()
        .await
        .with_context(|| format!("POST {base}/a2a/pair"))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        // The server answers 401/429 with the same shape for every failure
        // (invalid, expired, used), so this message never narrows a guess.
        let retry = body.get("retry_after_secs").and_then(|v| v.as_u64());
        anyhow::bail!(match (status.as_u16(), retry) {
            (429, Some(secs)) => format!("too many attempts; try again in {secs}s"),
            (429, None) => "too many attempts; slow down".to_string(),
            (401, _) => "code refused (invalid, expired or already used)".to_string(),
            (code, _) => format!("pairing failed: HTTP {code}"),
        });
    }
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow::anyhow!("pairing response carried no token"))?
        .to_string();
    let expires_at = body
        .get("expires_at")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let announcement = announce(base, &token).await?;
    Ok(Paired {
        host_id: announcement.0,
        label: if label.trim().is_empty() {
            announcement.2
        } else {
            label.trim().to_string()
        },
        url: base.to_string(),
        token,
        expires_at,
    })
}

/// `GET /a2a/routes` with the freshly issued token: `(host_id, routes, name)`.
///
/// `name` is the remote's own agent-card name, used as the label when the owner
/// typed none. A host that announces nothing (keyless gateway, or an older
/// build without route announcement) still pairs: the host id then falls back
/// to the URL host, which is stable enough to key a credential on and honest
/// about being derived rather than announced.
async fn announce(base: &str, token: &str) -> Result<(String, Vec<(String, String)>, String)> {
    let client = reqwest::Client::new();
    let mut host_id = String::new();
    let mut routes: Vec<(String, String)> = Vec::new();
    if let Ok(resp) = client
        .get(format!("{base}/a2a/routes"))
        .timeout(PAIR_TIMEOUT)
        .bearer_auth(token)
        .send()
        .await
        && resp.status().is_success()
        && let Ok(body) = resp.json::<serde_json::Value>().await
    {
        host_id = body
            .get("host_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if let Some(list) = body.get("routes").and_then(|v| v.as_array()) {
            for r in list {
                if let (Some(url), Some(kind)) = (
                    r.get("url").and_then(|v| v.as_str()),
                    r.get("kind").and_then(|v| v.as_str()),
                ) {
                    routes.push((kind.to_string(), url.to_string()));
                }
            }
        }
    }
    // Best effort: an older crab may not serve the agent card at all, and a
    // missing name is not a reason to fail a pairing that already has a token.
    let name = match client
        .get(format!("{base}/.well-known/agent.json"))
        .timeout(PAIR_TIMEOUT)
        .send()
        .await
    {
        Ok(resp) => resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    if host_id.is_empty() {
        host_id = derived_host_id(base);
    }
    Ok((host_id, routes, name))
}

/// Stable fallback id when the remote announces none: the URL's host and port.
/// Derived, and named so in the file, rather than inventing a random id that
/// would duplicate the row on every pairing.
fn derived_host_id(base: &str) -> String {
    let without_scheme = base
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base);
    let host = without_scheme.split('/').next().unwrap_or(without_scheme);
    format!("url:{host}")
}

/// What `/a2a/routes` said, for the caller that wants to seed the route store.
pub fn routes_from_announcement(body: &serde_json::Value) -> (Option<String>, Vec<(String, String)>) {
    let host_id = body
        .get("host_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut routes = Vec::new();
    if let Some(list) = body.get("routes").and_then(|v| v.as_array()) {
        for r in list {
            if let (Some(url), Some(kind)) = (
                r.get("url").and_then(|v| v.as_str()),
                r.get("kind").and_then(|v| v.as_str()),
            ) {
                routes.push((kind.to_string(), url.to_string()));
            }
        }
    }
    (host_id, routes)
}

/// Unix seconds, re-exported so callers in the GUI do not reach two modules.
pub fn now() -> u64 {
    now_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> InstanceStore {
        let dir = std::env::temp_dir().join(format!(
            "opencrabs-instance-store-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        InstanceStore::new(dir.join("instances.json"))
    }

    fn known(host: &str, label: &str, token: &str) -> KnownInstance {
        KnownInstance {
            host_id: host.to_string(),
            label: label.to_string(),
            url: format!("http://{host}:8080"),
            token: token.to_string(),
            added_at: 1,
            last_ok: 0,
        }
    }

    #[test]
    fn a_missing_file_is_an_empty_store() {
        let store = temp_store("missing");
        let file = store.load().expect("missing is not an error");
        assert!(file.instances.is_empty());
    }

    #[test]
    fn upsert_round_trips_and_replaces_by_host_id() {
        let store = temp_store("roundtrip");
        store.upsert(known("h1", "server i7", "ocpair_a")).unwrap();
        store.upsert(known("h2", "vps", "ocpair_b")).unwrap();
        // Same host, new token: a rotation, not a second row.
        store.upsert(known("h1", "server i7", "ocpair_c")).unwrap();
        let file = store.load().unwrap();
        assert_eq!(file.instances.len(), 2);
        let h1 = store.find("h1").unwrap().expect("h1 exists");
        assert_eq!(h1.token, "ocpair_c", "the later pairing wins");
        assert_eq!(h1.label, "server i7");
    }

    #[test]
    fn remove_reports_whether_a_row_went() {
        let store = temp_store("remove");
        store.upsert(known("h1", "a", "t")).unwrap();
        assert!(store.remove("h1").unwrap());
        assert!(!store.remove("h1").unwrap(), "second remove removes nothing");
        assert!(store.load().unwrap().instances.is_empty());
    }

    #[test]
    fn record_ok_stamps_only_the_named_host() {
        let store = temp_store("record");
        store.upsert(known("h1", "a", "t")).unwrap();
        store.upsert(known("h2", "b", "t2")).unwrap();
        store.record_ok("h2", 1234).unwrap();
        assert_eq!(store.find("h1").unwrap().unwrap().last_ok, 0);
        assert_eq!(store.find("h2").unwrap().unwrap().last_ok, 1234);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let store = temp_store("perms");
        store.upsert(known("h1", "a", "ocpair_secret")).unwrap();
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a token on disk is 0600 or it is a leak");
    }

    #[test]
    fn a_derived_id_is_stable_and_never_random() {
        assert_eq!(
            derived_host_id("http://192.168.1.7:8080/"),
            "url:192.168.1.7:8080"
        );
        assert_eq!(
            derived_host_id("http://192.168.1.7:8080"),
            derived_host_id("http://192.168.1.7:8080/"),
            "the same host must key the same row"
        );
    }

    #[test]
    fn routes_from_announcement_reads_the_wire_shape() {
        let body = serde_json::json!({
            "host_id": "abc123",
            "routes": [
                {"url": "http://192.168.1.7:8080", "kind": "lan"},
                {"url": "https://x.trycloudflare.com", "kind": "tunnel"}
            ]
        });
        let (host, routes) = routes_from_announcement(&body);
        assert_eq!(host.as_deref(), Some("abc123"));
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[1].0, "tunnel");
    }

    #[test]
    fn an_empty_announcement_yields_no_host() {
        let (host, routes) = routes_from_announcement(&serde_json::json!({
            "host_id": null, "routes": []
        }));
        assert!(host.is_none());
        assert!(routes.is_empty());
    }
}
