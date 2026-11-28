//! One operation a surface can ask of a crab, and the scope it needs.

use std::future::Future;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{BackendError, Ctx};

/// What a caller must hold to run an operation.
///
/// Flat and exact, like the A2A scopes in `remote::scopes`: holding `Admin`
/// does not imply `Read`. A pairing that should read and manage carries both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Look, never change.
    Read,
    /// Start or steer a turn.
    Send,
    /// Write to the other crab's ledger.
    Manage,
    /// Model, working directory, settings, shell, deletes.
    Admin,
}

impl Scope {
    /// Every scope, for the local door where the crab is asking itself.
    pub const ALL: &'static [Scope] = &[Scope::Read, Scope::Send, Scope::Manage, Scope::Admin];

    /// The wire string, in the same family as `remote::scopes`.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "a2a:read",
            Scope::Send => "a2a:send",
            Scope::Manage => "a2a:manage",
            Scope::Admin => "a2a:admin",
        }
    }

    /// The scope for a wire string, exact match only.
    pub fn parse(s: &str) -> Option<Scope> {
        Scope::ALL.iter().copied().find(|scope| scope.as_str() == s)
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One operation: its params are the value itself, its answer is `Reply`.
///
/// The value is what travels, so it must serialize; `run` is the only place
/// the work is written, and both the local backend and a remote dispatch call
/// it.
pub trait Op: Serialize + DeserializeOwned + Send + 'static {
    /// Wire name, `<domain>.<verb>`, for example `sessions.rename`.
    const NAME: &'static str;
    /// What a remote caller must hold.
    const SCOPE: Scope;
    /// The answer.
    type Reply: Serialize + DeserializeOwned + Send + 'static;

    /// Do the work against `ctx`.
    fn run(self, ctx: &Ctx) -> impl Future<Output = Result<Self::Reply, BackendError>> + Send;
}

/// Decode `params` as `O`, run it, encode the answer. The JSON door every
/// domain's dispatch goes through.
pub async fn run_json<O: Op>(
    ctx: &Ctx,
    params: serde_json::Value,
) -> Result<serde_json::Value, BackendError> {
    let op: O = serde_json::from_value(params)
        .map_err(|e| BackendError::BadParams(format!("{}: {e}", O::NAME)))?;
    let reply = op.run(ctx).await?;
    serde_json::to_value(reply)
        .map_err(|e| BackendError::Failed(format!("{}: reply encode: {e}", O::NAME)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_speak_the_a2a_wire_strings() {
        assert_eq!(Scope::Read.as_str(), "a2a:read");
        assert_eq!(Scope::Send.as_str(), "a2a:send");
        assert_eq!(Scope::Manage.as_str(), "a2a:manage");
        assert_eq!(Scope::Admin.as_str(), "a2a:admin");
    }

    #[test]
    fn the_read_and_send_strings_match_remote_scopes() {
        assert_eq!(Scope::Read.as_str(), crate::remote::scopes::SCOPE_READ);
        assert_eq!(Scope::Send.as_str(), crate::remote::scopes::SCOPE_SEND);
    }

    #[test]
    fn a_scope_parses_back_from_its_own_string_only() {
        for scope in Scope::ALL {
            assert_eq!(Scope::parse(scope.as_str()), Some(*scope));
        }
        assert_eq!(Scope::parse("a2a:READ"), None);
        assert_eq!(Scope::parse("read"), None);
    }
}
