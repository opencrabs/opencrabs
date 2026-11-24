//! Regression for #1521: the compaction walk tells the user what it is doing.
//!
//! After `/compact` the TUI showed one "requested" line, then nothing for
//! three full attempt budgets, then an error. Every provider timeout, every
//! move to the next provider and the exhausted chain were `tracing` lines
//! only. Manual compaction now reports each step; automatic compaction
//! reports only a failure, and a background summariser that dies is
//! announced where its result would have been applied.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::brain::agent::context::AgentContext;
use crate::brain::agent::service::AgentService;
use crate::brain::agent::service::compaction_notice::{
    CompactionNotifier, CompactionStep, describe,
};
use crate::brain::agent::service::{ProgressCallback, ProgressEvent};
use crate::brain::provider::{
    ContentBlock, LLMRequest, LLMResponse, Message, Provider, ProviderError, ProviderStream,
    TokenUsage,
};

/// Collects the alert lines a callback received.
fn recorder() -> (ProgressCallback, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let cb: ProgressCallback = Arc::new(move |_session, event| {
        if let ProgressEvent::SelfHealingAlert { message } = event {
            sink.lock().unwrap().push(message);
        }
    });
    (cb, seen)
}

fn failed(provider: &str) -> CompactionStep {
    CompactionStep::AttemptFailed {
        provider: provider.to_string(),
        reason: "timeout".to_string(),
    }
}

#[test]
fn every_step_has_a_line_that_names_the_provider() {
    assert_eq!(
        describe(&failed("qwen")),
        "Compaction: 'qwen' failed (timeout), walking the fallback chain"
    );
    assert_eq!(
        describe(&CompactionStep::TryingFallback {
            provider: "zai".to_string(),
            model: "glm-5.3-flash".to_string(),
        }),
        "Compaction: trying 'zai' with model 'glm-5.3-flash'"
    );
    assert_eq!(
        describe(&CompactionStep::Failed {
            detail: "All providers in the fallback chain failed.".to_string(),
        }),
        "Compaction failed: All providers in the fallback chain failed."
    );
}

#[test]
fn a_manual_notifier_reports_every_step() {
    let (cb, seen) = recorder();
    let n = CompactionNotifier::manual(Uuid::new_v4(), cb);
    n.step(failed("qwen"));
    n.step(CompactionStep::TryingFallback {
        provider: "zai".to_string(),
        model: "glm".to_string(),
    });
    n.step(CompactionStep::Failed {
        detail: "chain exhausted".to_string(),
    });
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[test]
fn an_auto_notifier_reports_only_a_failure() {
    let (cb, seen) = recorder();
    let n = CompactionNotifier::auto(Uuid::new_v4(), cb);
    n.step(failed("qwen"));
    n.step(CompactionStep::TryingFallback {
        provider: "zai".to_string(),
        model: "glm".to_string(),
    });
    assert!(
        seen.lock().unwrap().is_empty(),
        "automatic compaction stays quiet while it walks"
    );
    n.step(CompactionStep::Failed {
        detail: "chain exhausted".to_string(),
    });
    let lines = seen.lock().unwrap();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("Compaction failed:"));
}

#[test]
fn no_callback_means_no_notifier() {
    let id = Uuid::new_v4();
    assert!(CompactionNotifier::manual_from(id, None).is_none());
    assert!(CompactionNotifier::auto_from(id, None).is_none());
    let (cb, _) = recorder();
    assert!(CompactionNotifier::manual_from(id, Some(&cb)).is_some());
}

/// A summariser mock that either answers or times out the walk's way.
struct Mock {
    name: String,
    ok: bool,
}

#[async_trait]
impl Provider for Mock {
    async fn complete(
        &self,
        request: LLMRequest,
    ) -> crate::brain::provider::error::Result<LLMResponse> {
        self.answer(request)
    }

    async fn stream(
        &self,
        request: LLMRequest,
    ) -> crate::brain::provider::error::Result<ProviderStream> {
        let response = self.answer(request)?;
        let text = match response.content.first() {
            Some(ContentBlock::Text { text }) => text.clone(),
            _ => String::new(),
        };
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(crate::brain::provider::StreamEvent::MessageStart {
                message: crate::brain::provider::StreamMessage {
                    id: response.id,
                    model: response.model,
                    role: crate::brain::provider::Role::Assistant,
                    usage: TokenUsage::default(),
                },
            }),
            Ok(crate::brain::provider::StreamEvent::ContentBlockDelta {
                index: 0,
                delta: crate::brain::provider::ContentDelta::TextDelta { text },
            }),
            Ok(crate::brain::provider::StreamEvent::MessageStop),
        ])))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn default_model(&self) -> &str {
        "m"
    }

    fn supported_models(&self) -> Vec<String> {
        Vec::new()
    }

    fn context_window(&self, _model: &str) -> Option<u32> {
        Some(200_000)
    }

    fn calculate_cost(&self, _model: &str, _input_tokens: u32, _output_tokens: u32) -> f64 {
        0.0
    }
}

impl Mock {
    fn answer(&self, request: LLMRequest) -> crate::brain::provider::error::Result<LLMResponse> {
        if self.ok {
            Ok(LLMResponse {
                id: format!("{}-response", self.name),
                model: request.model,
                content: vec![ContentBlock::Text {
                    text: format!("summary from {}", self.name),
                }],
                stop_reason: None,
                usage: TokenUsage::default(),
                streaming_active_secs: None,
                tool_text_leak: false,
            })
        } else {
            Err(ProviderError::Timeout(300))
        }
    }
}

fn provider(name: &str, ok: bool) -> Arc<dyn Provider> {
    Arc::new(Mock {
        name: name.to_string(),
        ok,
    })
}

fn request() -> LLMRequest {
    LLMRequest::new("m", vec![Message::user("summarise")])
}

#[tokio::test]
async fn a_manual_walk_narrates_the_failure_and_the_fallback() {
    let (cb, seen) = recorder();
    let session = Uuid::new_v4();
    let notifier = CompactionNotifier::manual(session, cb);
    let primary = provider("primary", false);
    let fallback = provider("second", true);

    let response = AgentService::complete_compaction_request(
        &primary,
        &[fallback],
        request(),
        &CancellationToken::new(),
        std::time::Duration::from_secs(30),
        Some(&notifier),
    )
    .await
    .expect("served by the fallback");
    assert_eq!(response.id, "second-response");

    let lines = seen.lock().unwrap();
    assert_eq!(
        *lines,
        vec![
            "Compaction: 'primary' failed (timeout), walking the fallback chain".to_string(),
            "Compaction: trying 'second' with model 'm'".to_string(),
        ]
    );
}

#[tokio::test]
async fn an_exhausted_walk_ends_with_one_failure_line_under_auto() {
    let (cb, seen) = recorder();
    let notifier = CompactionNotifier::auto(Uuid::new_v4(), cb);
    let primary = provider("primary", false);
    let fallback = provider("second", false);

    AgentService::complete_compaction_request(
        &primary,
        &[fallback],
        request(),
        &CancellationToken::new(),
        std::time::Duration::from_secs(30),
        Some(&notifier),
    )
    .await
    .expect_err("nothing answered");

    let lines = seen.lock().unwrap();
    assert_eq!(lines.len(), 1, "auto reports the failure only: {lines:?}");
    assert!(lines[0].starts_with("Compaction failed: All providers in the fallback chain failed."));
}

#[tokio::test]
async fn a_primary_with_no_chain_still_reports_its_failure() {
    let (cb, seen) = recorder();
    let notifier = CompactionNotifier::auto(Uuid::new_v4(), cb);
    let primary = provider("only", false);

    AgentService::complete_compaction_request(
        &primary,
        &[],
        request(),
        &CancellationToken::new(),
        std::time::Duration::from_secs(30),
        Some(&notifier),
    )
    .await
    .expect_err("nothing answered");

    let lines = seen.lock().unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], "Compaction failed: 'only': timeout");
}

/// The manual `/compact` site hands a verbose notifier in, the automatic
/// sites hand a quiet one, and a finished background summariser that failed
/// is announced on the callback that would have applied it.
#[test]
fn call_sites_pick_the_right_notifier() {
    const TOOL_LOOP: &str = include_str!("../brain/agent/service/tool_loop.rs");
    const COMPACTION: &str = include_str!("../brain/agent/service/compaction.rs");
    let manual = TOOL_LOOP
        .find("if is_manual_compact {")
        .expect("manual site");
    assert!(TOOL_LOOP[manual..manual + 700].contains("CompactionNotifier::manual_from("));
    assert_eq!(
        TOOL_LOOP.matches("CompactionNotifier::auto_from(").count(),
        2,
        "emergency and hard-trigger compactions are automatic"
    );
    assert_eq!(
        COMPACTION.matches("CompactionNotifier::auto_from(").count(),
        2
    );
    let failed = COMPACTION
        .find("Background compaction failed after")
        .expect("background failure arm");
    assert!(COMPACTION[failed..failed + 700].contains("SelfHealingAlert"));
}

/// #1686 Defect 2: the waiting progress line reported the live fill, so a
/// compaction the 65% gate requested at 66% announced itself at 54%
/// (`bee04b00`, 2026-09-23) or 21% (`cb1c7a07`, 2026-09-22), a level that
/// cannot have requested one, printed beside a receipt naming the real 66%.
/// One event was reported as two facts because the two lines read two
/// different measurements.
#[test]
fn compaction_notice_carries_start_fill() {
    let (notice, live) = AgentService::waiting_report_levels(66.0, 54.0);
    assert_eq!(
        notice, 66.0,
        "the progress line carries the fill the compaction was requested at"
    );
    assert_eq!(
        live, 54.0,
        "the log keeps the live fill, which is its point"
    );
    assert!(
        notice > 65.0,
        "no progress line can sit below the gate that triggers compaction"
    );

    // The helper is only a fix if the waiting emit site uses it. Pin the
    // wiring so the payload cannot drift back to the live parameter. Anchor on
    // the call itself: the log string sits *below* it, so scanning forward from
    // the string would never reach the assignment it is meant to prove.
    const COMPACTION: &str = include_str!("../brain/agent/service/compaction.rs");
    let call = COMPACTION
        .find("Self::waiting_report_levels(")
        .expect("waiting helper call site");
    let window = &COMPACTION[call..call + 1500];
    assert!(
        window.contains("pending.snapshot_usage_pct"),
        "the waiting path must derive its levels from the recorded start fill"
    );
    assert!(
        window.contains("usage_pct: notice_pct"),
        "the Compacting payload must be the notice level, not the live one"
    );
    assert!(
        window.contains("log_pct,"),
        "the diagnostic keeps the live level, which is its whole point"
    );
}

/// #1686 Defect 2b: the receipt divided the raw local estimate by
/// `max_tokens`, dropping the provider anchor that `usage_percentage()` folds
/// in. Session `bee04b00` logged "Context compacted (FullWindow): now at 17%
/// (33169 tokens)" while the rendered line said 19%. One compaction, two
/// after-numbers, because the meter and the receipt measured one context on
/// two different bases.
#[test]
fn receipt_after_pct_is_the_meter_pct() {
    let mut context = AgentContext::new(Uuid::new_v4(), 200_000);
    // Sized to land in the incident's own range: bee04b00 receipted 19% on a
    // 200K window, so a context worth a rounding error would prove the
    // divergence exists without showing it costs anything visible.
    context.add_message(Message::user("x".repeat(300_000)));
    let estimated = context.token_count;
    assert!(estimated >= 2, "a message has to cost something to divide");

    // The provider reports the real context at half the local estimate: the
    // #1677 shape, where an anchored context's own count overshoots.
    context.record_provider_reported_tokens(estimated / 2);

    let meter = context.usage_percentage();
    let stale = estimated as f64 / context.max_tokens as f64 * 100.0;
    // Relative, not absolute points: the anchor halves the basis, so the gap
    // is a proportion of the measurement and holds whatever window size or
    // tokenizer the estimate happens to produce.
    assert!(
        stale - meter > meter * 0.25,
        "the two bases must genuinely differ, or this test proves nothing: \
         meter {meter:.2}% vs stale {stale:.2}%"
    );
    assert!(
        meter < stale,
        "an anchor under the estimate reads lower: {meter:.2}% vs {stale:.2}%"
    );

    // Receipt and meter log have to name the same number, so both sites call
    // the one function. `note_compaction_success` is private and this repo
    // never builds an AgentService in tests, so the emission is pinned at the
    // source instead of by calling it.
    const COMPACTION: &str = include_str!("../brain/agent/service/compaction.rs");
    let receipt = COMPACTION
        .find("fn note_compaction_success(")
        .expect("receipt fn");
    let body = &COMPACTION[receipt..receipt + 1600];
    assert!(
        body.contains("let after_pct = context.usage_percentage()"),
        "the receipt reads the meter's basis"
    );
    assert!(
        !body.contains("context.token_count as f64 / context.max_tokens as f64"),
        "the hand-rolled division is deleted, not kept as a second copy to drift"
    );

    const SERVICE_CTX: &str = include_str!("../brain/agent/service/context.rs");
    assert!(
        SERVICE_CTX.contains("context.usage_percentage()"),
        "the meter log at service/context.rs:1011 reads the same function"
    );
}

/// #3173: the terminal cut speaks. The wording names the loss plainly: no
/// summary was produced, the oldest messages were dropped, nothing before
/// the marker survives.
#[test]
fn the_truncated_alert_names_the_loss() {
    let msg = crate::brain::agent::service::compaction::truncated_alert_message();
    assert!(msg.contains("no summary"), "{msg}");
    assert!(msg.contains("dropped"), "{msg}");
    assert!(msg.contains("Nothing before the marker survives"), "{msg}");
}

/// #3173: the truncation branch emits the alert, not silence.
///
/// The success receipt never fires when every summariser attempt failed, so
/// the last-resort truncation is the only place that can tell the user
/// messages were dropped without a summary.
#[test]
fn the_truncation_branch_emits_the_alert() {
    const COMPACTION: &str = include_str!("../brain/agent/service/compaction.rs");
    let branch = COMPACTION
        .find("Last resort: every compaction attempt failed")
        .expect("the last-resort truncation branch must exist");
    let body = &COMPACTION[branch..branch + 1600];
    assert!(
        body.contains("ProgressEvent::SelfHealingAlert"),
        "the truncation branch must emit the honest alert (#3173)"
    );
    assert!(
        body.contains("truncated_alert_message()"),
        "the alert must carry the shared wording, not a divergent copy (#3173)"
    );
}

/// #3173: both Telegram alert arms lift the Compacting header pin.
///
/// A failed compaction never emits CompactionSummary, the only arm that
/// lifted the pin before, so the header stayed pinned to
/// "⏳ Compacting context..." forever after a failed walk.
#[test]
fn both_telegram_alert_arms_lift_the_pin() {
    for (path, src) in [
        (
            "progress.rs",
            include_str!("../channels/telegram/progress.rs"),
        ),
        ("resume.rs", include_str!("../channels/telegram/resume.rs")),
    ] {
        let mut lifts = 0;
        let mut cursor = 0;
        while let Some(off) = src[cursor..].find("ProgressEvent::SelfHealingAlert { message }") {
            let start = cursor + off;
            let body = &src[start..start + 500];
            assert!(
                body.contains("s.compacting = false;"),
                "{path}: every SelfHealingAlert arm must lift the Compacting pin (#3173)"
            );
            lifts += 1;
            cursor = start + 10;
        }
        assert!(lifts >= 1, "{path}: expected at least one alert arm");
    }
}
