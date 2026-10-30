//! Goal state types for autonomous task completion.

use serde::{Deserialize, Serialize};

/// The judge's verdict on whether a goal is satisfied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum GoalVerdict {
    /// Goal is complete.
    Done,
    /// Goal is not yet complete, continue working.
    Continue,
}

impl GoalVerdict {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_uppercase().as_str() {
            "DONE" => Self::Done,
            _ => Self::Continue,
        }
    }
}

/// The judge's structured response after evaluating goal progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeDecision {
    pub verdict: GoalVerdict,
    pub reason: String,
    /// Optional corrections or guidance for the next continuation.
    #[serde(default)]
    pub corrections: Option<String>,
}

impl JudgeDecision {
    /// Parse from raw JSON string. Fail-open: on parse error,
    /// return Continue so the loop keeps going rather than dying.
    pub fn parse_or_continue(raw: &str) -> Self {
        match serde_json::from_str::<JudgeDecision>(raw) {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(
                    "Goal judge returned unparseable JSON: {} — \
                     defaulting to CONTINUE (fail-open). Raw: {}",
                    e,
                    crate::utils::string::truncate_str(raw, 200)
                );
                JudgeDecision {
                    verdict: GoalVerdict::Continue,
                    reason: format!("judge parse error: {}", e),
                    corrections: None,
                }
            }
        }
    }
}

/// Decision from the goal manager after evaluating a turn.
#[derive(Debug, Clone)]
pub enum GoalDecision {
    /// Goal is satisfied, turn should complete normally.
    Done { reason: String },
    /// Goal needs more work, inject continuation and re-enter loop.
    Continue {
        continuation_prompt: String,
        corrections: Option<String>,
    },
    /// Goal budget exhausted or auto-paused due to parse failures.
    Paused { reason: String },
}

/// Maximum consecutive judge parse failures before auto-pause.
/// Protects against models that can't produce valid JSON.
pub const MAX_PARSE_FAILURES: u32 = 3;

/// Default maximum turns for a goal.
pub const DEFAULT_MAX_TURNS: u32 = 20;
