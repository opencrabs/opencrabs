//! Network fetch for `web_scrape`.
//!
//! Two ways to get a page's HTML. `fetch_static` is a plain reqwest GET with a
//! desktop browser User-Agent and a bounded timeout, which handles the common
//! case (server-rendered HTML) at near-zero cost. `is_js_shell` is a pure,
//! network-free heuristic that spots pages which ship almost no visible text
//! until their JavaScript runs. When it fires, and only then, the orchestrator
//! escalates to `fetch_rendered` (behind the `browser` feature) to let headless
//! Chrome render the DOM before we read it.

use std::time::Duration;

use reqwest::Client;

use super::clean::to_plain_text;

/// Desktop Chrome User-Agent. Some sites 403 non-browser agents or serve
/// degraded HTML to them, so the static fetch presents a mainstream UA.
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

/// Below this many non-whitespace characters of visible text, a page that also
/// ships script tags is treated as an unrendered SPA shell worth escalating.
const JS_SHELL_TEXT_THRESHOLD: usize = 200;

/// Fetch raw HTML over HTTP with a browser UA and a bounded timeout. Returns the
/// response body on 2xx, an error string otherwise.
pub async fn fetch_static(url: &str, timeout_secs: u64) -> Result<String, String> {
    // The redirect policy is a client-level setting, so it gets its own slot
    // and the root store is parsed once rather than once per scrape. The
    // deadline and the browser UA ride on the request.
    static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    let client = crate::utils::http::cached_client(&CLIENT, |b| {
        // Re-validate every redirect hop against the SSRF guard (OC-04).
        b.redirect(crate::brain::tools::ssrf::redirect_policy(10))
    });

    let resp = client
        .get(url)
        .timeout(Duration::from_secs(timeout_secs))
        .header(reqwest::header::USER_AGENT, BROWSER_UA)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                format!("Request timed out after {timeout_secs}s")
            } else if e.is_connect() {
                format!("Connection failed: {e}")
            } else {
                format!("Request failed: {e}")
            }
        })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!(
            "HTTP {} {} for {url}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Unknown")
        ));
    }

    resp.text()
        .await
        .map_err(|e| format!("Failed to read response body: {e}"))
}

/// Heuristic: does this HTML look like an unrendered JavaScript shell? True when
/// the page carries script tags yet reduces to almost no visible text once
/// scripts, styles, and tags are stripped. Pure and network-free so it is
/// directly unit-testable.
pub fn is_js_shell(html: &str) -> bool {
    if !html.to_ascii_lowercase().contains("<script") {
        return false;
    }
    let visible = to_plain_text(html);
    let visible_len = visible.chars().filter(|c| !c.is_whitespace()).count();
    visible_len < JS_SHELL_TEXT_THRESHOLD
}

/// Render `url` with headless Chrome and return the fully-hydrated HTML. Used
/// only when `is_js_shell` flags the static fetch as an empty SPA shell.
#[cfg(feature = "browser")]
pub async fn fetch_rendered(
    manager: &crate::brain::tools::browser::BrowserManager,
    session_id: uuid::Uuid,
    url: &str,
) -> Result<String, String> {
    let page = manager
        .get_or_create_session_page(session_id)
        .await
        .map_err(|e| format!("Browser error: {e}"))?;

    page.goto(url)
        .await
        .map_err(|e| format!("Navigation failed: {e}"))?;

    // Wait for network to settle rather than the bare `load` event, so
    // client-side hydration has a chance to populate the DOM before we read it.
    if let Err(e) = page
        .wait_for_network_almost_idle_with_timeout(Duration::from_secs(3))
        .await
    {
        tracing::debug!("web_scrape: network-idle wait timed out for {url} (proceeding): {e}");
    }

    page.content()
        .await
        .map_err(|e| format!("Failed to get rendered HTML: {e}"))
}
