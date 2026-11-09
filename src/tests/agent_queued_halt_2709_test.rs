//! #2709: a message queued while a turn runs must survive a turn that halts
//! on an option-surface tool.
//!
//! The bug (`tool_loop.rs`, the post-tool drain
//! `if let Some(ref queue_cb) = self.message_queue_callback`): a round that
//! called `suggest_options` sets `option_surface_halt_seen` /
//! `option_halt_iteration_has_text`, and the loop breaks at the top of the next
//! iteration. The drain still ran, so it took the message the user typed while
//! the turn was working, persisted its user row, created a fresh empty
//! assistant placeholder, and the turn ended milliseconds later without one
//! model request to answer it. The occurrence evidence is a GUI session where
//! the take has no flush receipt and the ledger holds a user row plus a
//! zero-length assistant row in the same second (issue #2709).
//!
//! These are behavioural tests: they drive the real tool loop with a mock
//! provider and a real queue callback, then read the ledger the loop wrote.

use crate::brain::agent::service::{AgentService, MessageQueueCallback, QueuedUserMessage};
use crate::brain::provider::{
    ContentBlock, LLMRequest, LLMResponse, Provider, ProviderStream, Role, StopReason, TokenUsage,
};
use crate::brain::tools::{Tool, ToolCapability, ToolExecutionContext, ToolRegistry, ToolResult};
use crate::db::Database;
use crate::services::{MessageService, ServiceContext, SessionService};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// What the user typed while the turn was running.
const QUEUE_TEXT: &str = "and now do the second half as well";
/// The halt round's own words: the option-surface tool needs text beside it or
/// `option_halt_iteration_has_text` stays false and no halt happens.
const HALT_TEXT: &str = "Here are the three follow-ups I can take next.";
/// A round two would answer the queued message; the fix means it never runs.
const SECOND_ROUND: &str = "SECOND-ROUND-2709 must never be produced.";

fn text_response(text: &str) -> LLMResponse {
    LLMResponse {
        id: "halt-2709-response".to_string(),
        model: "mock-model".to_string(),
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        stop_reason: Some(StopReason::EndTurn),
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            ..Default::default()
        },
        streaming_active_secs: None,
        tool_text_leak: false,
    }
}

/// Round 1 offers options, which halts the turn. Any later round answers the
/// queued message, which is exactly what must not happen here.
struct HaltProvider {
    rounds: Mutex<usize>,
}

impl HaltProvider {
    fn new() -> Self {
        Self {
            rounds: Mutex::new(0),
        }
    }

    fn rounds(&self) -> usize {
        *self.rounds.lock().unwrap()
    }
}

#[async_trait]
impl Provider for HaltProvider {
    async fn complete(&self, _request: LLMRequest) -> crate::brain::provider::Result<LLMResponse> {
        let round = {
            let mut guard = self.rounds.lock().unwrap();
            *guard += 1;
            *guard
        };
        if round == 1 {
            Ok(LLMResponse {
                id: "halt-2709-tool".to_string(),
                model: "mock-model".to_string(),
                content: vec![
                    ContentBlock::Text {
                        text: HALT_TEXT.to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "halt-tool-1".to_string(),
                        name: "halt_tool".to_string(),
                        input: serde_json::json!({"question": "what next?"}),
                    },
                ],
                stop_reason: Some(StopReason::ToolUse),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    ..Default::default()
                },
                streaming_active_secs: None,
                tool_text_leak: false,
            })
        } else {
            Ok(text_response(SECOND_ROUND))
        }
    }

    async fn stream(&self, request: LLMRequest) -> crate::brain::provider::Result<ProviderStream> {
        use crate::brain::provider::{ContentDelta, MessageDelta, StreamEvent, StreamMessage};

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
            match block {
                ContentBlock::Text { text } => {
                    events.push(Ok(StreamEvent::ContentBlockStart {
                        index: i,
                        content_block: ContentBlock::Text {
                            text: String::new(),
                        },
                    }));
                    events.push(Ok(StreamEvent::ContentBlockDelta {
                        index: i,
                        delta: ContentDelta::TextDelta { text: text.clone() },
                    }));
                    events.push(Ok(StreamEvent::ContentBlockStop { index: i }));
                }
                ContentBlock::ToolUse { id, name, input } => {
                    events.push(Ok(StreamEvent::ContentBlockStart {
                        index: i,
                        content_block: ContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: Value::Object(Default::default()),
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
                _ => {
                    events.push(Ok(StreamEvent::ContentBlockStart {
                        index: i,
                        content_block: block.clone(),
                    }));
                    events.push(Ok(StreamEvent::ContentBlockStop { index: i }));
                }
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
        "halt-mock"
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

/// The option-surface tool: `halts_turn()` is what ends the turn after its
/// result is flushed, the same policy `suggest_options` carries.
struct HaltTool;

#[async_trait]
impl Tool for HaltTool {
    fn name(&self) -> &str {
        "halt_tool"
    }

    fn description(&self) -> &str {
        "Offers follow-ups and ends the turn"
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolExecutionContext,
    ) -> crate::brain::tools::Result<ToolResult> {
        Ok(ToolResult::success("three follow-ups offered".to_string()))
    }

    fn halts_turn(&self) -> bool {
        true
    }
}

async fn fresh_session() -> (ServiceContext, Uuid) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let session = SessionService::new(context.clone())
        .create_session(Some("2709 queued halt test".to_string()))
        .await
        .unwrap();
    (context, session.id)
}

/// A queue that counts its drains and hands the text over only when drained,
/// so a test can tell "still waiting" from "taken and dropped".
fn counted_queue(session_id: Uuid) -> (MessageQueueCallback, Arc<Mutex<usize>>) {
    let calls = Arc::new(Mutex::new(0usize));
    let seen = calls.clone();
    let waiting = Arc::new(tokio::sync::Mutex::new(Some(QUEUE_TEXT.to_string())));
    let cb: MessageQueueCallback = Arc::new(move |sid: Uuid| {
        let waiting = waiting.clone();
        let seen = seen.clone();
        Box::pin(async move {
            if sid != session_id {
                return None;
            }
            *seen.lock().unwrap() += 1;
            waiting.lock().await.take().map(QueuedUserMessage::plain)
        })
    });
    (cb, calls)
}

/// `(role, content)` per row, in sequence order: enough to prove a row exists,
/// whose it is, and whether it ever got an answer written into it.
async fn ledger_rows(context: ServiceContext, session_id: Uuid) -> Vec<(String, String)> {
    MessageService::new(context)
        .list_messages_for_session(session_id)
        .await
        .unwrap()
        .into_iter()
        .map(|m| (m.role, m.content))
        .collect()
}

/// The regression: the halting turn must not consume the queue. The message
/// stays waiting for the surface, and no ledger row is written for it.
#[tokio::test]
async fn a_queued_message_survives_a_turn_that_halts_on_an_option_surface_tool() {
    let (context, session_id) = fresh_session().await;
    let provider = Arc::new(HaltProvider::new());
    let registry = ToolRegistry::new();
    registry.register(Arc::new(HaltTool));
    let (queue, drains) = counted_queue(session_id);

    let agent = AgentService::new_for_test(provider.clone(), context.clone())
        .await
        .with_tool_registry(Arc::new(registry))
        .with_auto_approve_tools(true)
        .with_message_queue_callback(Some(queue));

    let response = agent
        .send_message_with_tools_and_callback(
            session_id,
            "what should we do next".to_string(),
            None,
            None,
            None,
            None,
            "test",
            None,
            None,
        )
        .await
        .unwrap();

    // The halt still ends the turn after the option-surface tool, and the
    // halting round's own words are the answer.
    assert_eq!(
        provider.rounds(),
        1,
        "an option-surface halt must end the turn without a follow-up round"
    );
    assert!(
        response.content.contains(HALT_TEXT),
        "the halt answer must survive as the turn's response, got {:?}",
        response.content
    );

    // This is the fix: the queue was never drained, so the message the user
    // typed while the turn ran is still there for the surface to send.
    assert_eq!(
        *drains.lock().unwrap(),
        0,
        "a halting round must not drain the queue: the surface flushes it as the next turn (#2709)"
    );

    let messages = ledger_rows(context, session_id).await;
    assert!(
        !messages.iter().any(|(_, content)| content == QUEUE_TEXT),
        "the queued text must not reach the ledger from a halting round, got {:?}",
        messages
            .iter()
            .map(|(role, content)| (role.as_str(), content.len()))
            .collect::<Vec<_>>()
    );
    assert!(
        !messages
            .iter()
            .any(|(role, content)| role == "assistant" && content.trim().is_empty()),
        "a halting round must not leave an empty assistant placeholder behind: {:?}",
        messages
            .iter()
            .map(|(role, content)| (role.as_str(), content.len()))
            .collect::<Vec<_>>()
    );
}

/// Behavioural pin on the site itself, the same shape #1784 uses: the post-tool
/// drain is guarded by the halt predicate, and the guard is what keeps the
/// message queued.
#[test]
fn the_post_tool_drain_is_guarded_by_the_halt_predicate() {
    let src = include_str!("../brain/agent/service/tool_loop.rs");
    let marker = "Injecting queued user message between tool iterations";
    let start = src
        .find(marker)
        .expect("the post-tool queue drain site exists");
    // #2732: `.get` and never a range index. The offset is a byte count, so a
    // range index panics when it lands inside a multi-byte character, and the
    // byte-slice ratchet flags every such site. A `None` falls back to the
    // whole head, which only widens the window these assertions search.
    let head = src.get(..start).unwrap_or("");
    let before = head.get(head.len().saturating_sub(900)..).unwrap_or(head);

    assert!(
        before.contains(
            "let turn_halts_now = option_surface_halt_seen && option_halt_iteration_has_text;"
        ),
        "the halt predicate must name the same condition the loop breaks on (#2709)"
    );
    assert!(
        before.contains("if !turn_halts_now"),
        "the post-tool drain must be skipped when the turn is halting (#2709)"
    );
}
