//! The Discord tool card (#1880): repeats collapse, rows page five at a time,
//! the bar color and header follow the rows, and the Log file lists every call.

use crate::channels::discord::tool_card::{
    COLOR_CLEAN, COLOR_FAILED, COLOR_RUNNING, card_color, card_footer, card_title, clamp_page,
    collapse_repeats, log_markdown, page_count, page_slice, row_line,
};
use crate::channels::discord::tool_group::GroupEntry;

fn entry(name: &str, context: &str, status: Option<bool>) -> GroupEntry {
    GroupEntry {
        call_id: String::new(),
        task_id: None,
        name: name.to_string(),
        context: context.to_string(),
        status,
    }
}

#[test]
fn identical_consecutive_calls_collapse_into_one_row() {
    let entries = vec![
        entry("bash", " (cargo check)", Some(true)),
        entry("bash", " (cargo check)", Some(true)),
        entry("bash", " (cargo check)", Some(true)),
        entry("read_file", " (src/main.rs)", Some(true)),
    ];
    let rows = collapse_repeats(&entries);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].count, 3);
    assert_eq!(row_line(&rows[0]), "✅ **bash** (cargo check) ×3");
}

#[test]
fn a_repeat_with_different_arguments_stays_separate() {
    let entries = vec![
        entry("bash", " (cargo check)", Some(true)),
        entry("bash", " (cargo test)", Some(true)),
    ];
    assert_eq!(collapse_repeats(&entries).len(), 2);
}

#[test]
fn non_adjacent_repeats_keep_their_place() {
    let entries = vec![
        entry("bash", " (ls)", Some(true)),
        entry("read_file", " (a)", Some(true)),
        entry("bash", " (ls)", Some(true)),
    ];
    let rows = collapse_repeats(&entries);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.count == 1));
}

#[test]
fn a_collapsed_row_is_running_while_any_call_runs() {
    let entries = vec![
        entry("bash", " (x)", Some(true)),
        entry("bash", " (x)", None),
    ];
    let rows = collapse_repeats(&entries);
    assert_eq!(rows[0].status, None);
    assert_eq!(card_color(&rows), COLOR_RUNNING);
}

#[test]
fn a_collapsed_row_is_failed_when_any_call_failed() {
    let entries = vec![
        entry("bash", " (x)", Some(false)),
        entry("bash", " (x)", Some(true)),
    ];
    let rows = collapse_repeats(&entries);
    assert_eq!(rows[0].status, Some(false));
    assert_eq!(card_color(&rows), COLOR_FAILED);
}

#[test]
fn the_bar_is_green_when_everything_finished_clean() {
    let rows = collapse_repeats(&[entry("bash", " (x)", Some(true))]);
    assert_eq!(card_color(&rows), COLOR_CLEAN);
}

#[test]
fn pages_hold_five_rows_and_an_empty_card_is_one_page() {
    assert_eq!(page_count(0), 1);
    assert_eq!(page_count(5), 1);
    assert_eq!(page_count(6), 2);
    assert_eq!(page_count(14), 3);
    let rows: Vec<_> = (0..14)
        .map(|i| entry("t", &format!(" ({i})"), Some(true)))
        .collect();
    let rows = collapse_repeats(&rows);
    assert_eq!(page_slice(&rows, 0).len(), 5);
    assert_eq!(page_slice(&rows, 2).len(), 4);
    assert_eq!(page_slice(&rows, 2)[0].context, " (10)");
}

#[test]
fn an_out_of_range_page_is_clamped_to_the_last() {
    assert_eq!(clamp_page(9, 14), 2);
    assert_eq!(clamp_page(0, 0), 0);
}

#[test]
fn the_header_names_the_running_tool() {
    let rows = collapse_repeats(&[
        entry("read_file", " (src/main.rs)", Some(true)),
        entry("bash", " (grep appsflyer lib/)", None),
    ]);
    assert_eq!(card_title(&rows), "Running: bash (grep appsflyer lib/)");
}

#[test]
fn the_header_totals_a_finished_card() {
    let rows = collapse_repeats(&[entry("bash", " (x)", Some(true))]);
    assert_eq!(card_title(&rows), "Done · 1 rows");
    let failed = collapse_repeats(&[entry("bash", " (x)", Some(false))]);
    assert_eq!(card_title(&failed), "Done · 1 rows · 1 failed");
}

#[test]
fn the_footer_states_the_page_and_the_call_total() {
    let rows: Vec<_> = (0..14)
        .map(|i| entry("t", &format!(" ({i})"), Some(true)))
        .collect();
    let rows = collapse_repeats(&rows);
    assert_eq!(
        card_footer(&rows, 1, 38),
        "Page 2/3 · rows 6-10 of 14 (38 calls, repeats collapsed)"
    );
}

#[test]
fn the_log_lists_every_call_without_collapsing() {
    let entries = vec![
        entry("bash", " (x)", Some(true)),
        entry("bash", " (x)", Some(false)),
    ];
    let log = log_markdown(&entries);
    assert!(log.starts_with("# Tool calls"), "{log}");
    assert!(log.contains("2 calls · 1 failed"), "{log}");
    assert!(log.contains("1. ✅ **bash** (x)"), "{log}");
    assert!(log.contains("2. ❌ **bash** (x)"), "{log}");
}

// ── The card in Discord terms: embed, buttons and the paged view (#1880) ──

use crate::channels::discord::tool_group::{
    GroupState, render_components, render_content, render_embeds,
};
use std::time::Instant;

fn card_group(entries: usize, page: usize, expanded: bool) -> GroupState {
    GroupState {
        entries: (0..entries)
            .map(|i| entry("tool", &format!(" (step {i})"), Some(true)))
            .collect(),
        notes: Vec::new(),
        expanded,
        page,
        started_at: Instant::now(),
        settled: None,
    }
}

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("builders serialize")
}

#[test]
fn a_collapsed_group_has_no_card_and_its_text_has_no_rows() {
    let g = card_group(14, 0, false);
    assert!(render_embeds(&g).is_empty());
    assert!(!render_content(&g).contains("step 0"));
}

#[test]
fn an_expanded_group_shows_one_page_of_rows_in_the_card() {
    let g = card_group(14, 1, true);
    let card = json(&render_embeds(&g));
    assert!(card.contains("step 5") && card.contains("step 9"), "{card}");
    assert!(!card.contains("step 4"), "page 2 starts at row 6: {card}");
    assert!(card.contains("Page 2/3 · rows 6-10 of 14"), "{card}");
}

#[test]
fn the_last_page_is_clamped_when_the_page_is_out_of_range() {
    let g = card_group(14, 9, true);
    let card = json(&render_embeds(&g));
    assert!(card.contains("Page 3/3 · rows 11-14 of 14"), "{card}");
}

#[test]
fn the_card_names_the_running_tool_in_its_header() {
    let mut g = card_group(2, 0, true);
    g.entries[1].status = None;
    let card = json(&render_embeds(&g));
    assert!(card.contains("Running: tool (step 1)"), "{card}");
}

#[test]
fn collapsed_buttons_are_expand_and_log() {
    let json_rows = json(&render_components(&card_group(3, 0, false), 7));
    assert!(json_rows.contains("toolgroup:7"), "{json_rows}");
    assert!(json_rows.contains("tglog:7"), "{json_rows}");
    assert!(!json_rows.contains("tgpage:"), "{json_rows}");
}

#[test]
fn expanded_buttons_page_in_both_directions_and_disable_at_the_ends() {
    let first = json(&render_components(&card_group(14, 0, true), 7));
    assert!(
        first.contains("tgpage:7:0"),
        "previous stays on page 1: {first}"
    );
    assert!(first.contains("tgpage:7:1"), "next goes to page 2: {first}");
    let last = json(&render_components(&card_group(14, 2, true), 7));
    assert!(
        last.contains("tgpage:7:2"),
        "next stays on the last page: {last}"
    );
    assert!(
        last.contains("tgpage:7:1"),
        "previous goes to page 2: {last}"
    );
    assert!(last.contains("tglog:7") && last.contains("toolgroup:7"));
}

#[test]
fn the_log_is_the_full_list_not_one_page() {
    let g = card_group(14, 0, true);
    let log = log_markdown(&g.entries);
    assert!(log.contains("14. ✅ **tool** (step 13)"), "{log}");
}

// ── Matching a completion to its row (#1880: the stale gear) ──

fn with_id(name: &str, context: &str, call_id: &str, status: Option<bool>) -> GroupEntry {
    GroupEntry {
        name: name.to_string(),
        context: context.to_string(),
        call_id: call_id.to_string(),
        task_id: None,
        status,
    }
}

#[test]
fn a_completion_finishes_its_own_call_when_two_calls_of_one_tool_run() {
    use crate::channels::discord::tool_card::entry_matches_completion;
    let first = with_id("bash", " (grep a)", "call_1", None);
    let second = with_id("bash", " (grep b)", "call_2", None);
    // The second call finishes first, which the name-only match got wrong.
    assert!(entry_matches_completion(&second, "bash", "call_2"));
    assert!(!entry_matches_completion(&first, "bash", "call_2"));
}

#[test]
fn an_entry_without_an_id_still_matches_by_name() {
    use crate::channels::discord::tool_card::entry_matches_completion;
    let legacy = with_id("bash", " (x)", "", None);
    assert!(entry_matches_completion(&legacy, "bash", ""));
    assert!(entry_matches_completion(&legacy, "bash", "call_9"));
    assert!(!entry_matches_completion(&legacy, "read_file", "call_9"));
}

#[test]
fn the_discord_consumers_match_completions_by_call_id() {
    for src in [
        include_str!("../channels/discord/handler.rs"),
        include_str!("../channels/discord/interactions.rs"),
        include_str!("../channels/discord/tracked_turn.rs"),
    ] {
        assert!(
            src.contains("entry_matches_completion(") && src.contains("&tool_call_id"),
            "a consumer still matches completions by name alone"
        );
    }
}

// ── A card survives a restart: the stored snapshot (#1880) ──

#[test]
fn a_settled_card_round_trips_through_the_stored_snapshot() {
    use crate::channels::background_work::FlowOutcome;
    use crate::channels::discord::tool_card_store::{restore, snapshot};
    use crate::channels::discord::tool_group::SettledStatus;
    use std::time::Duration;

    let mut g = card_group(14, 2, true);
    g.entries[3].call_id = "call_3".to_string();
    g.entries[3].status = Some(false);
    g.notes = vec!["checking the build".to_string()];
    g.started_at = Instant::now() - Duration::from_secs(252);
    g.settled = Some(SettledStatus {
        elapsed: Duration::from_secs(247),
        ctx: Some("ctx: 105K/200K 53%".to_string()),
        waiting: None,
        outcome: Some(FlowOutcome::TimedOut),
    });

    let json = serde_json::to_string(&snapshot(&g)).expect("encodes");
    let stored = serde_json::from_str(&json).expect("decodes");
    let back = restore(stored);

    assert_eq!(back.entries.len(), 14);
    assert_eq!(back.entries[3].call_id, "call_3");
    assert_eq!(back.entries[3].status, Some(false));
    assert_eq!(back.notes, g.notes);
    assert!(back.expanded);
    assert_eq!(back.page, 2);
    let settled = back.settled.expect("settled survives");
    assert_eq!(settled.elapsed, Duration::from_secs(247));
    assert_eq!(settled.ctx.as_deref(), Some("ctx: 105K/200K 53%"));
    assert!(matches!(settled.outcome, Some(FlowOutcome::TimedOut)));
    // The clock resumes from the stored elapsed, not from zero.
    assert!(back.started_at.elapsed() >= Duration::from_secs(251));
}

#[test]
fn every_card_mutation_writes_through_to_sqlite() {
    let src = include_str!("../channels/discord/tool_group.rs");
    let writes = src.matches("self.persist_tool_group(").count();
    assert!(
        writes >= 7,
        "expected each mutation to persist, found {writes} write-through calls"
    );
}
