//! The compaction continuation document has ONE budget, shared by both sides (#1930).
//!
//! Observed 2026-10-04 (ops profile, live): the summariser request was allowed
//! `bounded_output_tokens(65_536, 200_000)` = 40 000 output tokens while the
//! input budget reserved a hard-coded `8_000 + 1_000` = 9 000 for it — a 4.44x
//! disagreement — and no code path trimmed the result. 208 of the 210 live
//! markers written that day exceeded a 3 000-token budget (mean 27.8 KB,
//! max 69.2 KB), and the worst session compacted 59 times in one day.
//!
//! Both sides now derive from the same arithmetic: the DOCUMENT budget
//! (`COMPACTION_SUMMARY_MAX_TOKENS`) plus a reasoning allowance for the request
//! (#1933). This test is the drift guard: it fails if either side re-acquires a
//! literal, which is the one edit that silently restores the 4.44x disagreement.

use crate::brain::agent::service::AgentService;
use crate::brain::agent::service::context::parse_context_manifest;
use crate::brain::agent::service::request_budget::{
    COMPACTION_SUMMARY_MAX_TOKENS, COMPACTION_SUMMARY_REASONING_HEADROOM_TOKENS,
    compaction_summary_input_reserve, compaction_summary_output_tokens,
    compaction_summary_request_allowance,
};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

/// The input reserve must COVER the output allowance.
///
/// This is the incident inverted: 9 000 reserved against 40 000 allowed.
#[test]
fn the_input_reserve_covers_the_output_allowance() {
    let reserve = compaction_summary_input_reserve();
    let allowance = compaction_summary_request_allowance() as usize;
    assert!(
        reserve > allowance,
        "input reserve ({reserve}) must exceed the summariser's output allowance \
         ({allowance}): a reserve smaller than the allowance hands the summariser \
         more input than the context window can hold (#1930)"
    );
    // #1933: the request allowance must be LARGER than the document budget, or
    // the reasoning is billed inside the document and truncates it.
    assert!(
        allowance > compaction_summary_output_tokens() as usize,
        "the request allowance ({allowance}) must exceed the document budget \
         ({}): equal values are exactly what cut the document at source (#1933)",
        compaction_summary_output_tokens()
    );
}

/// The reserve is the request allowance plus prompt headroom, and nothing else.
#[test]
fn the_reserve_is_derived_from_the_one_constant() {
    assert_eq!(
        compaction_summary_output_tokens(),
        COMPACTION_SUMMARY_MAX_TOKENS,
        "the DOCUMENT budget must BE the shared constant"
    );
    assert_eq!(
        compaction_summary_request_allowance(),
        COMPACTION_SUMMARY_MAX_TOKENS + COMPACTION_SUMMARY_REASONING_HEADROOM_TOKENS,
        "the request allowance must be the document budget plus the reasoning headroom (#1933)"
    );
    let headroom =
        compaction_summary_input_reserve() - compaction_summary_request_allowance() as usize;
    assert_eq!(
        headroom, 1_000,
        "the reserve must be the request allowance plus the prompt headroom"
    );
}

/// Both call sites reference the shared helpers, and neither has a literal left.
#[test]
fn both_call_sites_derive_from_the_constant() {
    let context_src = include_str!("../brain/agent/service/context.rs");
    let compaction_src = include_str!("../brain/agent/service/compaction.rs");

    assert!(
        context_src.contains("compaction_summary_input_reserve()"),
        "context.rs must size the input budget from the shared reserve helper"
    );
    assert!(
        !context_src.contains("8_000usize"),
        "context.rs must not carry a hard-coded output reserve literal (#1930)"
    );
    assert!(
        context_src.contains("compaction_summary_request_allowance()"),
        "the manual /compact call site must pass the shared REQUEST allowance (#1933)"
    );
    assert!(
        compaction_src.contains("compaction_summary_request_allowance()"),
        "the background compaction call site must pass the shared REQUEST allowance (#1933)"
    );
    // The trim guard still enforces the DOCUMENT budget, not the request
    // allowance: a generous allowance is safe only because the guard trims.
    assert!(
        context_src.contains("compaction_summary_output_tokens()"),
        "the trim guard must keep trimming to the DOCUMENT budget (#1933)"
    );
}

/// The rendered directive states the budget AND the order the budget is spent in.
///
/// Naming the size is not enough on its own: a budget with no retention order
/// lets the model spend it on whichever section it writes first, which is how
/// the load-bearing blocks (obligation status, manifest fence) get crowded out.
#[test]
fn the_rendered_directive_states_the_budget_and_the_retention_order() {
    let directive = AgentService::compaction_budget_directive(COMPACTION_SUMMARY_MAX_TOKENS);

    assert!(
        directive.contains(&COMPACTION_SUMMARY_MAX_TOKENS.to_string()),
        "the directive must state the token budget it enforces; got: {directive}"
    );
    assert!(
        directive.contains("context-manifest"),
        "the directive must name the mandatory context-manifest fence as a MUST-keep block"
    );
    assert!(
        directive.contains("FIRST TO GO"),
        "the directive must name what is dropped first when the budget is tight"
    );
    // The blocks a woken agent cannot work without must be named as MUST-keep,
    // not left to the model's judgement of what matters.
    for block in ["section 0", "section 8", "section 7"] {
        assert!(
            directive.contains(block),
            "the directive must name {block} among the MUST-keep blocks; got: {directive}"
        );
    }
}

/// The summariser prompt no longer orders maximum verbosity.
///
/// This is the incident's own wording, and it is what a 27.8 KB mean against a
/// 3 000-token budget was obeying.
#[test]
fn the_prompt_no_longer_orders_maximum_verbosity() {
    let system_prompt = AgentService::compaction_system_prompt();
    assert!(
        !system_prompt.contains("verbose"),
        "the summariser system prompt must not invite verbosity (#1930): {system_prompt}"
    );

    let context_src = include_str!("../brain/agent/service/context.rs");
    assert!(
        !context_src.contains("BE EXHAUSTIVE"),
        "the continuation-document prompt must not order maximum size (#1930)"
    );
}

// ---------------------------------------------------------------------------
// The post-generation trim guard (#1930, part 2).
//
// The prompt now ASKS for a 3 000-token document. Asking is not enforcing: on
// 2026-10-04 the live markers read a mean of 27 831 B (max 69 154 B, 208 of 210
// over budget), because a 40 000-token output allowance sat against a 9 000
// -token input reserve and nothing trimmed the result. `enforce_summary_budget`
// is that trimmer, and these tests pin its contract.
// ---------------------------------------------------------------------------

/// A tight budget for the fixture below: large enough that the must-keep blocks
/// fit, small enough that the bulk sections cannot.
///
/// Sized with margin rather than near the trimmed result: the fixture is
/// ~33.5 KB (~8 300 tokens), the guard trims it to ~1.4 KB (~360 tokens), and
/// the assertion is that it lands UNDER the budget. Pinning the budget at the
/// trimmed size would make the test a coin-flip on the tokenizer's exact ratio
/// (a red gate costs a whole CI cycle); 1 000 still forces every bulk section
/// out, so the guard is exercised exactly as intended.
const TIGHT_BUDGET: usize = 1_000;

/// A document shaped like the real one: small load-bearing blocks, large bulk.
fn oversized_summary() -> String {
    let bulk = |label: &str| {
        format!(
            "## {label}\n{}",
            "the summariser recorded this in great detail, at length, twice over, \
             with file paths, line numbers and quoted code, because it was told to be \
             exhaustive and nothing ever trimmed it. "
                .repeat(30)
        )
    };

    format!(
        "CRITICAL: The context window is at 91% capacity.\n\
         You are creating a COMPREHENSIVE CONTINUATION DOCUMENT.\n\n\
         ## 0. IMMEDIATE TASK (CRITICAL — MOST IMPORTANT SECTION)\n\
         **Obligation status: OPEN**\n\
         CONTINUE THIS TASK: finish the trim guard. Do NOT deviate.\n\n\
         {}\n{}\n{}\n{}\n{}\n{}\n\n\
         ## 7. Recovery Playbook\n\
         Run `git log --oneline -3` and read the plan file.\n\n\
         ## 8. Next Step\n\
         Add the guard, then commit it.\n\n\
         ## 9. Continuation Message\n\
         Picking the guard back up right now.\n\n\
         ## 10. Context Manifest (MANDATORY MACHINE-READABLE BLOCK)\n\
         ```context-manifest\n\
         active_skills:\n  - opencrabs-dev/editor.md\n\
         discard_skills:\n  - meta-factory\n\
         required_tools:\n  - plan\n\
         ```\n",
        bulk("1. Chronological Analysis"),
        bulk("2. Files Modified"),
        bulk("3. User Preferences & Constraints"),
        bulk("4. Errors & Corrections"),
        bulk("5. All User Messages"),
        bulk("6. Pending Tasks"),
    )
}

/// AC2 — a forced compaction under a tight budget keeps the two blocks a woken
/// agent cannot recover from anywhere else: the §0 obligation status and a
/// parseable `context-manifest` fence.
#[test]
fn a_trimmed_document_keeps_its_obligation_status_and_manifest_fence() {
    let oversized = oversized_summary();
    let before = crate::brain::tokenizer::count_tokens(&oversized);
    assert!(
        before > TIGHT_BUDGET,
        "the fixture must actually exceed the tight budget ({before} vs {TIGHT_BUDGET}), \
         or the guard is never exercised and this test proves nothing"
    );

    let trimmed = AgentService::enforce_summary_budget(oversized.clone(), TIGHT_BUDGET, &[]);
    let after = crate::brain::tokenizer::count_tokens(&trimmed);

    assert!(
        after < before,
        "the guard must drop content when the document is over budget ({before} -> {after})"
    );
    assert!(
        after <= TIGHT_BUDGET,
        "the trimmed document must fit the budget it was given ({after} > {TIGHT_BUDGET}):\n{trimmed}"
    );
    assert!(
        parse_context_manifest(&trimmed).is_some(),
        "the trimmed document must still carry a parseable context-manifest fence:\n{trimmed}"
    );
    assert!(
        trimmed.to_lowercase().contains("obligation status"),
        "the trimmed document must still carry the §0 obligation-status line:\n{trimmed}"
    );
    // And the bulk really was what went — a guard that keeps everything and
    // merely rewrites whitespace would satisfy none of the above.
    assert!(
        !trimmed.contains("## 1. Chronological Analysis\nthe summariser recorded"),
        "a bulk section's body must be dropped whole, not left in place:\n{trimmed}"
    );
}

/// A document that already fits is returned byte-for-byte: the guard is a
/// no-op on the normal path, so it cannot be blamed for reshaping summaries.
#[test]
fn a_document_within_budget_is_returned_unchanged() {
    let doc = "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\n\n\
               ## 10. Context Manifest\n```context-manifest\n\
               active_skills:\n  - opencrabs-dev\n```\n";
    let out = AgentService::enforce_summary_budget(
        doc.to_string(),
        COMPACTION_SUMMARY_MAX_TOKENS as usize,
        &[],
    );
    assert_eq!(
        out, doc,
        "an under-budget document must pass through untouched"
    );
}

/// The guard ships an over-budget document rather than cut the obligation out.
///
/// When the must-keep blocks alone exceed the budget there is nothing safe to
/// trim: silently deleting §0 would leave the woken agent with no task at all,
/// which is strictly worse than a large document. The guard must WARN and keep
/// them (log line `must-keep sections alone are N tokens`).
#[test]
fn the_guard_keeps_the_obligation_rather_than_trim_it() {
    let doc = format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\n{}\n\n\
         ## 10. Context Manifest\n```context-manifest\n\
         active_skills:\n  - opencrabs-dev\n```\n",
        "CONTINUE THIS TASK: keep going. ".repeat(400)
    );
    let out = AgentService::enforce_summary_budget(doc, 100, &[]);

    assert!(
        out.to_lowercase().contains("obligation status"),
        "the obligation status must survive even when it alone busts the budget"
    );
    assert!(
        parse_context_manifest(&out).is_some(),
        "the manifest fence must survive even when the budget cannot be met"
    );
}

/// The guard runs BEFORE the #482 artifact correction, inside the summariser.
///
/// Order matters: the correction is appended after trimming, so a correction can
/// never be trimmed away — and a correction that was trimmed would be the exact
/// silent-failure mode #482 exists to prevent.
#[test]
fn the_guard_runs_before_the_artifact_correction() {
    let src = include_str!("../brain/agent/service/context.rs");
    let fn_start = src
        .find("pub(super) async fn compute_compaction_summary(")
        .expect("compute_compaction_summary must exist");
    let guard = src
        .find("Self::enforce_summary_budget(")
        .expect("compute_compaction_summary must call the budget guard (#1930)");
    let correction = src
        .find("Self::flag_unbacked_artifacts(summary, &snapshot_messages)")
        .expect("the #482 artifact correction must still run");

    assert!(
        fn_start < guard,
        "the guard call must live inside compute_compaction_summary"
    );
    assert!(
        guard < correction,
        "the budget guard must run BEFORE the #482 artifact correction, so a \
         correction is never trimmed away (#1930)"
    );
}

/// #1933 (head-protect): the summariser prompt must order the load-bearing
/// blocks BEFORE the prose, because a MaxTokens cut removes the TAIL.
///
/// Measured 2026-10-05: 48 continuation documents stopped at
/// `MaxTokens (output 5500 / reasoning 0)` — the model writes a 5 500-token
/// document against a 3 000-token budget and the cut takes whatever was
/// written last. With §10 mandated "At the very end", the cut removed exactly
/// the machine-readable fence the harness needs, and `extract_manifest_block`
/// cannot re-attach a fence the source never carried.
///
/// The ordering lives inside the `base_prompt` format string, built in an
/// async fn that needs a provider — so, like the guard-order test above, this
/// reads the source text.
#[test]
fn the_prompt_emits_the_must_keep_blocks_before_the_prose() {
    let src = include_str!("../brain/agent/service/context.rs");
    let start = src
        .find("let base_prompt = format!(")
        .expect("the summariser prompt must be built in one format string");
    let end = start
        + src[start..]
            .find("\n        );")
            .expect("the prompt format string must close");
    let prompt = &src[start..end];

    let pos = |needle: &str| {
        prompt
            .find(needle)
            .unwrap_or_else(|| panic!("the summariser prompt must contain {needle:?}"))
    };

    let s0 = pos("## 0. IMMEDIATE TASK");
    let manifest = pos("## 10. Context Manifest");
    let s7 = pos("## 7. Recovery Playbook");
    let s8 = pos("## 8. Next Step");
    let s1 = pos("## 1. Chronological Analysis");
    let s6 = pos("## 6. Pending Tasks");
    let s9 = pos("## 9. Continuation Message");

    assert!(s0 < manifest, "§0 (the obligation) must be emitted first");
    assert!(
        manifest < s7 && s7 < s8,
        "the must-keep blocks must be emitted 0 -> manifest -> §7 -> §8 (#1933)"
    );
    assert!(
        s8 < s1,
        "§7/§8 must precede the prose (§1-§6), so a tail cut removes only prose (#1933)"
    );
    assert!(
        manifest < s1,
        "the `context-manifest` fence must be emitted AHEAD of the prose (§1-§6): \
         a tail cut of any size must not be able to remove it (#1933)"
    );
    assert!(s6 < s9, "§9 (the continuation message) stays last");
    assert!(
        !prompt.contains("At the very end"),
        "§10 must no longer send the manifest to the very end of the document: \
         that is the placement the MaxTokens cut removed (#1933)"
    );
}

// ---------------------------------------------------------------------------
// The soak test (#1930, step 5) — the gate that would have caught #474.
//
// #474 was a compaction SPIRAL: each continuation document was fed back into
// the next summarisation request, so the document grew round over round
// (measured max 146 KB on 2026-09-21). #1649 (delta scoping) stopped the
// growth, but nothing capped the SIZE: the 2026-10-04 live read is a mean of
// 27 831 B, max 69 154 B, 208 of 210 markers over the ~11.7 KB target. This
// test drives five rounds and pins both properties at once.
//
// It is a RUNTIME falsifier, not merely a compile one: the raw rounds are
// asserted to be over budget, so a build without the guard (where the raw
// document would be the output) fails the boundedness assertion — not just
// the build.
// ---------------------------------------------------------------------------

/// Round `n`'s raw continuation document — the spiral's shape: every round
/// larger than the last, and every one far over budget.
fn summary_of_round(round: usize) -> String {
    let repeats = 200 + round * 200;
    let bulk = "the summariser recorded this in great detail, at length, twice over, \
                with file paths, line numbers and quoted code. "
        .repeat(repeats);
    format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\n\
         CONTINUE THIS TASK: keep going.\n\n\
         ## 1. Chronological Analysis\n{bulk}\n\n\
         ## 2. Files Modified\n{bulk}\n\n\
         ## 7. Recovery Playbook\nRun `git log --oneline -3`.\n\n\
         ## 8. Next Step\nCommit the guard.\n\n\
         ## 10. Context Manifest\n```context-manifest\n\
         active_skills:\n  - opencrabs-dev/editor.md\n```\n"
    )
}

#[test]
fn five_forced_compactions_stay_bounded() {
    let budget = COMPACTION_SUMMARY_MAX_TOKENS as usize;
    let ceiling = budget + budget / 10; // budget x 1.1

    let mut raw_sizes = Vec::new();
    let mut out_sizes = Vec::new();

    for round in 0..5 {
        let raw = summary_of_round(round);
        let raw_tokens = crate::brain::tokenizer::count_tokens(&raw);

        // The pre-fix reality: the raw document is over budget in every round.
        // If this stops holding, the fixture has gone slack and the guard would
        // be a no-op on it — the test would prove nothing.
        assert!(
            raw_tokens > budget,
            "round {round}: the raw document must exceed the budget \
             ({raw_tokens} vs {budget}), or the guard is never exercised"
        );

        let out = AgentService::enforce_summary_budget(raw, budget, &[]);
        let out_tokens = crate::brain::tokenizer::count_tokens(&out);

        assert!(
            out_tokens <= ceiling,
            "round {round}: the guarded document must fit budget x1.1 \
             ({out_tokens} > {ceiling}) — this is the #474 spiral, uncapped"
        );

        raw_sizes.push(raw_tokens);
        out_sizes.push(out_tokens);
    }

    // The spiral's shape, asserted on the INPUT side: the raw rounds grow.
    for w in raw_sizes.windows(2) {
        assert!(
            w[1] > w[0],
            "the fixture's raw rounds must grow round over round: {raw_sizes:?}"
        );
    }

    // ...and the property the fix exists to provide: the OUTPUT series does not
    // grow. Pre-fix this is exactly `raw_sizes` (the guard is absent, so the raw
    // document is the output), which grows monotonically — so this assertion is
    // what fails on the base commit.
    assert!(
        !out_sizes.windows(2).all(|w| w[1] > w[0]),
        "the guarded series must not grow monotonically round over round \
         (pre-fix it did: {out_sizes:?})"
    );
    assert!(
        out_sizes.iter().all(|&s| s <= ceiling),
        "every guarded round must be bounded ({out_sizes:?} vs ceiling {ceiling})"
    );
}

// ---------------------------------------------------------------------------
// Visibility (#1933): a SHORT document can still be INCOMPLETE.
//
// The guard used to return early when `before <= budget`, so the one failure
// that removes the load-bearing blocks — a summariser cut at source
// (`stop_reason = MaxTokens`) — was exactly the one it never inspected. The
// first real compaction on the live box after #1930 shipped wrote a 4 134-byte
// marker with no `context-manifest` fence at all: under budget, and unseen.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct EventCapture {
    events: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl EventCapture {
    fn warns(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(level, _)| level == "WARN")
            .map(|(_, message)| message.clone())
            .collect()
    }
}

impl<S: tracing::Subscriber> Layer<S> for EventCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        self.events.lock().unwrap().push((
            event.metadata().level().to_string(),
            visitor.message.unwrap_or_default(),
        ));
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }
}

/// A fence-less document that FITS the budget must still WARN.
///
/// This is the live regression inverted: the marker that lost its fence was
/// under budget, so the old guard returned without looking.
#[test]
fn an_under_budget_document_without_a_fence_warns() {
    let doc = "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\n\
               CONTINUE THIS TASK: it was cut before the manifest was written.\n";
    let tokens = crate::brain::tokenizer::count_tokens(doc);
    assert!(
        tokens <= COMPACTION_SUMMARY_MAX_TOKENS as usize,
        "the fixture must fit the budget ({tokens} > {}), or it takes the trim \
         path and proves nothing about the early return (#1933)",
        COMPACTION_SUMMARY_MAX_TOKENS
    );

    let capture = EventCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let out = tracing::subscriber::with_default(subscriber, || {
        AgentService::enforce_summary_budget(
            doc.to_string(),
            COMPACTION_SUMMARY_MAX_TOKENS as usize,
            &[],
        )
    });

    assert_eq!(
        out, doc,
        "an under-budget document is still returned unchanged"
    );
    let warns = capture.warns();
    assert!(
        warns.iter().any(|m| m.contains("context-manifest")),
        "a fence-less under-budget document must WARN (#1933); captured: {warns:?}"
    );
}

/// A complete under-budget document stays silent: the new check must not turn
/// every normal compaction into a warning.
#[test]
fn a_complete_under_budget_document_does_not_warn() {
    let doc = "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\n\
               CONTINUE THIS TASK: keep going.\n\n\
               ## 10. Context Manifest\n```context-manifest\n\
               active_skills:\n  - opencrabs-dev\n```\n";
    let capture = EventCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _out = tracing::subscriber::with_default(subscriber, || {
        AgentService::enforce_summary_budget(
            doc.to_string(),
            COMPACTION_SUMMARY_MAX_TOKENS as usize,
            &[],
        )
    });

    assert!(
        capture.warns().is_empty(),
        "a complete under-budget document must not warn; captured: {:?}",
        capture.warns()
    );
}

/// A `MaxTokens` stop is a truncated document, whatever its token count reads.
#[test]
fn a_maxtokens_stop_warns_that_the_document_is_truncated() {
    let usage = crate::brain::provider::TokenUsage {
        output_tokens: 3_000,
        reasoning_tokens: 1_842,
        ..Default::default()
    };

    let capture = EventCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::with_default(subscriber, || {
        AgentService::warn_if_summary_truncated(
            Some(crate::brain::provider::StopReason::MaxTokens),
            &usage,
        );
    });

    let warns = capture.warns();
    assert!(
        warns
            .iter()
            .any(|m| m.contains("MaxTokens") && m.contains("#1933")),
        "a MaxTokens stop must WARN that the document is truncated (#1933); captured: {warns:?}"
    );
}

/// A normal end-of-turn stop is not a truncation and must not warn.
#[test]
fn an_endturn_stop_does_not_warn() {
    let usage = crate::brain::provider::TokenUsage::default();
    let capture = EventCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::with_default(subscriber, || {
        AgentService::warn_if_summary_truncated(
            Some(crate::brain::provider::StopReason::EndTurn),
            &usage,
        );
    });

    assert!(
        capture.warns().is_empty(),
        "an EndTurn stop is not a truncation; captured: {:?}",
        capture.warns()
    );
}

// ---------------------------------------------------------------------------
// #1933 (head-protect, end to end): a `MaxTokens` cut removes the TAIL, so
// the ordering the prompt now mandates is what decides whether the manifest
// fence survives. The pair below is the discriminating test — the same
// content, the same cut, opposite outcomes — because a fixture that only
// restates the new order could never fail.
// ---------------------------------------------------------------------------

/// A ~34 KB continuation document in the MANDATED order: §0, the §10 manifest
/// fence, §7, §8, then the bulk prose, then §9. Small head, large tail.
fn head_protected_document() -> String {
    let prose = |label: &str| {
        format!(
            "## {label}\n{}\n\n",
            "the summariser recorded this at length. ".repeat(200)
        )
    };
    format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\n\
         CONTINUE THIS TASK: keep going.\n\n\
         ## 10. Context Manifest\n```context-manifest\n\
         active_skills:\n  - opencrabs-dev\n```\n\n\
         ## 7. Recovery Playbook\nRun `git log --oneline -3`.\n\n\
         ## 8. Next Step\nCommit the guard.\n\n\
         {}{}{}{}{}{}\
         ## 9. Continuation Message\nPicking it back up.\n",
        prose("1. Chronological Analysis"),
        prose("2. Files Modified"),
        prose("3. User Preferences & Constraints"),
        prose("4. Errors & Corrections"),
        prose("5. All User Messages"),
        prose("6. Pending Tasks"),
    )
}

/// The SAME content in the PRE-FIX order: §10 "at the very end". The control
/// arm — a tail cut must be able to remove the fence here, or the test above
/// discriminates nothing.
fn manifest_last_document() -> String {
    let prose = |label: &str| {
        format!(
            "## {label}\n{}\n\n",
            "the summariser recorded this at length. ".repeat(200)
        )
    };
    format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\n\
         CONTINUE THIS TASK: keep going.\n\n\
         ## 7. Recovery Playbook\nRun `git log --oneline -3`.\n\n\
         ## 8. Next Step\nCommit the guard.\n\n\
         {}{}{}{}{}{}\
         ## 9. Continuation Message\nPicking it back up.\n\n\
         ## 10. Context Manifest\n```context-manifest\n\
         active_skills:\n  - opencrabs-dev\n```\n",
        prose("1. Chronological Analysis"),
        prose("2. Files Modified"),
        prose("3. User Preferences & Constraints"),
        prose("4. Errors & Corrections"),
        prose("5. All User Messages"),
        prose("6. Pending Tasks"),
    )
}

/// A tail cut — the `MaxTokens` failure — keeps the manifest fence only when
/// the head is protected. Both arms take the identical cut.
#[test]
fn a_maxtokens_cut_keeps_the_manifest_only_when_the_head_is_protected() {
    let head = head_protected_document();
    let old = manifest_last_document();
    // Both fixtures are the same size, so a cut at the same FRACTION is the
    // same cut in bytes: whatever the model wrote last is what is lost.
    let cut = |d: &str| d[..d.len() / 2].to_string();

    let cut_head = cut(&head);
    let cut_old = cut(&old);

    assert!(
        parse_context_manifest(&cut_head).is_some(),
        "a tail cut of a head-protected document must still carry the \
         context-manifest fence (#1933)"
    );
    assert!(
        cut_head.to_lowercase().contains("obligation status"),
        "§0 (the obligation) must survive the same cut (#1933)"
    );
    // The control: the pre-fix ordering loses the fence to the SAME cut. If
    // this ever stops holding, the fixture has gone slack and the assertion
    // above proves nothing about the ordering.
    assert!(
        parse_context_manifest(&cut_old).is_none(),
        "the control arm (manifest last) must LOSE the fence to the same cut, \
         or this test cannot distinguish the two orderings"
    );
}

/// (c) A document cut at source is VISIBLE: the `MaxTokens` stop surfaces the
/// existing truncation WARN, and the head-protected fence still parses after
/// the trim — degradation reported, payload intact.
#[test]
fn a_cut_document_surfaces_the_truncation_warn_and_keeps_its_fence() {
    let doc = head_protected_document();
    let cut = doc[..doc.len() / 2].to_string();

    let usage = crate::brain::provider::TokenUsage {
        output_tokens: 5_500,
        reasoning_tokens: 0,
        ..Default::default()
    };

    let capture = EventCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let out = tracing::subscriber::with_default(subscriber, || {
        AgentService::warn_if_summary_truncated(
            Some(crate::brain::provider::StopReason::MaxTokens),
            &usage,
        );
        AgentService::enforce_summary_budget(
            cut.clone(),
            COMPACTION_SUMMARY_MAX_TOKENS as usize,
            &[],
        )
    });

    let warns = capture.warns();
    assert!(
        warns
            .iter()
            .any(|m| m.contains("MaxTokens") && m.contains("#1933")),
        "a document cut at source must surface the truncation WARN (#1933); \
         captured: {warns:?}"
    );
    assert!(
        parse_context_manifest(&out).is_some(),
        "the head-protected fence must survive both the cut and the trim:\n{out}"
    );
}

/// #3172: the stop verdict that counts as a failed attempt.
///
/// Only a `MaxTokens` stop means the document is cut. `EndTurn`, a stop
/// sequence and a missing verdict are all complete documents.
#[test]
fn a_maxtokens_stop_is_a_cut_and_nothing_else_is() {
    use crate::brain::provider::StopReason;

    assert!(AgentService::summary_stop_is_cut(Some(
        StopReason::MaxTokens
    )));
    assert!(!AgentService::summary_stop_is_cut(Some(
        StopReason::EndTurn
    )));
    assert!(!AgentService::summary_stop_is_cut(Some(
        StopReason::StopSequence
    )));
    assert!(!AgentService::summary_stop_is_cut(None));
}

/// #3172: the cut must fail the attempt BEFORE the text is extracted.
///
/// The old order warned and carried on, so an `Ok` carrying a mutilated
/// document consumed zero retries and shipped as the marker. The new order
/// returns `Err` first, which feeds the caller's 3-attempt retry loop and,
/// on a terminal cut, the honest `CompactionOutcome::Truncated` marker.
#[test]
fn the_cut_check_fails_the_attempt_before_extraction() {
    let src = include_str!("../brain/agent/service/context.rs");
    let fn_start = src
        .find("pub(super) async fn compute_compaction_summary(")
        .expect("compute_compaction_summary must exist");
    let cut_check = src[fn_start..]
        .find("if Self::summary_stop_is_cut(response.stop_reason)")
        .expect("compute_compaction_summary must gate on the cut predicate (#3172)");
    let extraction = src[fn_start..]
        .find("let summary = Self::extract_text_from_response(&response);")
        .expect("the text extraction must still exist");

    assert!(
        cut_check < extraction,
        "the cut check must run BEFORE extract_text_from_response, so a cut \
         document never reaches the marker path (#3172)"
    );
    let err_return = &src[fn_start + cut_check..fn_start + extraction];
    assert!(
        err_return.contains("return Err("),
        "the cut check must fail the attempt with Err, not warn (#3172)"
    );
}
