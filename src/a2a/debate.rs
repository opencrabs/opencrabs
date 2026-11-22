//! Multi-round Bee Colony debate protocol.
//!
//! Implements Multi-Agent Debate (MAD) pattern over A2A protocol:
//!
//! ```text
//! Round 1: Queen → all Bees (independent research)
//!          Each Bee gets the topic + knowledge base context
//! Round 2: Queen collects → shares all outputs → Bees critique
//!          Each Bee sees everyone's R1 output + must critique/extend
//! Round N: Convergence check → consensus or vote
//! ```
//!
//! Based on ReConcile (ACL 2024) confidence-weighted voting.

use crate::a2a::types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ─── Debate Configuration ────────────────────────────────────

/// Configuration for a Bee Colony debate session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebateConfig {
    /// The research topic or question.
    pub topic: String,

    /// Number of Bee agents participating.
    pub num_bees: usize,

    /// Maximum debate rounds before forced conclusion.
    #[serde(default = "default_max_rounds")]
    pub max_rounds: usize,

    /// Confidence threshold for consensus (0.0 - 1.0).
    /// If all Bees agree with >= this confidence, debate ends.
    #[serde(default = "default_consensus_threshold")]
    pub consensus_threshold: f64,

    /// Optional knowledge base context to inject into Round 1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge_context: Vec<String>,

    /// Bee endpoint URLs (A2A servers to send tasks to).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bee_endpoints: Vec<String>,
}

fn default_max_rounds() -> usize {
    3
}

fn default_consensus_threshold() -> f64 {
    0.8
}

// ─── Debate State ────────────────────────────────────────────

/// A single Bee's response in a debate round.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeeResponse {
    /// Which Bee produced this response.
    pub bee_id: String,

    /// The Bee's endpoint URL.
    pub endpoint: String,

    /// The text content of the response.
    pub content: String,

    /// Confidence score (0.0 - 1.0) — how sure is this Bee?
    #[serde(default)]
    pub confidence: f64,

    /// Position or stance on the topic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,

    /// Key points extracted from the response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_points: Vec<String>,
}

/// A single round in the debate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebateRound {
    /// Round number (1-indexed).
    pub round_number: usize,

    /// The prompt sent to all Bees in this round.
    pub prompt: String,

    /// Responses collected from all Bees.
    pub responses: Vec<BeeResponse>,

    /// Consensus analysis after this round.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consensus: Option<ConsensusAnalysis>,
}

/// Analysis of consensus after a debate round.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsensusAnalysis {
    /// Average confidence across all Bees.
    pub avg_confidence: f64,

    /// Points that all Bees agree on.
    pub agreement_points: Vec<String>,

    /// Points of contention between Bees.
    pub contention_points: Vec<String>,

    /// Blind spots — topics no Bee addressed.
    pub blind_spots: Vec<String>,

    /// Whether consensus was reached.
    pub consensus_reached: bool,
}

/// The full state of a debate session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebateSession {
    /// Unique session ID.
    pub id: String,

    /// Debate configuration.
    pub config: DebateConfig,

    /// Current round number.
    pub current_round: usize,

    /// All completed rounds.
    pub rounds: Vec<DebateRound>,

    /// Final synthesis (populated when debate concludes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_synthesis: Option<String>,

    /// Debate state.
    pub state: DebateState,
}

/// State of the debate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DebateState {
    /// Waiting to start.
    Pending,
    /// A round is in progress.
    InRound,
    /// Waiting for Queen to analyze round results.
    Analyzing,
    /// Debate concluded with consensus.
    Concluded,
    /// Debate ended without consensus (max rounds reached).
    Exhausted,
}

// ─── Debate Engine ───────────────────────────────────────────

impl DebateSession {
    /// Create a new debate session.
    pub fn new(config: DebateConfig) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            current_round: 0,
            rounds: Vec::new(),
            final_synthesis: None,
            state: DebateState::Pending,
            config,
        }
    }

    /// Generate the Round 1 prompt (independent research).
    pub fn round1_prompt(&self) -> String {
        let mut prompt = format!(
            "## Debate Topic\n\n{}\n\n\
             ## Your Task (Round 1 — Independent Research)\n\n\
             Analyze this topic from your unique perspective. Provide:\n\
             1. Your **position** on the topic\n\
             2. **Key arguments** supporting your position\n\
             3. **Evidence** or reasoning\n\
             4. **Confidence score** (0.0-1.0) in your position\n\
             5. **Potential counterarguments** you anticipate\n",
            self.config.topic
        );

        // Inject knowledge base context if available
        if !self.config.knowledge_context.is_empty() {
            prompt.push_str("\n## Knowledge Base Context\n\n");
            prompt.push_str(
                "The following verified knowledge has been loaded. \
                 Use it to inform your analysis, but think beyond it:\n\n",
            );
            for (i, ctx) in self.config.knowledge_context.iter().enumerate() {
                prompt.push_str(&format!("### Source {}\n{}\n\n", i + 1, ctx));
            }
        }

        prompt
    }

    /// Generate a critique round prompt (Round 2+).
    /// Each Bee sees all previous responses and must critique/extend.
    pub fn critique_prompt(&self, round_num: usize) -> String {
        let prev_round = &self.rounds[round_num - 2]; // 0-indexed

        let mut prompt = format!(
            "## Debate Topic\n\n{}\n\n\
             ## Round {} — Critique & Synthesis\n\n\
             You have seen all participants' responses from Round {}. \
             Your task:\n\
             1. **Identify agreements** — what do most participants agree on?\n\
             2. **Challenge weak arguments** — which positions lack evidence?\n\
             3. **Synthesize insights** — combine the strongest ideas\n\
             4. **Update your position** if others' arguments changed your mind\n\
             5. **Confidence score** (0.0-1.0) — has your confidence changed?\n\n\
             ## Previous Round Responses\n\n",
            self.config.topic,
            round_num,
            round_num - 1,
        );

        for resp in &prev_round.responses {
            prompt.push_str(&format!(
                "### Bee {} (confidence: {:.1})\n{}\n\n",
                resp.bee_id, resp.confidence, resp.content
            ));
        }

        prompt
    }

    /// Build A2A messages for a debate round.
    pub fn build_round_messages(&self, round_num: usize) -> Vec<(String, Message)> {
        let prompt = if round_num == 1 {
            self.round1_prompt()
        } else {
            self.critique_prompt(round_num)
        };

        self.config
            .bee_endpoints
            .iter()
            .enumerate()
            .map(|(i, endpoint)| {
                let msg = Message {
                    message_id: Some(Uuid::new_v4().to_string()),
                    context_id: Some(self.id.clone()),
                    task_id: None,
                    role: Role::User,
                    parts: vec![Part::text(&prompt)],
                    metadata: Some({
                        let mut m = HashMap::new();
                        m.insert("debate_round".to_string(), serde_json::json!(round_num));
                        m.insert("bee_index".to_string(), serde_json::json!(i));
                        m.insert("debate_session_id".to_string(), serde_json::json!(self.id));
                        m
                    }),
                };
                (endpoint.clone(), msg)
            })
            .collect()
    }

    /// Analyze consensus from a round's responses.
    pub fn analyze_consensus(responses: &[BeeResponse], threshold: f64) -> ConsensusAnalysis {
        let avg_confidence = if responses.is_empty() {
            0.0
        } else {
            responses.iter().map(|r| r.confidence).sum::<f64>() / responses.len() as f64
        };

        // Simple position-based agreement detection
        let mut position_counts: HashMap<String, usize> = HashMap::new();
        for resp in responses {
            if let Some(ref pos) = resp.position {
                *position_counts.entry(pos.to_lowercase()).or_insert(0) += 1;
            }
        }

        let total = responses.len();
        let agreement_points: Vec<String> = position_counts
            .iter()
            .filter(|&(_, count)| *count as f64 / total as f64 >= threshold)
            .map(|(pos, count)| format!("{} ({}/{} agree)", pos, count, total))
            .collect();

        let contention_points: Vec<String> = position_counts
            .iter()
            .filter(|&(_, count)| {
                let ratio = *count as f64 / total as f64;
                ratio > 0.0 && ratio < threshold
            })
            .map(|(pos, count)| format!("{} ({}/{} agree)", pos, count, total))
            .collect();

        let consensus_reached = avg_confidence >= threshold && !agreement_points.is_empty();

        ConsensusAnalysis {
            avg_confidence,
            agreement_points,
            contention_points,
            blind_spots: vec![], // filled by LLM in production
            consensus_reached,
        }
    }

    /// Record a completed round.
    pub fn record_round(
        &mut self,
        round_number: usize,
        prompt: String,
        responses: Vec<BeeResponse>,
    ) {
        let consensus = Self::analyze_consensus(&responses, self.config.consensus_threshold);
        let concluded = consensus.consensus_reached || round_number >= self.config.max_rounds;

        self.rounds.push(DebateRound {
            round_number,
            prompt,
            responses,
            consensus: Some(consensus),
        });
        self.current_round = round_number;

        if concluded {
            self.state = if self
                .rounds
                .last()
                .and_then(|r| r.consensus.as_ref())
                .is_some_and(|c| c.consensus_reached)
            {
                DebateState::Concluded
            } else {
                DebateState::Exhausted
            };
        } else {
            self.state = DebateState::Analyzing;
        }
    }

    /// Generate a summary report of the debate.
    pub fn summary_report(&self) -> String {
        let mut report = format!(
            "# 🐝 Bee Colony Debate Report\n\n\
             **Topic:** {}\n\
             **Bees:** {}\n\
             **Rounds:** {}/{}\n\
             **State:** {:?}\n\n",
            self.config.topic,
            self.config.num_bees,
            self.current_round,
            self.config.max_rounds,
            self.state,
        );

        for round in &self.rounds {
            report.push_str(&format!("## Round {}\n\n", round.round_number));

            for resp in &round.responses {
                report.push_str(&format!(
                    "### Bee {} (confidence: {:.1})\n{}\n\n",
                    resp.bee_id, resp.confidence, resp.content
                ));
            }

            if let Some(ref consensus) = round.consensus {
                report.push_str(&format!(
                    "### Consensus Analysis\n\
                     - Avg Confidence: {:.2}\n\
                     - Consensus Reached: {}\n",
                    consensus.avg_confidence, consensus.consensus_reached,
                ));
                if !consensus.agreement_points.is_empty() {
                    report.push_str("- **Agreements:**\n");
                    for p in &consensus.agreement_points {
                        report.push_str(&format!("  - {}\n", p));
                    }
                }
                if !consensus.contention_points.is_empty() {
                    report.push_str("- **Contentions:**\n");
                    for p in &consensus.contention_points {
                        report.push_str(&format!("  - {}\n", p));
                    }
                }
                report.push('\n');
            }
        }

        if let Some(ref synthesis) = self.final_synthesis {
            report.push_str(&format!("## Final Synthesis\n\n{}\n", synthesis));
        }

        report
    }
}

// ─── Debate Execution Engine ────────────────────────────────

/// Errors that can occur during debate execution.
#[derive(Debug)]
pub enum DebateError {
    /// HTTP communication failure with a bee endpoint.
    Http(String),
    /// A2A protocol-level error (bad response, timeout, task failure).
    Protocol(String),
}

impl std::fmt::Display for DebateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {}", e),
            Self::Protocol(e) => write!(f, "Protocol error: {}", e),
        }
    }
}

/// Send a message to a bee endpoint via A2A JSON-RPC and poll until completion.
async fn send_a2a_message(
    client: &reqwest::Client,
    endpoint: &str,
    message: Message,
) -> Result<String, DebateError> {
    let rpc_request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "message/send".to_string(),
        params: serde_json::to_value(SendMessageParams {
            message,
            configuration: None,
            metadata: None,
        })
        .map_err(|e| DebateError::Protocol(e.to_string()))?,
        id: serde_json::json!(1),
    };

    let resp = client
        .post(endpoint)
        .json(&rpc_request)
        .send()
        .await
        .map_err(|e| DebateError::Http(format!("{}: {}", endpoint, e)))?;

    let rpc_response: JsonRpcResponse = resp
        .json()
        .await
        .map_err(|e| DebateError::Http(format!("Bad response from {}: {}", endpoint, e)))?;

    let task_id = rpc_response
        .result
        .as_ref()
        .and_then(|r| r.get("id"))
        .and_then(|id| id.as_str())
        .ok_or_else(|| DebateError::Protocol("No task ID in response".to_string()))?
        .to_string();

    // Poll for completion (2s interval, 5 min timeout)
    for _ in 0..150 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let get_request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks/get".to_string(),
            params: serde_json::json!({"id": task_id}),
            id: serde_json::json!(2),
        };

        let poll_resp = client
            .post(endpoint)
            .json(&get_request)
            .send()
            .await
            .map_err(|e| DebateError::Http(e.to_string()))?;

        let poll_rpc: JsonRpcResponse = poll_resp
            .json()
            .await
            .map_err(|e| DebateError::Http(e.to_string()))?;

        if let Some(result) = &poll_rpc.result {
            let state = result
                .get("status")
                .and_then(|s| s.get("state"))
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");

            match state {
                "completed" => {
                    let text = result
                        .get("artifacts")
                        .and_then(|a| a.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|art| art.get("parts"))
                        .and_then(|p| p.as_array())
                        .and_then(|parts| parts.first())
                        .and_then(|part| part.get("text"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("[No response text]")
                        .to_string();
                    return Ok(text);
                }
                "failed" | "canceled" | "rejected" => {
                    let msg = result
                        .get("status")
                        .and_then(|s| s.get("message"))
                        .and_then(|m| m.get("parts"))
                        .and_then(|p| p.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|part| part.get("text"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("Unknown error");
                    return Err(DebateError::Protocol(format!("Task {}: {}", state, msg)));
                }
                _ => continue,
            }
        }
    }

    Err(DebateError::Protocol(format!(
        "Task {} timed out after 5 minutes",
        task_id
    )))
}

/// Extract confidence score from response text (heuristic).
///
/// Looks for patterns like "confidence: 0.85" or "Confidence score: 0.9".
fn extract_confidence(text: &str) -> f64 {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.find("confidence") {
        let after = &text[pos..];
        for word in after.split_whitespace().skip(1) {
            let clean: String = word
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(val) = clean.parse::<f64>()
                && (0.0..=1.0).contains(&val)
            {
                return val;
            }
        }
    }
    0.5
}

/// Run a full multi-round debate across bee endpoints.
///
/// 1. Loads knowledge context from QMD memory if not pre-populated
/// 2. Sends round prompts to all bee endpoints concurrently via A2A JSON-RPC
/// 3. Collects responses, checks consensus
/// 4. Repeats or concludes
pub async fn run_debate(mut config: DebateConfig) -> Result<DebateSession, DebateError> {
    // Load knowledge context from QMD if not pre-populated
    if config.knowledge_context.is_empty()
        && let Ok(store) = crate::memory::get_store()
    {
        match crate::memory::search(store, &config.topic, 10).await {
            Ok(results) => {
                config.knowledge_context = results
                    .iter()
                    .map(|r| format!("[{}]\n{}", r.path, r.snippet))
                    .collect();
                tracing::info!(
                    "Loaded {} knowledge context items for debate",
                    config.knowledge_context.len()
                );
            }
            Err(e) => {
                tracing::warn!("Memory search failed for debate context: {}", e);
            }
        }
    }

    let mut session = DebateSession::new(config);
    let client = crate::utils::http::shared();

    for round_num in 1..=session.config.max_rounds {
        session.state = DebateState::InRound;

        let prompt = if round_num == 1 {
            session.round1_prompt()
        } else {
            session.critique_prompt(round_num)
        };

        let messages = session.build_round_messages(round_num);

        // Send to all bees concurrently
        let mut handles = Vec::new();
        for (i, (endpoint, msg)) in messages.into_iter().enumerate() {
            // shared() hands back a &'static Client and &T is Copy, so the
            // spawned task takes the reference itself. Cloning here would
            // resolve to Client::clone and build an owned client per message.
            handles.push(tokio::spawn(async move {
                let result = send_a2a_message(client, &endpoint, msg).await;
                (i, endpoint, result)
            }));
        }

        // Collect responses
        let mut responses = Vec::new();
        for handle in handles {
            match handle.await {
                Ok((i, endpoint, Ok(content))) => {
                    let confidence = extract_confidence(&content);
                    responses.push(BeeResponse {
                        bee_id: format!("bee-{}", i),
                        endpoint,
                        content,
                        confidence,
                        position: None,
                        key_points: vec![],
                    });
                }
                Ok((i, endpoint, Err(e))) => {
                    tracing::error!("Bee {} ({}) failed: {}", i, endpoint, e);
                }
                Err(e) => {
                    tracing::error!("Bee task panicked: {}", e);
                }
            }
        }

        if responses.is_empty() {
            return Err(DebateError::Protocol(
                "All bees failed to respond".to_string(),
            ));
        }

        session.record_round(round_num, prompt, responses);

        if session.state == DebateState::Concluded || session.state == DebateState::Exhausted {
            break;
        }
    }

    Ok(session)
}
