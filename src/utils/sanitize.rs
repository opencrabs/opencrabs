//! Tool input sanitization for safe display in approval messages and UI.
//!
//! When the agent calls tools like `http_request` or `bash`, the inputs may
//! contain API keys, Authorization headers, passwords, or tokens. This module
//! redacts those values before they are shown to users in Telegram/WhatsApp
//! approval dialogs or the TUI, while preserving enough context (field names,
//! non-sensitive values) for the user to understand what the tool is doing.
//!
//! Redaction can be disabled via `config.agent.redact_sensitive_data = false`
//! for sysadmin/devops work where seeing IPs, tokens, and passwords is
//! necessary.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};

/// Check if sensitive data redaction is enabled.
/// Loads the config and returns the `agent.redact_sensitive_data` flag.
/// Defaults to `true` if config cannot be loaded.
fn is_redaction_enabled() -> bool {
    crate::config::Config::load()
        .map(|c| c.agent.redact_sensitive_data)
        .unwrap_or(true)
}

/// Field name patterns (case-insensitive) whose values are always redacted.
const SENSITIVE_KEYS: &[&str] = &[
    "authorization",
    "api_key",
    "apikey",
    "api-key",
    "x-api-key",
    "x-auth-token",
    "x-access-token",
    "token",
    "secret",
    "password",
    "passwd",
    "pass",
    "credential",
    "credentials",
    "access_token",
    "refresh_token",
    "client_secret",
    "private_key",
    "auth",
    "bearer",
    "email",
];

/// Regex-like patterns in bash commands to redact inline secrets.
/// Each tuple is (prefix_to_find, chars_to_keep_after_prefix).
/// We redact the rest of the token after these prefixes.
const COMMAND_SENSITIVE_PATTERNS: &[&str] = &[
    "bearer ",
    "authorization: ",
    "x-api-key: ",
    "x-auth-token: ",
    "api_key=",
    "apikey=",
    "api-key=",
    "token=",
    "secret=",
    "password=",
    "passwd=",
    "access_token=",
    // Environment variable suffixes that typically hold secrets
    "_pass=",
    "_password=",
    "_passwd=",
    "_secret=",
    "_token=",
    "_key=",
    "_apikey=",
    "_api_key=",
    "_credential=",
    "_credentials=",
    "_auth=",
];

/// Returns true if a JSON object key looks like it holds a sensitive value.
fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    SENSITIVE_KEYS
        .iter()
        .any(|&pat| lower == pat || lower.contains(pat))
}

/// Redact sensitive values from a bash command string.
/// Handles patterns like:
///   -H "Authorization: Bearer sk-xxx"
///   --header "X-Api-Key: abc123"
///   https://user:password@host/path
///   api_key=abc123
///
/// `pub(crate)` so the TUI's one-line tool-call summary can redact a
/// command before display — the expanded view already redacts via
/// `redact_tool_input`, this closes the same leak in the collapsed line.
pub(crate) fn redact_command(cmd: &str) -> String {
    // Skip redaction if disabled via config
    if !is_redaction_enabled() {
        return cmd.to_string();
    }

    let mut result = cmd.to_string();

    // Redact URL passwords: https://user:PASSWORD@host → https://user:[REDACTED]@host
    // Simple approach: find ://word:word@ patterns
    if let Some(at_pos) = result.find("://") {
        let rest = &result[at_pos + 3..];
        if let Some(at_sign) = rest.find('@')
            && let Some(colon) = rest[..at_sign].find(':')
        {
            let pass_start = at_pos + 3 + colon + 1;
            let pass_end = at_pos + 3 + at_sign;
            if pass_start < pass_end && pass_end <= result.len() {
                result.replace_range(pass_start..pass_end, "[REDACTED]");
            }
        }
    }

    // Redact inline header values and query params (case-insensitive).
    //
    // All COMMAND_SENSITIVE_PATTERNS are pure ASCII. We use find_case_insensitive
    // (below) instead of to_lowercase() to avoid the byte-offset mismatch problem:
    // to_lowercase() can expand Unicode chars (e.g. 'İ' → 'i̇', 2→3 bytes), causing
    // positions from the lowered string to misalign with the original string, which
    // in turn causes wrong redaction or panic on slicing.
    for pattern in COMMAND_SENSITIVE_PATTERNS {
        let mut search_start = 0;
        while let Some(abs_pos) = find_case_insensitive(&result[search_start..], pattern) {
            let true_pos = search_start + abs_pos;
            let after = true_pos + pattern.len();
            if after > result.len() {
                break;
            }
            // A quoted value must redact its CONTENTS (#879). Previously the
            // quote was just another terminator, so for `TOKEN="secret"` the
            // delimiter search returned 0, `secret_end == after`, the guard
            // below failed and NOTHING was redacted — the quoted form, which is
            // the more common shell spelling, defeated redaction entirely.
            let rest = &result[after..];
            let (value_start, terminators): (usize, &[char]) = match rest.chars().next() {
                // Opening quote: step over it and end at the matching one, so
                // spaces inside a quoted secret do not cut it short either.
                Some('"') => (after + 1, &['"', '\n']),
                Some('\'') => (after + 1, &['\'', '\n']),
                _ => (after, &['"', '\'', ' ', '&', '\n']),
            };
            let secret_end = result[value_start..]
                .find(terminators)
                .map(|p| value_start + p)
                .unwrap_or(result.len());
            if secret_end > value_start {
                result.replace_range(value_start..secret_end, "[REDACTED]");
            }
            // Advance past where we just redacted to avoid infinite loop
            search_start = after.saturating_add("[REDACTED]".len());
            if search_start >= result.len() {
                break;
            }
        }
    }

    // 5. Redact environment variable assignments with sensitive suffixes
    result = ENV_SECRET_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let var_name = caps.get(1).unwrap().as_str();
            format!("{var_name}=[REDACTED]")
        })
        .into_owned();

    // 6. Redact piped secrets: echo "secret" | command
    //    The secret value inside quotes is replaced with [REDACTED]
    result = PIPED_SECRET_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let full_match = caps.get(0).unwrap().as_str();
            let secret = caps.get(1).unwrap().as_str();
            full_match.replace(secret, "[REDACTED]")
        })
        .into_owned();

    // 7. Redact IPv4 addresses (server IPs, infrastructure addresses).
    //    Keeps 127.0.0.1 and 0.0.0.0 as they are non-sensitive.
    result = IPV4_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let ip = caps.get(1).unwrap().as_str();
            if ip == "127.0.0.1" || ip == "0.0.0.0" {
                ip.to_string()
            } else {
                "[IP_REDACTED]".to_string()
            }
        })
        .into_owned();

    result
}

/// Find a case-insensitive ASCII substring in `haystack`, returning the byte
/// position of the first match. Returns None if not found.
///
/// Unlike haystack.to_lowercase().find(), this never expands the haystack,
/// so returned positions are always valid indices in the original string.
fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    debug_assert!(needle.is_ascii());
    if needle.is_empty() {
        return Some(0);
    }
    let first = needle.as_bytes()[0];
    let rest = &needle[1..];
    for (pos, chunk) in haystack.as_bytes().windows(needle.len()).enumerate() {
        if chunk[0].eq_ignore_ascii_case(&first)
            && chunk[1..]
                .iter()
                .enumerate()
                .all(|(i, &b)| b.eq_ignore_ascii_case(&rest.as_bytes()[i]))
        {
            return Some(pos);
        }
    }
    None
}

/// Recursively replace the user's home directory path with `~` in all strings.
///
/// Transforms `/Users/adolfousierstudio/srv/rs/opencrabs` → `~/srv/rs/opencrabs`
/// This makes tool call displays much cleaner in the TUI and channels.
fn shrink_home_paths(value: &Value) -> Value {
    // Cross-platform home directory detection:
    // - macOS/Linux: HOME
    // - Windows: USERPROFILE (or HOMEDRIVE + HOMEPATH as fallback)
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .or_else(|_| {
            let drive = std::env::var("HOMEDRIVE").unwrap_or_default();
            let path = std::env::var("HOMEPATH").unwrap_or_default();
            let combined = format!("{}{}", drive, path);
            if combined.is_empty() {
                Err(std::env::VarError::NotPresent)
            } else {
                Ok(combined)
            }
        });

    let home = match home {
        Ok(h) if !h.is_empty() => h,
        _ => return value.clone(),
    };

    shrink_home_paths_inner(value, &home)
}

fn shrink_home_paths_inner(value: &Value, home: &str) -> Value {
    match value {
        Value::String(s) => {
            // Replace home path at the start of the string, or anywhere inside it
            let shortened = s.replace(home, "~");
            Value::String(shortened)
        }
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), shrink_home_paths_inner(v, home));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|v| shrink_home_paths_inner(v, home))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Recursively redact sensitive fields from a tool input JSON value.
///
/// - Object keys matching `SENSITIVE_KEYS` have their string values replaced
///   with `"[REDACTED]"`
/// - The `command` field (bash) has inline secret patterns redacted
/// - The `headers` object has all values for sensitive header names redacted
/// - Arrays and nested objects are recursively processed
/// - Home directory paths are shortened to `~` for cleaner display
pub fn redact_tool_input(value: &Value) -> Value {
    // Skip redaction if disabled via config
    if !is_redaction_enabled() {
        return value.clone();
    }

    // First shrink home paths, then redact secrets
    let shortened = shrink_home_paths(value);
    redact_value(&shortened, None)
}

fn redact_value(value: &Value, parent_key: Option<&str>) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                let redacted = if is_sensitive_key(k) {
                    // Redact the value regardless of type
                    Value::String("[REDACTED]".to_string())
                } else if k == "command" {
                    // Bash command: apply inline pattern redaction
                    match v.as_str() {
                        Some(cmd) => Value::String(redact_command(cmd)),
                        None => redact_value(v, Some(k)),
                    }
                } else if k == "headers" {
                    // Headers object: redact values for sensitive header names
                    redact_headers_object(v)
                } else if k == "query" || k == "params" {
                    // Query params object: redact sensitive param values
                    redact_value(v, Some(k))
                } else if k == "url" {
                    // URLs may have passwords embedded
                    match v.as_str() {
                        Some(url) => Value::String(redact_command(url)),
                        None => redact_value(v, Some(k)),
                    }
                } else {
                    redact_value(v, Some(k))
                };
                out.insert(k.clone(), redacted);
            }
            Value::Object(out)
        }
        Value::Array(arr) => {
            // If parent key is sensitive, redact the whole array
            if parent_key.map(is_sensitive_key).unwrap_or(false) {
                Value::String("[REDACTED]".to_string())
            } else {
                Value::Array(arr.iter().map(|v| redact_value(v, None)).collect())
            }
        }
        Value::String(s) => {
            if parent_key.map(is_sensitive_key).unwrap_or(false) {
                Value::String("[REDACTED]".to_string())
            } else {
                Value::String(s.clone())
            }
        }
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Free-text secret redaction (for thinking streams, responses, intermediate text)
// ---------------------------------------------------------------------------

/// Known API key prefixes with their minimum total length.
/// Format: (prefix, min_suffix_len) — token must have at least this many chars
/// after the prefix to be considered a real key (avoids false positives on
/// short strings that happen to start with a prefix).
///
/// When adding new prefixes: prefer longer, more specific prefixes first
/// (e.g. `sk-proj-` before `sk-`). The scanner checks all prefixes, so order
/// only affects which prefix is shown in `[prefix][REDACTED]` output.
const KEY_PREFIXES: &[(&str, usize)] = &[
    // --- AI / LLM providers ---
    ("sk-proj-", 20),      // OpenAI project keys
    ("sk-ant-api03-", 20), // Anthropic API keys (full prefix)
    ("sk-ant-", 20),       // Anthropic API keys (short prefix)
    ("sk-or-v1-", 20),     // OpenRouter keys
    ("sk-cp-", 20),        // MiniMax keys
    ("sk-", 20),           // Generic OpenAI-style keys (DeepSeek, etc.)
    ("gsk_", 15),          // Groq keys
    ("nvapi-", 15),        // NVIDIA NIM keys
    ("AIzaSy", 20),        // Google AI / Firebase keys
    ("ya29.", 20),         // Google OAuth access tokens
    ("pplx-", 20),         // Perplexity keys
    ("hf_", 15),           // HuggingFace tokens
    ("r8_", 20),           // Replicate tokens
    // --- Cloud providers ---
    ("AKIA", 12), // AWS access key IDs (exactly 20 chars total)
    ("ASIA", 12), // AWS STS temporary credentials
    ("ABIA", 12), // AWS STS (another variant)
    ("ACCA", 12), // AWS CloudFront
    // --- Azure ---
    // Azure keys are 32-char base64 but headers are caught by SENSITIVE_KEYS.
    // Connection strings have a known prefix:
    ("DefaultEndpointsProtocol=", 10), // Azure Storage connection strings
    // --- Payments ---
    ("sk_live_", 15), // Stripe secret keys (live)
    ("sk_test_", 15), // Stripe secret keys (test)
    ("pk_live_", 15), // Stripe publishable keys (live)
    ("pk_test_", 15), // Stripe publishable keys (test)
    ("rk_live_", 15), // Stripe restricted keys (live)
    ("rk_test_", 15), // Stripe restricted keys (test)
    ("sq0atp-", 15),  // Square access tokens
    ("sq0csp-", 15),  // Square application secrets
    // --- Git / DevOps ---
    ("ghp_", 15),            // GitHub personal tokens
    ("gho_", 15),            // GitHub OAuth tokens
    ("ghu_", 15),            // GitHub user-to-server tokens
    ("ghs_", 15),            // GitHub server-to-server tokens
    ("github_pat_", 15),     // GitHub fine-grained tokens
    ("glpat-", 15),          // GitLab personal tokens
    ("gloas-", 15),          // GitLab OAuth tokens
    ("npm_", 15),            // npm registry tokens
    ("pypi-AgEIcHlwaS", 10), // PyPI tokens
    // --- Communication ---
    ("xoxb-", 15),    // Slack bot tokens
    ("xoxp-", 15),    // Slack user tokens
    ("xapp-", 15),    // Slack app tokens
    ("xoxs-", 15),    // Slack session tokens
    ("SG.", 20),      // SendGrid API keys
    ("xkeysib-", 15), // Brevo (Sendinblue) keys
    // --- E-commerce / SaaS ---
    ("shpat_", 15),   // Shopify admin tokens
    ("shpca_", 15),   // Shopify custom app tokens
    ("shppa_", 15),   // Shopify partner tokens
    ("shpss_", 15),   // Shopify shared secret
    ("ntn_", 15),     // Notion API tokens
    ("lin_api_", 15), // Linear API keys
    ("aio_", 15),     // Airtable tokens
    ("phc_", 15),     // PostHog keys
    // --- Infrastructure / Monitoring ---
    ("sntrys_", 15),         // Sentry auth tokens
    ("dop_v1_", 15),         // DigitalOcean tokens
    ("tskey-", 15),          // Tailscale keys
    ("tvly-", 15),           // Tavily web search keys
    ("hvs.", 15),            // HashiCorp Vault service tokens
    ("vault:v1:", 10),       // HashiCorp Vault transit-encrypted
    ("AGE-SECRET-KEY-", 10), // age encryption keys
    // --- Social / Meta ---
    ("EAA", 30),  // Facebook/Meta long-lived tokens
    ("ATTA", 30), // Trello tokens
    // --- Auth / Crypto ---
    ("eyJ", 30),    // JWT tokens (base64 of `{"`)
    ("whsec_", 15), // Webhook signing secrets
    // --- OpenCrabs built-in provider ---
    // Install tokens issued by the inference proxy. The hex rule cannot see
    // them: `_` is a word character, so there is no boundary before the digits.
    ("ocp_", 15), // OpenCrabs inference proxy install tokens
];

/// Regex for long hex strings that look like API tokens (32+ hex chars).
/// Lowered from 40 to 32 to catch Azure keys, custom service tokens, etc.
/// UUIDs are safe — they contain dashes so won't match contiguous hex.
static HEX_TOKEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b[0-9a-fA-F]{32,}\b").unwrap());

/// Regex for mixed alphanumeric tokens (28+ chars containing both letters and digits).
/// Catches opaque tokens like custom service keys that have no prefix.
/// Won't match pure words, pure numbers, or structured strings with separators.
static MIXED_ALNUM_TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b[a-zA-Z0-9]{28,}\b").unwrap());

/// Regex for provider tokens shaped `<digits>:<opaque>` — the Telegram bot
/// token form (#879).
///
/// `MIXED_ALNUM_TOKEN_RE` cannot see these: the colon, hyphens and underscores
/// break the run, so no 28+ alphanumeric stretch exists to match. Without this
/// there was no structural net under the prefix matcher, which is why one
/// quoting bug was enough to leak a live token.
static COLON_TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b\d{6,12}:[A-Za-z0-9_-]{30,}\b").unwrap());

/// Regex for environment variable assignments with sensitive suffixes.
/// Matches UPPERCASE_VAR_NAME="value" or UPPERCASE_VAR_NAME=value where the
/// name ends in _PASS, _SECRET, _TOKEN, _KEY, _APIKEY, _API_KEY, _CREDENTIAL,
/// _CREDENTIALS, or _AUTH (case-insensitive suffix, uppercase name required).
/// Only matches names starting with uppercase to avoid catching lowercase
/// field names like auth_token that have dedicated prefix handlers.
static ENV_SECRET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"\b((?:[A-Z][A-Z0-9_]*_)?(?:PASS|PASSWORD|PASSWD|SECRET|TOKEN|KEY|APIKEY|API_KEY|CREDENTIAL|CREDENTIALS|AUTH))\s*=\s*(?:")?([^\s"']+)"#).unwrap()
});

/// Regex for sensitive `key=value` assignments and URL query params in any
/// casing — `?api_key=sk-xxx`, `&token=...`, `password = "..."`. ENV_SECRET_RE
/// only catches UPPERCASE env vars; this covers the lowercase/query-param
/// form that appears in provider-error URLs and is otherwise missed by the
/// prefix/length-based passes (a short, unknown-prefix token slips through).
/// Keys are an explicit allowlist — bare `key=` is excluded to avoid
/// redacting `primary_key=5`, `cache_key=foo`, etc. The value runs to the
/// next delimiter (`&`, quote, whitespace, `;`, bracket).
static KEYVAL_SECRET_RE: Lazy<Regex> = Lazy::new(|| {
    // The value class excludes `[` so this pass never consumes an existing
    // `[REDACTED_TOKEN]` placeholder left by an earlier pass (HEX/MIXED run
    // first) — that would mangle it to `[REDACTED]]`.
    Regex::new(
        r#"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|auth[_-]?token|client[_-]?secret|private[_-]?key|secret|password|passwd|token)\s*=\s*"?([^\s"'`,)\]}>|;&\[]+)"#,
    )
    .unwrap()
});

/// Regex for labeled credentials: `Password: value`, `Token: value`, etc.
/// Unlike KEYVAL_SECRET_RE (which matches `=`), this catches the `:` separator
/// used in structured text output, markdown, and shell-style credential lists.
static LABELED_CRED_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?i)\b(password|passwd|pass|token|secret|api[_ ]?key|credential|auth|email)\s*:\s*"?([^\s"',;`\[]+)"?"#
    ).unwrap()
});

/// Regex for credentials embedded in a URL authority:
/// `scheme://user:PASSWORD@host`. Redacts the password, keeps the rest so
/// the URL stays recognizable. ENV/prefix passes don't see these.
static URL_PASSWORD_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(://[^:/\s@]+:)([^@/\s]+)(@)").unwrap());

/// Regex for piped secrets: echo "secret" | command or echo 'secret' | command
/// Catches patterns where a secret is piped to docker login, kubectl, etc.
static PIPED_SECRET_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?i)echo\s+["']([a-zA-Z0-9_\-+/=]{20,})["']\s*\|"#).unwrap());

/// Regex for IPv4 addresses: four octets of 1-3 digits separated by dots.
/// Uses word boundaries to avoid matching inside longer strings.
/// Excludes common version-like patterns (e.g. `0.0.0.0`, `1.0.0`, `v1.2.3`).
static IPV4_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})\b").unwrap());

/// Regex for Qwen 3 / DeepSeek SentencePiece-style tool-call markers.
/// Matches the full token family: `<|tool<sep>calls_section_begin|>`,
/// `<|tool<sep>call_begin|>`, `<|tool<sep>call_end|>`,
/// `<|tool<sep>calls_section_end|>`, `<|tool<sep>call_argument_begin|>`,
/// where `<sep>` is either U+2581 (`▁`, the real SentencePiece word-
/// boundary char emitted by Qwen 3 vLLM endpoints) or an ASCII `_`
/// (emitted by some quantizations / forks).
///
/// Why a catch-all rather than enumerating each marker: when Qwen 3
/// degrades into repetition mode it can emit unknown variants like
/// `<|tool▁call_metadata|>` or future tokens we haven't seen yet. The
/// `[^|]*` middle drops anything that fits the bracket shape, so the
/// regex stays correct as the format grows. The leading `<|tool` plus
/// the U+2581-or-underscore separator is specific enough that prose
/// like `<|tool of choice|>` will not match (no separator follows).
static QWEN_TOOL_MARKER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<\|tool[\u{2581}_][^|]*\|>").unwrap());

/// ANSI CSI sequences: `ESC [ <params> <final>`, params may include space
/// (DECSCUSR `\x1b[2 q`). Model or tool text carrying these gets EXECUTED by
/// the terminal the moment ratatui writes the span buffer (#1719 garble).
static ANSI_CSI_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\x1b\[[0-9;:?<=>! ]*[A-Za-z@]").unwrap());

/// OSC sequences (window title rewrites etc.), BEL- or ST-terminated.
static ANSI_OSC_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").unwrap());

/// Remaining two-byte ESC sequences. ECMA-48 final bytes span 0x30-0x7E
/// (`ESC 7` save, `ESC 8` restore, `ESC c` reset, `ESC M` reverse index...).
static ANSI_ESC_SINGLE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\x1b[0-~]").unwrap());

/// The one outbound chokepoint: strip LLM artifacts, then redact secrets.
///
/// Every channel path that puts model prose in front of a user needs both
/// steps, and a path that only strips artifacts leaks credentials (#2102:
/// seven reaction/intermediate/comment paths called
/// [`strip_llm_artifacts`] alone). Calling this instead of the two
/// functions separately makes the pair impossible to forget, and keeps the
/// ordering fixed: artifacts first, because a redacted-but-still-tagged body
/// is not clean text, and because `strip_llm_artifacts` can drop a whole
/// block that held a key, so redacting afterwards does strictly less work.
///
/// Idempotent for already-sanitized text, like both halves.
pub fn sanitize_outbound(text: &str) -> String {
    redact_secrets(&strip_llm_artifacts(text))
}

/// Redact API keys and tokens from free-form text (thinking, responses, etc.).
///
/// Catches:
/// - Known provider key prefixes (`sk-proj-...`, `xoxb-...`, `AIzaSy...`, etc.)
/// - Long hex token strings (32+ chars — Azure keys, custom tokens, etc.)
/// - Mixed alphanumeric tokens (28+ chars with both letters and digits)
/// - Bearer/Authorization inline patterns
///
/// Preserves the prefix so the user can see *what kind* of key was redacted.
pub fn redact_secrets(text: &str) -> String {
    if !is_redaction_enabled() {
        return text.to_string();
    }
    redact_secrets_core(text)
}

/// Scope-aware redaction (#677): decide whether to redact from the DM/group
/// scope resolver ([`crate::config::AgentConfig::redact_for`]) rather than the
/// global flag — so a DM shows secrets to the owner while group/channel chats
/// scrub them. Defaults to redacting if config can't be loaded.
pub fn redact_secrets_scoped(text: &str, is_dm: bool) -> String {
    let should = crate::config::Config::load()
        .map(|c| c.agent.redact_for(is_dm))
        .unwrap_or(true);
    if !should {
        return text.to_string();
    }
    redact_secrets_core(text)
}

/// The secret-scrubbing core, with NO config check — gated by
/// [`redact_secrets`] (global flag) or [`redact_secrets_scoped`] (scope).
/// Unconditional secret scrub for the log writer (OC-05).
///
/// The channel-output redactor is gated by `agent.redact_sensitive_data`, which
/// is a display concern: a DM may deliberately show the owner their own keys.
/// Logs are different — a provider key or channel token written to
/// `~/.opencrabs/logs/` or a daemon stderr is a credential on disk regardless of
/// that flag, so this ignores the flag and always scrubs. Borrows when there is
/// nothing to redact, so the common log line allocates nothing.
pub(crate) fn redact_secrets_for_logs(text: &str) -> std::borrow::Cow<'_, str> {
    let out = redact_secrets_core(text);
    if out == text {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(out)
    }
}

fn redact_secrets_core(text: &str) -> String {
    let mut result = text.to_string();

    // 1. Redact known key prefixes — keep prefix, replace rest with [REDACTED]
    // All KEY_PREFIXES are ASCII, so we use find_case_insensitive to avoid
    // to_lowercase() Unicode-expansion causing byte-offset misalignment.
    for &(prefix, min_suffix_len) in KEY_PREFIXES {
        let mut search_from = 0;
        while let Some(abs_pos) = find_case_insensitive(&result[search_from..], prefix) {
            let true_pos = search_from + abs_pos;
            let after = true_pos + prefix.len();
            // Guard: after can exceed result.len() when Unicode chars in the
            // prefix region expanded on to_lowercase(). find_case_insensitive
            // avoids this, but we keep the guard as defence-in-depth.
            if after > result.len() {
                break;
            }
            // Find end of token: next whitespace, quote, comma, backtick, or end
            let end = result[after..]
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            '"' | '\'' | ',' | '`' | ')' | ']' | '}' | '>' | '|' | ';'
                        )
                })
                .map(|p| after + p)
                .unwrap_or(result.len());
            let suffix_len = end - after;
            if suffix_len >= min_suffix_len {
                // Keep prefix visible, redact the rest
                result.replace_range(after..end, "[REDACTED]");
            }
            // Advance past the replacement (or, when we didn't replace,
            // past `prefix.len()` worth of bytes so we don't re-scan
            // the same prefix forever). MUST snap to a UTF-8 char
            // boundary — `after + "[REDACTED]".len()` is a byte
            // arithmetic operation and can land inside a multi-byte
            // character (e.g. `→` U+2192 is 3 bytes, so `after + 10`
            // can stop at byte 2 of 3 inside it). The next iteration
            // would then panic at the slice on line 461 with
            // `start byte index N is not a char boundary; it is inside
            // '→' (bytes M..M+3)`. Repro: Ctrl+O expand a thinking
            // block that contains `→` after a prefix-shaped string.
            let raw_next = after.saturating_add("[REDACTED]".len());
            search_from = if raw_next >= result.len() {
                result.len()
            } else {
                // Snap forward to a valid char boundary. `ceil_char_boundary`
                // is stable since Rust 1.79.
                result.ceil_char_boundary(raw_next)
            };
            if search_from >= result.len() {
                break;
            }
        }
    }

    // Helper: a 28+ char alphanumeric blob preceded by `/` is almost always
    // a URL path segment, not a secret. Google Drive `/file/d/<33-char-id>`,
    // Docs `/document/d/<id>`, YouTube `/watch?v=<id>`, GitHub
    // `/commit/<40-char-sha>`, etc. — all public identifiers that users
    // need to click. 2026-04-18 22:29 screenshot: a Drive file ID got
    // redacted to `[REDACTED_TOKEN]`, breaking the link. Real secrets
    // appear after `=`, `:`, or whitespace, never after `/`.
    let is_url_path_segment =
        |input: &str, match_start: usize| -> bool { input[..match_start].ends_with('/') };

    // 2. Redact long hex tokens (32+ chars — catches Azure keys, custom service tokens, etc.)
    result = HEX_TOKEN_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            let token = m.as_str();
            if is_url_path_segment(&result, m.start()) {
                token.to_string()
            } else {
                "[REDACTED_TOKEN]".to_string()
            }
        })
        .into_owned();

    // 2b. Redact `<digits>:<opaque>` provider tokens (#879). The colon and the
    //     hyphens/underscores break the run MIXED_ALNUM_TOKEN_RE looks for, so
    //     without this pass nothing structural catches this shape and the
    //     prefix matcher is the only line of defence.
    result = COLON_TOKEN_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            if is_url_path_segment(&result, m.start()) {
                m.as_str().to_string()
            } else {
                "[REDACTED_TOKEN]".to_string()
            }
        })
        .into_owned();

    // 3. Redact mixed alphanumeric tokens (28+ chars with both letters AND digits).
    //    Catches opaque keys like custom service tokens with no prefix.
    //    Only redacts if the match contains both digits and letters (avoids
    //    false positives on long words like "acknowledgement" or pure numbers).
    result = MIXED_ALNUM_TOKEN_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            let token = m.as_str();
            let has_digit = token.chars().any(|c| c.is_ascii_digit());
            let has_alpha = token.chars().any(|c| c.is_ascii_alphabetic());
            if has_digit && has_alpha && !is_url_path_segment(&result, m.start()) {
                "[REDACTED_TOKEN]".to_string()
            } else {
                token.to_string()
            }
        })
        .into_owned();

    // 4. Redact inline "Bearer <token>" patterns (ASCII-only, same fix as redact_command)
    for pattern in &["bearer ", "authorization: bearer "] {
        let mut search_start = 0;
        while let Some(abs_pos) = find_case_insensitive(&result[search_start..], pattern) {
            let true_pos = search_start + abs_pos;
            let after = true_pos + pattern.len();
            // Bounds check: after can exceed result.len() when earlier chars
            // expanded on to_lowercase() — same Unicode issue as in redact_command.
            if after > result.len() {
                break;
            }
            let end = result[after..]
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ')'))
                .map(|p| after + p)
                .unwrap_or(result.len());
            if end > after {
                result.replace_range(after..end, "[REDACTED]");
            }
            search_start = after.saturating_add("[REDACTED]".len());
            if search_start >= result.len() {
                break;
            }
        }
    }

    // 5. Redact environment variable assignments with sensitive suffixes
    result = ENV_SECRET_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let var_name = caps.get(1).unwrap().as_str();
            format!("{var_name}=[REDACTED]")
        })
        .into_owned();

    // 6. Redact piped secrets: echo "secret" | command
    //    The secret value inside quotes is replaced with [REDACTED]
    result = PIPED_SECRET_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let full_match = caps.get(0).unwrap().as_str();
            let secret = caps.get(1).unwrap().as_str();
            full_match.replace(secret, "[REDACTED]")
        })
        .into_owned();

    // 5b. Redact lowercase / query-param sensitive assignments that
    //     ENV_SECRET_RE (uppercase-only) misses: `?api_key=sk-xxx`,
    //     `&token=...`, `password="..."`. These appear in provider-error
    //     URLs surfaced to channels and the TUI; a short, unknown-prefix
    //     token would otherwise slip past every other pass.
    result = KEYVAL_SECRET_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let key = caps.get(1).unwrap().as_str();
            format!("{key}=[REDACTED]")
        })
        .into_owned();

    // 5b2. Redact labeled credentials: `Password: value`, `Token: value`, etc.
    //      Unlike KEYVAL_SECRET_RE (which matches `=`), this catches `:` separator
    //      used in structured text, markdown, and shell-style credential lists.
    result = LABELED_CRED_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let key = caps.get(1).unwrap().as_str();
            // Don't redact if the value looks like a key prefix fragment
            // left over from earlier redaction passes (short alphanumeric
            // blobs like `eyJ`, `xoxb-`, `shpat_`). Real credentials are
            // almost always 8+ chars. This prevents double-redacting values
            // already handled by KEY_PREFIXES or the mixed-alnum pass.
            if let Some(val) = caps.get(2) {
                let v = val.as_str();
                if v.len() < 8 {
                    return caps.get(0).unwrap().as_str().to_string();
                }
            }
            format!("{key}: [REDACTED]")
        })
        .into_owned();

    // 5c. Redact credentials embedded in a URL authority
    //     (`scheme://user:PASSWORD@host`). Keeps the user and host so the
    //     URL stays recognizable.
    result = URL_PASSWORD_RE
        .replace_all(&result, |caps: &regex::Captures| {
            format!(
                "{}[REDACTED]{}",
                caps.get(1).unwrap().as_str(),
                caps.get(3).unwrap().as_str()
            )
        })
        .into_owned();

    // 7. Redact IPv4 addresses (server IPs, infrastructure addresses).
    //    Keeps 127.0.0.1 and 0.0.0.0 as they are non-sensitive.
    result = IPV4_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let ip = caps.get(1).unwrap().as_str();
            if ip == "127.0.0.1" || ip == "0.0.0.0" {
                ip.to_string()
            } else {
                "[IP_REDACTED]".to_string()
            }
        })
        .into_owned();

    result
}

/// Strip LLM-hallucinated artifacts from text before external delivery.
///
/// Removes:
/// - HTML comments (`<!-- tools-v2: ... -->`, `<!-- lens -->`, etc.)
/// - XML tool-call blocks (`<tool_call>`, `<tool_code>`, `<minimax:tool_call>`, etc.)
/// - Cursor / Aider / Cline-style `CODE_EDIT_BLOCK` fenced blocks that
///   some fine-tuned models (qwen-3.7-max-thinking observed 2026-05-30
///   14:13) emit as text instead of calling `edit_file`. They look
///   like ```` ```lang|CODE_EDIT_BLOCK|/abs/path/to/file ```` and leak
///   the full file contents to whichever channel renders the
///   response.
///
/// Use this on any text sent to Telegram, HTTP webhooks, or other external channels.
/// Move persisted `<!-- reasoning -->…<!-- /reasoning -->` blocks out of a
/// stored assistant body. Returns the body with those spans removed and the
/// concatenated reasoning text (`None` when there was none).
///
/// `preserve_thinking` models (Qwen Model Studio / DashScope, Moonshot kimi)
/// require the reasoning returned as the separate `reasoning_content` field and
/// reject it inside `content` (#654). [`strip_llm_artifacts`] only removes the
/// marker tags via `strip_html_comments`, so the reasoning text otherwise
/// survives as plain content on reload — feeding the model its own reasoning as
/// content and making it spill fresh chain-of-thought into the answer. Hoisting
/// it into the message `thinking` column lets `from_db_messages` rehydrate it as
/// a leading `ContentBlock::Thinking`.
pub fn hoist_reasoning_blocks(content: &str) -> (String, Option<String>) {
    static REASONING_BLOCK_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?s)<!-- reasoning -->(.*?)<!-- /reasoning -->").unwrap());
    static MULTI_BLANK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{3,}").unwrap());

    if !content.contains("<!-- reasoning -->") {
        return (content.to_string(), None);
    }

    let mut parts: Vec<String> = Vec::new();
    for cap in REASONING_BLOCK_RE.captures_iter(content) {
        if let Some(inner) = cap.get(1) {
            let trimmed = inner.as_str().trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }

    let cleaned = REASONING_BLOCK_RE.replace_all(content, "");
    let cleaned = MULTI_BLANK.replace_all(cleaned.trim(), "\n\n").to_string();
    let reasoning = if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    };
    (cleaned, reasoning)
}

/// Remove every `<!-- phantom_blocked=1 -->` … `<!-- /phantom_blocked=1 -->`
/// section from message content (#1172).
///
/// Phantom-blocked iterations are persisted to the DB under these paired
/// markers so a misclassified deliverable stays recoverable to humans — but
/// that same text re-entering LLM context as assistant history is what turned
/// one Telegram session into 34 piled-up phantom entries (#86): the model
/// reads its own discarded narrations and emits fresh ones from them.
/// Callers rebuilding LLM context from DB rows strip whole flagged sections;
/// TUI/CLI replay paths keep them, since there the content drives
/// human-visible history.
///
/// An unpaired open marker strips everything to the end of the content (a
/// truncated write must not leak the tail back into context). Content outside
/// flagged sections passes through untouched.
pub fn strip_phantom_blocked(content: &str) -> String {
    const OPEN: &str = "<!-- phantom_blocked=1 -->";
    const CLOSE: &str = "<!-- /phantom_blocked=1 -->";
    if !content.contains(OPEN) {
        return content.to_string();
    }
    let mut result = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find(OPEN) {
        result.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(close_rel) => {
                // Cut marker + body + closing marker, then one following
                // newline so consecutive blocks don't stack blank lines.
                let after = &rest[start + close_rel + CLOSE.len()..];
                rest = after.strip_prefix('\n').unwrap_or(after);
            }
            None => {
                // Unpaired open: treat the tail as blocked (#86 fail-safe).
                rest = "";
            }
        }
    }
    result.push_str(rest);
    result
}

pub fn strip_llm_artifacts(text: &str) -> String {
    use crate::brain::agent::service::AgentService;

    let mut result = text.to_string();
    // Raw ANSI escapes FIRST (#1719): a single ESC byte in model/tool output
    // means whatever follows is terminal grammar, not prose. Structured
    // styling happens in widgets; inline escapes are always garbage here and
    // get executed by the terminal when the span buffer is drawn.
    if result.contains('\x1b') {
        result = ANSI_CSI_RE.replace_all(&result, "").into_owned();
        result = ANSI_OSC_RE.replace_all(&result, "").into_owned();
        result = ANSI_ESC_SINGLE_RE.replace_all(&result, "").into_owned();
    }
    // Qwen 3 / DeepSeek SentencePiece-style tool-call tokens.
    // The streaming filter in custom_openai_compatible.rs catches
    // these in real time, but a chunk-boundary near a marker can
    // briefly leak one through, and the format may grow new
    // variants the streaming filter doesn't know yet. This regex
    // sweeps anything matching `<|tool<sep>...|>` where <sep> is
    // either U+2581 (the real SP word-boundary char Qwen emits) or
    // an ASCII underscore (emitted by some quantizations). Matches
    // the open/close tag family in one pass: section_begin,
    // section_end, call_begin, call_end, call_argument_begin, etc.
    if result.contains("<|tool") {
        result = QWEN_TOOL_MARKER_RE.replace_all(&result, "").into_owned();
    }
    // Reasoning blocks go FIRST, before the generic comment sweep (#903).
    //
    // `<!-- reasoning -->` and `<!-- /reasoning -->` are two separate
    // self-closed comments, so `strip_html_comments` deleted the markers and
    // left the prose between them standing as ordinary message text. The
    // agent's own deliberation therefore shipped to Telegram and the TUI as if
    // it were the answer — and, being written in the past tense about work
    // done, it fed fabricated-sounding claims into the phantom detectors.
    //
    // `hoist_reasoning_blocks` already removes the whole block correctly; it
    // was only ever called on the context-rebuild path (`tool_loop.rs:745`),
    // never on delivery. Here the hoisted reasoning is discarded rather than
    // returned: this function's contract is "text safe to show a user", and
    // callers that need the reasoning already call hoist directly.
    if result.contains("<!-- reasoning -->") {
        result = hoist_reasoning_blocks(&result).0;
    }
    if result.contains("<!--") {
        result = AgentService::strip_html_comments(&result);
    }
    if result.contains("CODE_EDIT_BLOCK") {
        result = strip_code_edit_block_fences(&result);
    }
    // Strip `<thinking>` / `<think>` / `<reasoning>` blocks that models
    // emit inline. Some providers (e.g. mimo) promote these to
    // ReasoningDelta, but chunk boundaries can leak raw tags into the
    // response text.
    //
    // `<thinking>` needs its own gate: it is not a superstring match for
    // `<think>` (the byte after `<think` is `i`, not `>`), so for as long
    // as only the shorter form was checked this variant passed through
    // untouched on every provider.
    if result.contains("<thinking>") {
        result = strip_thinking_tags(&result);
    }
    if result.contains("<think>") {
        result = strip_think_tags(&result);
    }
    if result.contains("<reasoning>") {
        result = strip_reasoning_tags(&result);
    }
    // Two strip paths:
    //   1. Matched-pair JSON-bearing tool-call blocks — only stripped
    //      when parse_xml_tool_calls confirms there's real call JSON
    //      inside, so prose like "we fixed the <tool_call> bug" survives.
    //   2. Orphan close tags (`</tool_result>`, `</tool_call>`, etc.) —
    //      always stripped because models routinely emit them alone when
    //      the opener was eaten by an earlier pass or never produced.
    //      2026-05-28 user report: `</tool_result>` rendered visibly
    //      between paragraphs in the TUI.
    if AgentService::has_xml_tool_block(&result) {
        let parsed = AgentService::parse_xml_tool_calls(&result);
        if !parsed.is_empty() {
            result = AgentService::strip_xml_tool_calls(&result);
        }
    }
    // Orphan-close pass runs unconditionally — it's safe because it
    // only matches close tags that are alone on their line (with
    // optional surrounding whitespace).
    if result.contains("</tool_result>")
        || result.contains("</tool_call>")
        || result.contains("</tool_use>")
        || result.contains("</invoke>")
        || result.contains("</function_calls>")
        || result.contains("</qwen:tool_call>")
        || result.contains("</minimax:tool_call>")
    {
        result = AgentService::strip_xml_tool_calls(&result);
    }
    // Typographic dash normalization (#1745): prompt rules do not bind
    // model output reliably, so em-dashes and en-dashes are normalized
    // mechanically at this one boundary every channel already funnels
    // through before display.
    if result.contains(['\u{2014}', '\u{2013}']) {
        result = normalize_dashes(&result);
    }
    result
}

/// Normalize typographic dashes in assistant prose to plain ASCII (#1745).
///
/// Em-dash: the spaced form collapses to a bare colon ("want \u{2014} the"
/// becomes "want: the"); the other arrangements map to ":" as well. En-dash
/// becomes a plain hyphen so ranges stay natural ("1\u{2013}3" becomes
/// "1-3"). Order matters: the spaced forms must run before the glued
/// fallback.
///
/// Table-row exemption (#1969): a line whose first non-blank char is `|` is
/// a markdown table row, and models write em dashes there as the empty-cell
/// marker ("| \u{2014} |"). Colonizing a cell corrupts the table's content
/// while the structure survives, which reads as silent garble (real case:
/// Ops cherry-pick table, rows #757/#697, 2026-10-06). On table rows both
/// dash kinds collapse to a plain hyphen-minus instead; every other line
/// keeps the original chain. Splitting is newline-inclusive so trailing
/// newlines and CRLF bytes survive byte-for-byte.
pub(crate) fn normalize_dashes(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.trim_start().starts_with('|') {
                line.replace(['\u{2014}', '\u{2013}'], "-")
            } else {
                line.replace(" \u{2014}", ":")
                    .replace("\u{2014} ", ": ")
                    .replace('\u{2014}', ":")
                    .replace('\u{2013}', "-")
            }
        })
        .collect()
}

/// Strip a matched `<tag>...</tag>` block, and handle an unclosed opener
/// by POSITION rather than by deleting the rest of the message.
///
/// The old per-tag copies dropped everything from an unclosed opener
/// onward. That is right for a stream cut mid-reasoning, where the
/// opener sits at the very start and every byte after it is reasoning.
/// It is destructive anywhere else: a message that merely *mentions* a
/// tag in prose ("the sanitizer gates on `<think>`") lost every
/// character after the mention, silently, with no error and no log
/// line. Three live repros in one chat thread, each eating the rest of
/// a delivered answer.
///
/// So: a leading opener still strips the remainder, a mid-text opener
/// drops only the tag token. This matches the policy the sibling
/// `<!-- reasoning -->` path already settled on, pinned by
/// `sanitize_reasoning_leak_test::an_unclosed_marker_does_not_eat_the_answer`.
///
/// Taking `close` as a parameter also removes a hand-counted length: the
/// `<reasoning>` copy advanced by 13 for a 12-byte close tag and ate the
/// first character after every block it stripped.
fn strip_tag_block(text: &str, open: &str, close: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(start) = remaining.find(open) {
        result.push_str(&remaining[..start]);
        if let Some(end) = remaining[start..].find(close) {
            remaining = &remaining[start + end + close.len()..];
        } else if result.trim().is_empty() {
            // Opener leads the message: a truncated reasoning stream.
            remaining = "";
        } else {
            // Opener appears after real text: prose, not reasoning.
            // Drop the token, keep everything the user wrote.
            remaining = &remaining[start + open.len()..];
        }
    }
    result.push_str(remaining);
    result.trim().to_string()
}

/// Strip `<think>...</think>` blocks from LLM output.
pub(crate) fn strip_think_tags(text: &str) -> String {
    strip_tag_block(text, "<think>", "</think>")
}

/// Strip `<thinking>...</thinking>` blocks from LLM output.
///
/// A separate pass from [`strip_think_tags`] because the two never
/// overlap: `find("<think>")` cannot match inside `<thinking>` (the
/// byte after `<think` is `i`, not `>`), so the shorter stripper was
/// blind to this form and it reached users verbatim. CLI providers
/// serialize prior reasoning back into the prompt in exactly this
/// shape (`claude_cli.rs`, `codex_cli.rs`, `opencode_cli.rs`,
/// `command_code_cli.rs`), so models read it in their own history and
/// reproduce it as plain output text.
pub(crate) fn strip_thinking_tags(text: &str) -> String {
    strip_tag_block(text, "<thinking>", "</thinking>")
}

/// Strip `<reasoning>...</reasoning>` blocks from LLM output.
pub(crate) fn strip_reasoning_tags(text: &str) -> String {
    strip_tag_block(text, "<reasoning>", "</reasoning>")
}

/// Strip `CODE_EDIT_BLOCK`-style fenced blocks the LLM emits as text
/// when it confuses a Cursor/Aider/Cline IDE edit format with an
/// actual tool call.
///
/// Pattern:
/// ````text
/// ```<language>|CODE_EDIT_BLOCK|<absolute path>
/// <file contents -- often hundreds of lines>
/// ```
/// ````
///
/// Replace the whole block (header line + body + closing fence) with
/// a single short notice so the user knows the agent TRIED to edit a
/// file but used the wrong format — and so the chat doesn't leak the
/// full file contents through Telegram / Slack / Discord etc. The
/// notice mentions the path so the user can verify nothing weird was
/// attempted.
///
/// Why not silently drop the path too: the path is the one piece of
/// signal worth keeping (the user can audit what file the agent
/// thought it was touching). The body is just regurgitated file
/// content with zero new information.
pub(crate) fn strip_code_edit_block_fences(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if is_code_edit_block_open(line) {
            let path = extract_code_edit_block_path(line).unwrap_or("(unknown)");
            // Skip ahead to the closing ``` fence. If the model
            // forgot the closer (truncated stream) we still drop the
            // rest of the input so partial file contents don't leak.
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim_start().starts_with("```") {
                j += 1;
            }
            out.push(format!(
                "[Agent attempted to edit `{path}` using an unsupported \
                 inline-edit format. The change was NOT applied — agent \
                 should retry via the `edit_file` tool.]"
            ));
            // j points at the closing ```; skip past it (or to EOF
            // if the closer was missing).
            i = j.saturating_add(1);
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    out.join("\n")
}

/// True for a line that opens a `CODE_EDIT_BLOCK` fenced block.
/// Tolerant of leading whitespace and unknown language tags — the
/// `CODE_EDIT_BLOCK` marker is the load-bearing piece.
fn is_code_edit_block_open(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") && trimmed.contains("CODE_EDIT_BLOCK")
}

/// Extract the path component from a `CODE_EDIT_BLOCK` opener line.
/// Format: ` ```<lang>|CODE_EDIT_BLOCK|<path>`. Returns None when
/// the path slot is missing.
fn extract_code_edit_block_path(line: &str) -> Option<&str> {
    let after = line.split("CODE_EDIT_BLOCK").nth(1)?;
    let path = after.trim_start_matches('|').trim();
    if path.is_empty() { None } else { Some(path) }
}

/// Redact values inside a headers object for known sensitive header names.
fn redact_headers_object(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                let redacted = if is_sensitive_key(k) {
                    Value::String("[REDACTED]".to_string())
                } else {
                    v.clone()
                };
                out.insert(k.clone(), redacted);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}
