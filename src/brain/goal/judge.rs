//! Goal judge — evaluates whether a goal is satisfied after each turn.

use crate::brain::goal::types::{GoalVerdict, JudgeDecision};
use crate::brain::provider::{LLMRequest, Message, Provider};

/// The judge system prompt. Asks a simple yes/no with structured JSON output.
const JUDGE_SYSTEM: &str = r#"You are a goal-evaluation judge. Your ONLY job is to decide whether the assistant's last response satisfies the user's goal.

You will receive:
- The GOAL: what the user wants accomplished
- The LAST RESPONSE: the assistant's most recent output

Respond with ONLY a JSON object (no markdown, no code fences, no extra text):
{
  "verdict": "DONE" or "CONTINUE",
  "reason": "brief explanation of why",
  "corrections": "optional guidance for what the assistant should do next (only if CONTINUE)"
}

Rules:
- DONE: the goal is fully satisfied. The work is complete.
- CONTINUE: the goal is NOT yet fully satisfied. More work is needed.
- Be generous: if the response clearly addresses the goal, say DONE.
- Be strict only when the response is incomplete or wrong.
- If the assistant says it can't do something, that's DONE with reason explaining the block.
- If the response contains errors or the assistant is still mid-task, say CONTINUE.
- Return ONLY the JSON object. Nothing else."#;

/// Run the goal judge: make an auxiliary LLM call to check if the goal is met.
///
/// Uses the same provider as the session (no separate auxiliary model).
/// The judge call is lightweight: system prompt + goal + last response.
///
/// Retries once on parse failure (empty response or unparseable JSON) before
/// returning the fail-open Continue decision. This handles transient issues
/// like reasoning models burning all tokens on CoT.
///
/// Returns a `JudgeDecision`. On any error, fail-open (Continue).
pub async fn judge_goal(
    provider: &dyn Provider,
    model: &str,
    goal: &str,
    last_response: &str,
) -> JudgeDecision {
    // Truncate last_response to avoid blowing the judge's context window.
    // The last 4k chars is usually enough to determine completion.
    let truncated_response = if last_response.len() > 4000 {
        crate::utils::string::truncate_str_tail(last_response, 4000)
    } else {
        last_response
    };

    let user_prompt = format!("GOAL:\n{}\n\nLAST RESPONSE:\n{}", goal, truncated_response);

    // Try up to 2 times: original call + one retry on parse/empty failure.
    for attempt in 1..=2 {
        let request = LLMRequest::new(model.to_string(), vec![Message::user(user_prompt.clone())])
            .with_system(JUDGE_SYSTEM.to_string())
            .with_max_tokens(4096);

        match provider.complete(request).await {
            Ok(response) => {
                let raw = extract_text(&response);
                if raw.trim().is_empty() {
                    tracing::warn!("Goal judge returned empty response (attempt {}/2)", attempt);
                    if attempt < 2 {
                        continue;
                    }
                    return JudgeDecision {
                        verdict: GoalVerdict::Continue,
                        reason: "judge returned empty response".to_string(),
                        corrections: None,
                    };
                }

                let decision = JudgeDecision::parse_or_continue(&raw);

                // Retry once on parse error (unparseable JSON)
                if decision.verdict == GoalVerdict::Continue
                    && decision.reason.starts_with("judge parse error")
                {
                    tracing::warn!(
                        "Goal judge parse failed (attempt {}/2): {}",
                        attempt,
                        decision.reason
                    );
                    if attempt < 2 {
                        continue;
                    }
                }

                tracing::info!(
                    "Goal judge verdict: {:?} — {}",
                    decision.verdict,
                    decision.reason
                );
                return decision;
            }
            Err(e) => {
                tracing::warn!("Goal judge LLM call failed (attempt {}/2): {}", attempt, e);
                if attempt < 2 {
                    continue;
                }
                return JudgeDecision {
                    verdict: GoalVerdict::Continue,
                    reason: format!("judge call error: {}", e),
                    corrections: None,
                };
            }
        }
    }

    // Unreachable — the loop always returns. Satisfies the compiler.
    JudgeDecision {
        verdict: GoalVerdict::Continue,
        reason: "judge unreachable".to_string(),
        corrections: None,
    }
}

/// Extract text from LLMResponse content blocks.
fn extract_text(response: &crate::brain::provider::LLMResponse) -> String {
    let mut text = String::new();
    for block in &response.content {
        if let crate::brain::provider::ContentBlock::Text { text: t } = block {
            text.push_str(t);
        }
    }
    text
}
