//! One-time pairing codes and scoped session tokens.
//!
//! Flow: `opencrabs connect` (CLI, its own process) mints a short code and
//! prints it beside the tunnel URL. The client POSTs the code to the public
//! `/a2a/pair` endpoint; the daemon verifies it against this store, consumes
//! it (single use) and issues a long-lived session token. Both processes
//! share one JSON file under `state/remote/`, coordinated with an exclusive
//! `flock` and atomic temp+rename writes — that file is the whole IPC.
//!
//! Secrets at rest: codes and tokens are stored only as SHA-256 (base64url).
//! Plaintext exists exactly once, at mint/issue time, in the returning
//! process. Verification hashes the presented secret and compares digests.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{now_secs, scopes::is_known_scope};
use crate::config::flock::{self, FlockOutcome};

/// Crockford-flavoured 32-char alphabet: no I, L, O, U. Codes get read
/// aloud from a terminal and typed on a phone.
pub(crate) const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Code length: 32^8 = 2^40 guess space. Brute force is hopeless under the
/// global lockout below plus the server's per-IP rate limit.
pub const CODE_LEN: usize = 8;
/// Code lifetime, seconds.
pub const CODE_TTL_SECS: u64 = 300;
/// Session token lifetime, seconds (30 days).
pub const TOKEN_TTL_SECS: u64 = 30 * 86_400;
/// Prefix on every issued token: greppable, recognizable, revocable.
pub const TOKEN_PREFIX: &str = "ocpair_";
/// Consecutive failed verifies before the whole store locks out pairing.
pub const MAX_FAIL_STREAK: u32 = 10;
/// Lockout duration once tripped, seconds.
pub const LOCKOUT_SECS: u64 = 900;
/// Sessions expired/revoked longer than this get swept from the file.
const SESSION_RETENTION_SECS: u64 = 7 * 86_400;
/// Label cap; longer input is truncated, not rejected.
const MAX_LABEL_LEN: usize = 64;

/// SHA-256 of a secret, base64url (43 chars). Storage form for codes+tokens.
pub(crate) fn sha_b64(input: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(input.as_bytes()))
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    rand::rng().fill_bytes(&mut buf);
    buf
}

fn sanitize_label(label: &str) -> String {
    let cleaned: String = label
        .trim()
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(MAX_LABEL_LEN)
        .collect();
    if cleaned.is_empty() {
        "unnamed".to_string()
    } else {
        cleaned
    }
}

/// A minted, not-yet-redeemed pairing code. Only the hash is stored.
#[derive(Debug, Serialize, Deserialize)]
pub struct PendingCode {
    pub code_hash: String,
    pub scopes: Vec<String>,
    pub created_at: u64,
    pub expires_at: u64,
}

/// An issued session. The token itself only ever existed at issue time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Stable public identifier: first 12 chars of the token hash.
    pub id: String,
    pub token_hash: String,
    pub label: String,
    pub scopes: Vec<String>,
    pub created_at: u64,
    pub expires_at: u64,
    pub revoked_at: Option<u64>,
}

impl Session {
    /// Usable right now: not revoked, not expired.
    pub fn active(&self, now: u64) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }
}

/// Owner-facing session summary: no hashes, no secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub id: String,
    pub label: String,
    pub scopes: Vec<String>,
    pub created_at: u64,
    pub expires_at: u64,
    pub active: bool,
}

/// On-disk shape of `state/remote/pairing.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct StoreFile {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Consecutive failed verify_code calls (global, resets on success).
    #[serde(default)]
    pub fail_streak: u32,
    /// Unix seconds until which all pairing is refused.
    #[serde(default)]
    pub locked_until: Option<u64>,
    #[serde(default)]
    pub codes: Vec<PendingCode>,
    #[serde(default)]
    pub sessions: Vec<Session>,
}

fn default_version() -> u32 {
    1
}

impl Default for StoreFile {
    fn default() -> Self {
        Self {
            version: 1,
            fail_streak: 0,
            locked_until: None,
            codes: Vec::new(),
            sessions: Vec::new(),
        }
    }
}

/// Why a code verification failed. The HTTP layer maps these:
/// `Locked` → 429, `NoMatch` → 401, `Storage` → 500.
#[derive(Debug)]
pub enum VerifyError {
    Locked { retry_after_secs: u64 },
    NoMatch,
    Storage(anyhow::Error),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locked { retry_after_secs } => {
                write!(
                    f,
                    "pairing locked after repeated failures; retry in {retry_after_secs}s"
                )
            }
            Self::NoMatch => write!(f, "pairing code invalid, expired, or already used"),
            Self::Storage(e) => write!(f, "pairing store error: {e}"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// The pairing store: one JSON file + its flock sibling.
///
/// Constructed with an explicit path so tests stay hermetic (tempfile);
/// production uses [`PairingStore::default_path`].
#[derive(Clone)]
pub struct PairingStore {
    path: PathBuf,
}

impl PairingStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `<opencrabs_home>/state/remote/pairing.json`
    pub fn default_path() -> PathBuf {
        crate::config::opencrabs_home()
            .join("state")
            .join("remote")
            .join("pairing.json")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Mint a single-use code carrying `scopes` (validated, deduped).
    /// Returns the plaintext code — the only moment it exists.
    pub fn mint_code(&self, scopes: &[String]) -> Result<String> {
        if scopes.is_empty() {
            anyhow::bail!("a pairing code must carry at least one scope");
        }
        for s in scopes {
            if !is_known_scope(s) {
                anyhow::bail!("unknown scope {s:?}");
            }
        }
        let mut dedup = scopes.to_vec();
        dedup.sort();
        dedup.dedup();

        let code: String = random_bytes::<8>()
            .iter()
            .map(|b| CODE_ALPHABET[(b & 31) as usize] as char)
            .collect();
        let hash = sha_b64(&code);
        let now = now_secs();

        self.with_locked_store(|st| {
            st.codes.push(PendingCode {
                code_hash: hash,
                scopes: dedup,
                created_at: now,
                expires_at: now + CODE_TTL_SECS,
            });
            Ok(())
        })?;
        Ok(code)
    }

    /// Verify a presented code; on success consume it and issue a session.
    /// Wrong guesses don't reveal which codes exist: every failure is
    /// `NoMatch`, and a global fail streak eventually locks pairing.
    pub fn verify_code(
        &self,
        code: &str,
        label: &str,
    ) -> std::result::Result<(String, Session), VerifyError> {
        let now = now_secs();
        let hash = sha_b64(code.trim());
        let label = sanitize_label(label);

        self.with_locked_store(
            |st| -> Result<std::result::Result<(String, Session), VerifyError>> {
                if let Some(until) = st.locked_until {
                    if now < until {
                        return Ok(Err(VerifyError::Locked {
                            retry_after_secs: until - now,
                        }));
                    }
                    st.locked_until = None;
                }
                let found = st
                    .codes
                    .iter()
                    .position(|c| c.code_hash == hash && c.expires_at > now);
                let Some(idx) = found else {
                    st.fail_streak += 1;
                    if st.fail_streak >= MAX_FAIL_STREAK {
                        st.locked_until = Some(now + LOCKOUT_SECS);
                        st.fail_streak = 0;
                    }
                    return Ok(Err(VerifyError::NoMatch));
                };
                let pending = st.codes.remove(idx);
                st.fail_streak = 0;

                let token = format!(
                    "{TOKEN_PREFIX}{}",
                    URL_SAFE_NO_PAD.encode(random_bytes::<32>())
                );
                let token_hash = sha_b64(&token);
                let session = Session {
                    id: token_hash[..12].to_string(),
                    token_hash,
                    label,
                    scopes: pending.scopes,
                    created_at: now,
                    expires_at: now + TOKEN_TTL_SECS,
                    revoked_at: None,
                };
                st.sessions.push(session.clone());
                Ok(Ok((token, session)))
            },
        )
        .map_err(VerifyError::Storage)?
    }

    /// Hot path for the auth middleware: hash, look up, check liveness.
    /// Read-only (no lock): the writer's atomic rename guarantees a
    /// complete file, and a stale read is corrected by the next request.
    pub fn verify_token(&self, token: &str) -> Option<Session> {
        if !token.starts_with(TOKEN_PREFIX) {
            return None;
        }
        let hash = sha_b64(token);
        let now = now_secs();
        let store = self.load().ok()?;
        store
            .sessions
            .iter()
            .find(|s| s.token_hash == hash && s.active(now))
            .cloned()
    }

    /// Revoke every active session matching an id or label exactly.
    /// Returns how many were revoked.
    pub fn revoke(&self, id_or_label: &str) -> Result<usize> {
        let now = now_secs();
        self.with_locked_store(|st| {
            let mut n = 0;
            for s in st.sessions.iter_mut() {
                if s.active(now) && (s.id == id_or_label || s.label == id_or_label) {
                    s.revoked_at = Some(now);
                    n += 1;
                }
            }
            Ok(n)
        })
    }

    /// All sessions (active and not) as owner-safe views.
    pub fn list_sessions(&self) -> Result<Vec<SessionView>> {
        let now = now_secs();
        let store = self.load()?;
        Ok(store
            .sessions
            .iter()
            .map(|s| SessionView {
                id: s.id.clone(),
                label: s.label.clone(),
                scopes: s.scopes.clone(),
                created_at: s.created_at,
                expires_at: s.expires_at,
                active: s.active(now),
            })
            .collect())
    }

    /// Load → mutate → sweep → atomic-save under an exclusive flock.
    fn with_locked_store<T>(&self, f: impl FnOnce(&mut StoreFile) -> Result<T>) -> Result<T> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let lock_path = self.lock_path();
        let lock_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        match flock::exclusive(lock_file.as_raw_fd(), false) {
            FlockOutcome::Acquired => {}
            FlockOutcome::Held => {
                anyhow::bail!("pairing lock unexpectedly held in non-blocking mode")
            }
            FlockOutcome::Failed(e) => {
                return Err(anyhow::Error::new(e)).context("flock pairing.lock");
            }
        }

        let mut store = self.load()?;
        let out = f(&mut store)?;
        sweep(&mut store, now_secs());
        self.save(&store)?;
        Ok(out)
    }

    fn load(&self) -> Result<StoreFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse pairing store {}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile::default()),
            Err(e) => {
                Err(anyhow::Error::new(e)).with_context(|| format!("read {}", self.path.display()))
            }
        }
    }

    /// Atomic write: temp file → 0600 → fsync → rename over the target.
    /// A reader never observes a partial file.
    fn save(&self, store: &StoreFile) -> Result<()> {
        let json = serde_json::to_vec_pretty(store).context("serialize pairing store")?;
        let tmp = PathBuf::from(format!("{}.tmp", self.path.display()));
        {
            let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .context("chmod 0600 pairing store")?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), self.path.display()))?;
        Ok(())
    }

    fn lock_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.lock", self.path.display()))
    }
}

/// Drop expired codes and long-dead sessions. Runs on every locked write.
fn sweep(store: &mut StoreFile, now: u64) {
    store.codes.retain(|c| c.expires_at > now);
    store.sessions.retain(|s| {
        let dead_since =
            s.revoked_at
                .unwrap_or(0)
                .max(if s.expires_at <= now { s.expires_at } else { 0 });
        dead_since == 0 || now.saturating_sub(dead_since) < SESSION_RETENTION_SECS
    });
}
