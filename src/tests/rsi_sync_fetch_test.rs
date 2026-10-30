//! Fetch classification and error rendering for the template sync (#2169).
//!
//! The defects these pin: one DNS flap failed every tracked file because
//! nothing retried, and the message the user saw was `reqwest`'s outer
//! wrapper, which names neither the cause nor a next step. Classification is
//! what decides whether a retry is allowed at all, so it is asserted here
//! rather than trusted by eye.

use crate::brain::rsi_sync::{
    FetchError, FileSyncResult, cause_chain, retry_fetch, summarize_sync_failures,
};

#[test]
fn transport_error_is_retryable() {
    let err = FetchError::Transport {
        detail: "dns error: failed to lookup address information".to_string(),
    };
    assert!(
        err.is_retryable(),
        "a fetch that never reached the server must be retried once"
    );
}

#[test]
fn body_error_is_retryable() {
    let err = FetchError::Body {
        detail: "connection closed before message completed".to_string(),
    };
    assert!(
        err.is_retryable(),
        "a body cut short is a transport problem, not an answer"
    );
}

#[test]
fn status_error_is_not_retryable() {
    let err = FetchError::Status { code: 404 };
    assert!(
        !err.is_retryable(),
        "a 404 is the server's answer; retrying only doubles the wait"
    );
}

#[test]
fn status_error_describe_names_the_code_and_the_next_step() {
    let msg = FetchError::Status { code: 404 }.describe();
    assert!(
        msg.contains("HTTP 404"),
        "the status code must survive into the message: {msg}"
    );
    assert!(
        !msg.contains(".md"),
        "the cause must not name a file: every file that hit it shares this \
         text, and the collapse names them once: {msg}"
    );
    assert!(
        msg.contains("code fix"),
        "the user needs to know a retry will not help: {msg}"
    );
}

#[test]
fn transport_describe_names_the_cause_and_the_retry() {
    let msg = FetchError::Transport {
        detail: "dns error: failed to lookup address information".to_string(),
    }
    .describe();
    assert!(
        msg.contains("dns error"),
        "the inner cause must survive into the message: {msg}"
    );
    assert!(
        msg.contains("retries"),
        "the user needs to know the next cycle retries: {msg}"
    );
}

// --- cause_chain: the outer wrapper alone is what hid the reason ---

#[derive(Debug)]
struct Leaf(&'static str);

impl std::fmt::Display for Leaf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for Leaf {}

#[derive(Debug)]
struct Wrapper(&'static str, Leaf);

impl std::fmt::Display for Wrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for Wrapper {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.1)
    }
}

#[test]
fn cause_chain_names_the_inner_error() {
    let err = Wrapper("error sending request for url (...)", Leaf("dns error"));
    let chain = cause_chain(&err);
    assert!(
        chain.contains("error sending request"),
        "the wrapper is kept so the context is not lost: {chain}"
    );
    assert!(
        chain.contains("dns error"),
        "the inner cause is the whole point of walking source(): {chain}"
    );
}

#[test]
fn cause_chain_does_not_repeat_a_self_describing_error() {
    let err = Leaf("connection refused");
    let chain = cause_chain(&err);
    assert_eq!(
        chain, "connection refused",
        "an error with no source must not be printed twice"
    );
}

// --- retry_fetch: one retry absorbs a flap, a status code is not a flap ---

/// Count attempts and fail the first `fail_times` with a transport error.
async fn drive(
    fail_times: u8,
    fail_with: impl Fn() -> FetchError + Copy,
) -> (Result<String, FetchError>, u8) {
    let mut calls = 0u8;
    let result = retry_fetch("AGENTS.md", || {
        calls += 1;
        let this_call = calls;
        let err = fail_with();
        async move {
            if this_call <= fail_times {
                Err(err)
            } else {
                Ok("body".to_string())
            }
        }
    })
    .await;
    (result, calls)
}

#[tokio::test]
async fn transport_failure_is_retried_once_and_can_succeed() {
    let (result, calls) = drive(1, || FetchError::Transport {
        detail: "dns error".to_string(),
    })
    .await;
    assert!(
        result.is_ok(),
        "the second attempt must be allowed to succeed: {result:?}"
    );
    assert_eq!(
        calls, 2,
        "a transport failure gets exactly one retry, not more"
    );
}

#[tokio::test]
async fn transport_failure_is_not_retried_forever() {
    let (result, calls) = drive(99, || FetchError::Transport {
        detail: "dns error".to_string(),
    })
    .await;
    assert!(
        result.is_err(),
        "an unreachable host must surface the failure, not loop"
    );
    assert_eq!(
        calls, 2,
        "attempts are capped at TEMPLATE_MAX_ATTEMPTS so an outage is not a longer wall"
    );
}

#[tokio::test]
async fn status_failure_is_not_retried() {
    let (result, calls) = drive(99, || FetchError::Status { code: 404 }).await;
    assert!(
        matches!(result, Err(FetchError::Status { code: 404 })),
        "the status must survive to the caller: {result:?}"
    );
    assert_eq!(
        calls, 1,
        "a 404 is the server's answer, so one attempt is the whole story"
    );
}

#[tokio::test]
async fn body_failure_is_retried_once() {
    let (result, calls) = drive(1, || FetchError::Body {
        detail: "connection closed before message completed".to_string(),
    })
    .await;
    assert!(
        result.is_ok(),
        "a body cut short must be retried: {result:?}"
    );
    assert_eq!(calls, 2, "body failures are retryable, once");
}

// --- the join: one line per cause, not one sentence per file ---

/// A failed result carrying `cause` for `filename`.
fn failed(filename: &str, cause: &str) -> FileSyncResult {
    FileSyncResult {
        filename: filename.to_string(),
        synced: false,
        sections_added: 0,
        error: Some(cause.to_string()),
        bailed_for_cap: None,
    }
}

const DNS_CAUSE: &str = "could not reach the template host (dns error) after 2 attempts; \
                         check the network or proxy, the next RSI cycle retries";
const HTTP_CAUSE: &str = "upstream answered HTTP 404; the tracked path is stale, which \
                          needs a code fix rather than a retry";

#[test]
fn sync_failures_collapse_to_one_line() {
    let results: Vec<FileSyncResult> = ["AGENTS.md", "TOOLS.md", "SOUL.md", "USER.md"]
        .iter()
        .map(|f| failed(f, DNS_CAUSE))
        .collect();

    let msg = summarize_sync_failures(&results).expect("failures must produce a line");

    assert!(
        !msg.contains('\n'),
        "the whole point is one line for the user to read: {msg}"
    );
    assert!(
        msg.contains("all 4 tracked files failed"),
        "a cause shared by every failure collapses to one sentence with the count: {msg}"
    );
    assert_eq!(
        msg.matches("dns error").count(),
        1,
        "the cause must appear once, not once per file: {msg}"
    );
    assert!(
        !msg.contains("AGENTS.md: could not reach the template host for AGENTS.md"),
        "the old join named the file twice; the cause must carry no filename: {msg}"
    );
    assert!(
        msg.contains("retries"),
        "the collapsed line must keep the next step: {msg}"
    );
}

#[test]
fn distinct_causes_get_one_clause_each() {
    let results = vec![
        failed("AGENTS.md", DNS_CAUSE),
        failed("TOOLS.md", DNS_CAUSE),
        failed("SOUL.md", HTTP_CAUSE),
    ];

    let msg = summarize_sync_failures(&results).expect("failures must produce a line");

    assert!(
        !msg.contains('\n'),
        "distinct causes still render on one line: {msg}"
    );
    assert!(
        msg.contains("2 (AGENTS.md, TOOLS.md) failed:"),
        "a shared cause names its files once with the count: {msg}"
    );
    assert!(
        msg.contains("1 (SOUL.md) failed:"),
        "each distinct cause gets its own clause: {msg}"
    );
    assert_eq!(
        msg.matches("HTTP 404").count(),
        1,
        "a cause is never repeated per file: {msg}"
    );
}

#[test]
fn single_failure_names_its_file_once() {
    let msg = summarize_sync_failures(&[failed("AGENTS.md", DNS_CAUSE)])
        .expect("one failure must produce a line");

    assert!(
        msg.starts_with("AGENTS.md failed:"),
        "with one failure there is no count worth naming, just the file: {msg}"
    );
    assert_eq!(
        msg.matches("AGENTS.md").count(),
        1,
        "the file must be named exactly once: {msg}"
    );
}

#[test]
fn clean_sync_emits_no_failure_line() {
    let results = vec![FileSyncResult {
        filename: "AGENTS.md".to_string(),
        synced: true,
        sections_added: 3,
        error: None,
        bailed_for_cap: None,
    }];
    assert!(
        summarize_sync_failures(&results).is_none(),
        "a clean pass must not emit a TemplateSyncFailed notification"
    );
}

// --- the cause reaches the user, and the network is only touched once ---

/// The source of the module under test, for structural sentinels.
const SOURCE: &str = include_str!("../brain/rsi_sync.rs");

/// Slice out a top-level function body. Rustfmt indents nested blocks, so a
/// closing brace at column 0 is the end of the item.
fn fn_body(name: &str) -> &'static str {
    let needle = format!("fn {name}(");
    let start = SOURCE
        .find(&needle)
        .unwrap_or_else(|| panic!("{name} must exist in rsi_sync.rs"));
    let rest = &SOURCE[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{name} must end with a closing brace at column 0"));
    &rest[..end + 3]
}

#[test]
fn fetch_error_names_the_cause() {
    let transport = FetchError::Transport {
        detail: "dns error: failed to lookup address information".to_string(),
    };
    let msg = transport.describe();
    assert!(
        msg.contains("dns error: failed to lookup address information"),
        "the cause itself must reach the user, not just reqwest's outer wrapper: {msg}"
    );
    assert!(
        !msg.contains(".md"),
        "the cause carries no filename; the join names the files once: {msg}"
    );

    let body = FetchError::Body {
        detail: "connection closed before message completed".to_string(),
    };
    assert!(
        body.describe()
            .contains("connection closed before message completed"),
        "a body failure must name what went wrong, not only that it did"
    );
}

#[test]
fn one_fetch_per_tracked_file() {
    let body = fn_body("sync_single_file");
    assert!(
        !body.contains("fetch_template("),
        "sync_single_file must consume the body fetched by its caller; a second fetch here is the duplicate request #2169 removed"
    );
    assert!(
        body.contains("upstream"),
        "sync_single_file receives the already-fetched body as a parameter"
    );
    assert_eq!(
        SOURCE.matches("fetch_template(").count(),
        2,
        "exactly one definition and one call site; a second call site doubles the requests per pass"
    );
}

#[test]
fn template_client_has_timeouts() {
    let body = fn_body("template_client");
    assert!(
        body.contains(".connect_timeout(TEMPLATE_CONNECT_TIMEOUT)"),
        "a connect timeout is what stops a blackholed SYN from hanging forever"
    );
    assert!(
        body.contains(".timeout(TEMPLATE_REQUEST_TIMEOUT)"),
        "a whole-call timeout is what stops a stalled body from wedging the RSI engine"
    );
    assert!(
        !SOURCE.contains("reqwest::get("),
        "a bare reqwest::get has no timeout at all, which is the defect #2169 reports"
    );
}
