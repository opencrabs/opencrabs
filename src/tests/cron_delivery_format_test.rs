//! Cron reports leave through the same converters the chat paths use (#2069).
//!
//! Discord and Slack have no table markup, so a pipe table reaches either
//! channel as its own source text, in a proportional font where no column
//! lines up. Every interactive path rewrites the text before sending it; cron
//! delivery was the one outbound path that did not, and a real
//! `adguard-daily-stats` report arrived in Discord as 18 lines of literal
//! pipes.
//!
//! These tests sit on the delivery seam (`*_chunks_for_delivery`), not on the
//! converters, which already have their own unit tests. The seam is where the
//! two halves meet: converting a table puts a fenced grid into the text, and
//! the chunker then has to split that text without leaving a chunk with an
//! unclosed fence.

#![cfg(any(feature = "discord", feature = "slack"))]

#[cfg(feature = "discord")]
use crate::cron::scheduler::discord_chunks_for_delivery;
#[cfg(feature = "slack")]
use crate::cron::scheduler::slack_chunks_for_delivery;

/// The table from the report that opened #2069, plus a second one so the
/// report carries more than a single table.
#[cfg(feature = "discord")]
const REPORT: &str = "\
## AdGuard daily stats

| Metrik | Nilai |
|---|---|
| Total queries | 11.370 |
| Blocked | 2.950 (25,95%) |
| Allowed | 8.420 (74,05%) |
| Avg response time | 5,62 ms |

Top clients:

| Client | Queries |
|---|---|
| 192.168.1.10 | 4.201 |
| 192.168.1.24 | 2.118 |";

/// A three-column table, the shape Slack renders as a label plus `└`
/// continuations.
#[cfg(feature = "slack")]
const SLACK_REPORT: &str = "\
## Container state

| Metric | Before | After |
|---|---|---|
| RestartCount | 272 | 0 |
| Uptime | 30 min | 1h 44m |";

#[cfg(feature = "discord")]
#[test]
fn a_cron_report_reaches_discord_with_its_tables_converted() {
    let chunks = discord_chunks_for_delivery(REPORT);
    let joined = chunks.join("\n");

    assert!(
        !joined.contains("| Metrik |"),
        "the raw table row survived delivery: {joined}"
    );
    assert!(
        !joined.contains("|---|---|"),
        "the raw separator row survived delivery: {joined}"
    );
    assert!(
        !joined.contains("| Client |"),
        "the second table survived delivery: {joined}"
    );
    assert!(
        chunks.iter().any(|c| c.contains("```text")),
        "no grid fence in any chunk: {joined}"
    );
    for row in ["Total queries", "Avg response time", "192.168.1.24"] {
        assert!(joined.contains(row), "row '{row}' was dropped: {joined}");
    }
}

/// The grid render puts a table inside a fence, and the report is chunked at
/// 2000 bytes. A chunker that breaks at the nearest newline cuts the grid open:
/// one chunk ends inside the fence, the next starts mid-table. Discord parses
/// each message on its own, so every chunk has to carry an even number of
/// fence markers. The padding walks the table across the boundary, because the
/// failure only shows when the break lands inside the grid.
#[cfg(feature = "discord")]
#[test]
fn a_grid_straddling_the_chunk_boundary_leaves_no_fence_open() {
    let mut table = String::from("| Host | Metric | Value |\n|---|---|---|\n");
    for i in 0..24 {
        table.push_str(&format!("| node-{i:02} | cpu.load | {} |\n", i * 137));
    }

    for pad in 1800..2150 {
        let report = format!("{}\n{table}", "x".repeat(pad));
        let chunks = discord_chunks_for_delivery(&report);

        for (i, chunk) in chunks.iter().enumerate() {
            let fences = chunk.matches("```").count();
            assert_eq!(
                fences % 2,
                0,
                "chunk {i} of a {pad}-byte report leaves a fence open \
                 ({fences} fence markers): {chunk}"
            );
        }
        assert!(
            chunks.join("\n").contains("node-23"),
            "the last grid row was lost at padding {pad}"
        );
    }
}

#[cfg(feature = "discord")]
#[test]
fn a_report_without_tables_is_delivered_unchanged() {
    let plain = "AdGuard daily stats\n\nAll clear today, nothing to convert.";
    assert_eq!(discord_chunks_for_delivery(plain), vec![plain.to_string()]);
}

#[cfg(feature = "slack")]
#[test]
fn a_cron_report_reaches_slack_with_its_table_and_heading_converted() {
    let chunks = slack_chunks_for_delivery(SLACK_REPORT);
    let joined = chunks.join("\n");

    assert!(
        !joined.contains('|'),
        "the raw table survived delivery: {joined}"
    );
    assert!(
        !joined.contains("##"),
        "the raw heading survived delivery: {joined}"
    );
    assert!(
        joined.contains("*CONTAINER STATE*"),
        "heading was not converted: {joined}"
    );
    assert!(
        joined.contains("*RestartCount*"),
        "row label missing: {joined}"
    );
    assert!(
        joined.contains("└ Before: 272") && joined.contains("└ After: 0"),
        "continuations missing: {joined}"
    );
    assert!(
        joined.contains("└ Before: 30 min") && joined.contains("└ After: 1h 44m"),
        "second row lost: {joined}"
    );
}

#[cfg(feature = "slack")]
#[test]
fn a_slack_report_without_tables_is_delivered_unchanged() {
    let plain = "AdGuard daily stats\n\nAll clear today, nothing to convert.";
    assert_eq!(slack_chunks_for_delivery(plain), vec![plain.to_string()]);
}
