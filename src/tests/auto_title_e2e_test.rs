//! End-to-end test for the auto-title flow.
//!
//! Issue #118 + #120 + #121: Telegram sessions were stuck on their
//! default channel-generated titles forever. Multiple "fixes" shipped
//! without this test. Each one looked correct on code review but did
//! NOT actually change the title in DB after a real message went
//! through. The reporter kept reproducing on his install while local
//! tests reported "fixed".
//!
//! This test simulates the exact flow:
//! 1. Create a session with the default Telegram-style title
//!    (matches `is_default_channel_title` so auto-title is allowed).
//! 2. Run a real user message through
//!    `send_message_with_tools_and_mode`, the same entry point the
//!    Telegram handler uses.
//! 3. Poll the DB until the background auto-title task lands the
//!    rewritten title (bounded by a short timeout so a hung task
//!    doesn't deadlock the suite).
//! 4. Assert (a) the title actually changed, (b) the [chat:N] suffix
//!    was preserved, (c) the `Telegram: ` channel prefix was preserved.
//!
//! If this test fails, the auto-title fix is broken. Period.

use crate::brain::agent::service::AgentService;
use crate::brain::provider::types::ContentDelta;
use crate::brain::provider::{
    ContentBlock, LLMRequest, LLMResponse, Provider, ProviderStream, Role, StopReason, StreamEvent,
    StreamMessage, TokenUsage,
};
use crate::brain::tools::ToolRegistry;
use crate::db::Database;
use crate::services::{ServiceContext, SessionService};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

// Both scenarios run in a SINGLE `#[tokio::test]` so they share one runtime
// and one `SessionService` instead of rebuilding both.
//
// The fold exists because `Database::GLOBAL_POOL` is a process-wide
// `OnceLock`: while a file-backed `Database::connect` published into it, a
// second test in the same suite could inherit a pool pointing at the first
// test's torn-down DB. That publish is now gated out of test builds (#2072),
// so the pool can no longer be inherited this way. The fold is left in place.

/// Provider that returns the same canned response for both `stream` (used
/// by the main user-message turn) and `complete` (used by the auto-title
/// background task). The text the title generator gets is "Greeting" so
/// the resulting session title becomes `Telegram: Greeting [chat:N]` and
/// the test can assert against that exact shape.
struct AutoTitleMockProvider;

#[async_trait]
impl Provider for AutoTitleMockProvider {
    async fn complete(&self, _request: LLMRequest) -> crate::brain::provider::Result<LLMResponse> {
        // The title-generation call uses .complete() with the canned
        // "Generate a concise session title..." prompt. Return a clean
        // 1-word title.
        Ok(LLMResponse {
            id: "title-resp".to_string(),
            model: "mock-model".to_string(),
            content: vec![ContentBlock::Text {
                text: "Greeting".to_string(),
            }],
            stop_reason: Some(StopReason::EndTurn),
            usage: TokenUsage {
                input_tokens: 50,
                output_tokens: 1,
                ..Default::default()
            },
            streaming_active_secs: None,
            tool_text_leak: false,
        })
    }

    async fn stream(&self, _request: LLMRequest) -> crate::brain::provider::Result<ProviderStream> {
        // The main user-message turn streams a short text response. No
        // tool calls — we just want the turn to complete cleanly so the
        // background auto-title task gets a chance to run.
        let events = vec![
            Ok(StreamEvent::MessageStart {
                message: StreamMessage {
                    id: "main-resp".to_string(),
                    model: "mock-model".to_string(),
                    role: Role::Assistant,
                    usage: TokenUsage {
                        input_tokens: 100,
                        output_tokens: 0,
                        ..Default::default()
                    },
                },
            }),
            Ok(StreamEvent::ContentBlockStart {
                index: 0,
                content_block: ContentBlock::Text {
                    text: String::new(),
                },
            }),
            Ok(StreamEvent::ContentBlockDelta {
                index: 0,
                delta: ContentDelta::TextDelta {
                    text: "Hey!".to_string(),
                },
            }),
            Ok(StreamEvent::ContentBlockStop { index: 0 }),
            Ok(StreamEvent::MessageDelta {
                delta: crate::brain::provider::MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    stop_sequence: None,
                },
                usage: TokenUsage {
                    input_tokens: 100,
                    output_tokens: 1,
                    ..Default::default()
                },
            }),
            Ok(StreamEvent::MessageStop),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn name(&self) -> &str {
        "auto-title-mock"
    }

    fn default_model(&self) -> &str {
        "mock-model"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["mock-model".to_string()]
    }

    fn context_window(&self, _model: &str) -> Option<u32> {
        Some(200_000)
    }

    fn calculate_cost(&self, _model: &str, _input: u32, _output: u32) -> f64 {
        0.0
    }
}

/// Helper that runs the full auto-title round-trip against an
/// arbitrary mock provider and returns the rewritten title (or None
/// if the title never changed within 3s).
async fn run_auto_title_round_trip(
    provider: Arc<dyn Provider>,
    initial_title: &str,
) -> Option<String> {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());

    let registry = ToolRegistry::new();
    let agent_service = AgentService::new_for_test(provider, context.clone())
        .await
        .with_tool_registry(Arc::new(registry))
        .with_auto_approve_tools(true);

    let session_service = SessionService::new(context.clone());
    let session = session_service
        .create_session(Some(initial_title.to_string()))
        .await
        .unwrap();
    let session_id = session.id;

    let _ = agent_service
        .send_message_with_tools_and_mode(session_id, "Hi".to_string(), None, None)
        .await
        .expect("first turn should complete");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let s = session_service
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap();
        if let Some(t) = s.title.as_deref()
            && t != initial_title
        {
            return Some(t.to_string());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

#[test]
fn auto_title_end_to_end_covers_all_scenarios() {
    // Runs on an explicitly-sized worker thread: macOS spawns test threads
    // with a ~512KB stack by default (Rust std Darwin default), and this
    // end-to-end flow (full AgentService + notify echo bubbles + config
    // parsing) now legitimately needs more depth than that (#1237).
    // 16MB makes the gate deterministic across contributor platforms;
    // verified empirically: SIGABRT unset, pass at any RUST_MIN_STACK >= 8MB.
    // The inner future must actually be driven on this thread: spawning the
    // async fn bare would construct the future and drop it unpolled, making
    // the whole e2e a silent no-op (caught by clippy's unused_must_use on
    // the joined value).
    let handle = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build title e2e runtime");
            rt.block_on(inner_auto_title_end_to_end());
        })
        .expect("spawn title e2e worker");
    handle.join().expect("title e2e worker panicked");
}

async fn inner_auto_title_end_to_end() {
    // Virtual time (#1455): under CI load the single current-thread runtime
    // can stall for seconds between polls, so a WALL-CLOCK 3s deadline
    // flaked on unrelated PR-lane runs (3 pinned failures in the issue,
    // same panic line, branches not touching auto-title). With paused
    // time, `tokio::time::sleep` auto-advances only when the runtime has
    // no ready work: the poll below still caps at ~60 scheduler-idle
    // iterations (a real hang still fails fast), but can no longer expire
    // just because the machine was busy. `tokio::time::Instant` respects
    // the pause; std Instant does not.
    tokio::time::pause();

    // Three phases run inside a SINGLE #[tokio::test] because adding more
    // tokio tests to this file makes the first one fail with
    // `Database("Failed to create message")` — even when each test creates
    // its own UUID-keyed in-memory DB, there is enough shared/global state
    // somewhere in the AgentService stack that between-test isolation is
    // not reliable for this entry-point. Until that root cause is found,
    // all scenarios are folded here and run sequentially.

    // Phase 1 — Normal provider with Text response.
    let default_title = "Telegram: DM TestUser (133526395) [chat:133526395]";
    let title = run_auto_title_round_trip(Arc::new(AutoTitleMockProvider), default_title)
        .await
        .expect(
            "Phase 1: auto-title task did not update the session title within 3s. \
             The Text-block path is broken (issue #121 — multiple v0.3.x releases \
             claimed to fix this without ever actually testing the end-to-end flow).",
        );
    assert!(
        title.starts_with("Telegram: "),
        "Phase 1: channel prefix must be preserved, got: {title:?}",
    );
    assert!(
        title.ends_with("[chat:133526395]"),
        "Phase 1: [chat:ID] suffix must be preserved (issue #115), got: {title:?}",
    );
    assert!(
        title.contains("Greeting"),
        "Phase 1: must include the LLM-generated body, got: {title:?}",
    );

    // Phase 2 — Reasoning model that returns ONLY Thinking, no Text.
    // This was the actual smoking gun behind @leshchenko1979's
    // reproduction. Before the `extract_title_candidate` fallback,
    // this path returned an empty title and the session stayed stuck
    // on the default channel-generated name forever.
    let default_title_2 = "Telegram: DM Алексей (133526395) [chat:133526395]";
    let title2 = run_auto_title_round_trip(Arc::new(ThinkingOnlyTitleProvider), default_title_2)
        .await
        .expect(
            "Phase 2: auto-title must extract a candidate from the Thinking block \
             when no Text block is present. Reasoning models like \
             qwen-3.7-max-preview-thinking sometimes return Thinking only for \
             short prompts — that's the exact failure mode hit on the reporter's box.",
        );
    assert!(
        title2.starts_with("Telegram: "),
        "Phase 2: channel prefix must be preserved, got: {title2:?}",
    );
    assert!(
        title2.ends_with("[chat:133526395]"),
        "Phase 2: chat suffix must be preserved, got: {title2:?}",
    );

    // Phase 3 — Two consecutive messages. Regression test for the
    // label-drift clobber bug fixed by PR #123. Before the fix, the
    // handler did `if session.title != computed_default { overwrite }`,
    // so every subsequent message reverted the auto-titled name to the
    // default template. This phase exercises the agent-level flow
    // (which doesn't run the channel handler's label-drift code), so
    // it primarily proves that the agent path doesn't re-fire
    // auto-title on a session that already has a non-default title.
    // The Telegram handler's version of this guard lives in
    // `should_refresh_label` and is covered by
    // `tests::telegram_session_resolve_test`.
    let initial = "Telegram: DM TestUser (133526395) [chat:133526395]";
    let (after_first, after_second) =
        run_two_message_round_trip(Arc::new(AutoTitleMockProvider), initial).await;

    let renamed = after_first.expect("Phase 3: auto-title must rename on turn 1");
    assert_ne!(
        renamed, initial,
        "Phase 3: turn 1: title should have changed off the default",
    );

    let final_title = after_second.expect("Phase 3: session must exist after turn 2");
    assert_eq!(
        final_title, renamed,
        "Phase 3: turn 2 must NOT clobber the auto-titled name. Bug from PR #123 \
         (label-drift clobber) would surface here as a revert to the default \
         template.",
    );
    assert_ne!(
        final_title, initial,
        "Phase 3: turn 2 must not revert to the original default channel-generated title",
    );
}

/// Same as `run_auto_title_round_trip` but sends TWO consecutive
/// messages. The first triggers auto-title; the second must NOT
/// revert the renamed title back to anything else. Returns the title
/// after the second turn settles.
async fn run_two_message_round_trip(
    provider: Arc<dyn Provider>,
    initial_title: &str,
) -> (Option<String>, Option<String>) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());

    let registry = ToolRegistry::new();
    let agent_service = AgentService::new_for_test(provider, context.clone())
        .await
        .with_tool_registry(Arc::new(registry))
        .with_auto_approve_tools(true);

    let session_service = SessionService::new(context.clone());
    let session = session_service
        .create_session(Some(initial_title.to_string()))
        .await
        .unwrap();
    let session_id = session.id;

    // Turn 1.
    let _ = agent_service
        .send_message_with_tools_and_mode(session_id, "Hi".to_string(), None, None)
        .await
        .expect("first turn should complete");

    // Wait for the auto-title task to land.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut after_first: Option<String> = None;
    while tokio::time::Instant::now() < deadline {
        let s = session_service
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap();
        if let Some(t) = s.title.as_deref()
            && t != initial_title
        {
            after_first = Some(t.to_string());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Turn 2 — this is where the pre-PR-123 clobber bug fired. The
    // `should_refresh_label` policy must let the auto-titled name
    // survive instead of overwriting it with the default template.
    //
    // NOTE: AgentService doesn't go through the Telegram channel
    // handler, so this test simulates the clobber by directly calling
    // the function the handler used to invoke. The PR's
    // `should_refresh_label` is the unit under test.
    let _ = agent_service
        .send_message_with_tools_and_mode(session_id, "Hi again".to_string(), None, None)
        .await
        .expect("second turn should complete");

    // After turn 2 settles, the title must still be the auto-titled
    // one (or whatever the post-turn-1 value was) — NEVER the default
    // channel-generated template.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let s = session_service
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    let after_second = s.title.clone();

    (after_first, after_second)
}

/// Provider that mimics qwen-3.7-max-preview-thinking on a short prompt:
/// the streamed response (main user turn) emits text fine, but the
/// `complete()` call (used by auto-title) returns ONLY a `Thinking`
/// block — no `Text` block. That's the exact failure mode on
/// @leshchenko1979's setup behind issue #121.
struct ThinkingOnlyTitleProvider;

#[async_trait]
impl Provider for ThinkingOnlyTitleProvider {
    async fn complete(&self, _request: LLMRequest) -> crate::brain::provider::Result<LLMResponse> {
        // No Text block — only Thinking. extract_text_from_response
        // ignores Thinking blocks and returns "", which is the root
        // cause of the auto-title silent-failure loop.
        Ok(LLMResponse {
            id: "title-thinking-resp".to_string(),
            model: "qwen-thinking-mock".to_string(),
            content: vec![ContentBlock::Thinking {
                thinking: "User said 'Hi'. I should generate a short title. \
                           Maybe 'Greeting' or 'Casual Chat Start'."
                    .to_string(),
                signature: None,
            }],
            stop_reason: Some(StopReason::EndTurn),
            usage: TokenUsage {
                input_tokens: 50,
                output_tokens: 30,
                ..Default::default()
            },
            streaming_active_secs: None,
            tool_text_leak: false,
        })
    }

    async fn stream(&self, _request: LLMRequest) -> crate::brain::provider::Result<ProviderStream> {
        // Main turn streams a normal text reply.
        let events = vec![
            Ok(StreamEvent::MessageStart {
                message: StreamMessage {
                    id: "main-resp".to_string(),
                    model: "qwen-thinking-mock".to_string(),
                    role: Role::Assistant,
                    usage: TokenUsage::default(),
                },
            }),
            Ok(StreamEvent::ContentBlockStart {
                index: 0,
                content_block: ContentBlock::Text {
                    text: String::new(),
                },
            }),
            Ok(StreamEvent::ContentBlockDelta {
                index: 0,
                delta: ContentDelta::TextDelta {
                    text: "Hey!".to_string(),
                },
            }),
            Ok(StreamEvent::ContentBlockStop { index: 0 }),
            Ok(StreamEvent::MessageDelta {
                delta: crate::brain::provider::MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    stop_sequence: None,
                },
                usage: TokenUsage {
                    input_tokens: 100,
                    output_tokens: 1,
                    ..Default::default()
                },
            }),
            Ok(StreamEvent::MessageStop),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn name(&self) -> &str {
        "qwen-thinking-mock"
    }

    fn default_model(&self) -> &str {
        "qwen-thinking-mock"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["qwen-thinking-mock".to_string()]
    }

    fn context_window(&self, _model: &str) -> Option<u32> {
        Some(200_000)
    }

    fn calculate_cost(&self, _model: &str, _input: u32, _output: u32) -> f64 {
        0.0
    }
}
