//! Updating the crab's own binary.
//!
//! `/evolve` downloads the latest release and swaps it in for the process
//! that runs it. With a crab elsewhere that process is the crab, never the
//! window that typed the command, so the work is an operation the crab runs.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::brain::tools::ToolResult;

use super::{BackendError, Ctx, Op, Scope};

/// Run `/evolve` on the crab, as the terminal does: download the latest
/// release, install it, and answer the line the window shows (#2406).
///
/// Admin: it replaces the binary the crab runs on its own machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evolve {
    /// The session the command was typed in, for the tool's own logs.
    pub session_id: Uuid,
}

impl Op for Evolve {
    const NAME: &'static str = "update.evolve";
    const SCOPE: Scope = Scope::Admin;
    type Reply = String;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        Ok(evolve(self.session_id).await)
    }
}

/// The evolve tool, run on this process for `session_id`, worded for the
/// window.
#[cfg(not(test))]
async fn evolve(session_id: Uuid) -> String {
    let tool = crate::brain::tools::evolve::EvolveTool::new(None);
    let ctx = crate::brain::tools::ToolExecutionContext::new(session_id);
    match crate::brain::tools::Tool::execute(&tool, serde_json::json!({}), &ctx).await {
        Ok(result) => outcome(result),
        Err(e) => format!("Evolve error: {e}"),
    }
}

/// Under test the tool never runs: a real evolve would download a release
/// and replace the test binary. The parity tests get one fixed refusal, worded
/// by the same [`outcome`] the real run goes through.
#[cfg(test)]
async fn evolve(_session_id: Uuid) -> String {
    outcome(ToolResult::error(TEST_REFUSAL.to_string()))
}

#[cfg(test)]
const TEST_REFUSAL: &str = "evolve does not run under test";

/// The line a finished evolve leaves in the window.
fn outcome(result: ToolResult) -> String {
    if result.success {
        if result.output.is_empty() {
            "Evolve finished.".to_string()
        } else {
            result.output
        }
    } else {
        format!(
            "Evolve failed: {}",
            result.error.unwrap_or_else(|| "unknown error".to_string())
        )
    }
}

ops! { Evolve }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    #[tokio::test]
    async fn evolve_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(
            gateway,
            Evolve {
                session_id: Uuid::nil(),
            }
        );
        assert_eq!(
            answer["Ok"],
            serde_json::json!(format!("Evolve failed: {TEST_REFUSAL}"))
        );
    }

    #[tokio::test]
    async fn evolve_needs_admin() {
        // It replaces the crab's binary: nothing less than admin runs it.
        let gateway = Gateway::start().await;
        forbidden!(
            gateway,
            Evolve {
                session_id: Uuid::nil(),
            }
        );
    }

    #[test]
    fn a_finished_evolve_reads_as_the_window_always_showed_it() {
        assert_eq!(
            outcome(ToolResult::success(String::new())),
            "Evolve finished."
        );
        assert_eq!(
            outcome(ToolResult::success("Updated to v9".to_string())),
            "Updated to v9"
        );
        assert_eq!(
            outcome(ToolResult::error("no network".to_string())),
            "Evolve failed: no network"
        );
        let mut silent = ToolResult::error(String::new());
        silent.error = None;
        assert_eq!(outcome(silent), "Evolve failed: unknown error");
    }
}
