//! Release-asset fetch resilience (#2229).
//!
//! `/evolve` fetches the platform archive first and the 514 byte `SHA256SUMS`
//! second. A single dropped connection on that second request aborted an update
//! whose 46 MB archive had already come down over the same route, and then told
//! the user to build from source, which is wrong advice for a dropped packet.
//!
//! These drive the real fetch path against a loopback stub that drops
//! connections on purpose. The stub is reachable only because
//! `verify_asset_checksum_with_hosts` takes an injected allowlist; production
//! passes `ALLOWED_HOSTS`, which has no loopback entry.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::brain::tools::evolve::verify::{
    FETCH_ATTEMPTS, VerifyFailure, failure_message, get_with_retry,
    verify_asset_checksum_with_hosts,
};

/// The only host this file trusts: the loopback stub below.
const LOOPBACK: &[&str] = &["127.0.0.1"];

const ASSET: &str = "opencrabs-v0.5.14-aarch64-apple-darwin.tar.gz";

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// A one-route HTTP stub. The first `drop_first` connections are accepted and
/// closed without a response, which is what a dropped packet looks like to the
/// client. Every later connection gets `body` with a 200.
///
/// Returns the URL and a counter of accepted connections.
async fn spawn_stub(drop_first: usize, body: String) -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback stub");
    let addr = listener.local_addr().expect("stub local addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let n = counter.fetch_add(1, Ordering::SeqCst);
            if n < drop_first {
                // Close without answering: transport failure for the client.
                drop(stream);
                continue;
            }
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\
                     Connection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    (format!("http://127.0.0.1:{}/SHA256SUMS", addr.port()), hits)
}

fn release_pointing_at(url: &str) -> serde_json::Value {
    json!({
        "assets": [
            {"name": "SHA256SUMS", "browser_download_url": url},
        ]
    })
}

// ── retry on a dropped connection ───────────────────────────────────────────

#[tokio::test]
async fn a_dropped_checksum_connection_is_retried_and_verification_still_passes() {
    let archive = b"pretend this is the downloaded archive";
    let sums = format!("{}  {ASSET}\n", sha256_hex(archive));
    let (url, hits) = spawn_stub(1, sums).await;

    let result = verify_asset_checksum_with_hosts(
        &reqwest::Client::new(),
        &release_pointing_at(&url),
        ASSET,
        archive,
        LOOPBACK,
    )
    .await;

    assert_eq!(
        result,
        Ok(()),
        "one dropped connection must not abort verification of an archive that already downloaded"
    );
    assert!(
        hits.load(Ordering::SeqCst) >= 2,
        "the dropped connection must be retried, got {} attempt(s)",
        hits.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn a_checksum_fetch_that_never_connects_reports_the_attempt_count() {
    // Bind then drop, so the port is closed and every attempt is refused.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind then close");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);

    let err = get_with_retry(
        &reqwest::Client::new(),
        &format!("http://127.0.0.1:{port}/SHA256SUMS"),
    )
    .await
    .expect_err("a closed port must never produce a response");

    assert!(
        err.contains(&format!("after {FETCH_ATTEMPTS} attempts")),
        "the message must name the retry budget, got: {err}"
    );
}

#[tokio::test]
async fn a_transient_failure_points_at_evolve_instead_of_building_from_source() {
    let message = failure_message(&VerifyFailure::Transient(format!(
        "request failed after {FETCH_ATTEMPTS} attempts (last error: timed out)"
    )));

    assert!(
        message.contains("/evolve"),
        "a dropped packet must tell the user to retry, got: {message}"
    );
    assert!(
        !message.contains("Build from source"),
        "building from source is wrong advice for a transport failure, got: {message}"
    );
}

// ── integrity failures never retry and never soften ─────────────────────────

#[tokio::test]
async fn a_digest_mismatch_refuses_on_the_first_attempt_with_no_retry() {
    let sums = format!("{}  {ASSET}\n", "0".repeat(64));
    let (url, hits) = spawn_stub(0, sums).await;

    let failure = verify_asset_checksum_with_hosts(
        &reqwest::Client::new(),
        &release_pointing_at(&url),
        ASSET,
        b"bytes that do not match",
        LOOPBACK,
    )
    .await
    .expect_err("a wrong digest must refuse the swap");

    assert!(
        !failure.is_transient(),
        "an integrity failure is not retryable: {failure:?}"
    );
    let message = failure_message(&failure);
    assert!(
        message.contains("Refusing to update"),
        "integrity failures keep the hard refusal, got: {message}"
    );
    assert!(
        message.contains("Build from source instead"),
        "integrity failures keep the build-from-source advice, got: {message}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a digest mismatch must not burn the retry budget"
    );
}

#[tokio::test]
async fn an_off_list_host_is_refused_before_any_request_is_made() {
    let (url, hits) = spawn_stub(0, String::new()).await;

    let failure = verify_asset_checksum_with_hosts(
        &reqwest::Client::new(),
        &release_pointing_at(&url),
        ASSET,
        b"bytes",
        &["objects.githubusercontent.com"],
    )
    .await
    .expect_err("loopback is not on this allowlist");

    assert!(
        !failure.is_transient(),
        "an unexpected host is an integrity failure: {failure:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "an off-list host must be refused before the request is sent"
    );
}
