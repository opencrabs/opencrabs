//! A remote call must not pay for setup it does not need (measured 21 Nov).
//!
//! Every probe of every crab, every five seconds, used to build a Tokio runtime
//! and a `reqwest::Client`; every transcript fetch built another pair. The
//! client half was the expensive one: a fresh client means a fresh TLS
//! configuration, a fresh 180 KB CA parse and a fresh connection pool, which is
//! exactly the cost `utils::http::shared()` exists to remove (#2843). The
//! runtime half is cheaper but not free, and it was paid twice per switch
//! before a single byte went on the wire.
//!
//! The measured switch felt slow for reasons that had nothing to do with the
//! network: `session/list` answers in 10-25 ms over the tailnet, and the strip
//! still lagged. These tests pin the setup cost so it cannot creep back while
//! the latency work goes after the real cause.

use std::path::Path;

fn read_src(relative: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|e| panic!("read {relative}: {e}"))
}

/// The client module borrows the process-wide client. Building one here would
/// put a second CA load and a second pool in the path of every probe.
#[test]
fn the_remote_client_borrows_the_shared_http_client() {
    let source = read_src("src/remote/instance_client.rs");
    assert!(
        !source.contains("reqwest::Client::new()"),
        "instance_client builds its own client again; every probe reloads the CA bundle"
    );
    assert!(
        !source.contains("reqwest::Client::builder()"),
        "instance_client builds its own client again; every probe reloads the CA bundle"
    );
    assert!(
        source.contains("crate::utils::http::shared()"),
        "instance_client no longer borrows the shared client"
    );
}

/// The runtime is a `LazyLock`, so it is built on first use and then reused.
/// A `Builder::new_*().build()` inside a function body would mean one runtime
/// per call, which is the shape that made the strip slow.
#[test]
fn the_remote_runtime_is_built_once() {
    let source = read_src("src/remote/instance_client.rs");
    assert!(
        source.contains("LazyLock<tokio::runtime::Runtime>"),
        "the remote runtime is no longer a LazyLock; it is being rebuilt per call"
    );
    assert!(
        source.contains("pub fn block_on"),
        "the shared runtime has no entry point for non-async callers"
    );
}

/// `block_on` is a real door: it drives a future to completion and hands back
/// its value, twice in a row, on the same runtime.
#[test]
fn block_on_drives_a_future_and_returns_its_value() {
    let first: u32 =
        crate::remote::instance_client::block_on(async { Ok(41) }).expect("first call");
    let second: u32 =
        crate::remote::instance_client::block_on(async { Ok(first + 1) }).expect("second call");
    assert_eq!(second, 42);
}

/// The parsers are pure, but the transport is not: a `Target` with no scheme
/// must fail as an error and not as a panic, because the strip feeds it
/// whatever the owner typed into the pairing form.
#[test]
fn a_malformed_address_is_an_error_not_a_panic() {
    let target = crate::remote::instance_client::Target::new("nao-e-um-endereco", None);
    let result =
        crate::remote::instance_client::block_on(crate::remote::instance_client::probe(&target));
    assert!(
        result.is_err(),
        "a target with no scheme must fail, not hang"
    );
}
