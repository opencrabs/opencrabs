//! RSI Template Sync — Upstream brain file template synchronization.
//!
//! Checks for new releases, fetches updated templates from the public repo,
//! diffs against local brain files, and appends only new sections.
//!
//! State is persisted to `~/.opencrabs/rsi/state.toml`:
//! ```toml
//! last_synced_version = "0.3.14"
//! last_sync_date = "2026-04-27T21:00:00Z"
//!
//! [files]
//! SOUL.md = "2026-04-27T21:00:00Z"
//! TOOLS.md = "2026-04-27T21:00:00Z"
//! ```
//!
//! Flow:
//! 1. Version gate — compare `last_synced_version` to `crate::VERSION`. No change = bail.
//! 2. Backup all tracked files to `rsi/backups/`.
//! 3. Fetch upstream templates from raw GitHub URLs.
//! 4. Diff: extract sections in upstream that don't exist locally.
//! 5. Merge: append new sections. Log to `rsi/improvements.md`.
//! 6. Sanity check: verify file isn't empty. If failed, restore from backup.
//! 7. Update state.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::brain::tools::brain_file_safety;

/// GitHub raw URL base for templates.
/// Raw base for the repository ROOT, not the templates directory: tracked
/// files now live in both places (brain templates under
/// `src/docs/reference/templates`, config examples at the root), so each
/// entry carries its own repo-relative path (#819).
const TEMPLATE_BASE_URL: &str = "https://raw.githubusercontent.com/opencrabs/opencrabs/main";

/// Connect budget for a template fetch, matching the house pattern already in
/// `embedding.rs`, `gemini.rs`, `anthropic.rs` and
/// `custom_openai_compatible.rs`. `reqwest`'s own default is no timeout at
/// all, which is how a blackholed connection once wedged an embedding call
/// forever (#1062). The sync runs on the RSI task ahead of the startup digest
/// and the analysis loop, so an unbounded fetch there stalls the whole engine
/// rather than just the download (#2169).
const TEMPLATE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Total budget for one fetch, headers and body together. A template is a few
/// KB, so 60s only ever binds on a server that accepts and then stalls.
const TEMPLATE_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Attempts per template. One retry absorbs a single DNS flap; more than that
/// turns a real outage into a longer wall of the same error.
const TEMPLATE_MAX_ATTEMPTS: u8 = 2;

/// Build the bounded client used for every template fetch.
fn template_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(TEMPLATE_CONNECT_TIMEOUT)
        .timeout(TEMPLATE_REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("Failed to build template sync client: {e}"))
}

/// A brain file: same name locally and under the templates directory.
macro_rules! md {
    ($name:literal) => {
        TrackedTemplate {
            local: $name,
            upstream: concat!("src/docs/reference/templates/", $name),
            kind: TemplateKind::Markdown,
        }
    };
}

/// A config example: `foo.toml` locally, `foo.toml.example` at the repo root.
macro_rules! toml_example {
    ($name:literal) => {
        TrackedTemplate {
            local: $name,
            upstream: concat!($name, ".example"),
            kind: TemplateKind::Toml,
        }
    };
}

/// How a template's content is merged into the user's copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    /// Prose: append `## ` sections the local copy lacks.
    Markdown,
    /// Config: add missing keys, never touch existing values (#819).
    Toml,
}

/// A template tracked for upstream sync.
///
/// The local name and the upstream path differ for config examples: the repo
/// ships `usage_pricing.toml.example` at its root, while the user holds
/// `usage_pricing.toml` in their home. Carrying both explicitly is what lets
/// examples be tracked at all.
#[derive(Debug, Clone, Copy)]
pub struct TrackedTemplate {
    /// Filename inside `~/.opencrabs`.
    pub local: &'static str,
    /// Path relative to the repository root.
    pub upstream: &'static str,
    pub kind: TemplateKind,
}

/// Everything tracked for upstream sync.
///
/// The `.toml.example` entries are why #816 and #817 could not reach users:
/// the examples gained pricing for two models, nothing carried it into the
/// live `usage_pricing.toml`, and `/usage` reported $0.00 on real spend.
const TRACKED: &[TrackedTemplate] = &[
    // Brain files — prose, merged by section, shipped under the templates dir.
    //
    // SOUL.md, USER.md and MEMORY.md are deliberately ABSENT (#1119). They are
    // user-owned: personality/voice, this user's identity, and accumulated
    // private memory. Upstream has no authority over any of them. Merging
    // template sections in injected placeholder content into real user data,
    // and pushed operational sections (Operating Rules, Hard Rules) into
    // SOUL.md, a file whose own `**Owns:**` header declares it personality
    // only. Seeding still creates all three on profile creation; they are
    // simply never merged into afterwards.
    md!("AGENTS.md"),
    md!("TOOLS.md"),
    md!("CODE.md"),
    md!("SECURITY.md"),
    md!("BOOT.md"),
    md!("HEARTBEAT.md"),
    // Config examples — merged by key, additively, shipped at the repo root
    // with a `.example` suffix the local copy does not carry.
    toml_example!("usage_pricing.toml"),
    toml_example!("config.toml"),
    toml_example!("commands.toml"),
    toml_example!("tools.toml"),
    toml_example!("rtk_filters.toml"),
    // keys.toml is deliberately NOT tracked: it holds credentials and the
    // upstream example carries only placeholders, so merging it would add
    // dummy keys to a working install.
];

/// The tracked set, exposed for tests (#823). The failure mode for a wrong
/// path or kind is silence, so it is asserted rather than eyeballed.
pub const TRACKED_FOR_TEST: &[TrackedTemplate] = TRACKED;

/// Parsed state from `rsi/state.toml`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncState {
    pub last_synced_version: String,
    pub last_sync_date: String,
    pub file_dates: HashMap<String, String>,
    /// Upstream content fingerprint per tracked file (#820).
    ///
    /// The gate used to be version equality, which asks "has the app been
    /// upgraded" when the question is "is upstream different from mine". Those
    /// diverge whenever a template is fixed after a release, which is the
    /// normal case: #816 and #817 landed ~21 hours after the v0.3.75 bump and
    /// were therefore undeliverable until the next release.
    ///
    /// A fingerprint rather than a timestamp because timestamps lie in both
    /// directions: a file can be rewritten with identical content by a rebase
    /// or a reformat, and a mirror can serve a stale `Last-Modified`. Content
    /// equality is the only thing that answers "is there anything to do".
    pub file_hashes: HashMap<String, String>,
}

/// Fingerprint upstream content for the change gate (#820).
pub fn content_fingerprint(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

impl SyncState {
    /// Load state from `~/.opencrabs/rsi/state.toml`.
    pub fn load() -> Self {
        let path = Self::state_path();
        if !path.exists() {
            return Self::default();
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("RSI sync: failed to read state.toml: {e}");
                return Self::default();
            }
        };

        let mut state = Self::default();
        let mut in_files_section = false;
        let mut in_hashes_section = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed == "[files]" {
                in_files_section = true;
                in_hashes_section = false;
                continue;
            }
            if trimmed == "[hashes]" {
                in_hashes_section = true;
                in_files_section = false;
                continue;
            }
            if trimmed.starts_with('[') {
                in_files_section = false;
                in_hashes_section = false;
                continue;
            }

            if let Some((key, value)) = trimmed.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                if in_hashes_section {
                    state.file_hashes.insert(key.to_string(), value.to_string());
                } else if in_files_section {
                    state.file_dates.insert(key.to_string(), value.to_string());
                } else if key == "last_synced_version" {
                    state.last_synced_version = value.to_string();
                } else if key == "last_sync_date" {
                    state.last_sync_date = value.to_string();
                }
            }
        }

        state
    }

    /// Save state to `~/.opencrabs/rsi/state.toml`.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut content = format!(
            "last_synced_version = \"{}\"\nlast_sync_date = \"{}\"\n\n[files]\n",
            self.last_synced_version, self.last_sync_date
        );

        for (file, date) in &self.file_dates {
            content.push_str(&format!("{file} = \"{date}\"\n"));
        }

        content.push_str("\n[hashes]\n");
        for (file, hash) in &self.file_hashes {
            content.push_str(&format!("{file} = \"{hash}\"\n"));
        }

        std::fs::write(&path, content)
    }

    fn state_path() -> PathBuf {
        crate::config::opencrabs_home().join("rsi/state.toml")
    }
}

/// Result of a single file sync attempt.
#[derive(Debug, Clone, Default)]
pub struct FileSyncResult {
    pub filename: String,
    pub synced: bool,
    pub sections_added: usize,
    pub error: Option<String>,
    /// `Some(report)` when the sync bailed because the merged content
    /// would exceed `[brain.caps] <filename>` (or `default_cap`).
    /// `synced=false` in that case too, but `bailed_for_cap` distinguishes
    /// "cap reached, user must act" from "transient error, will retry".
    /// Issue #164 fix 2.
    pub bailed_for_cap: Option<CapBailReport>,
}

/// Collapse per-file sync failures into one line per distinct cause (#2169).
///
/// The old join produced one sentence per file, so a single DNS flap rendered
/// eleven near-identical sentences, each naming its file twice. The cause is
/// what varies the message, not the file, so group on it: a cause shared by
/// every failure becomes one sentence, and several causes become one clause
/// each, joined on a single line because the notification renders as one.
///
/// Returns `None` when nothing failed.
pub(crate) fn summarize_sync_failures(results: &[FileSyncResult]) -> Option<String> {
    let mut causes: Vec<&str> = Vec::new();
    let mut files_by_cause: Vec<Vec<&str>> = Vec::new();
    let mut failed = 0usize;

    for result in results {
        let Some(cause) = result.error.as_deref() else {
            continue;
        };
        failed += 1;
        match causes.iter().position(|seen| *seen == cause) {
            Some(idx) => files_by_cause[idx].push(result.filename.as_str()),
            None => {
                causes.push(cause);
                files_by_cause.push(vec![result.filename.as_str()]);
            }
        }
    }

    if failed == 0 {
        return None;
    }

    if failed == 1 {
        return Some(format!("{} failed: {}", files_by_cause[0][0], causes[0]));
    }

    if causes.len() == 1 {
        return Some(format!("all {failed} tracked files failed: {}", causes[0]));
    }

    let clauses: Vec<String> = causes
        .iter()
        .zip(files_by_cause.iter())
        .map(|(cause, files)| format!("{} ({}) failed: {cause}", files.len(), files.join(", ")))
        .collect();
    Some(clauses.join("; "))
}

/// Diagnostic surfaced when `sync_single_file` refuses to write because
/// the merged content would exceed the configured per-file line cap. The
/// user sees this via tracing + an entry appended to
/// `~/.opencrabs/rsi/improvements.md` so they can either raise the cap,
/// prune the file, or add the offending sections to the pruned sidecar.
#[derive(Debug, Clone, Default)]
pub struct CapBailReport {
    pub filename: String,
    pub local_lines: usize,
    pub upstream_lines: usize,
    pub merged_lines: usize,
    pub cap: usize,
    /// Up to 3 largest new sections (`## Header (N lines)`) that the
    /// sync would have added. Helps the user judge whether to raise the
    /// cap or prune those headers specifically.
    pub top_new_sections: Vec<String>,
}

/// Which of the two cap-bail states a report is in (#1583).
///
/// The distinction decides how the bail is reported. A `Transient` bail
/// can resolve itself (upstream changes, a `pruned.toml` entry, a cap
/// raise), so it keeps per-cycle reporting. A `Permanent` bail cannot:
/// the file is over the cap before any merge is considered, so every
/// cycle hits the same wall forever — and per-cycle reporting of a state
/// that cannot change is exactly the 126-duplicate-entries bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapBailState {
    /// `local_lines > cap`: the local file alone exceeds its cap. No
    /// merge size can ever fit, so sync can never unstick it — only
    /// raising the cap or an owner-approved cleanup pass can. Reported
    /// once per state fingerprint, not once per cycle.
    Permanent,
    /// `local <= cap < merged`: the file fits, but the pending merge
    /// would push it over. Resolvable by ordinary means (including
    /// pruning the offending upstream headers), so it keeps per-cycle
    /// reporting and can clear on its own.
    Transient,
}

/// Width, in lines, of the local-size bucket inside a permanent-bail
/// fingerprint. An append-only file drifts up a few lines a week through
/// no state change; the bucket absorbs that drift so a re-emit means the
/// file meaningfully grew or the cap moved, not that Tuesday happened.
const CAP_BAIL_BUCKET_LINES: usize = 25;

impl CapBailReport {
    /// Classify this bail (#1583). `local_lines > cap` is `Permanent`
    /// regardless of how many new sections triggered the check: even with
    /// zero new sections the file is already over budget, so the deadlock
    /// is a property of the file, not of this particular merge.
    pub fn state(&self) -> CapBailState {
        if self.local_lines > self.cap {
            CapBailState::Permanent
        } else {
            CapBailState::Transient
        }
    }

    /// Fingerprint of the Permanent state: filename + cap + bucketed
    /// local size, hashed (only equality ever matters, not the value).
    /// Two bails with the same fingerprint are the same deadlocked state
    /// re-observed; the second one is noise.
    pub fn permanent_fingerprint(&self) -> String {
        cap_bail_fingerprint(&self.filename, self.cap, self.local_lines)
    }
}

/// State fingerprint for a permanent cap-bail (#1583). Separated from the
/// method so the regression tests can vary the inputs directly.
pub fn cap_bail_fingerprint(filename: &str, cap: usize, local_lines: usize) -> String {
    content_fingerprint(&format!(
        "{filename}|{cap}|{}",
        local_lines / CAP_BAIL_BUCKET_LINES
    ))
}

/// Last-emitted permanent-bail fingerprint per file (#1583), persisted at
/// `~/.opencrabs/rsi/cap_bail_state.toml`.
///
/// This is the memory that makes dedup survive across cycles: the engine
/// runs `sync_templates` on every start and hourly after that, and each
/// run is a fresh in-process decision — without a sidecar, every process
/// would re-emit the first permanent bail it sees. Entries are written
/// only when a permanent bail is actually emitted, so a healthy install
/// never grows this file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapBailLedger {
    /// filename → fingerprint of the permanent state last reported.
    pub emitted_fingerprints: HashMap<String, String>,
}

impl CapBailLedger {
    /// Load from `~/.opencrabs/rsi/cap_bail_state.toml`.
    pub fn load() -> Self {
        let path = Self::ledger_path();
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(c) => Self::parse(&c),
            Err(e) => {
                tracing::warn!(
                    "RSI sync cap-bail ledger: failed to read {}: {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Parse the sidecar. `pub(crate)` so the regression tests under
    /// `src/tests/` can round-trip the format without touching the real
    /// `~/.opencrabs`.
    pub(crate) fn parse(content: &str) -> Self {
        let mut emitted_fingerprints = HashMap::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
                continue;
            }
            if let Some((file, fingerprint)) = trimmed.split_once('=') {
                emitted_fingerprints.insert(
                    file.trim().trim_matches('"').to_string(),
                    fingerprint.trim().trim_matches('"').to_string(),
                );
            }
        }
        Self {
            emitted_fingerprints,
        }
    }

    /// Render the sidecar bytes, keys sorted for deterministic output.
    pub(crate) fn render(&self) -> String {
        let mut out = String::from(
            "# Last-emitted permanent cap-bail fingerprint per file (#1583).\n\
             # Identical fingerprints are suppressed; a changed one re-emits.\n",
        );
        let mut files: Vec<&String> = self.emitted_fingerprints.keys().collect();
        files.sort();
        for file in files {
            out.push_str(&format!(
                "\"{file}\" = \"{}\"\n",
                self.emitted_fingerprints[file]
            ));
        }
        out
    }

    /// Persist to `~/.opencrabs/rsi/cap_bail_state.toml`.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::ledger_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, self.render())
    }

    fn ledger_path() -> PathBuf {
        crate::config::opencrabs_home().join("rsi/cap_bail_state.toml")
    }

    /// The dedup gate: should this permanent bail be reported?
    ///
    /// Returns `true` (and records the fingerprint) the first time a file
    /// enters a given state, and `false` for every identical
    /// re-observation. Taking the decision and the record in one call
    /// means the caller cannot report without remembering, or remember
    /// without reporting. Transient bails never touch this ledger — they
    /// keep per-cycle reporting because they can clear themselves.
    pub fn gate(&mut self, report: &CapBailReport) -> bool {
        let fingerprint = report.permanent_fingerprint();
        if self.emitted_fingerprints.get(&report.filename) == Some(&fingerprint) {
            return false;
        }
        self.emitted_fingerprints
            .insert(report.filename.clone(), fingerprint);
        true
    }
}

/// Whether an upgrade happened since the last sync.
///
/// No longer a gate (#820): it decides nothing about whether to fetch, because
/// a template fixed AFTER a release is invisible to it. Kept because the
/// version is still worth recording and logging. The real gate is per-file
/// content equality, applied in `sync_single_file`.
pub fn version_changed(state: &SyncState) -> bool {
    state.last_synced_version != crate::VERSION
}

/// Has upstream changed since the last time this file was merged (#820)?
///
/// `None` stored means never synced, which counts as changed so a first run
/// still merges. Identical means there is nothing to do: no merge, no backup,
/// no write, no log entry. That silence is the point — RSI already writes a
/// digest hourly, and a sync that reports "checked, nothing to do" every pass
/// buries the entries that mean something.
pub fn upstream_changed(state: &SyncState, local_name: &str, upstream_content: &str) -> bool {
    match state.file_hashes.get(local_name) {
        Some(seen) => seen != &content_fingerprint(upstream_content),
        None => true,
    }
}

/// Why a template fetch failed, and whether trying again could help (#2169).
///
/// The distinction is the whole point. A transport failure never reached a
/// server, so one retry absorbs a DNS flap. A status code IS an answer, so
/// retrying it only doubles the wait for the same no.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FetchError {
    /// Never got an answer: DNS, refused connection, TLS, or the timeout
    /// firing. Worth one retry.
    Transport { detail: String },
    /// The server answered, and the answer was no.
    Status { code: u16 },
    /// Headers arrived but the body did not finish. Worth one retry.
    Body { detail: String },
}

impl FetchError {
    /// Whether a second attempt could plausibly succeed.
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(self, FetchError::Transport { .. } | FetchError::Body { .. })
    }

    /// The line the user sees. It names what happened and what to do next,
    /// because "error sending request for url (...)" says neither.
    ///
    /// Deliberately carries no filename (#2169). The cause is shared by every
    /// file that hit it, so the caller groups by this text and names the
    /// affected files once. Embedding the filename here produced joins like
    /// "AGENTS.md: could not reach the template host for AGENTS.md".
    pub(crate) fn describe(&self) -> String {
        match self {
            FetchError::Transport { detail } => format!(
                "could not reach the template host ({detail}) after \
                 {TEMPLATE_MAX_ATTEMPTS} attempts; check the network or proxy, the next \
                 RSI cycle retries"
            ),
            FetchError::Status { code } => format!(
                "upstream answered HTTP {code}; the tracked path is stale, \
                 which needs a code fix rather than a retry"
            ),
            FetchError::Body { detail } => format!(
                "template body did not arrive ({detail}); the next RSI \
                 cycle retries"
            ),
        }
    }
}

/// Join the `source()` chain so the inner cause survives into the message.
///
/// `reqwest::Error`'s `Display` is only the outer wrapper ("error sending
/// request for url (...)"), while the reason (dns error, connection refused,
/// timed out) lives one or more levels down. Printing just the wrapper is how
/// a DNS failure read as a generic network error in the field.
pub(crate) fn cause_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = current {
        let text = e.to_string();
        // Drop exact repeats so a self-describing error does not print twice.
        if parts.last() != Some(&text) {
            parts.push(text);
        }
        current = e.source();
    }
    match parts.len() {
        0 => String::new(),
        1 => parts.pop().unwrap_or_default(),
        _ => parts.join(": "),
    }
}

/// Map a `reqwest` send failure onto the classification above.
pub(crate) fn classify_send_error(err: &reqwest::Error) -> FetchError {
    if let Some(status) = err.status() {
        return FetchError::Status {
            code: status.as_u16(),
        };
    }
    if err.is_body() {
        return FetchError::Body {
            detail: cause_chain(err),
        };
    }
    FetchError::Transport {
        detail: cause_chain(err),
    }
}

/// One fetch attempt, no retry.
///
/// Split from the retry loop on purpose: the loop is the policy under test,
/// and a test can only drive it if the attempt is a separate unit it can
/// replace with a fake.
async fn fetch_once(path: &str) -> Result<String, FetchError> {
    let url = format!("{TEMPLATE_BASE_URL}/{path}");
    let client = template_client().map_err(|e| FetchError::Transport { detail: e })?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| classify_send_error(&e))?;

    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Status {
            code: status.as_u16(),
        });
    }

    response.text().await.map_err(|e| FetchError::Body {
        detail: cause_chain(&e),
    })
}

/// Run `attempt` up to `TEMPLATE_MAX_ATTEMPTS` times, retrying only what
/// `is_retryable` allows.
///
/// The seam is the closure. Production closes over `fetch_once`; a test passes
/// a fake that fails on demand, so the retry policy is asserted without a
/// network round trip. Retrying a status code would only double the wait for
/// the same answer, which is why the guard reads `is_retryable` rather than
/// counting attempts alone.
///
/// The closure takes no argument on purpose. A `FnMut(&str) -> Fut` bound
/// cannot be satisfied by an `async fn`: the returned future borrows the
/// argument, so it is a different type for every lifetime, while `Fut` names
/// exactly one. Closing over the path instead keeps each call site's future a
/// single concrete type.
pub(crate) async fn retry_fetch<F, Fut>(path: &str, mut attempt: F) -> Result<String, FetchError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<String, FetchError>>,
{
    let attempts = TEMPLATE_MAX_ATTEMPTS.max(1);
    let mut tries = 0u8;
    loop {
        tries += 1;
        match attempt().await {
            Ok(body) => return Ok(body),
            Err(err) => {
                if !err.is_retryable() || tries >= attempts {
                    return Err(err);
                }
                tracing::debug!(
                    "RSI sync: {} attempt {}/{} failed, retrying: {}",
                    path,
                    tries,
                    attempts,
                    err.describe()
                );
            }
        }
    }
}

/// Fetch a single template. `path` is relative to the repository root.
///
/// Bounded twice over (#2169). The client caps the connect and the whole
/// request, so a blackholed host can no longer wedge the RSI task forever,
/// and a transport or body failure gets one more attempt so a single DNS
/// flap does not fail every tracked file in the pass.
pub(crate) async fn fetch_template(path: &str) -> Result<String, FetchError> {
    retry_fetch(path, || fetch_once(path)).await
}

/// Extract sections from upstream that don't exist in local content.
///
/// Strategy: append-only, never overwrite user customizations.
///
/// Two levels of diff:
/// 1. New top-level sections (## Header) that don't exist locally → append entire section
/// 2. New subsections (### Header) under existing top-level sections → append just the subsection
///
/// This ensures user's personalized content under any header is preserved,
/// while still catching new upstream additions at both heading levels.
///
/// Returns the new sections as a string ready to append.
pub fn extract_new_sections(local: &str, upstream: &str) -> String {
    let local_headers: std::collections::HashSet<String> =
        extract_section_headers(local).into_iter().collect();

    // Parse upstream into (header_level, header_line, content_lines) blocks
    let mut blocks: Vec<(usize, String, Vec<String>)> = Vec::new();
    let mut current_level = 0;
    let mut current_header = String::new();
    let mut current_content = Vec::new();

    for line in upstream.lines() {
        let level = if line.starts_with("## ") {
            2
        } else if line.starts_with("### ") {
            3
        } else {
            0
        };

        if level >= 2 {
            // Flush previous block
            if !current_header.is_empty() {
                blocks.push((
                    current_level,
                    current_header.clone(),
                    current_content.clone(),
                ));
            }
            current_level = level;
            current_header = line.to_string();
            current_content = vec![line.to_string()];
        } else if !current_header.is_empty() {
            current_content.push(line.to_string());
        }
    }
    // Flush last block
    if !current_header.is_empty() {
        blocks.push((current_level, current_header, current_content));
    }

    let mut new_sections = Vec::new();

    for (level, header, content) in &blocks {
        if *level == 2 {
            // Top-level section: if header doesn't exist locally, include entire section
            if !local_headers.contains(header) {
                new_sections.push(content.join("\n"));
            }
        } else if *level == 3 {
            // Subsection: if this ### header doesn't exist locally, include it
            // (even if its parent ## section exists locally)
            if !local_headers.contains(header) {
                new_sections.push(content.join("\n"));
            }
        }
    }

    if new_sections.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", new_sections.join("\n\n"))
    }
}

/// Extract all ## and ### heading lines from markdown.
pub(crate) fn extract_section_headers(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|line| line.starts_with("## ") || line.starts_with("### "))
        .map(|line| line.to_string())
        .collect()
}

/// Backup directory for RSI sync.
/// Merge an upstream config example into the user's live file (#819).
///
/// Additive only: keys the local file lacks are added, values it already has
/// are never touched, because those may be deliberate customisations. This is
/// what carries new model pricing (#816, #817) into a live install without
/// resetting rates the user set themselves.
fn sync_toml_file(
    local_path: &Path,
    filename: &str,
    local_content: &str,
    upstream_content: &str,
) -> FileSyncResult {
    let (merged, report) =
        match crate::brain::toml_merge::merge_additive(local_content, upstream_content) {
            Ok(v) => v,
            Err(e) => {
                // A malformed file on either side leaves the local one untouched.
                // Rewriting a working config from a broken template would be worse
                // than skipping the update.
                return FileSyncResult {
                    filename: filename.to_string(),
                    synced: false,
                    sections_added: 0,
                    error: Some(format!("{filename}: {e}")),
                    bailed_for_cap: None,
                };
            }
        };

    if report.is_empty() {
        tracing::debug!("RSI sync: {filename} has no new keys, skipping");
        return FileSyncResult {
            filename: filename.to_string(),
            synced: true,
            sections_added: 0,
            error: None,
            bailed_for_cap: None,
        };
    }

    // Back up before writing, matching the markdown path: a config the user
    // depends on must be recoverable if the merge turns out wrong.
    let backup = backups_dir().join(format!("{filename}.bak"));
    if let Err(e) = std::fs::write(&backup, local_content) {
        return FileSyncResult {
            filename: filename.to_string(),
            synced: false,
            sections_added: 0,
            error: Some(format!("{filename}: failed to back up before merge: {e}")),
            bailed_for_cap: None,
        };
    }

    if let Err(e) = std::fs::write(local_path, &merged) {
        return FileSyncResult {
            filename: filename.to_string(),
            synced: false,
            sections_added: 0,
            error: Some(format!("{filename}: failed to write merge: {e}")),
            bailed_for_cap: None,
        };
    }

    // Name what arrived rather than logging "updated": a config change the
    // user cannot see is a config change they cannot audit.
    tracing::info!(
        "RSI sync: {filename} gained {} key(s): {}",
        report.added.len(),
        report.added.join(", ")
    );
    log_toml_merge_to_improvements(filename, &report);

    FileSyncResult {
        filename: filename.to_string(),
        synced: true,
        sections_added: report.added.len(),
        error: None,
        bailed_for_cap: None,
    }
}

/// Record a config merge in `rsi/improvements.md`, listing the keys added.
fn log_toml_merge_to_improvements(filename: &str, report: &crate::brain::toml_merge::MergeReport) {
    let path = crate::config::opencrabs_home().join("rsi/improvements.md");
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("RSI sync: failed to create rsi dir for improvements log: {e}");
        return;
    }
    let entry = format!(
        "\n## {} — {filename} config sync\n\nAdded {} key(s) from the upstream example:\n{}\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
        report.added.len(),
        report
            .added
            .iter()
            .map(|k| format!("- `{k}`"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(entry.as_bytes()) {
                tracing::warn!("RSI sync: failed to append config merge to improvements: {e}");
            }
        }
        Err(e) => tracing::warn!("RSI sync: failed to open improvements log: {e}"),
    }
}

fn backups_dir() -> PathBuf {
    crate::config::opencrabs_home().join("rsi/backups")
}

/// Ensure backups directory exists.
fn ensure_backups_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(backups_dir())
}

/// Run the full template sync.
///
/// Returns a list of per-file results.
pub async fn sync_templates() -> Vec<FileSyncResult> {
    let home = crate::config::opencrabs_home();
    let mut state = SyncState::load();

    // No version gate (#820). Whether the app was upgraded says nothing about
    // whether a template changed: #816 and #817 landed ~21 hours AFTER the
    // v0.3.75 bump, so a version-equality check kept them undeliverable
    // indefinitely. Each file now decides for itself by content, and a file
    // whose upstream is unchanged costs one comparison and writes nothing.
    if version_changed(&state) {
        tracing::info!(
            "RSI sync: version changed from {} to {}.",
            state.last_synced_version,
            crate::VERSION
        );
    }

    // Ensure directories
    if let Err(e) = ensure_backups_dir() {
        tracing::warn!("RSI sync: failed to create backups dir: {e}");
        return vec![FileSyncResult {
            filename: "_setup".to_string(),
            synced: false,
            sections_added: 0,
            error: Some(format!("Failed to create backups dir: {e}")),
            bailed_for_cap: None,
        }];
    }

    let mut results = Vec::new();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Recovery seed for profiles created before the
    // `seed_brain_templates` fix landed: if the home directory is
    // missing the core brain files entirely (counted as "more than
    // half of the templates are missing"), call the same template
    // seeder `create_profile` uses. This rescues old `opencrabs
    // profile create <name>` installs whose brain dir was left blank.
    // Existing files are NOT overwritten by the seeder, so a healthy
    // install is unaffected.
    seed_missing_templates_if_blank(&home);

    for tracked in TRACKED {
        let local_path = home.join(tracked.local);

        // Skip files that don't exist locally (don't create new brain files)
        if !local_path.exists() {
            tracing::debug!(
                "RSI sync: {} does not exist locally, skipping",
                tracked.local
            );
            continue;
        }

        // Fetched once, here, so the fingerprint recorded is exactly what was
        // considered and a merge and its record cannot disagree (#820). The
        // same result is handed to `sync_single_file`, which used to fetch the
        // identical URL a second time per pass (#2169).
        let upstream = fetch_template(tracked.upstream).await;

        let result = sync_single_file(&local_path, tracked, &upstream, &now);
        if result.synced {
            state
                .file_dates
                .insert(tracked.local.to_string(), now.clone());
            // Record what upstream looked like, so an unchanged file does
            // nothing next pass. Only on success: a failed sync must retry
            // rather than mark itself as seen.
            if let Ok(ref content) = upstream {
                state
                    .file_hashes
                    .insert(tracked.local.to_string(), content_fingerprint(content));
            }
        }
        results.push(result);
    }

    // Update state
    state.last_synced_version = crate::VERSION.to_string();
    state.last_sync_date = now;
    if let Err(e) = state.save() {
        tracing::warn!("RSI sync: failed to save state.toml: {e}");
    }

    results
}

/// Recovery seed: if `home` is missing more than half of the core
/// brain-file templates, run `seed_brain_templates` to restore them.
/// Used by `sync_templates` to rescue profiles created before the
/// `create_profile` template-seeding fix.
///
/// The threshold (more than half missing) prevents a healthy install
/// from triggering re-seeding when only one or two non-template files
/// happen to be absent (e.g. user intentionally deleted USER.md). A
/// brand-new empty profile dir, by contrast, will have all 8 missing
/// and definitely needs seeding.
fn seed_missing_templates_if_blank(home: &std::path::Path) {
    const CORE: &[&str] = &[
        "SOUL.md",
        "USER.md",
        "AGENTS.md",
        "TOOLS.md",
        "MEMORY.md",
        "CODE.md",
        "SECURITY.md",
    ];
    let missing = CORE.iter().filter(|f| !home.join(f).exists()).count();
    if missing * 2 <= CORE.len() {
        return;
    }
    tracing::info!(
        "RSI sync: home '{}' is missing {}/{} core brain files — re-seeding from templates",
        home.display(),
        missing,
        CORE.len(),
    );
    crate::config::profile::seed_brain_templates(home);
}

/// Test re-export of `top_new_sections_by_size` so the regression tests
/// under `src/tests/` can exercise the ranking without going through the
/// `sync_single_file` path (which needs disk + config).
pub fn top_new_sections_by_size_for_test(new_sections: &str, n: usize) -> Vec<String> {
    top_new_sections_by_size(new_sections, n)
}

/// Extract the top-N largest new sections (by line count) from the appended
/// content. Returns formatted strings like `"## Section Name (42 lines)"`.
/// Used by the cap-bail report so the user knows which headers dominate.
fn top_new_sections_by_size(new_sections: &str, n: usize) -> Vec<String> {
    let mut by_header: Vec<(String, usize)> = Vec::new();
    let mut current_header: Option<String> = None;
    let mut current_count: usize = 0;
    for line in new_sections.lines() {
        if line.starts_with("## ") {
            if let Some(h) = current_header.take() {
                by_header.push((h, current_count));
            }
            current_header = Some(line.to_string());
            current_count = 1;
        } else if current_header.is_some() {
            current_count += 1;
        }
    }
    if let Some(h) = current_header {
        by_header.push((h, current_count));
    }
    by_header.sort_by_key(|b| std::cmp::Reverse(b.1));
    by_header
        .into_iter()
        .take(n)
        .map(|(h, c)| format!("{h} ({c} lines)"))
        .collect()
}

/// Build the improvements.md entry for a cap bail (#1583 wording split).
///
/// `pub(crate)` so the regression tests under `src/tests/` can pin the
/// Permanent wording — which must name the two real resolution levers and
/// warn that pruning upstream headers will not help — without disk I/O.
pub(crate) fn cap_bail_entry(report: &CapBailReport) -> String {
    let top_list = if report.top_new_sections.is_empty() {
        "(none detected)".to_string()
    } else {
        report
            .top_new_sections
            .iter()
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match report.state() {
        CapBailState::Transient => format!(
            "\n## [Bailed] Sync cap exceeded for {filename}\n\n\
             **Date:** {date}\n\
             **Cap:** {cap} lines\n\
             **Local file size:** {local} lines\n\
             **Upstream template size:** {upstream} lines\n\
             **Merged would be:** {merged} lines\n\
             **Top new sections that would have been added:**\n{top}\n\n\
             To resolve: raise `[brain.caps].{filename}` in config.toml, prune \
             the file, or add the offending headers to your `rsi/pruned.toml`.\n",
            filename = report.filename,
            date = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
            cap = report.cap,
            local = report.local_lines,
            upstream = report.upstream_lines,
            merged = report.merged_lines,
            top = top_list,
        ),
        CapBailState::Permanent => format!(
            "\n## [Bailed] Sync permanently capped for {filename}\n\n\
             **Date:** {date}\n\
             **Cap:** {cap} lines\n\
             **Local file size:** {local} lines — over the cap on its own\n\
             **Upstream template size:** {upstream} lines\n\
             **Merged would be:** {merged} lines\n\
             **Top new sections withheld:**\n{top}\n\n\
             {filename} is over the cap on its own: no merge can fit under \
             {cap} lines, so template sync cannot fix this and upstream \
             sections stay withheld until the cap or the file changes.\n\n\
             To resolve (either):\n\
             - raise `[brain.caps].{filename}` in config.toml, or\n\
             - run an owner-approved cleanup pass (`write_opencrabs_file` with \
             `cleanup_intent`, or a `/compact`-style prune) to bring the file \
             back under the cap.\n\n\
             Pruning upstream headers in `rsi/pruned.toml` will NOT unstick \
             this — the local file alone is over the cap.\n\n\
             (Reported once per state change — file size bucket + cap — not \
             on every sync cycle.)\n",
            filename = report.filename,
            date = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
            cap = report.cap,
            local = report.local_lines,
            upstream = report.upstream_lines,
            merged = report.merged_lines,
            top = top_list,
        ),
    }
}

/// Append a cap-bail diagnostic to `~/.opencrabs/rsi/improvements.md`
/// so the user sees it next session without having to scrape stdout.
fn log_cap_bail_to_improvements(report: &CapBailReport) {
    let home = crate::config::opencrabs_home();
    let improvements_path = home.join("rsi/improvements.md");
    if let Some(parent) = improvements_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("RSI sync cap-bail: failed to create rsi dir for improvements log: {e}");
        return;
    }
    let entry = cap_bail_entry(report);
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&improvements_path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(entry.as_bytes()) {
                tracing::warn!("RSI sync cap-bail: failed to append entry to improvements.md: {e}");
            }
        }
        Err(e) => {
            tracing::warn!("RSI sync cap-bail: failed to open improvements.md for append: {e}");
        }
    }
}

/// Sync a single brain file.
fn sync_single_file(
    local_path: &Path,
    tracked: &TrackedTemplate,
    upstream: &Result<String, FetchError>,
    _timestamp: &str,
) -> FileSyncResult {
    let filename = tracked.local;
    // 1. Read local content
    let local_content = match std::fs::read_to_string(local_path) {
        Ok(c) => c,
        Err(e) => {
            return FileSyncResult {
                filename: filename.to_string(),
                synced: false,
                sections_added: 0,
                error: Some(format!("Failed to read local {filename}: {e}")),
                bailed_for_cap: None,
            };
        }
    };

    // 2. Upstream body, already fetched once by the caller (#2169). A transport
    // failure is reported here with its real cause instead of being flattened
    // into `None` and lost.
    let upstream_content = match upstream {
        Ok(c) => c,
        Err(e) => {
            return FileSyncResult {
                filename: filename.to_string(),
                synced: false,
                sections_added: 0,
                error: Some(e.describe()),
                bailed_for_cap: None,
            };
        }
    };

    // 3. Nothing to do if upstream is byte-identical to what was last merged
    // (#820). Checked before any merge, backup or write, so an unchanged file
    // costs one comparison and produces no side effects at all — no log line
    // either, since RSI already writes a digest hourly and "checked, nothing
    // to do" on every pass buries the entries that mean something.
    {
        let state = SyncState::load();
        if !upstream_changed(&state, filename, upstream_content) {
            return FileSyncResult {
                filename: filename.to_string(),
                synced: true,
                sections_added: 0,
                error: None,
                bailed_for_cap: None,
            };
        }
    }

    // 4. TOML takes a different route entirely (#819). Sections, pruning and
    // line caps are all prose concepts; a config file merges by key, and
    // appending a `## ` block to it would produce a duplicate table that stops
    // the file parsing.
    if tracked.kind == TemplateKind::Toml {
        return sync_toml_file(local_path, filename, &local_content, upstream_content);
    }

    // 3. Extract new sections
    let new_sections = extract_new_sections(&local_content, upstream_content);
    if new_sections.trim().is_empty() {
        tracing::info!("RSI sync: {filename} has no new sections, skipping");
        return FileSyncResult {
            filename: filename.to_string(),
            synced: true,
            sections_added: 0,
            error: None,
            bailed_for_cap: None,
        };
    }

    // 3b. Filter out sections the user has previously pruned
    let pruned_state = crate::brain::rsi_pruned::PrunedState::load();
    let new_sections =
        crate::brain::rsi_pruned::filter_pruned_sections(&new_sections, &pruned_state, filename);
    if new_sections.trim().is_empty() {
        tracing::info!("RSI sync: {filename} — all new sections were pruned by user, skipping");
        return FileSyncResult {
            filename: filename.to_string(),
            synced: true,
            sections_added: 0,
            error: None,
            bailed_for_cap: None,
        };
    }

    let sections_count = new_sections
        .lines()
        .filter(|l| l.starts_with("## "))
        .count();

    // 3c. Per-file line cap (issue #164 fix 2). Compute the merged line
    // count and BAIL if it would exceed the configured cap. The cap is
    // read from `[brain.caps] <filename>` with `[brain] default_cap` as
    // the fallback (500 by default). Bailing means no write, no append
    // to improvements.md beyond the warning entry below, and the caller
    // sees `bailed_for_cap = Some(...)` so Mission Control can surface
    // the situation distinctly from a transient I/O failure.
    let brain_cfg = crate::config::Config::current().brain.clone();
    let cap = brain_cfg.cap_for(filename);
    let merged_line_count = local_content.lines().count() + new_sections.lines().count();
    if merged_line_count > cap {
        let report = CapBailReport {
            filename: filename.to_string(),
            local_lines: local_content.lines().count(),
            upstream_lines: upstream_content.lines().count(),
            merged_lines: merged_line_count,
            cap,
            top_new_sections: top_new_sections_by_size(&new_sections, 3),
        };
        // #1583: a file already over the cap on its own is deadlocked —
        // no merge can pass while local > cap, so reporting that state
        // every cycle is pure noise (126 identical entries on this
        // install). Transient bails keep per-cycle reporting because
        // they can clear themselves.
        if report.state() == CapBailState::Permanent {
            let mut ledger = CapBailLedger::load();
            if ledger.gate(&report) {
                tracing::warn!(
                    "RSI sync: {filename} BAILED — local file is {local} lines, over \
                     the {cap}-line cap on its own. Sync cannot help; raise \
                     [brain.caps].{filename} or run an owner-approved cleanup pass. \
                     Identical bails are suppressed until the state changes.",
                    local = report.local_lines,
                    cap = report.cap,
                );
                log_cap_bail_to_improvements(&report);
                if let Err(e) = ledger.save() {
                    tracing::warn!(
                        "RSI sync cap-bail: failed to persist dedup ledger: {e} \
                         (the permanent bail will re-emit next cycle)"
                    );
                }
            } else {
                tracing::debug!(
                    "RSI sync: {filename} still permanently over the {}-line cap \
                     (state fingerprint unchanged) — duplicate bail suppressed",
                    report.cap
                );
            }
        } else {
            tracing::warn!(
                "RSI sync: {filename} BAILED — merged would be {merged} lines, cap is {cap}. \
                 Top new sections: {top:?}. Raise [brain.caps].{filename} or prune sections.",
                merged = report.merged_lines,
                cap = report.cap,
                top = report.top_new_sections,
            );
            log_cap_bail_to_improvements(&report);
        }
        return FileSyncResult {
            filename: filename.to_string(),
            synced: false,
            sections_added: 0,
            error: None,
            bailed_for_cap: Some(report),
        };
    }

    // 4. Backup before writing
    match brain_file_safety::backup_before_write(local_path) {
        Ok(Some(backup_path)) => {
            tracing::info!(
                "RSI sync: backed up {filename} to {}",
                backup_path.display()
            );
        }
        Ok(None) => {
            tracing::debug!("RSI sync: {filename} has no existing backup (file is new)");
        }
        Err(e) => {
            tracing::warn!("RSI sync: failed to backup {filename}: {e}");
        }
    }

    // 5. Append new sections
    let updated = format!("{}{}", local_content, new_sections);

    // Sanity check: file must not be empty
    if updated.trim().is_empty() {
        return FileSyncResult {
            filename: filename.to_string(),
            synced: false,
            sections_added: 0,
            error: Some("Sanity check failed: merged content is empty".to_string()),
            bailed_for_cap: None,
        };
    }

    if let Err(e) = std::fs::write(local_path, &updated) {
        return FileSyncResult {
            filename: filename.to_string(),
            synced: false,
            sections_added: 0,
            error: Some(format!("Failed to write {filename}: {e}")),
            bailed_for_cap: None,
        };
    }

    // 6. Log to improvements.md
    let home = crate::config::opencrabs_home();
    let improvements_path = home.join("rsi/improvements.md");
    let entry = format!(
        "\n## [Synced] Upstream template sync for {filename}\n\n\
         **Date:** {}\n\
         **Version:** {}\n\
         **Sections added:** {sections_count}\n\
         **Status:** Applied (upstream sync)\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
        crate::VERSION,
    );
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&improvements_path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(entry.as_bytes()) {
                tracing::warn!(
                    "RSI sync: failed to append synced-entry for {filename} to improvements.md: {e}"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                "RSI sync: failed to open improvements.md for synced-entry append on {filename}: {e}"
            );
        }
    }

    tracing::info!(
        "RSI sync: synced {filename} (+{sections_count} sections from upstream v{})",
        crate::VERSION
    );

    FileSyncResult {
        filename: filename.to_string(),
        synced: true,
        sections_added: sections_count,
        error: None,
        bailed_for_cap: None,
    }
}
