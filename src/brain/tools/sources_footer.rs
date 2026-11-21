//! Deterministic Sources footer (#1883).
//!
//! Search-backed answers carried source links only when the model felt like
//! citing them. This module harvests the URLs the search tools already render
//! into their own output (`🔗` / `URL:` lines) and appends a bounded markdown
//! link list to the delivered final answer at the turn's settle point, so a
//! search-backed reply always carries its sources without model cooperation.
//! URLs the answer already links are dropped rather than repeated.

use std::collections::HashSet;

use crate::brain::tools::web_search::{entry_url, is_entry_start, normalize_url};

/// Tools whose rendered output carries harvestable source URLs. Scoped to the
/// search engines: `http_request` / `web_scrape` / `browser_navigate` handle
/// user-supplied URLs the model already knows, out of scope by design.
pub(crate) const SEARCH_TOOLS: &[&str] = &[
    "web_search",
    "exa_search",
    "brave_search",
    "serper_search",
    "keenable_search",
];

/// Bounded footer: five links max, in engine-result order, first engine wins.
pub(crate) const MAX_SOURCES: usize = 5;

/// True when `name` is one of the four search engines whose outputs are
/// harvested into the footer.
pub(crate) fn is_search_tool(name: &str) -> bool {
    SEARCH_TOOLS.contains(&name)
}

/// Harvest `(title, url)` pairs from raw search-tool outputs in order, with
/// cross-engine dedup: the first occurrence of a URL wins, compared through
/// the same normalization #1731 uses for engine fanout (case- and
/// trailing-slash-insensitive). A URL line preceded by a `N. Title` line gets
/// that title; otherwise the footer falls back to the bare URL.
pub(crate) fn harvest_search_sources(outputs: &[String]) -> Vec<(Option<String>, String)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<(Option<String>, String)> = Vec::new();
    for output in outputs {
        let mut pending_title: Option<String> = None;
        for line in output.lines() {
            if let Some(url) = entry_url(line) {
                if seen.insert(normalize_url(url)) {
                    out.push((pending_title.take(), url.to_string()));
                }
                pending_title = None;
            } else if is_entry_start(line) {
                pending_title = line
                    .trim_start()
                    .split_once(". ")
                    .map(|(_, title)| title.trim().to_string());
            }
        }
    }
    out
}

/// Build the footer for `answer`: drops URLs the answer already links, caps
/// the list at `MAX_SOURCES`, returns `None` when nothing would be added.
/// Plain markdown link list, channel-safe for Discord/WhatsApp (no tables).
/// Titles come from engine output and ride the same delivered-text handling
/// as the rest of the answer.
pub(crate) fn build_sources_footer(
    sources: &[(Option<String>, String)],
    answer: &str,
) -> Option<String> {
    // Exactly-once (#1883): an answer that already carries a Sources block
    // never gets a second one, even when the cap truncated fresh URLs on the
    // first pass. Without this, a >5-source answer re-appended on a replay
    // path would double-foot, because the cut URLs are not "already cited".
    if answer.contains("**Sources**") {
        return None;
    }
    let mut entries: Vec<String> = Vec::new();
    for (title, url) in sources {
        if answer.contains(url.as_str()) {
            continue;
        }
        match title {
            Some(t) if !t.trim().is_empty() => {
                entries.push(format!("- [{}]({})", t.trim(), url));
            }
            _ => entries.push(format!("- {}", url)),
        }
        if entries.len() >= MAX_SOURCES {
            break;
        }
    }
    if entries.is_empty() {
        return None;
    }
    Some(format!("**Sources**\n{}", entries.join("\n")))
}

/// The footer this turn's search outputs warrant, or `None`: no search tool
/// ran, or every harvested source is already cited in the answer. Pure, so
/// the settle-point wiring stays one branch.
pub(crate) fn footer_for(answer: &str, search_outputs: &[String]) -> Option<String> {
    if search_outputs.is_empty() {
        return None;
    }
    build_sources_footer(&harvest_search_sources(search_outputs), answer)
}
