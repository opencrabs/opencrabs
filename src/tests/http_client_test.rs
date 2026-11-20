//! One reqwest client per process (#2843).
//!
//! Every `reqwest::Client` build configures TLS from scratch and loads the
//! native root store, so a 180 KB CA bundle gets parsed, ASN.1 decoded and
//! stored again for each construction. A boot profile put 43% of a tokio
//! worker's samples inside `X509_load_cert_crl_file_ex`, with the bundle open
//! in 268 of 300 `/proc` samples.
//!
//! Two things keep that from coming back: the accessor must hand out one
//! client, and the paths that build in a burst or inside a retry loop must
//! borrow it instead of building their own.

use std::path::Path;

fn read_src(relative: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|e| panic!("read {relative}: {e}"))
}

/// The accessor is a real `OnceLock`: same client on every call, so the root
/// store loads once and one connection pool serves the whole process.
#[test]
fn shared_client_is_built_once() {
    let first = crate::utils::http::shared() as *const reqwest::Client;
    let second = crate::utils::http::shared() as *const reqwest::Client;
    assert_eq!(first, second, "shared() handed back two different clients");
}

/// The DuckDuckGo loop builds one request per attempt. It used to build a
/// client per attempt, so a single search that rotated through the agent list
/// paid the CA load once per try.
#[test]
fn web_search_builds_no_client_inside_its_retry_loop() {
    let source = read_src("src/brain/tools/web_search.rs");
    assert!(
        !source.contains("reqwest::Client"),
        "web_search constructs its own client again; every attempt reloads the CA bundle"
    );
    assert!(
        source.contains("crate::utils::http::shared()"),
        "web_search no longer uses the shared client"
    );
}

/// Provider model discovery runs in the GUI boot burst, which is where the
/// profile was taken.
#[test]
fn provider_model_fetch_borrows_the_shared_client() {
    let source = read_src("src/ui_core/model_selector/fetch.rs");
    assert!(
        !source.contains("reqwest::Client::new()"),
        "provider model fetch builds a fresh client; the boot burst pays the CA load per provider"
    );
    assert!(
        !source.contains("reqwest::Client::builder()"),
        "provider model fetch builds a fresh client; the boot burst pays the CA load per provider"
    );
    assert!(
        source.contains("crate::utils::http::shared()"),
        "provider model fetch no longer uses the shared client"
    );
}
