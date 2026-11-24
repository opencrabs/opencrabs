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

/// The test build hands back a fresh client per call: each `#[tokio::test]`
/// runs its own runtime, and a pool born in one runtime driven by another
/// fails every request outright. The production contract (one `OnceLock`
/// client per process) cannot hold under `cfg(test)`, so it is verified by
/// source scan instead, and the runtime check pins the test-side contract.
#[test]
fn shared_client_matches_its_build_contract() {
    // Test-side contract: distinct clients per call, so no pool ever crosses
    // a runtime boundary between two tests.
    let first = crate::utils::http::shared() as *const reqwest::Client;
    let second = crate::utils::http::shared() as *const reqwest::Client;
    assert_ne!(first, second, "shared() under test must hand back distinct clients");

    // Production-side contract: the process-wide OnceLock still exists and
    // wraps build_shared, so the root store loads exactly once per daemon.
    let source = read_src("src/utils/http.rs");
    assert!(
        source.contains("#[cfg(not(test))]\n    static CLIENT: OnceLock<reqwest::Client>"),
        "production shared() no longer uses a process-wide OnceLock; the CA bundle would load per call"
    );
    assert!(
        source.contains("CLIENT.get_or_init(build_shared)"),
        "production shared() no longer initializes the OnceLock with build_shared"
    );
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

/// Walk the tree so the invariant is checked against the whole crate, not a
/// hand-listed sample that a new module would slip past.
fn rust_sources_under(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => panic!("read dir {}: {e}", dir.display()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources_under(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The acceptance criterion from #3038, as a test instead of a grep somebody
/// has to remember to run. `src/utils` is the accessor itself plus the inbound
/// downloader, which owns a client on purpose (see its module docs).
#[test]
fn no_module_outside_utils_builds_its_own_client() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    rust_sources_under(&root, &mut sources);
    assert!(
        sources.len() > 100,
        "walk found only {} files",
        sources.len()
    );

    let mut offenders = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        if relative.starts_with("src/tests/") || relative.starts_with("src/utils/") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        if source.contains("reqwest::Client::new()")
            || source.contains("reqwest::Client::builder()")
        {
            offenders.push(relative);
        }
    }

    assert!(
        offenders.is_empty(),
        "these modules build a reqwest client per call and reload the CA bundle: {offenders:#?}"
    );
}

/// The search fan-out is the path the issue named first: three engines, one
/// client build each, per query.
#[test]
fn the_search_engines_borrow_the_shared_client() {
    for file in [
        "src/brain/tools/brave_search.rs",
        "src/brain/tools/serper_search.rs",
        "src/brain/tools/exa_search.rs",
    ] {
        let source = read_src(file);
        assert!(
            !source.contains("Client::builder()") && !source.contains("Client::new()"),
            "{file} builds its own client again; every search reloads the CA bundle"
        );
        assert!(
            source.contains("crate::utils::http::"),
            "{file} no longer uses the shared client helpers"
        );
    }
}

/// The scheduler ticks on a timer and delivers through four separate call
/// sites, so it was the worst offender per unit of uptime.
#[test]
fn the_cron_scheduler_borrows_the_shared_client() {
    let source = read_src("src/cron/scheduler.rs");
    assert!(
        !source.contains("reqwest::Client::new()"),
        "the cron scheduler builds a client per delivery again"
    );
    assert!(
        source.contains("crate::utils::http::shared()"),
        "the cron scheduler no longer uses the shared client"
    );
}

/// Streams must keep their no-total-ceiling client and buffered calls must keep
/// their ceiling, but both now come from `utils::http` so the root store loads
/// once. A provider that reintroduces a local builder undoes that silently.
#[test]
fn provider_transports_come_from_utils_http() {
    for file in [
        "src/brain/provider/anthropic.rs",
        "src/brain/provider/gemini.rs",
        "src/brain/provider/custom_openai_compatible.rs",
    ] {
        let source = read_src(file);
        assert!(
            !source.contains("Client::builder()"),
            "{file} builds its own provider transport again"
        );
        assert!(
            source.contains("streaming_transport()"),
            "{file} no longer takes the stream transport from utils::http"
        );
        assert!(
            source.contains("buffered_transport("),
            "{file} no longer takes the buffered transport from utils::http"
        );
    }
}

/// `cached_client` is what lets a caller keep a client-level setting (a
/// redirect policy, a ceiling) without paying the build per call. Same slot
/// must mean same client.
#[test]
fn cached_client_reuses_one_client_per_slot() {
    static SLOT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let first = crate::utils::http::cached_client(&SLOT, |b| b) as *const reqwest::Client;
    let second = crate::utils::http::cached_client(&SLOT, |b| b) as *const reqwest::Client;
    assert_eq!(
        first, second,
        "cached_client built a second client for one slot"
    );
}

/// Providers resolve their ceiling from config, and `with_timeout` used to
/// rebuild on every call. One client per distinct ceiling, and no more.
#[test]
fn buffered_transport_is_cached_per_ceiling() {
    use std::time::Duration;

    let fifteen = Duration::from_secs(15);
    let first = crate::utils::http::buffered_transport_cached(fifteen);
    let second = crate::utils::http::buffered_transport_cached(fifteen);
    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "the same ceiling produced two clients; every call reloads the CA bundle"
    );

    let other = crate::utils::http::buffered_transport_cached(Duration::from_secs(300));
    assert!(
        !std::sync::Arc::ptr_eq(&first, &other),
        "two different ceilings shared one client, so one call would inherit the other's deadline"
    );
}

/// The stream transport is a process singleton: two providers must share one
/// pool and one root store, not one each.
#[test]
fn streaming_transport_is_a_process_singleton() {
    let first = crate::utils::http::streaming_transport_ref() as *const reqwest::Client;
    let second = crate::utils::http::streaming_transport_ref() as *const reqwest::Client;
    assert_eq!(
        first, second,
        "streaming_transport built a second client; providers no longer share one pool"
    );
}
