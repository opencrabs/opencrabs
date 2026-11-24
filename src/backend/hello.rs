//! The handshake: protocol version and the operations a crab knows.
//!
//! A surface asks [`Hello`] first. The answer names the protocol the crab
//! speaks, its build, the scopes the caller holds, the capabilities it gets
//! and every operation the crab declares with the scope each one needs. A
//! surface greys out what is missing from that answer instead of calling it
//! and failing.
//!
//! Compatibility between a crab at version N and a surface at N-1 (or the
//! other way round) rests on two rules, both tested below: a field the reader
//! does not know is ignored, and a field the writer did not send takes its
//! default. A list entry the reader cannot decode (a scope, a capability or
//! an operation scope from a newer crab) is dropped, not fatal.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use super::{BackendError, Cap, Ctx, Op, Scope};

/// The protocol generation this crab speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// The protocol name and generation as the agent card announces it.
pub const PROTOCOL: &str = "gui-backend/1";

/// Ask the crab what it speaks. Takes no params.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hello {}

/// One operation the crab declares, and the scope a caller needs for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownOp {
    /// Wire name, `<domain>.<verb>`.
    pub name: String,
    /// What a remote caller must hold to run it.
    pub scope: Scope,
}

/// What [`Hello`] answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReply {
    /// [`PROTOCOL`] of the crab that answered.
    pub protocol: String,
    /// The crab's build, `crate::VERSION`.
    pub version: String,
    /// The scopes the caller holds.
    #[serde(default, deserialize_with = "known_only")]
    pub scopes: Vec<Scope>,
    /// The capabilities the caller's backend has.
    #[serde(default, deserialize_with = "known_only")]
    pub caps: Vec<Cap>,
    /// Every operation the crab declares.
    #[serde(default, deserialize_with = "known_only")]
    pub ops: Vec<KnownOp>,
}

impl HelloReply {
    /// The answer for a caller holding `granted`, reaching the crab locally
    /// (`local`) or across the wire.
    ///
    /// [`Hello::run`] is the local door and answers for every scope and a
    /// local backend. A server that speaks for a token answers with
    /// `for_caller(token_scopes, false)`.
    pub fn for_caller(granted: &[Scope], local: bool) -> Self {
        Self {
            protocol: PROTOCOL.to_string(),
            version: crate::VERSION.to_string(),
            scopes: Scope::ALL
                .iter()
                .copied()
                .filter(|scope| granted.contains(scope))
                .collect(),
            caps: Cap::ALL
                .iter()
                .copied()
                .filter(|cap| cap.available(local))
                .collect(),
            ops: super::registry()
                .into_iter()
                .map(|(name, scope)| KnownOp {
                    name: name.to_string(),
                    scope,
                })
                .collect(),
        }
    }
}

impl Op for Hello {
    const NAME: &'static str = "hello.handshake";
    const SCOPE: Scope = Scope::Read;
    type Reply = HelloReply;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        Ok(HelloReply::for_caller(Scope::ALL, true))
    }
}

/// Decode a list, dropping the entries this build does not understand.
fn known_only<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect())
}

ops! { Hello }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ctx::temp_ctx;
    use crate::backend::parity::{Gateway, forbidden, parity};
    use serde_json::json;

    const FIXTURE: &str = include_str!("../../tests/fixtures/gui_backend/Hello.json");

    #[test]
    fn the_protocol_name_carries_its_version() {
        assert_eq!(PROTOCOL, format!("gui-backend/{PROTOCOL_VERSION}"));
    }

    #[tokio::test]
    async fn hello_answers_the_protocol_and_every_operation() {
        let (_dir, ctx) = temp_ctx().await;
        let reply = Hello {}.run(&ctx).await.expect("hello");
        assert_eq!(reply.protocol, PROTOCOL);
        assert_eq!(reply.version, crate::VERSION);
        assert_eq!(reply.scopes, Scope::ALL.to_vec());
        assert_eq!(reply.caps, Cap::ALL.to_vec());
        let names: Vec<&str> = reply.ops.iter().map(|op| op.name.as_str()).collect();
        let declared: Vec<&str> = crate::backend::registry()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, declared);
        assert!(
            reply.ops.contains(&KnownOp {
                name: Hello::NAME.to_string(),
                scope: Scope::Read,
            }),
            "{:?}",
            reply.ops
        );
    }

    #[test]
    fn a_remote_caller_sees_only_its_scopes_and_its_capabilities() {
        let reply = HelloReply::for_caller(&[Scope::Send, Scope::Read], false);
        assert_eq!(reply.scopes, vec![Scope::Read, Scope::Send]);
        assert!(!reply.caps.contains(&Cap::OpenTerminal), "{:?}", reply.caps);
        assert!(reply.caps.contains(&Cap::OpenExternal), "{:?}", reply.caps);
    }

    #[test]
    fn the_reply_survives_the_wire() {
        let reply = HelloReply::for_caller(Scope::ALL, true);
        let json = serde_json::to_string(&reply).expect("encode");
        let back: HelloReply = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, reply);
    }

    #[test]
    fn the_fixture_is_a_hello_and_its_answer() {
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture is JSON");
        assert_eq!(fixture["name"], Hello::NAME);
        let _: Hello = serde_json::from_value(fixture["params"].clone()).expect("params decode");
        let reply: HelloReply =
            serde_json::from_value(fixture["reply"].clone()).expect("reply decode");
        assert_eq!(reply.protocol, PROTOCOL);
        // Nothing in the fixture is dropped or renamed on the way back: every
        // field and value it shows is one this build reads and writes.
        assert_eq!(
            serde_json::to_value(&reply).expect("encode"),
            fixture["reply"],
            "the fixture holds something this build does not keep"
        );
    }

    #[test]
    fn an_older_crab_answer_without_the_newer_fields_decodes() {
        // N-1: a crab that only knew the protocol and its version.
        let reply: HelloReply = serde_json::from_value(json!({
            "protocol": "gui-backend/1",
            "version": "0.0.1",
        }))
        .expect("an N-1 answer decodes");
        assert!(reply.scopes.is_empty());
        assert!(reply.caps.is_empty());
        assert!(reply.ops.is_empty());
    }

    #[test]
    fn a_newer_crab_answer_with_unknown_fields_and_values_decodes() {
        // N+1: a field this build has never seen, a scope, a capability and
        // an operation scope it cannot name. The known parts survive.
        let reply: HelloReply = serde_json::from_value(json!({
            "protocol": "gui-backend/1",
            "version": "99.0.0",
            "uptime_secs": 42,
            "scopes": ["read", "root"],
            "caps": ["open_terminal", "teleport"],
            "ops": [
                { "name": "sessions.load", "scope": "read" },
                { "name": "future.op", "scope": "root" },
                { "name": "future.other", "scope": "send", "streams": true },
            ],
        }))
        .expect("an N+1 answer decodes");
        assert_eq!(reply.scopes, vec![Scope::Read]);
        assert_eq!(reply.caps, vec![Cap::OpenTerminal]);
        assert_eq!(
            reply.ops,
            vec![
                KnownOp {
                    name: "sessions.load".into(),
                    scope: Scope::Read,
                },
                KnownOp {
                    name: "future.other".into(),
                    scope: Scope::Send,
                },
            ]
        );
    }

    #[test]
    fn a_hello_with_params_from_a_newer_surface_decodes() {
        let _: Hello = serde_json::from_value(json!({ "want": ["caps"] }))
            .expect("unknown params are ignored");
    }

    #[tokio::test]
    async fn hello_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(gateway, Hello {});
        assert_eq!(answer["Ok"]["protocol"], PROTOCOL, "{answer}");
        forbidden!(gateway, Hello {});
    }
}
