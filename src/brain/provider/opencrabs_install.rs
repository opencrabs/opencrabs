//! Install-token client for the built-in OpenCrabs provider (#2047).
//!
//! Each install generates a random ID once and asks the inference proxy for
//! its own caller token. This module is pure: it builds the request, reads the
//! answer, and names each refusal. The HTTP call and the storage live
//! elsewhere, so every branch here is testable without a network.

use serde_json::{Value, json};

/// Proxy base URL. The install route and the chat route both live under it.
pub const OPENCRABS_BASE_URL: &str = "https://inference.opencrabs.com/v1";

/// Context window of the built-in provider, in tokens.
///
/// Fixed on purpose. The endpoint is free and serves one model with a 200k
/// window, so the budget never exceeds this and `providers.opencrabs.context_window`
/// is not honoured. The key is dropped before the generic provider setup reads
/// it, so it is inert in both directions rather than clamped (#2066).
pub const OPENCRABS_CONTEXT_WINDOW: u32 = 200_000;

/// Host of the built-in inference endpoint. Used to recognise the endpoint
/// when it is reached under a provider name other than `opencrabs`.
const OPENCRABS_HOST: &str = "inference.opencrabs.com";

/// The host `base_url` addresses, lowercased, with the scheme, any userinfo,
/// the port, the path, the query and the fragment removed.
///
/// Returns `None` when the input is empty or does not parse as a URL. A bare
/// `host/path` is accepted by supplying a scheme, because `base_url` is
/// configured without one in some setups.
fn host_of(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Without this, `inference.opencrabs.com:8080/v1` parses with
    // `inference.opencrabs.com` as the SCHEME and no host at all.
    let with_scheme;
    let candidate = if trimmed.contains("://") {
        trimmed
    } else {
        with_scheme = format!("https://{trimmed}");
        &with_scheme
    };
    url::Url::parse(candidate)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
}

/// True when `base_url` points at the built-in inference endpoint.
///
/// Deliberately a host check and not a provider-name check, mirroring
/// `is_official_host` in `crate::config::wire_dialect`: a `[providers.custom.*]`
/// section that copies the install token reaches the same endpoint under a
/// different name, and the fixed window has to hold for it too (#2067).
///
/// The comparison is the parsed HOST against [`OPENCRABS_HOST`], never a
/// substring of the whole URL (#2234). Five call sites hang off this answer,
/// including the retry ladder and both voice routes, so a URL that merely
/// carries the string in its query, its path or as a prefix of another domain
/// must not be treated as the built-in endpoint.
pub fn is_opencrabs_endpoint(base_url: Option<&str>) -> bool {
    base_url
        .and_then(host_of)
        .is_some_and(|host| host == OPENCRABS_HOST)
}

/// Path of the install route, relative to [`OPENCRABS_BASE_URL`].
pub const INSTALL_PATH: &str = "/install";

/// Length of a generated install ID, in hex characters. The proxy accepts 8 to
/// 64 characters of `[A-Za-z0-9_-]`, so 32 hex characters sit well inside it.
const INSTALL_ID_LEN: usize = 32;

/// A caller token the proxy issued for one install.
#[derive(Clone, PartialEq, Eq)]
pub struct IssuedToken {
    pub token: String,
    pub label: String,
}

/// The token is a bearer secret, so `Debug` never prints it.
impl std::fmt::Debug for IssuedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedToken")
            .field("token", &"<redacted>")
            .field("label", &self.label)
            .finish()
    }
}

/// Why the proxy did not issue a token. Each variant maps to its own message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// The ID is malformed. Our own generator never produces one, so this is a bug.
    IdRejected,
    /// A token already exists for this ID. The stored token was lost, so the
    /// caller asks for a reissue against the same ID rather than a new one.
    AlreadyIssued,
    /// The proxy has turned issuance off.
    IssuanceOff,
    /// The proxy is rate limiting this address.
    RateLimited,
    /// The proxy failed while issuing.
    ProxyFailed,
    /// The proxy answered with a status we do not expect.
    Unexpected(u16),
    /// The success body did not carry a token.
    MalformedReply,
    /// The proxy could not be reached at all: DNS, connection or timeout.
    Unreachable,
    /// The identity or its token could not be written on this machine.
    StorageFailed,
}

/// Generate a fresh install ID: 32 random hex characters.
pub fn new_install_id() -> String {
    let mut id = String::with_capacity(INSTALL_ID_LEN);
    while id.len() < INSTALL_ID_LEN {
        id.push_str(&format!("{:016x}", rand::random::<u64>()));
    }
    id.truncate(INSTALL_ID_LEN);
    id
}

/// Whether an ID has the shape the proxy accepts: 8 to 64 characters of
/// `[A-Za-z0-9_-]`. Checked before a request so a bad stored ID is caught early.
pub fn is_valid_install_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The JSON body for the install route.
pub fn install_request_body(install_id: &str) -> Value {
    json!({ "install_id": install_id })
}

/// The JSON body that asks the proxy to reissue a token for a known ID (#2198).
///
/// The ID is the proof of ownership: a machine that still holds it is the same
/// install that was issued the lost token. The extra field is additive, so a
/// proxy that predates reissue ignores it and answers `409` exactly as before,
/// which is what the acquire loop already handles.
pub fn reissue_request_body(install_id: &str) -> Value {
    json!({ "install_id": install_id, "reissue": true })
}

/// Read the proxy's answer to the install route.
///
/// A `200` must carry a non-empty `token`. Every other status maps to an
/// [`InstallError`], so the caller never has to read status codes itself.
pub fn parse_install_response(status: u16, body: &str) -> Result<IssuedToken, InstallError> {
    match status {
        200 => {
            let value: Value =
                serde_json::from_str(body).map_err(|_| InstallError::MalformedReply)?;
            let token = value
                .get("token")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .ok_or(InstallError::MalformedReply)?;
            let label = value
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(IssuedToken {
                token: token.to_string(),
                label: label.to_string(),
            })
        }
        400 => Err(InstallError::IdRejected),
        403 | 404 => Err(InstallError::IssuanceOff),
        409 => Err(InstallError::AlreadyIssued),
        429 => Err(InstallError::RateLimited),
        500..=599 => Err(InstallError::ProxyFailed),
        other => Err(InstallError::Unexpected(other)),
    }
}

/// The message shown to the user for a refusal. Each cause gets its own text,
/// so a user can tell a dead proxy from a refused token from a closed route.
pub fn install_error_message(err: &InstallError) -> &'static str {
    match err {
        InstallError::IdRejected => {
            "The built-in OpenCrabs provider could not register this install: the install ID was rejected. Please report this."
        }
        InstallError::AlreadyIssued => {
            "The built-in OpenCrabs provider refused to reissue this install's token. Please report this."
        }
        InstallError::IssuanceOff => {
            "The built-in OpenCrabs provider is not issuing tokens right now. Choose another provider in Advanced setup."
        }
        InstallError::RateLimited => {
            "The built-in OpenCrabs provider is rate limiting this connection. Wait a minute and try again."
        }
        InstallError::ProxyFailed => {
            "The built-in OpenCrabs provider failed while issuing a token. Try again later or choose another provider."
        }
        InstallError::Unexpected(_) => {
            "The built-in OpenCrabs provider gave an unexpected answer. Try again later or choose another provider."
        }
        InstallError::MalformedReply => {
            "The built-in OpenCrabs provider sent a reply without a token. Try again later or choose another provider."
        }
        InstallError::Unreachable => {
            "Could not reach the built-in OpenCrabs provider. Check your connection, or choose another provider."
        }
        InstallError::StorageFailed => {
            "The built-in OpenCrabs provider could not save its install identity on this machine. Check the OpenCrabs home directory permissions and try again."
        }
    }
}
