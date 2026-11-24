//! Mission control: load and decide.
//!
//! Operations land here as their GUI doors move behind the contract.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::brain::mission_control::McInboxKind;
use crate::brain::prompt_builder::BrainLoader;
use crate::brain::tools::ToolRegistry;
use crate::brain::tools::dynamic::DynamicToolLoader;
use crate::brain::tools::rsi_proposals::RsiProposalsTool;

use super::{BackendError, Ctx, Op, Scope};

/// What a key on the inbox asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// Install the proposal: the loaders write it live.
    Apply,
    /// Archive it without installing.
    Reject,
}

/// The wire shape of [`McInboxKind`], which carries no serde of its own.
#[allow(dead_code)]
#[derive(Serialize, Deserialize)]
#[serde(remote = "McInboxKind")]
enum McInboxKindWire {
    ProposedTool,
    ProposedCommand,
    ProposedSkill,
}

/// Apply or reject one proposal with the paths already resolved.
///
/// Sync and path-explicit so a test can point every write at a scratch home
/// without the override having to reach a spawned thread (the profile
/// override is a task-local and does not cross `std::thread::spawn`).
pub fn decide_with(
    registry: Arc<ToolRegistry>,
    id: &str,
    kind: &McInboxKind,
    decision: Decision,
    tools_path: PathBuf,
    brain_path: PathBuf,
) -> Result<String, String> {
    let tool = RsiProposalsTool::new(registry, tools_path, brain_path);
    match decision {
        Decision::Apply => tool.apply(id, kind),
        Decision::Reject => tool.reject(id, None),
    }
}

/// Apply or reject one RSI proposal from the Mission Control inbox. The
/// reply is the tool's own message.
///
/// The writes (tools.toml, commands.toml, SKILL.md, the archive) land in this
/// crab's own files: the two paths are resolved here, the same ones the
/// terminal resolves, and the tool registry is the crab's service's. Before
/// boot finished there is no registry, and the answer is `NotReady`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionDecide {
    /// The proposal's inbox id (`prop_tool_<uuid>`, `prop_cmd_<uuid>`, ...).
    pub id: String,
    /// What kind of artifact it is.
    #[serde(with = "McInboxKindWire")]
    pub kind: McInboxKind,
    /// Apply or reject.
    pub decision: Decision,
}

impl Op for MissionDecide {
    const NAME: &'static str = "mission.decide";
    const SCOPE: Scope = Scope::Manage;
    type Reply = String;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let Some(tools_path) = DynamicToolLoader::default_path() else {
            return Err(BackendError::Failed(
                "no tools.toml path for this instance".to_string(),
            ));
        };
        let brain_path = BrainLoader::resolve_path();
        let registry = ctx.service()?.tool_registry().clone();
        decide_with(
            registry,
            &self.id,
            &self.kind,
            self.decision,
            tools_path,
            brain_path,
        )
        .map_err(BackendError::Failed)
    }
}

ops! { MissionDecide }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    #[test]
    fn the_inbox_kind_crosses_the_wire() {
        for kind in [
            McInboxKind::ProposedTool,
            McInboxKind::ProposedCommand,
            McInboxKind::ProposedSkill,
        ] {
            let op = MissionDecide {
                id: "prop_tool_x".into(),
                kind,
                decision: Decision::Reject,
            };
            let json = serde_json::to_value(&op).expect("encode");
            let back: MissionDecide = serde_json::from_value(json).expect("decode");
            assert_eq!(back.kind, kind);
            assert_eq!(back.decision, Decision::Reject);
        }
    }

    #[tokio::test]
    async fn mission_decide_has_parity() {
        // The gateway crab has no agent service, so both doors answer
        // `NotReady` and nothing is written: the parity is in the refusal.
        let gateway = Gateway::start().await;
        let answer = parity!(
            gateway,
            MissionDecide {
                id: "prop_tool_missing".into(),
                kind: McInboxKind::ProposedTool,
                decision: Decision::Reject,
            }
        );
        assert_eq!(answer, serde_json::json!({ "Err": { "kind": "not_ready" } }));

        forbidden!(
            gateway,
            MissionDecide {
                id: "prop_tool_missing".into(),
                kind: McInboxKind::ProposedTool,
                decision: Decision::Apply,
            }
        );
    }
}
