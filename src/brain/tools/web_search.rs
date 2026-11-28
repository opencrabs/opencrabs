//! Web Search Tool (Unified)
//!
//! Single search entry point that fans out to DuckDuckGo, Exa, Brave, and
//! Serper in parallel, merges results, and returns the best combined set.
//! The agent calls one tool and never thinks about which engine answered.
//!
//! Engines:
//! - **DuckDuckGo** (always available, free, captcha-prone)
//! - **Exa** (free MCP endpoint or direct API with `EXA_API_KEY`)
//! - **Brave** (requires `BRAVE_API_KEY`, registered only when configured)
//! - **Serper** (Google SERP, requires configured key, #1731)
//! - **Keenable** (optional, keyless public endpoint or keyed API, #2939)
//!
//! DDG captcha detection (HTTP 202 + structural form heuristic) silently
//! drops DDG from the result pool instead of surfacing an error.

use super::brave_search::BraveSearchTool;
use super::error::{Result, ToolError};
use super::exa_search::ExaSearchTool;
use super::keenable_search::KeenableSearchTool;
use super::serper_search::SerperSearchTool;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// Unified web search tool. Fans out to DDG + Exa (+ Brave, + Serper,
/// + Keenable when configured) in parallel and merges results.
#[derive(Default)]
pub struct WebSearchTool {
    exa: Option<Arc<ExaSearchTool>>,
    brave: Option<Arc<BraveSearchTool>>,
    serper: Option<Arc<SerperSearchTool>>,
    keenable: Option<Arc<KeenableSearchTool>>,
    /// Test seam so fan-out tests can mock DuckDuckGo Lite without a network call.
    #[cfg(test)]
    ddg_base_url: Option<String>,
}

impl WebSearchTool {
    /// Create a unified search tool with optional engine references.
    /// When all are `None`, behaves as DDG-only (backward compat).
    pub fn new(
        exa: Option<Arc<ExaSearchTool>>,
        brave: Option<Arc<BraveSearchTool>>,
        serper: Option<Arc<SerperSearchTool>>,
        keenable: Option<Arc<KeenableSearchTool>>,
    ) -> Self {
        Self {
            exa,
            brave,
            serper,
            keenable,
            #[cfg(test)]
            ddg_base_url: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_ddg_base_url(mut self, url: impl Into<String>) -> Self {
        self.ddg_base_url = Some(url.into());
        self
    }

    /// DuckDuckGo search (extracted for parallel fan-out).
    async fn search_ddg(&self, query: &str, max_results: usize) -> Result<ToolResult> {
        let url = ddg_url(self, query);

        let mut last_err = String::new();
        for (attempt, ua) in DDG_USER_AGENTS.iter().enumerate() {
            // Shared client so the CA bundle loads once per process; the
            // user-agent and the deadline belong to this attempt (#2843). The
            // client used to be built inside this loop, so one search that
            // rotated through the agent list paid the TLS root store load once
            // per attempt.
            let request = crate::utils::http::shared()
                .get(&url)
                .timeout(std::time::Duration::from_secs(10))
                .header(reqwest::header::USER_AGENT, *ua);

            match request.send().await {
                // DDG bot-detection challenge: HTTP 202 is their
                // protocol-level "you're blocked" signal. Stable across
                // deploys. Short-circuit instead of burning UA rotations.
                Ok(response) if response.status().as_u16() == 202 => {
                    return Ok(ToolResult::error(
                        "DuckDuckGo returned a bot-detection challenge (HTTP 202).".to_string(),
                    ));
                }
                Ok(response) if response.status().is_success() => {
                    let html = response.text().await.map_err(|e| {
                        ToolError::Execution(format!("Failed to read response: {e}"))
                    })?;
                    let results = parse_lite_results(&html, max_results);
                    // Zero results + form present = challenge page, not a
                    // genuine empty result. Structural check, no reliance on
                    // internal JS filenames DDG can rename at any time.
                    if results.is_empty() && html.contains("<form") {
                        return Ok(ToolResult::error(
                            "DuckDuckGo returned a captcha/challenge page instead of results."
                                .to_string(),
                        ));
                    }
                    return Ok(ToolResult::success(format_results(query, &results)));
                }
                Ok(response) => {
                    last_err = format!("HTTP {}", response.status());
                    tracing::warn!(
                        "web_search: DuckDuckGo returned {} (UA {}/{}) — rotating User-Agent",
                        response.status(),
                        attempt + 1,
                        DDG_USER_AGENTS.len(),
                    );
                }
                Err(e) => {
                    last_err = e.to_string();
                    tracing::warn!(
                        "web_search: request failed (UA {}/{}): {e}",
                        attempt + 1,
                        DDG_USER_AGENTS.len(),
                    );
                }
            }

            // Brief backoff before the next UA (skipped after the last attempt).
            if attempt + 1 < DDG_USER_AGENTS.len() {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }

        Ok(ToolResult::error(format!(
            "DuckDuckGo search failed after {} attempts (last error: {last_err}).",
            DDG_USER_AGENTS.len(),
        )))
    }
}

fn ddg_url(tool: &WebSearchTool, query: &str) -> String {
    let encoded = urlencoding::encode(query);
    #[cfg(test)]
    if let Some(base) = &tool.ddg_base_url {
        return format!("{}/lite/?q={encoded}", base.trim_end_matches('/'));
    }
    #[cfg(not(test))]
    let _ = tool;
    format!("https://lite.duckduckgo.com/lite/?q={encoded}")
}

#[derive(Debug, Deserialize, Serialize)]
struct SearchInput {
    /// Search query
    query: String,

    /// Maximum number of results to return
    #[serde(default = "default_max_results")]
    max_results: usize,
}

fn default_max_results() -> usize {
    5
}

// DuckDuckGo HTML search result
#[derive(Debug, Deserialize)]
pub(crate) struct SearchResult {
    pub(crate) title: String,
    pub(crate) url: String,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the internet for real-time information. Fans out to \
         DuckDuckGo, Exa, Brave, Serper (Google, when configured), and Keenable in \
         parallel and returns merged, deduplicated results. \
         \n\nDEFAULT web-research tool — use this for any \"find me info \
         about X\" / \"what's the latest Y\" / \"check the docs for Z\" \
         request unless the user explicitly asks for browser interaction. \
         Always pick a search tool over `browser_navigate` for research. \
         \n\nFor GitHub content (issues, PRs, repos, code search) use the \
         `gh` CLI via `bash` instead — it returns structured JSON and is \
         authenticated."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query (e.g., 'latest Node.js LTS release', 'Rust async programming')"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5)",
                    "default": 5,
                    "minimum": 1,
                    "maximum": 10
                }
            },
            "required": ["query"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    fn requires_approval(&self) -> bool {
        false // Web search is generally safe (read-only)
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let input: SearchInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;

        if input.query.trim().is_empty() {
            return Err(ToolError::InvalidInput("Query cannot be empty".to_string()));
        }

        if input.max_results == 0 || input.max_results > 10 {
            return Err(ToolError::InvalidInput(
                "max_results must be between 1 and 10".to_string(),
            ));
        }

        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let parsed: SearchInput = serde_json::from_value(input.clone())?;

        // No extra engines configured: DDG-only (backward compat with
        // tool_setup.rs registration before config is loaded).
        if self.exa.is_none()
            && self.brave.is_none()
            && self.serper.is_none()
            && self.keenable.is_none()
        {
            return self.search_ddg(&parsed.query, parsed.max_results).await;
        }

        // Fan out to all engines in parallel.
        let ddg_query = parsed.query.clone();
        let ddg_max = parsed.max_results;
        let ddg_fut = self.search_ddg(&ddg_query, ddg_max);

        let exa_input = input.clone();
        let exa_fut = async {
            match &self.exa {
                Some(exa) => Some(exa.execute(exa_input, context).await),
                None => None,
            }
        };

        let brave_input = input.clone();
        let brave_fut = async {
            match &self.brave {
                Some(brave) => Some(brave.execute(brave_input, context).await),
                None => None,
            }
        };

        let serper_input = input.clone();
        let serper_fut = async {
            match &self.serper {
                Some(serper) => Some(serper.execute(serper_input, context).await),
                None => None,
            }
        };

        let keenable_input = input.clone();
        let keenable_fut = async {
            match &self.keenable {
                Some(keenable) => Some(keenable.execute(keenable_input, context).await),
                None => None,
            }
        };

        let (ddg_res, exa_res, brave_res, serper_res, keenable_res) =
            tokio::join!(ddg_fut, exa_fut, brave_fut, serper_fut, keenable_fut);

        // Collect successful outputs. DDG errors (captcha, rate limit)
        // are silently dropped; the other engines fill the gap.
        let mut sections: Vec<String> = Vec::new();

        match ddg_res {
            Ok(ref r) if r.success => sections.push(r.output.clone()),
            Ok(r) => {
                tracing::debug!("web_search: DDG dropped: {}", r.output);
            }
            Err(e) => {
                tracing::debug!("web_search: DDG failed: {e}");
            }
        }

        if let Some(Ok(r)) = exa_res
            && r.success
        {
            sections.push(r.output);
        }

        if let Some(Ok(r)) = brave_res
            && r.success
        {
            sections.push(r.output);
        }

        if let Some(Ok(r)) = serper_res
            && r.success
        {
            sections.push(r.output);
        }

        if let Some(Ok(r)) = keenable_res
            && r.success
        {
            sections.push(r.output);
        }

        if sections.is_empty() {
            return Ok(ToolResult::error(
                "All search engines failed or returned no results. \
                 Try rephrasing the query or using http_request against a search API."
                    .to_string(),
            ));
        }

        Ok(ToolResult::success(dedup_sections(sections)))
    }
}

/// Entry line of a rendered search result that carries its URL. Covers both
/// render formats in this module: Brave/Serper `   URL: https://...` and
/// DuckDuckGo `   🔗 https://...`.
pub(crate) fn entry_url(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("URL:")
        .or_else(|| trimmed.strip_prefix("🔗"))?;
    let url = rest.trim();
    (url.starts_with("http://") || url.starts_with("https://")).then_some(url)
}

/// First line of a rendered result entry: `1. Title` / `10. Title`.
pub(crate) fn is_entry_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    match trimmed.split_once(". ") {
        Some((num, _)) => !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// Canonical form for dup comparison: case-insensitive, trailing slash
///-insensitive. Intentionally not full URL canonicalization.
pub(crate) fn normalize_url(url: &str) -> String {
    url.trim_end_matches('/').to_lowercase()
}

/// Merge engine sections, dropping later result entries whose URL was already
/// returned by an earlier engine (#1731). Google, Brave, and DDG overlap hard
/// on popular queries, and the merged output used to repeat the same hit once
/// per engine. First occurrence wins, so engine priority order is preserved.
/// Entries without a recognizable URL line (unknown render format) are kept
/// verbatim, and a section reduced to header-only by dedup is dropped whole.
pub(crate) fn dedup_sections(sections: Vec<String>) -> String {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut merged: Vec<String> = Vec::new();

    for section in sections {
        // Was this section rendered in the numbered-entry format at all?
        // Sections in an unknown format (no `N.` lines) pass through
        // untouched; only entry-based sections can be reduced to
        // header-only by dedup.
        let entry_based = section.lines().any(is_entry_start);
        let mut out: Vec<String> = Vec::new();
        let mut entry_start: Option<usize> = None;
        let mut dropping = false;
        let mut mutated = false;

        for line in section.lines() {
            if is_entry_start(line) {
                dropping = false;
                entry_start = Some(out.len());
            }
            if !dropping
                && let Some(url) = entry_url(line)
                && !seen.insert(normalize_url(url))
            {
                // Duplicate: unwind this entry's already-pushed lines
                // (its title came before the URL line) and skip its tail.
                if let Some(start) = entry_start {
                    out.truncate(start);
                }
                dropping = true;
                mutated = true;
            }
            if !dropping {
                out.push(line.to_string());
            }
        }

        // Keep the section if an entry survived dedup, or if it never was
        // entry-based (unknown render format). A section reduced to
        // header-only (every entry was a duplicate) is dropped whole.
        if !entry_based || out.iter().any(|l| is_entry_start(l)) {
            // Untouched sections pass through byte-verbatim; only a
            // section that lost entries is rebuilt from surviving lines.
            merged.push(if mutated { out.join("\n") } else { section });
        }
    }

    merged.join("\n\n")
}

/// Rotated User-Agent pool for DuckDuckGo Lite (#525). DDG blocks repetitive UA
/// patterns from one IP, so trying a few realistic browser UAs recovers most
/// transient 403s.
pub(crate) const DDG_USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.3 Safari/605.1.15",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:123.0) Gecko/20100101 Firefox/123.0",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36",
];

/// Format parsed search results into the tool's human-readable output.
pub(crate) fn format_results(query: &str, results: &[SearchResult]) -> String {
    let mut output = String::new();
    output.push_str(&format!("🔍 Search results for: \"{query}\"\n\n"));
    if results.is_empty() {
        output.push_str("ℹ️  No results found. Try:\n");
        output.push_str("  • Rephrasing your query\n");
        output.push_str("  • Using more specific keywords\n");
        output.push_str("  • Searching for a different topic\n");
    } else {
        for (i, result) in results.iter().enumerate() {
            output.push_str(&format!("{}. {}\n", i + 1, result.title));
            output.push_str(&format!("   🔗 {}\n\n", result.url));
        }
    }
    output
}

/// Parse DuckDuckGo Lite HTML response to extract search results
pub(crate) fn parse_lite_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Find result links in the HTML
    // DDG Lite uses <a> tags with class "result-link" for search results
    let link_regex =
        regex::Regex::new(r#"<a[^>]*class="result-link"[^>]*href="([^"]*)"[^>]*>([^<]*)</a>"#)
            .unwrap_or_else(|_| {
                regex::Regex::new(r#"<a[^>]*href="([^"]*)"[^>]*>([^<]*)</a>"#).unwrap()
            });

    for cap in link_regex.captures_iter(html) {
        if results.len() >= max_results {
            break;
        }

        let url = cap
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let title = cap
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();

        // Skip non-http links and empty titles
        if url.starts_with("http") && !title.trim().is_empty() {
            results.push(SearchResult { title, url });
        }
    }

    // Fallback: if no results found with class="result-link", try generic link parsing
    if results.is_empty() {
        let generic_regex =
            regex::Regex::new(r#"<a[^>]*href="(https?://[^"]*)"[^>]*>([^<]{10,})</a>"#).unwrap();

        for cap in generic_regex.captures_iter(html) {
            if results.len() >= max_results {
                break;
            }

            let url = cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let title = cap
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();

            // Skip duckduckgo own links
            if !url.contains("duckduckgo.com") && !title.trim().is_empty() {
                results.push(SearchResult { title, url });
            }
        }
    }

    results
}
