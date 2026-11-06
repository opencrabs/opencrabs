//! #1649 delta-only compaction probes.
//!
//! One invariant per failure mode from the issue:
//! 1. Nesting — a prior marker never re-enters the summariser input.
//! 2. Attribution — the triggering prompt is part of the delta.
//! 3. Delta apply — a frozen segment appends; the first marker survives.
//! 4. Consolidation — crowded segments switch scope and merge into one
//!    untagged (superseding) marker, tail preserved.
//! 5. First-compaction parity — no marker means full window and an empty
//!    prompt prelude (byte-identical classic prompt).
//! 6. Seam parity — sync and background consume the same input helper.
//! 7. Reload — a segment stream reloads with every segment kept; a legacy
//!    stream is byte-identical to the old loader behaviour.

use crate::brain::agent::context::{
    AgentContext, COMPACTION_MARKER_PREFIX, CompactionScope, SEGMENT_SENTINEL,
};
use crate::brain::agent::service::AgentService;
use crate::brain::provider::{ContentBlock, Message};

fn ctx(max_tokens: usize) -> AgentContext {
    AgentContext::new(uuid::Uuid::nil(), max_tokens)
}

fn text_of(msg: &Message) -> String {
    msg.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A DB stream row shaped like the real writers (#175: `user` role, content
/// BEGINNING with the marker prefix).
fn db_row(content: String) -> crate::db::models::Message {
    crate::db::models::Message {
        forward: None,
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::nil(),
        role: "user".to_string(),
        content,
        sequence: 0,
        created_at: chrono::Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    }
}

#[test]
fn delta_input_excludes_prior_frozen_segments() {
    let mut c = ctx(100_000);
    c.add_message(Message::user("q1"));
    c.add_message(Message::assistant("a1"));
    c.compact_with_summary("SUMMARY ONE about the first task".to_string(), 0);

    c.add_message(Message::user("q2"));
    c.add_message(Message::assistant("a2"));

    let (scope, input, _) = c.compaction_input();
    assert_eq!(scope, CompactionScope::DeltaSinceMarker);
    let texts: Vec<String> = input.iter().map(text_of).collect();
    assert_eq!(
        texts,
        vec!["q2".to_string(), "a2".to_string()],
        "delta input must be exactly the post-marker messages"
    );
    assert!(
        !texts.iter().any(|t| t.contains("SUMMARY ONE")),
        "prior marker re-entered the summariser input — the #1649 nesting bug"
    );
}

#[test]
fn delta_input_keeps_the_triggering_prompt() {
    let mut c = ctx(100_000);
    c.add_message(Message::user("q1"));
    c.compact_with_summary("SUMMARY ONE".to_string(), 0);
    c.add_message(Message::user("continue with stage two NOW"));

    let (_, input, _) = c.compaction_input();
    let last = input.last().expect("delta is non-empty");
    assert!(
        text_of(last).contains("continue with stage two NOW"),
        "the triggering prompt must be part of the delta"
    );
}

#[test]
fn delta_apply_appends_a_frozen_segment_and_keeps_the_first() {
    let mut c = ctx(100_000);
    c.add_message(Message::user("q1"));
    c.compact_with_summary("SUMMARY ONE".to_string(), 0);
    c.add_message(Message::user("q2"));
    c.add_message(Message::assistant("a2"));

    AgentService::apply_scoped_compaction_summary(
        &mut c,
        CompactionScope::DeltaSinceMarker,
        "SEGMENT TWO body",
    );

    assert_eq!(
        c.messages.len(),
        2,
        "delta apply must collapse to [marker1, marker2]"
    );
    let first = text_of(&c.messages[0]);
    let second = text_of(&c.messages[1]);
    assert!(
        first.contains("SUMMARY ONE"),
        "the first marker must survive verbatim"
    );
    assert!(second.contains("SEGMENT TWO body"));
    assert!(
        second.contains(SEGMENT_SENTINEL),
        "delta markers carry the segment sentinel"
    );
    assert!(
        !first.contains(SEGMENT_SENTINEL),
        "the full-window marker stays untagged"
    );
}

#[test]
fn crowded_segments_consolidate_into_one_superseding_marker() {
    let mut c = ctx(2_000);
    c.add_message(Message::user("q1"));
    c.compact_with_summary(format!("FIRST {}", "x".repeat(8_000)), 0);
    c.add_message(Message::user("q2"));
    AgentService::apply_scoped_compaction_summary(
        &mut c,
        CompactionScope::DeltaSinceMarker,
        &format!("SECOND {}", "y".repeat(8_000)),
    );
    c.add_message(Message::user("q3 live tail"));

    assert_eq!(c.compaction_scope(), CompactionScope::SegmentConsolidation);

    let (scope, input, _) = c.compaction_input();
    assert_eq!(scope, CompactionScope::SegmentConsolidation);
    assert_eq!(
        input.len(),
        1,
        "consolidation input is the joined segments as one message"
    );
    let joined = text_of(&input[0]);
    assert!(joined.contains("FIRST") && joined.contains("SECOND"));

    AgentService::apply_scoped_compaction_summary(
        &mut c,
        CompactionScope::SegmentConsolidation,
        "MERGED SEGMENTS",
    );

    assert_eq!(c.messages.len(), 2, "one marker plus the live tail");
    assert!(text_of(&c.messages[0]).contains("MERGED SEGMENTS"));
    assert!(
        !text_of(&c.messages[0]).contains(SEGMENT_SENTINEL),
        "the consolidation marker is untagged — the loader treats it as a boundary"
    );
    assert!(text_of(&c.messages[1]).contains("live tail"));
}

#[test]
fn first_compaction_is_full_window_with_the_classic_prompt() {
    let mut c = ctx(100_000);
    for m in [
        Message::user("q1"),
        Message::assistant("a1"),
        Message::user("q2"),
    ] {
        c.add_message(m);
    }

    let (scope, input, tokens) = c.compaction_input();
    assert_eq!(scope, CompactionScope::FullWindow);
    assert_eq!(input.len(), 3);
    assert_eq!(tokens, c.token_count);

    // The classic prompt is byte-identical: empty prelude for FullWindow.
    assert_eq!(
        AgentService::compaction_scope_prelude(CompactionScope::FullWindow),
        ""
    );
    let delta_prelude = AgentService::compaction_scope_prelude(CompactionScope::DeltaSinceMarker);
    assert!(delta_prelude.starts_with("SCOPE:") && delta_prelude.contains('\n'));
}

#[test]
fn sync_and_background_paths_share_the_input_seam() {
    // Fresh context: what compact_context sends by hand (whole window +
    // FullWindow) is exactly what compaction_input() returns — the two paths
    // cannot drift because there is only one input helper.
    let mut fresh = ctx(100_000);
    fresh.add_message(Message::user("q1"));
    fresh.add_message(Message::assistant("a1"));
    let (scope, input, tokens) = fresh.compaction_input();
    assert_eq!(scope, CompactionScope::FullWindow);
    assert_eq!(input.len(), fresh.messages.len());
    assert_eq!(tokens, fresh.token_count);

    // Post-marker context: the spawn's scope and the apply's scope come from
    // the same helper, so a delta summary can never be applied as a
    // full-window clear (which would wipe the frozen segments).
    let mut marked = ctx(100_000);
    marked.add_message(Message::user("q1"));
    marked.compact_with_summary("SUMMARY ONE".to_string(), 0);
    marked.add_message(Message::user("q2"));
    let scope = marked.compaction_scope();
    assert_eq!(scope, CompactionScope::DeltaSinceMarker);
    AgentService::apply_scoped_compaction_summary(&mut marked, scope, "SEGMENT TWO");
    assert!(
        marked
            .messages
            .iter()
            .any(|m| text_of(m).contains("SUMMARY ONE"))
    );
}

#[test]
fn segment_streams_reload_and_legacy_streams_stay_byte_identical() {
    // Segment stream: anchor on the last NON-segment marker, keep everything
    // from it onward — no segment is dropped on reload.
    let rows = vec![
        db_row(format!("{COMPACTION_MARKER_PREFIX} — SUMMARY ONE")),
        db_row(format!(
            "{COMPACTION_MARKER_PREFIX} — {SEGMENT_SENTINEL} SEGMENT TWO"
        )),
        db_row("q3".to_string()),
    ];
    let kept = AgentService::messages_from_last_compaction(rows);
    assert_eq!(kept.len(), 3, "segments must survive reload");
    assert!(kept[1].content.contains(SEGMENT_SENTINEL));

    // Legacy stream: byte-identical to the old loader — everything from the
    // (only, untagged) marker onward.
    let legacy = vec![
        db_row("stale pre-marker".to_string()),
        db_row(format!("{COMPACTION_MARKER_PREFIX} — SUMMARY ONE")),
        db_row("q2".to_string()),
    ];
    let kept = AgentService::messages_from_last_compaction(legacy);
    assert_eq!(kept.len(), 2);
    assert!(kept[0].content.contains("SUMMARY ONE"));
    assert_eq!(kept[1].content, "q2");
}

/// #1928: the sentinel is matched ANCHORED at the banner, never as a
/// substring. A full-window summary whose BODY quotes "DELTA SEGMENT." is
/// still the reload boundary — a bare `contains` skipped it, so the reload
/// anchored on the newest segment and dropped the restart.
#[test]
fn sentinel_quoted_in_a_summary_body_does_not_hide_the_boundary() {
    let rows = vec![
        db_row("stale pre-marker".to_string()),
        // A real full-window boundary whose summary body happens to quote the
        // sentinel — e.g. a lane describing this very bug in prose.
        db_row(format!(
            "{COMPACTION_MARKER_PREFIX} — The conversation was automatically compacted.\n\n\
             We were fixing the DELTA SEGMENT. loader bug."
        )),
        // A real delta segment sitting on top of it.
        db_row(format!(
            "{COMPACTION_MARKER_PREFIX} — {SEGMENT_SENTINEL} SEGMENT TWO"
        )),
        db_row("q3".to_string()),
    ];
    let kept = AgentService::messages_from_last_compaction(rows);
    // The boundary is the full-window marker (row 1), NOT the segment (row 2):
    // the reload keeps `[boundary][segment][tail]`. A `contains` match would
    // skip BOTH, find no boundary, and return the whole stream unanchored.
    assert_eq!(
        kept.len(),
        3,
        "the quoted-sentinel boundary must anchor the reload"
    );
    assert!(kept[0].content.contains("DELTA SEGMENT. loader bug"));
    assert!(kept[1].content.contains(SEGMENT_SENTINEL));
    assert_eq!(kept[2].content, "q3");
}

/// #1928-C: the PERSIST seam. The production chain is
/// `apply_scoped_compaction_summary` (welds the marker into the live context
/// and — post-#1928 — returns its exact bytes) → `CompactionOutcome::Summarised(applied)`
/// → `marker("")`, which is what `apply_compaction_continuation` writes to the DB.
///
/// A delta compaction must persist a row that BEGINS with the delta banner.
/// On the pre-#1928 tree `marker("")` rebuilt the full-window banner from the
/// summary, so the sentinel never reached the DB and the delta leg below fails
/// at the `persisted` assertion — that is the defect this test pins.
#[test]
fn delta_persist_seam_writes_the_sentinel_banner() {
    use crate::brain::agent::service::compaction::CompactionOutcome;

    let delta_banner = format!("{COMPACTION_MARKER_PREFIX} — {SEGMENT_SENTINEL}");

    // ── Delta leg: a prior marker makes the scope a delta. ─────────────
    let mut c = ctx(100_000);
    c.add_message(Message::user("q1"));
    c.compact_with_summary("SUMMARY ONE".to_string(), 0);
    c.add_message(Message::user("q2"));

    let applied = AgentService::apply_scoped_compaction_summary(
        &mut c,
        CompactionScope::DeltaSinceMarker,
        "SEGMENT TWO body",
    );
    assert!(
        applied.starts_with(&delta_banner),
        "the apply step must weld a sentinel-bannered marker; got: {applied}"
    );

    // The exact construction the persist path performs.
    let persisted = CompactionOutcome::Summarised(applied.clone()).marker("");
    assert!(
        persisted.starts_with(&delta_banner),
        "a delta compaction must persist a row beginning with the delta banner; \
         got: {persisted}"
    );
    assert_eq!(
        persisted, applied,
        "the persist path must write the applied bytes verbatim, not a rebuilt banner"
    );

    // ── Control: a full window must NOT look like a segment. ───────────
    let mut f = ctx(100_000);
    f.add_message(Message::user("q1"));
    let applied_full = AgentService::apply_scoped_compaction_summary(
        &mut f,
        CompactionScope::FullWindow,
        "FULL BODY",
    );
    assert!(
        !applied_full.starts_with(&delta_banner),
        "control: a full-window marker must not carry the sentinel banner"
    );
    let persisted_full = CompactionOutcome::Summarised(applied_full.clone()).marker("");
    assert!(
        !persisted_full.starts_with(&delta_banner),
        "control: the full-window persist path must stay untagged, or the loader \
         would skip the real boundary"
    );
    assert_eq!(persisted_full, applied_full);

    // ── Negative control: the pre-#1928 rebuild must fail the predicate. ──
    // This is the exact string the old `marker()` emitted for a Summarised
    // outcome — the full-window banner wrapped around the payload. If the
    // predicate above ever stopped discriminating, this control would catch
    // it: the rebuild does NOT begin with the delta banner.
    let legacy_rebuild = format!(
        "[CONTEXT COMPACTION — The conversation was automatically compacted. \
         Below is a structured summary of everything before this point.]\n\n{applied}"
    );
    assert!(
        !legacy_rebuild.starts_with(&delta_banner),
        "the pre-#1928 rebuild must not satisfy the sentinel-banner predicate"
    );
}
