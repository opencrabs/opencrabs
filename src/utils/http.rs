//! One HTTP client per process (#2843).
//!
//! Building a `reqwest::Client` is not free. Each build constructs a TLS
//! configuration and loads the native root store, so a 180 KB CA bundle is
//! parsed, ASN.1 decoded and stored again for every client. A profile taken
//! while the GUI booted put 43% of a tokio worker's samples inside
//! `X509_load_cert_crl_file_ex`, with the bundle open in 268 of 300 `/proc`
//! samples.
//!
//! A per-call timeout or user-agent never needed its own root store, so the
//! process keeps one client and callers carry those differences on the
//! request:
//!
//! ```ignore
//! let response = crate::utils::http::shared()
//!     .get(url)
//!     .timeout(std::time::Duration::from_secs(10))
//!     .header(reqwest::header::USER_AGENT, user_agent)
//!     .send()
//!     .await?;
//! ```
//!
//! The shared client sets no request timeout, on purpose: it is borrowed by
//! paths with very different deadlines, from a 10 s search to a 20 MB
//! attachment download, and a client-wide deadline would apply to all of them.
//! A caller that knows how long its request should take sets `.timeout(...)`
//! on the request.

use std::sync::OnceLock;
use std::time::Duration;

/// How long a TCP and TLS handshake may take. A host that cannot complete one
/// in this window is unreachable, and failing here leaves the retry loop free
/// to try the next candidate instead of holding a half-open socket.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The process-wide client, built on first use.
///
/// Also carries the connection pool: one client means connections are reused
/// across requests instead of a fresh pool, and a fresh DNS lookup, per call.
pub fn shared() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            // A build failure would take every caller down at once, so keep
            // going with the default client and leave the reason in the logs.
            .unwrap_or_else(|e| {
                tracing::error!("shared HTTP client build failed ({e:#}); using the default");
                reqwest::Client::new()
            })
    })
}
