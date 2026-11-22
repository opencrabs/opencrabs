//! Client identification headers, resolved per gateway.
//!
//! Requests on the OpenAI-compatible path carry only `Authorization`,
//! `Content-Type` and `Accept`. `reqwest` sends no `User-Agent` of its own, so
//! until a provider opts in, OpenCrabs reaches a gateway anonymously. OpenCode
//! Zen is the first to act on that: it asked for a `User-Agent` and a stable
//! per-conversation `X-Opencode-Session`, and warned that requests without the
//! session header may be rejected (#1329).
//!
//! The same anonymity applies to every other gateway we talk to, so this
//! module resolves identity per host: z.ai (#1331), ModelScope (#1332),
//! Kimi/Moonshot (#1333) and DeepSeek (#1334), which users reach through a
//! custom endpoint since it has no first-class provider.
//!
//! ## Why this is per-vendor and not a default on the client
//!
//! A gateway fingerprints the header set it receives. [`super::qwen`]
//! documents how sending headers a provider does not expect drops us into a
//! stricter rate-limit bucket, and both User-Agents already in the codebase
//! (`QwenCode/...`, `GitHubCopilotChat/...`) deliberately impersonate other
//! clients for that reason. Setting `.user_agent(...)` on the shared client
//! would silently overwrite those, so identity is opt-in per host and
//! [`headers_for`] returns nothing for anything it does not recognise.

use uuid::Uuid;

/// OpenCode Zen's per-conversation header.
const OPENCODE_SESSION_HEADER: &str = "X-Opencode-Session";

/// The `User-Agent` OpenCrabs identifies itself with, e.g. `OpenCrabs/0.3.83`.
/// The version comes from the crate so a release never has to remember to bump
/// a second copy.
///
/// Unlike the Qwen and Copilot User-Agents, this one is honest: it names us
/// rather than impersonating another client. Gateways asking who is calling
/// (OpenCode's "Unknown client") want exactly this.
pub fn user_agent() -> String {
    format!("OpenCrabs/{}", crate::VERSION)
}

/// A gateway that has an identity contract with us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    /// Wants a User-Agent and a per-conversation session id (#1329).
    OpenCode,
    /// Documents `Accept-Language` in every request example (#1331).
    Zai,
    /// #1332.
    ModelScope,
    /// #1333.
    Kimi,
    /// #1334.
    DeepSeek,
    /// Our own inference endpoint (#3138). It asks for nothing beyond the
    /// User-Agent, and it is the one host whose gate depends on that answer:
    /// the proxy tells an OpenCrabs install from any other client by it.
    OpenCrabs,
}

/// The host of `base_url`, lowercased, with any scheme, userinfo, port and
/// path removed. Returns the input unchanged when it carries no scheme, which
/// is what makes a bare `host/path` still resolve.
fn host_of(base_url: &str) -> String {
    let rest = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let host = rest.split('/').next().unwrap_or(rest);
    let host = host.split('@').next_back().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    host.to_ascii_lowercase()
}

/// Whether `host` is `domain` itself or a subdomain of it. Suffix-matching on
/// a dot-prefixed domain is what stops `evil-opencode.ai.example.com` and
/// `notopencode.ai` from matching `opencode.ai`.
fn is_host(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

impl Vendor {
    /// The vendor serving `base_url`, matched on host alone.
    ///
    /// Host-only is deliberate. Model ids are not a safe signal here: a
    /// `deepseek-*` model is served by OpenRouter, NVIDIA and ModelScope too,
    /// so keying on the id would send DeepSeek-flavoured identity to an
    /// unrelated gateway, which is the mis-fingerprinting this module exists
    /// to avoid. `deepseek_reasoning::serves_deepseek` matches id-or-host
    /// because reasoning knobs travel with the model; identity does not.
    pub fn from_base_url(base_url: &str) -> Option<Self> {
        let host = host_of(base_url);
        // Every OpenCode catalogue variant a deployment configures
        // (opencode-kimi, opencode-qwen, ...) differs only in model list and
        // posts to this one host.
        if is_host(&host, "opencode.ai") {
            return Some(Self::OpenCode);
        }
        if is_host(&host, "z.ai") || is_host(&host, "bigmodel.cn") {
            return Some(Self::Zai);
        }
        // Both the documented .cn endpoint and the .ai host this deployment
        // is configured against.
        if is_host(&host, "modelscope.cn") || is_host(&host, "modelscope.ai") {
            return Some(Self::ModelScope);
        }
        if is_host(&host, "kimi.com") || is_host(&host, "moonshot.ai") {
            return Some(Self::Kimi);
        }
        if is_host(&host, "deepseek.com") {
            return Some(Self::DeepSeek);
        }
        // The endpoint we own. Every other entry here is a courtesy to a
        // gateway that asked; this one is the only host where the header is
        // also what the server uses to recognise us (#3138).
        if is_host(&host, "inference.opencrabs.com") {
            return Some(Self::OpenCrabs);
        }
        None
    }
}

/// A stable id for requests made outside any conversation, such as the
/// model-catalogue fetch that runs before a session exists. Held for the
/// process lifetime so those stay in one bucket instead of looking like a new
/// client on every call.
fn process_session_id() -> &'static str {
    use std::sync::OnceLock;
    static SESSION: OnceLock<String> = OnceLock::new();
    SESSION.get_or_init(|| Uuid::new_v4().to_string())
}

/// Identity headers for one request to `base_url`; empty for any host without
/// an identity contract.
///
/// `session` is the conversation the request belongs to. Where a gateway wants
/// it and it is absent, the stable per-process id stands in: a well-formed id
/// matters more to the gateway than an accurate one, and omitting the header
/// is the failure case.
pub fn headers_for(base_url: &str, session: Option<Uuid>) -> Vec<(String, String)> {
    let Some(vendor) = Vendor::from_base_url(base_url) else {
        return Vec::new();
    };

    // Every vendor here asked, directly or by documented example, to know who
    // is calling.
    let mut headers = vec![("User-Agent".to_string(), user_agent())];

    match vendor {
        Vendor::OpenCode => {
            let session = session
                .map(|s| s.to_string())
                .unwrap_or_else(|| process_session_id().to_string());
            headers.push((OPENCODE_SESSION_HEADER.to_string(), session));
            // The identity pair the OpenRouter block already sends, so
            // OpenCrabs names itself the same way on every gateway that asks.
            headers.push(("X-Title".to_string(), "OpenCrabs".to_string()));
            headers.push((
                "HTTP-Referer".to_string(),
                "https://opencrabs.com".to_string(),
            ));
        }
        // z.ai shows `Accept-Language` in every documented request example.
        // It selects the language of service-side messages, and English is
        // the right default for a surface whose logs and errors are read in
        // English; there is no locale setting to derive it from.
        Vendor::Zai => headers.push(("Accept-Language".to_string(), "en-US,en".to_string())),
        // These document only Authorization and Content-Type. The
        // User-Agent above is the whole contract, and for our own endpoint it
        // is the only one the proxy reads (#3138).
        Vendor::ModelScope | Vendor::Kimi | Vendor::DeepSeek | Vendor::OpenCrabs => {}
    }

    headers
}
