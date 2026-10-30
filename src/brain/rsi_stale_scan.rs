//! Deterministic stale-claim scanning for brain files (#1240).
//!
//! Part of the RFC in issue #1240: the RSI cycle appends guidance forever but
//! nothing ever marks an existing rule as superseded when the world it
//! describes (binaries, providers, config keys, paths) changes. This module
//! adds the deterministic scan pass:
//!
//! 1. classify every line of every brain file: `Prescription` (an imperative
//!    rule that claims something about world state), `HistoricalExempt`
//!    (dated incident notes, violation ledgers, post-mortems — exempt by
//!    design; serializes as `historical_exempt`), or `Neutral`.
//! 2. extract claim anchors from prescriptions: file paths, `[config.keys]`,
//!    provider names, and command-like binaries.
//! 3. verify each anchor with ZERO model or network calls (`PATH` search,
//!    membership against the compiled config schema — the serde structs in
//!    `src/config/types.rs`, never the `*.example` files, per RFC design
//!    decision 3 — and `fs::metadata`). Anything that cannot be decided
//!    without guessing returns [`Verdict::Unverifiable`], never a false
//!    stale flag.
//! 4. split stale prescriptions into amendment actions (RFC design decision
//!    2, append-only reality respected): `reword_via_update` — the cycle
//!    agent may sharpen the line in place via `self_improve action='update'`,
//!    which rewords but never drops content — versus `surface_to_user` —
//!    proposed removals (e.g. a rule whose path vanished) queue for owner
//!    sign-off and are NEVER executed autonomously. The variant set is
//!    closed: there is no delete action, and the serde slug pin plus the
//!    `decide_action` matrix test in `src/tests/rsi_stale_scan_test.rs`
//!    force any widening to be a conscious, reviewed change.
//!
//! The scan is READ-ONLY. Findings are surfaced to the RSI cycle agent as
//! input for `self_improve action='update'` or queued for owner sign-off;
//! nothing here deletes or rewrites anything. Only anchor-verified staleness
//! flags (missing binary / unconfigured provider / unknown config key /
//! vanished path) — never "this rule seems old".
//!
//! Tests live in `src/tests/rsi_stale_scan_test.rs` (repo rule: no inline
//! `#[cfg(test)]` blocks).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::config::{Config, EmbeddingConfig, FallbackProviderConfig, ProviderConfig};
use crate::utils::providers::{configured_providers, normalize_provider_name};

// ---------------------------------------------------------------- verdicts

/// Outcome of verifying one anchor against the world. Every verifier in this
/// module returns this type — never a bool — so "could not decide" stays
/// distinguishable from "verified present" and "verified absent" all the way
/// into the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Anchor verified present in the world.
    Ok,
    /// Anchor references world state that no longer exists.
    Stale,
    /// Not decidable without guessing — skipped, never flagged.
    Unverifiable,
}

/// What kind of world-state claim an anchor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKind {
    Binary,
    ProviderName,
    ConfigKey,
    FilePath,
}

/// All anchor kinds, for matrix-style tests over [`decide_action`].
pub const ALL_ANCHOR_KINDS: [AnchorKind; 4] = [
    AnchorKind::Binary,
    AnchorKind::ProviderName,
    AnchorKind::ConfigKey,
    AnchorKind::FilePath,
];

/// Line classification (RFC design decision 1: dated incident notes and
/// ledgers are history — skip; imperative present-tense rules are
/// prescriptions — verify).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineClass {
    /// Imperative present-tense rule — verify its anchors.
    Prescription,
    /// Dated note / ledger / post-mortem — history, exempt from scanning.
    /// Serializes as `historical_exempt`: brain files deliberately mention
    /// dead tools inside dated lessons, and the audit trail must never flag
    /// itself.
    HistoricalExempt,
    /// Neither — prose, headings, blank lines.
    Neutral,
}

/// What should happen about a finding (RFC design decision 2: append-only
/// reality respected). The variant set is deliberately closed — there is NO
/// delete/removal action; `FindingAction::as_str` is an exhaustive match
/// WITHOUT a wildcard arm, so adding a variant (e.g. a hypothetical
/// `Delete`) breaks compilation of this module, and the slug pin in
/// `src/tests/rsi_stale_scan_test.rs` fails if one ever appears on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingAction {
    /// Feed to the cycle agent as `self_improve action='update'` input: it
    /// may sharpen the line in place. `update` rewords — it never drops the
    /// line, so append-only protected brains stay append-only.
    RewordViaUpdate,
    /// Queue for explicit owner review (e.g. a path the owner may have moved
    /// deliberately — the natural fix is a *removal*). Autonomous correction
    /// must not touch it; proposed removals never execute autonomously.
    SurfaceToUser,
    /// Nothing — anchor verified ok or unverifiable.
    None,
}

impl FindingAction {
    /// Stable slug for the ledger / logs (`reword_via_update`,
    /// `surface_to_user`, `none`). Doubles as the never-delete compile
    /// guard: the match below is exhaustive with no wildcard arm, so a new
    /// variant cannot be added without consciously updating this — and the
    /// pin test — first.
    // Only referenced from tests so far, but kept in every build (not
    // `#[cfg(test)]`) so `cargo build` — not just `cargo test` — fails on a
    // new variant.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn as_str(&self) -> &'static str {
        match self {
            FindingAction::RewordViaUpdate => "reword_via_update",
            FindingAction::SurfaceToUser => "surface_to_user",
            FindingAction::None => "none",
        }
    }
}

/// One flagged (or cleared) claim. All anchors are recorded — including
/// `Ok` and `Unverifiable` — because the RFC ledger
/// (`rsi/stale_scan.json`) records `{rule, anchor, verdict, last_verified}`
/// for dedup across cycles, and a re-verified-Ok anchor is exactly the
/// signal that stops re-flagging.
#[derive(Debug, Clone, Serialize)]
pub struct StaleFinding {
    pub file: String,
    pub line_no: usize,
    pub line: String,
    pub anchor: String,
    pub anchor_kind: AnchorKind,
    pub verdict: Verdict,
    pub action: FindingAction,
    pub evidence: String,
}

impl StaleFinding {
    /// Stable ledger key for dedup across cycles (RFC step 6).
    pub fn unique_key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.file,
            self.line_no,
            self.anchor_kind_str(),
            self.anchor
        )
    }

    fn anchor_kind_str(&self) -> &'static str {
        match self.anchor_kind {
            AnchorKind::Binary => "binary",
            AnchorKind::ProviderName => "provider",
            AnchorKind::ConfigKey => "config_key",
            AnchorKind::FilePath => "file_path",
        }
    }
}

// ---------------------------------------------------------------- classification

const MONTHS: &[&str] = &[
    "jan",
    "feb",
    "mar",
    "apr",
    "may",
    "jun",
    "jul",
    "aug",
    "sep",
    "oct",
    "nov",
    "dec",
    "january",
    "february",
    "march",
    "april",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Words whose presence marks a line as historical record rather than live
/// guidance (RFC design decision 1: ledgers and dated lessons are exempt).
const HISTORICAL_HINTS: &[&str] = &[
    "violation",
    "ledger",
    "observed",
    "incident",
    "post-mortem",
    "postmortem",
    "reverted",
    "lesson learned",
    "history",
    "archived",
    "superseded",
    "deprecation log",
];

/// First-word imperative starters that mark a bullet as a prescription.
const IMPERATIVE_STARTERS: &[&str] = &[
    "never", "always", "must", "do", "don't", "dont", "use", "avoid", "prefer", "run", "check",
    "skip", "stop", "set", "enable", "disable", "keep", "require", "verify", "ensure", "call",
    "pass", "write", "read", "stage", "commit", "reply", "respond", "load", "before", "when", "if",
    "for", "treat", "route", "match", "send", "fetch", "grep", "clone",
];

/// Shell builtins that are never present on `PATH` but always exist. A rule
/// saying "use `cd`" is not stale just because `which cd` finds nothing.
const SHELL_BUILTINS: &[&str] = &[
    ":", ".", "[", "alias", "bg", "bind", "break", "builtin", "caller", "cd", "command", "compgen",
    "complete", "continue", "declare", "dirs", "disown", "echo", "enable", "eval", "exec", "exit",
    "export", "false", "fc", "fg", "getopts", "hash", "help", "history", "jobs", "kill", "let",
    "local", "logout", "popd", "printf", "pushd", "pwd", "read", "return", "select", "set",
    "shift", "shopt", "source", "suspend", "test", "times", "trap", "true", "type", "typeset",
    "ulimit", "umask", "unalias", "unset", "wait",
];

/// ASCII word-boundary substring test: `word` must be delimited by
/// non-alphanumeric bytes (or string edges). Byte-level on purpose —
/// comparing against ASCII needles is UTF-8 safe since we never slice.
fn contains_ascii_word(low: &str, word: &[u8]) -> bool {
    let b = low.as_bytes();
    let n = word.len();
    if n == 0 || b.len() < n {
        return false;
    }
    for i in 0..=b.len() - n {
        if &b[i..i + n] != word {
            continue;
        }
        let before_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let after_ok = i + n == b.len() || !b[i + n].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

pub(crate) fn has_date_marker(line: &str) -> bool {
    let low = line.to_ascii_lowercase();
    // Word-boundary aware: plain substring would fire on "mar"ker, "aug"ment,
    // "dec"imal, "jun"ior — each falsely exempting a real prescription.
    if MONTHS
        .iter()
        .any(|m| contains_ascii_word(&low, m.as_bytes()))
    {
        return true;
    }
    // Bare 4-digit year (19xx / 20xx) as a standalone numeric token, e.g.
    // "shipped in 2026", "2026-08-22". Digit boundaries on both sides keep
    // session-hex ids like "fd72101f" from counting as years.
    let b = low.as_bytes();
    for i in 0..b.len().saturating_sub(3) {
        if b[i..i + 4].iter().all(u8::is_ascii_digit)
            && (b[i] == b'1' || b[i] == b'2')
            && (i == 0 || !b[i - 1].is_ascii_digit())
            && (i + 4 >= b.len() || !b[i + 4].is_ascii_digit())
        {
            return true;
        }
    }
    false
}

/// Classify one raw brain-file line (RFC design decision 1).
///
/// Precedence order:
///
/// 1. any dated-incident / ledger marker → [`LineClass::HistoricalExempt`].
///    This wins even when the line is ALSO imperative ("never use `cmd-wrap`
///    (removed Aug 10)") — history is history, and the audit trail must
///    never flag itself;
/// 2. else imperative mood (a bullet or sentence starting with an imperative
///    verb, no dated refs) → [`LineClass::Prescription`] — its anchors get
///    verified;
/// 3. else [`LineClass::Neutral`] — prose, headings, blank lines.
///
/// Misses here are conservative in the safe direction: a prescription
/// wrongly classified exempt is a stale rule left undetected (noise stays
/// flat), while the reverse would flag the audit trail itself — the failure
/// mode the RFC explicitly forbids.
pub fn classify_line(raw: &str) -> LineClass {
    let t = raw.trim();
    let low = t.to_ascii_lowercase();
    if HISTORICAL_HINTS.iter().any(|h| low.contains(h)) || has_date_marker(t) {
        return LineClass::HistoricalExempt;
    }
    let body = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .unwrap_or(t);
    let first_word = body
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '\'')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if IMPERATIVE_STARTERS.contains(&first_word.as_str()) {
        LineClass::Prescription
    } else {
        LineClass::Neutral
    }
}

// ---------------------------------------------------------------- amendment mechanics

/// Decide the amendment action for one verified anchor (RFC design decision
/// 2). Pure, so the boundary between the two amendment classes is
/// unit-test-visible:
///
/// - `Ok` / `Unverifiable` → [`FindingAction::None`] — nothing to amend.
/// - `Stale` binary / config key / provider name →
///   [`FindingAction::RewordViaUpdate`]: the rule *wording* is wrong about
///   the world but the rule itself stays; the cycle agent sharpens it in
///   place via `self_improve action='update'`, which respects append-only
///   protected brains.
/// - `Stale` file path → [`FindingAction::SurfaceToUser`]: a vanished path
///   usually means the rule's subject is gone, so the proposed fix is a
///   *removal* — and proposed removals are NEVER executed autonomously; they
///   queue for explicit owner sign-off.
pub fn decide_action(kind: AnchorKind, verdict: Verdict) -> FindingAction {
    match verdict {
        Verdict::Ok | Verdict::Unverifiable => FindingAction::None,
        Verdict::Stale => match kind {
            AnchorKind::FilePath => FindingAction::SurfaceToUser,
            AnchorKind::Binary | AnchorKind::ProviderName | AnchorKind::ConfigKey => {
                FindingAction::RewordViaUpdate
            }
        },
    }
}

// ---------------------------------------------------------------- extraction

/// Backtick-delimited spans on a line, preserving order.
pub fn backtick_spans(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        match after.find('`') {
            Some(close) => {
                out.push(after[..close].to_string());
                rest = &after[close + 1..];
            }
            None => break,
        }
    }
    out
}

/// Does the line carry command-shaped context around `span`? Guards against
/// PATH-checking function names like `scan_after_brain_write` that merely
/// sit in backticks.
fn command_like(span: &str, line: &str) -> bool {
    let low = line.to_ascii_lowercase();
    if low.contains("$ ") {
        return true;
    }
    // span adjacent to a CLI flag
    if low.contains("--") && !span.starts_with("--") && line.contains(span) {
        return true;
    }
    let clean_span = span.trim_start_matches("$ ");
    // "invoke "/"call " added (#1240 review pin): real brain rules phrase
    // commands this way ("- always invoke `cmd-wrap` before ..."); without
    // them such prescriptions silently never verify. Trailing space keeps
    // past-tense prose ("was invoked", "is called by") unmatched.
    for cue in [
        "run ", "via ", "using ", "execute ", "invoke ", "call ", "> ",
    ] {
        let mut from = 0usize;
        while let Some(rel) = low[from..].find(cue) {
            let pos = from + rel;
            from = pos + cue.len();
            // cue must start a word ("rerun" must not match "run")
            if pos > 0 && low.as_bytes()[pos - 1].is_ascii_alphanumeric() {
                continue;
            }
            // the span may sit directly after the cue or in backticks after it
            let tail = line[pos + cue.len()..].trim_start().trim_start_matches('`');
            if tail.starts_with(clean_span) {
                return true;
            }
        }
    }
    false
}

/// Classify a backticked span into an anchor kind, or `None` when the span
/// is deliberately NOT treated as world state (slash commands / skills,
/// react directives, snake_case function identifiers outside command
/// context, bare prose fragments).
pub fn anchor_kind(span: &str, line: &str) -> Option<AnchorKind> {
    let s = span.trim();
    if s.is_empty() {
        return None;
    }
    // Reaction directives / channel markers.
    if s.starts_with("<<") || (s.starts_with('<') && s.ends_with(">>")) {
        return None;
    }
    if s.starts_with('[') && s.ends_with(']') {
        return Some(AnchorKind::ConfigKey);
    }
    // Path-shaped BEFORE the slash-command guard: an absolute path
    // (`/no/such/file.md`) also starts with '/'. A slash command's only
    // slash is the leading one (`/help`, `/models`), so a span whose body
    // still contains '/' or a file extension is a path, not a command.
    let body = s.strip_prefix('/').unwrap_or(s);
    let looks_path = body.contains('/')
        || ["*.md", "*.rs", "*.toml", "*.json", "*.yaml", "*.yml"]
            .iter()
            .any(|ext| body.ends_with(&ext[1..]));
    if looks_path {
        return Some(AnchorKind::FilePath);
    }
    // Slash commands and skill refs are OpenCrabs-internal, not binaries.
    if s.starts_with('/') {
        return None;
    }
    if command_like(s, line) {
        return Some(AnchorKind::Binary);
    }
    None
}

/// Extract provider names mentioned as `provider[s] [of ]NAME` on a line.
/// Only exact-phrase mentions count; bare words never become provider
/// anchors. Sorted + deduped.
pub fn provider_mentions(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let low = line.to_ascii_lowercase();
    for cue in ["provider ", "providers "] {
        let mut from = 0usize;
        while let Some(rel) = low[from..].find(cue) {
            let start = from + rel + cue.len();
            // Skip an opening backtick or quote right after the cue so
            // "provider `anthropic`" yields `anthropic`, not an empty name.
            let tail = &low[start..];
            let tail = tail.trim_start().trim_start_matches(['`', '"', '\'']);
            let name: String = tail
                .chars()
                .take_while(|c| {
                    c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.' || *c == ':'
                })
                .collect();
            let trimmed = name.trim_end_matches(['\'', '"', '`', ',']).to_string();
            if !trimmed.is_empty() {
                out.push(trimmed);
            }
            from = start + name.len().max(1);
        }
    }
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------- verification

fn expand_home(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        if !home.is_empty() {
            return PathBuf::from(home).join(rest);
        }
    }
    p.to_path_buf()
}

/// Verify a binary/command anchor by searching `PATH` for an executable with
/// that name (RFC verification step 3: "binary/tool exists?"). For spans
/// carrying arguments (`cargo clippy --all-features`) only the first token
/// is the program name, so only that token is looked up. Shell builtins
/// verify `Ok` — they exist even though `which cd` finds nothing.
pub fn verify_binary(span: &str) -> Verdict {
    let clean = span.trim().trim_start_matches("$ ");
    // First whitespace-delimited token is the program; the rest is argv.
    let bin = clean.split_whitespace().next().unwrap_or("");
    if bin.is_empty() {
        return Verdict::Unverifiable;
    }
    if SHELL_BUILTINS.contains(&bin) {
        return Verdict::Ok;
    }
    if bin.contains('/') {
        // Path-shaped programs were classified as FilePath anchors upstream;
        // if one reaches here anyway, a PATH search would be a guess.
        return Verdict::Unverifiable;
    }
    let dirs = std::env::var("PATH").unwrap_or_default();
    for dir in dirs.split(':') {
        if dir.is_empty() {
            continue;
        }
        if let Ok(md) = std::fs::metadata(Path::new(dir).join(bin)) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                    return Verdict::Ok;
                }
            }
            #[cfg(not(unix))]
            if md.is_file() {
                return Verdict::Ok;
            }
        }
    }
    Verdict::Stale
}

/// Verify a provider anchor against THIS install's configured-provider
/// table (`utils::providers::configured_providers` — the same table
/// `/models` and the pickers use; local lookup, no network). A provider
/// that is merely known to the registry but not configured here is stale
/// guidance: rules should describe this install's reality.
pub fn verify_provider(name: &str, config: &Config) -> Verdict {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Verdict::Unverifiable;
    }
    let id = normalize_provider_name(trimmed);
    if configured_providers(&config.providers)
        .iter()
        .any(|(have, _)| *have == id)
    {
        Verdict::Ok
    } else {
        Verdict::Stale
    }
}

// The config-schema witness below is built from the compiled structs via
// their serde impls (RFC design decision 3: the constants/schema are the
// source of truth — never `config.toml.example`). The old independent mirror
// of the loader's `KNOWN_TOP_LEVEL_KEYS` in the drift-guard test is gone:
// #83 replaced the hand-maintained section lists with the struct itself (a
// serde_ignored pass), so `verify_config_key` below and the write guard and
// loader typo-warning in `src/config` all share one undriftable registry.

/// One fully-populated provider section, used to fill the witness gap left
/// by `skip_serializing_if` on every optional field of `ProviderConfig`.
/// The leaf NAMES are the schema; sentinel VALUES never leave this module.
fn full_provider_sentinel() -> serde_json::Value {
    let cfg = ProviderConfig {
        enabled: true,
        api_key: Some("k".into()),
        base_url: Some("https://example.invalid".into()),
        default_model: Some("m".into()),
        models: vec!["m".into()],
        force_default: false,
        vision_model: Some("v".into()),
        vision_max_tokens: Some(4096),
        generation_model: Some("g".into()),
        stt_model: Some("stt-m".into()),
        tts_model: Some("tts-m2".into()),
        context_window: Some(1),
        endpoint_type: Some("api".into()),
        wire_dialect: Some(crate::config::WireDialect::Auto),
        plan: Some("moderato".into()),
        reasoning_effort: Some("max".into()),
        voice: Some("voice".into()),
        model: Some("tts-m".into()),
        enable_thinking: Some(true),
        cache_enabled: Some(true),
        cache_ttl: Some(300),
        timeout_secs: Some(30),
        stream_idle_timeout_secs: Some(20),
        thinking_loop_timeout_secs: Some(600),
    };
    serde_json::to_value(cfg).expect("ProviderConfig serializes")
}

fn full_fallback_sentinel() -> serde_json::Value {
    let cfg = FallbackProviderConfig {
        generation: vec![],
        enabled: true,
        provider: Some("p".into()),
        providers: vec!["p".into()],
        vision: vec!["p".into()],
    };
    serde_json::to_value(cfg).expect("FallbackProviderConfig serializes")
}

/// Union the exemplar provider leaves into every provider-shaped object
/// under `[providers.*]` (they all share the `ProviderConfig` schema:
/// built-ins, `custom.*`, `web_search.exa/brave`, `image.gemini`, and the
/// maps under `stt`/`tts`). Depth-first so nested maps get the leaves too.
/// `fallback` is excluded — it is a different schema and gets its own
/// sentinel.
fn union_provider_leaves(
    node: &mut serde_json::Value,
    exemplar: &serde_json::Map<String, serde_json::Value>,
) {
    let Some(obj) = node.as_object_mut() else {
        return; // null (unset Option) — the walker treats it as a map, unverifiable
    };
    for (_k, v) in obj.iter_mut() {
        union_provider_leaves(v, exemplar);
    }
    for (k, v) in exemplar {
        obj.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

/// The embedded config schema as a JSON tree, built once per process from
/// the COMPILED struct definitions in `src/config/types.rs` via their serde
/// `Serialize` impls (RFC design decision 3: constants/schema source of
/// truth — never the `config.toml.example`, which drifts worse than code).
///
/// Serde's `skip_serializing_if`/`skip` annotations leave holes a default
/// witness can't see (`Option` fields, empty vecs, keys.toml-only fields).
/// Those holes are patched with sentinel values so leaf membership stays
/// exact; the patched paths are exactly the annotated fields and are
/// enumerated in the patch code below, each with a comment naming the
/// annotation that required it.
fn schema_witness() -> &'static serde_json::Value {
    static WITNESS: OnceLock<serde_json::Value> = OnceLock::new();
    WITNESS.get_or_init(|| {
        let mut witness =
            serde_json::to_value(Config::default()).expect("Config::default serializes");

        // `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
        // (AgentConfig::eval_providers) and
        // `skip_serializing_if = "Option::is_none"` (redact_group/redact_dm)
        // leave [agent] without these three leaves.
        if let Some(agent) = witness.get_mut("agent").and_then(|a| a.as_object_mut()) {
            agent.insert("eval_providers".into(), serde_json::json!(["sentinel"]));
            agent.insert("redact_group".into(), serde_json::json!(true));
            agent.insert("redact_dm".into(), serde_json::json!(false));
        }

        // `[memory.embedding]` is an Option<EmbeddingConfig> that defaults
        // to `null` mid-path; a default instance serializes every leaf KEY
        // (as nulls), which is all membership needs.
        if let Some(memory) = witness.get_mut("memory").and_then(|m| m.as_object_mut())
            && let Ok(embedding) = serde_json::to_value(EmbeddingConfig::default())
        {
            memory.insert("embedding".into(), embedding);
        }

        // Every provider-shaped section under [providers.*] shares the
        // ProviderConfig schema but serializes as `null` while unset
        // (Option fields without skip_serializing_if), hiding its leaves.
        // The built-in fields (anthropic … vertex) are Option<ProviderConfig>,
        // so a null there is replaced by the full exemplar. The map-valued
        // siblings keep their null — unknown user-chosen keys are honestly
        // Unverifiable: `custom.*` (named custom providers) and the
        // fixed-field tables `stt`/`tts`, whose child schemas aren't
        // enumerable from a default witness. `web_search` / `image` are
        // known fixed-field tables of ProviderConfig, built from their real
        // schemas.
        let exemplar = full_provider_sentinel();
        let exemplar_map = exemplar.as_object().cloned().unwrap_or_default();
        if let Some(providers) = witness.get_mut("providers").and_then(|p| p.as_object_mut()) {
            // FallbackProviderConfig — its own schema, own sentinel.
            providers.insert("fallback".into(), full_fallback_sentinel());
            // web_search / image: known fixed-field tables of ProviderConfig.
            providers.insert(
                "web_search".into(),
                serde_json::json!({ "exa": exemplar, "brave": exemplar }),
            );
            providers.insert("image".into(), serde_json::json!({ "gemini": exemplar }));
            for (k, v) in providers.iter_mut() {
                if matches!(k.as_str(), "custom" | "stt" | "tts") {
                    continue; // map-valued: leave null → Unverifiable
                }
                if v.is_null() {
                    *v = exemplar.clone();
                } else {
                    union_provider_leaves(v, &exemplar_map);
                }
            }
        }
        witness
    })
}

/// Verify a `[dotted.config.key]` anchor against the embedded schema witness
/// (RFC design decision 3). Membership walk, never a guess:
///
/// - resolves fully → [`Verdict::Ok`];
/// - a segment missing under a NON-EMPTY object → that parent is a struct
///   with a known, visible field set → [`Verdict::Stale`];
/// - a segment missing under an EMPTY object or `null` → the parent is a
///   map (e.g. `channels.telegram.groups.*`, `providers.custom.*`) whose
///   keys are user-chosen, not schema → [`Verdict::Unverifiable`];
/// - path continues through a scalar leaf (e.g. `[database.path.x]`) →
///   malformed, but not evidence the cited key is gone →
///   [`Verdict::Unverifiable`].
pub fn verify_config_key(span: &str) -> Verdict {
    let inner = span.trim().trim_start_matches('[').trim_end_matches(']');
    if inner.is_empty() {
        return Verdict::Unverifiable;
    }
    // `gateway` is a serde alias of `a2a`; the witness holds the field name.
    let normalized = inner
        .strip_prefix("gateway")
        .map(|rest| format!("a2a{rest}"));
    let path: &str = normalized.as_deref().unwrap_or(inner);
    let mut node = schema_witness();
    let segments: Vec<&str> = path
        .split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match node {
            serde_json::Value::Object(obj) => {
                match obj.get(*seg) {
                    Some(child) => {
                        if last {
                            return Verdict::Ok;
                        }
                        node = child;
                    }
                    None => {
                        if obj.is_empty() {
                            // Empty map — children are user keys, not schema.
                            return Verdict::Unverifiable;
                        }
                        return Verdict::Stale;
                    }
                }
            }
            serde_json::Value::Null => return Verdict::Unverifiable,
            // Scalar mid-path: schema says this is a leaf; deeper is
            // malformed rather than stale.
            _ => return Verdict::Unverifiable,
        }
    }
    Verdict::Ok
}

/// Verify a file-path anchor with `fs::metadata` (RFC verification step 3:
/// "file path still exists?"). Conservative by design:
///
/// - `~` is expanded (`~/` → `$HOME`);
/// - absolute paths decide fully: exists → `Ok`, missing → `Stale`;
/// - glob patterns (`*`, `?`, `[`) → `Unverifiable` — existence of a
///   pattern is not decidable with metadata;
/// - relative paths resolve against the process cwd at best; absence from
///   cwd is NOT evidence the path is gone (it may be repo- or
///   session-relative), so a missing relative path is `Unverifiable`, never
///   a stale flag.
pub fn verify_path(span: &str) -> Verdict {
    let s = span.trim();
    if s.is_empty() {
        return Verdict::Unverifiable;
    }
    if s.contains(['*', '?', '[']) && !s.starts_with('[') {
        return Verdict::Unverifiable;
    }
    let expanded = expand_home(Path::new(s));
    if expanded.is_absolute() {
        if expanded.exists() {
            Verdict::Ok
        } else {
            Verdict::Stale
        }
    } else if expanded.exists() {
        Verdict::Ok
    } else {
        Verdict::Unverifiable
    }
}

// ---------------------------------------------------------------- scan

/// Scan every canonical brain file under `brain_root`. Read-only: this
/// produces findings, it never edits anything (RFC guardrail).
pub fn scan_brain_files(config: &Config, brain_root: &Path) -> Vec<StaleFinding> {
    let mut findings = Vec::new();

    for file in crate::memory::BRAIN_FILES {
        let path = brain_root.join(file);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (idx, raw_line) in content.lines().enumerate() {
            if classify_line(raw_line) != LineClass::Prescription {
                continue;
            }
            let spans = backtick_spans(raw_line);
            let mentions = provider_mentions(raw_line);

            for span in &spans {
                let kind = if mentions.iter().any(|m| m.eq_ignore_ascii_case(span.trim())) {
                    // A backticked name right where the line says "provider
                    // X" is a provider anchor, not a binary.
                    AnchorKind::ProviderName
                } else {
                    match anchor_kind(span, raw_line) {
                        Some(k) => k,
                        None => continue,
                    }
                };
                findings.push(make_finding(
                    file,
                    idx + 1,
                    raw_line,
                    span.trim(),
                    kind,
                    config,
                ));
            }

            // Provider mentions outside backticks ("use provider zhipu for
            // vision") — only those not already covered by a backticked
            // span above, so one mention yields one finding.
            for mention in &mentions {
                if spans.iter().any(|s| s.trim().eq_ignore_ascii_case(mention)) {
                    continue;
                }
                findings.push(make_finding(
                    file,
                    idx + 1,
                    raw_line,
                    mention,
                    AnchorKind::ProviderName,
                    config,
                ));
            }
        }
    }
    findings
}

fn make_finding(
    file: &str,
    line_no: usize,
    raw_line: &str,
    anchor: &str,
    kind: AnchorKind,
    config: &Config,
) -> StaleFinding {
    let (verdict, evidence) = match kind {
        AnchorKind::FilePath => match verify_path(anchor) {
            Verdict::Ok => (Verdict::Ok, "path exists (fs::metadata)".to_string()),
            Verdict::Stale => (Verdict::Stale, "path does not exist".to_string()),
            Verdict::Unverifiable => (
                Verdict::Unverifiable,
                "glob pattern / relative path — existence not decidable".to_string(),
            ),
        },
        AnchorKind::ConfigKey => match verify_config_key(anchor) {
            Verdict::Ok => (Verdict::Ok, "key present in embedded config schema".into()),
            Verdict::Stale => (
                Verdict::Stale,
                "key absent from embedded config schema (compiled types, not *.example)".into(),
            ),
            Verdict::Unverifiable => (
                Verdict::Unverifiable,
                "map-valued/malformed key path".into(),
            ),
        },
        AnchorKind::Binary => match verify_binary(anchor) {
            Verdict::Ok => (
                Verdict::Ok,
                "executable found on PATH (or shell builtin)".into(),
            ),
            Verdict::Stale => (Verdict::Stale, "no executable on PATH".into()),
            Verdict::Unverifiable => (Verdict::Unverifiable, "not a decidable binary".into()),
        },
        AnchorKind::ProviderName => match verify_provider(anchor, config) {
            Verdict::Ok => (Verdict::Ok, "provider in configured-provider table".into()),
            Verdict::Stale => (
                Verdict::Stale,
                "provider not in this install's configured-provider table".into(),
            ),
            Verdict::Unverifiable => (Verdict::Unverifiable, "empty provider name".into()),
        },
    };
    StaleFinding {
        file: file.to_string(),
        line_no,
        line: raw_line.to_string(),
        anchor: anchor.to_string(),
        anchor_kind: kind,
        verdict,
        action: decide_action(kind, verdict),
        evidence,
    }
}
