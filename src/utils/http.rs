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
//!
//! Two settings cannot ride on a request, because reqwest keeps them on the
//! client: the redirect policy and the pool tuning. For those the tree uses
//! [`cached_client`] with its own slot, so the build still happens once, and
//! the model providers use [`streaming_transport`] and [`buffered_transport`],
//! which carry the pool and keepalive tuning their streams depend on.

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
    // Each #[tokio::test] runs its own runtime, and a reqwest pool born in one
    // runtime driven by another fails every request outright. Production has
    // one daemon runtime, so a single process-wide client is correct there;
    // under test a fresh client per call sidesteps the runtime mismatch wholly.
    #[cfg(test)]
    {
        Box::leak(Box::new(build_shared()))
    }
    #[cfg(not(test))]
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    #[cfg(not(test))]
    CLIENT.get_or_init(build_shared)
}

fn build_shared() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        // A build failure would take every caller down at once, so keep
        // going with the default client and leave the reason in the logs.
        .unwrap_or_else(|e| {
            tracing::error!("shared HTTP client build failed ({e:#}); using the default");
            reqwest::Client::new()
        })
}

/// Build a client once and hand the same one back on every later call.
///
/// For the settings reqwest keeps on the client rather than the request, such
/// as the redirect policy. The slot belongs to the caller, so two callers that
/// need different policies get different clients, and each one still pays the
/// root-store load a single time.
///
/// ```
/// static GUARDED: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
/// let client = crate::utils::http::cached_client(&GUARDED, |b| {
///     b.redirect(reqwest::redirect::Policy::none())
/// });
/// ```
pub fn cached_client(
    slot: &'static OnceLock<reqwest::Client>,
    configure: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
) -> &'static reqwest::Client {
    slot.get_or_init(|| {
        configure(reqwest::Client::builder())
            .build()
            // Same call as `shared()`: a build failure must not take the
            // process down, and the reason belongs in the logs.
            .unwrap_or_else(|e| {
                tracing::error!("cached HTTP client build failed ({e:#}); using the default");
                reqwest::Client::new()
            })
    })
}

/// Connect budget for a model provider call. A handshake that cannot finish in
/// this window is a dead host, and failing here lets the caller retry the next
/// candidate instead of holding a half-open socket.
const TRANSPORT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an idle connection stays in the pool before it is closed.
const TRANSPORT_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Idle connections kept per host. Model traffic goes to a handful of hosts
/// with long gaps between calls, so a deep pool costs sockets nobody reuses.
const TRANSPORT_IDLE_PER_HOST: usize = 2;

/// OS-level keepalive probes. A stream whose peer vanished is detected in tens
/// of seconds instead of waiting out the request ceiling (#1687).
const TRANSPORT_TCP_KEEPALIVE: Duration = Duration::from_secs(15);

/// Apply the pool and keepalive tuning every provider transport shares.
fn provider_tuning(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    builder
        .connect_timeout(TRANSPORT_CONNECT_TIMEOUT)
        .pool_idle_timeout(TRANSPORT_POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(TRANSPORT_IDLE_PER_HOST)
        .tcp_keepalive(TRANSPORT_TCP_KEEPALIVE)
}

/// The client a provider streams with: pool and keepalive tuning, and
/// deliberately NO total request ceiling.
///
/// reqwest's `.timeout()` bounds the whole exchange INCLUDING the body read,
/// so on an SSE stream it is a wall-clock guillotine on a healthy response
/// (#1687). A dead stream is the app-level inter-chunk guard's job, and a
/// handshake that never lands is still bounded by the connect timeout.
///
/// Built once for the whole process: every provider used to build its own, and
/// each build parsed the root store again.
///
/// The reference form exists so a test can prove two calls hand back the same
/// client instead of two that merely look alike.
pub(crate) fn streaming_transport_ref() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    cached_client(&CLIENT, provider_tuning)
}

/// [`streaming_transport_ref`] as an owned handle, which is what a provider
/// stores in its struct.
pub fn streaming_transport() -> reqwest::Client {
    streaming_transport_ref().clone()
}

/// The client a provider uses for a call that buffers its whole body: title
/// generation, compaction and `/models`. Same tuning as [`streaming_transport`]
/// plus a total ceiling.
///
/// Cached per ceiling value, because providers resolve that ceiling from
/// config, and `with_timeout` used to rebuild the client, and reload the root
/// store, every time it was called.
pub fn buffered_transport(timeout: Duration) -> reqwest::Client {
    buffered_transport_cached(timeout).as_ref().clone()
}

/// The cached entry [`buffered_transport`] hands out.
///
/// Shared by `Arc` rather than by value so the test can compare identities:
/// two clients built for the same ceiling are the same allocation, and the
/// pointer comparison is what proves it.
pub(crate) fn buffered_transport_cached(timeout: Duration) -> std::sync::Arc<reqwest::Client> {
    use std::sync::{Arc, Mutex};

    // One cache entry: the ceiling it was built for, and the client.
    type CeilingClient = (Duration, Arc<reqwest::Client>);

    static CLIENTS: OnceLock<Mutex<Vec<CeilingClient>>> = OnceLock::new();
    let cache = CLIENTS.get_or_init(|| Mutex::new(Vec::new()));
    let mut entries = cache.lock().unwrap_or_else(|poison| poison.into_inner());

    if let Some((_, client)) = entries.iter().find(|(value, _)| *value == timeout) {
        return Arc::clone(client);
    }

    let client = Arc::new(
        provider_tuning(reqwest::Client::builder())
            .timeout(timeout)
            .build()
            .unwrap_or_else(|e| {
                tracing::error!("provider HTTP client build failed ({e:#}); using the default");
                reqwest::Client::new()
            }),
    );
    entries.push((timeout, Arc::clone(&client)));
    client
}
