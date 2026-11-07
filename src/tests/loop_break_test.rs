//! A tool-call loop guard hands the turn to the next provider and names the
//! call it dropped (#1397).
//!
//! On 2026-09-05 the near-match guard ended four turns in one session: PR
//! merges and file pages that differed only by number were counted as one
//! call, the pending call was discarded, and the fallback chain was never
//! consulted. The pure tests pin the error and its message; the turn-level
//! tests drive a provider that never stops re-issuing one call through the
//! real service and check that the chain rotates before the turn ends.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::json;

use crate::brain::agent::error::AgentError;
use crate::brain::agent::service::AgentService;
use crate::brain::agent::service::loop_break::{
    chain_exhausted, describe_pending_calls, loop_break_error,
};
use crate::brain::provider::{
    ContentBlock, ContentDelta, FallbackProvider, LLMRequest, LLMResponse, MessageDelta, Provider,
    ProviderError, ProviderStream, Role, StopReason, StreamEvent, StreamMessage, TokenUsage,
};
use crate::db::Database;
use crate::services::{ServiceContext, SessionService};
use crate::tests::agent_service_mocks::MockProvider;

// ---- describe_pending_calls ----

#[test]
fn pending_call_is_named_with_its_raw_arguments() {
    let uses = vec![(
        "id-1".to_string(),
        "bash".to_string(),
        json!({"command": "gh pr merge 1390 --squash --admin --delete-branch"}),
    )];
    let d = describe_pending_calls(&uses);
    assert!(d.starts_with("bash "), "{d}");
    assert!(
        d.contains("gh pr merge 1390 --squash --admin --delete-branch"),
        "{d}"
    );
}

#[test]
fn several_pending_calls_are_listed_in_order() {
    let uses = vec![
        (
            "a".to_string(),
            "grep".to_string(),
            json!({"pattern": "#1394"}),
        ),
        (
            "b".to_string(),
            "read_file".to_string(),
            json!({"path": "x.rs"}),
        ),
    ];
    let d = describe_pending_calls(&uses);
    let grep_at = d.find("grep ").unwrap();
    let read_at = d.find("read_file ").unwrap();
    assert!(grep_at < read_at, "{d}");
    assert!(d.contains("; "), "{d}");
}

#[test]
fn oversized_pending_call_is_cut_with_an_ellipsis_by_chars() {
    let long = "é".repeat(600);
    let uses = vec![(
        "a".to_string(),
        "bash".to_string(),
        json!({"command": long}),
    )];
    let d = describe_pending_calls(&uses);
    assert_eq!(d.chars().count(), 240, "{}", d.chars().count());
    assert!(d.ends_with('…'), "{d}");
}

#[test]
fn no_pending_calls_describe_as_empty() {
    assert_eq!(describe_pending_calls(&[]), "");
}

// ---- loop_break_error ----

#[test]
fn loop_break_is_the_error_the_rotation_wrapper_matches() {
    let err = loop_break_error("near-identical tool-call loop", "bash", 4, 8, "bash {..}");
    assert!(
        matches!(
            err,
            AgentError::Provider(ProviderError::ToolLoop(_))
        ),
        "{err:?}"
    );
}

#[test]
fn loop_break_message_names_guard_label_count_window_and_dropped_call() {
    let err = loop_break_error(
        "identical-call loop",
        "sed",
        4,
        8,
        "bash {\"command\":\"sed -n '495,580p' agent.rs\"}",
    );
    let AgentError::Provider(ProviderError::ToolLoop(msg)) = err else {
        panic!("wrong variant");
    };
    assert!(
        msg.starts_with("identical-call loop: 'sed' recurred 4x in the last 8 steps"),
        "{msg}"
    );
    assert!(
        msg.contains("dropped call: bash {\"command\":\"sed -n '495,580p' agent.rs\"}"),
        "{msg}"
    );
}

// ---- chain_exhausted ----

#[test]
fn exhausted_chain_says_how_many_were_tried_and_that_nothing_is_queued() {
    let err = chain_exhausted(loop_break_error("g", "bash", 4, 8, "bash {}"), 3);
    let AgentError::Provider(ProviderError::ToolLoop(msg)) = err else {
        panic!("wrong variant");
    };
    assert!(msg.contains("dropped call: bash {}"), "{msg}");
    assert!(msg.contains("(3 tried)"), "{msg}");
    assert!(msg.contains("Nothing is queued"), "{msg}");
    assert!(msg.contains("say the word to resume"), "{msg}");
}

#[test]
fn exhausted_chain_leaves_other_errors_alone() {
    let err = chain_exhausted(AgentError::ToolError("boom".to_string()), 2);
    assert!(
        matches!(err, AgentError::ToolError(ref m) if m == "boom"),
        "{err:?}"
    );
}

// ---- turn-level: the guard rotates the chain instead of ending the turn ----

/// Re-issues the same tool call on every request, forever. What the loop
/// guards saw from the ModelScope session on 2026-09-05, minus the model.
struct RepeatingCallProvider {
    calls: Arc<AtomicUsize>,
}

impl RepeatingCallProvider {
    fn response(&self) -> LLMResponse {
        LLMResponse {
            id: "repeat".to_string(),
            model: "mock-model".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: format!("call-{}", self.calls.load(Ordering::SeqCst)),
                name: "probe_tool".to_string(),
                input: json!({"command": "gh pr merge 1390 --squash --admin --delete-branch"}),
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                ..Default::default()
            },
            streaming_active_secs: None,
            tool_text_leak: false,
        }
    }
}

#[async_trait]
impl Provider for RepeatingCallProvider {
    async fn complete(&self, _request: LLMRequest) -> crate::brain::provider::Result<LLMResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.response())
    }

    async fn stream(&self, request: LLMRequest) -> crate::brain::provider::Result<ProviderStream> {
        let response = self.complete(request).await?;
        let mut events = vec![Ok(StreamEvent::MessageStart {
            message: StreamMessage {
                id: response.id.clone(),
                model: response.model.clone(),
                role: Role::Assistant,
                usage: response.usage,
            },
        })];
        for (i, block) in response.content.iter().enumerate() {
            if let ContentBlock::ToolUse { id, name, input } = block {
                events.push(Ok(StreamEvent::ContentBlockStart {
                    index: i,
                    content_block: ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::Value::Object(Default::default()),
                    },
                }));
                events.push(Ok(StreamEvent::ContentBlockDelta {
                    index: i,
                    delta: ContentDelta::InputJsonDelta {
                        partial_json: serde_json::to_string(input).unwrap_or_default(),
                    },
                }));
                events.push(Ok(StreamEvent::ContentBlockStop { index: i }));
            }
        }
        events.push(Ok(StreamEvent::MessageDelta {
            delta: MessageDelta {
                stop_reason: response.stop_reason,
                stop_sequence: None,
            },
            usage: response.usage,
        }));
        events.push(Ok(StreamEvent::MessageStop));
        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn name(&self) -> &str {
        "repeating-call-provider"
    }

    fn default_model(&self) -> &str {
        "mock-model"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["mock-model".to_string()]
    }

    fn context_window(&self, _model: &str) -> Option<u32> {
        Some(4096)
    }

    fn calculate_cost(&self, _model: &str, _input: u32, _output: u32) -> f64 {
        0.0
    }
}

async fn service_with(provider: Arc<dyn Provider>) -> (AgentService, ServiceContext) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let service = AgentService::new_for_test(provider, context.clone()).await;
    (service, context)
}

#[tokio::test]
async fn a_looping_model_hands_the_turn_to_the_next_provider() {
    let calls = Arc::new(AtomicUsize::new(0));
    let looping: Arc<dyn Provider> = Arc::new(RepeatingCallProvider {
        calls: calls.clone(),
    });
    let chain = Arc::new(FallbackProvider::new(looping, vec![Arc::new(MockProvider)]));
    let (service, context) = service_with(chain.clone() as Arc<dyn Provider>).await;
    let session = SessionService::new(context)
        .create_session(Some("loop rotation".to_string()))
        .await
        .unwrap();

    let result = service
        .send_message_with_tools(session.id, "merge the PRs".to_string(), None)
        .await;

    assert!(
        result.is_ok(),
        "the turn must survive a loop-guard break when a fallback exists: {result:?}"
    );
    assert!(
        calls.load(Ordering::SeqCst) >= 4,
        "the guard needs its nudge-then-break run before rotating: {} calls",
        calls.load(Ordering::SeqCst)
    );
    assert!(
        chain.active_subprovider_name().is_some(),
        "the chain must have promoted the fallback"
    );
}

#[tokio::test]
async fn an_exhausted_chain_ends_the_turn_naming_the_dropped_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let looping: Arc<dyn Provider> = Arc::new(RepeatingCallProvider { calls });
    let (service, context) = service_with(looping).await;
    let session = SessionService::new(context)
        .create_session(Some("loop no fallback".to_string()))
        .await
        .unwrap();

    let result = service
        .send_message_with_tools(session.id, "merge the PRs".to_string(), None)
        .await;

    let Err(AgentError::Provider(ProviderError::ToolLoop(msg))) = result else {
        panic!("expected the loop error once the chain is exhausted: {result:?}");
    };
    assert!(msg.contains("'probe_tool' recurred"), "{msg}");
    assert!(
        msg.contains("dropped call: probe_tool {\"command\":\"gh pr merge 1390"),
        "the dropped call must be named verbatim: {msg}"
    );
    assert!(msg.contains("(1 tried)"), "{msg}");
    assert!(msg.contains("Nothing is queued"), "{msg}");
}

#[test]
fn distinct_loop_reasons_for_tool_and_announcement_guards() {
    // #2465: tool loops must be attributed to "tool_loop", announcement loops to "announcement_loop"
    let tool_err = loop_break_error("near-identical tool-call loop", "bash", 4, 8, "bash {}");
    assert!(matches!(tool_err, AgentError::Provider(ProviderError::ToolLoop(_))));

    let announce_err = AgentError::Provider(ProviderError::AnnouncementLoop(
        "near-identical announcements repeated within the turn".to_string()
    ));
    assert!(matches!(announce_err, AgentError::Provider(ProviderError::AnnouncementLoop(_))));
}
