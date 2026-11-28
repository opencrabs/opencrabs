//! What an operation answers when it cannot answer. Serializable, because a
//! remote caller reads it off the wire.

use serde::{Deserialize, Serialize};

use super::Scope;

/// Why an operation did not produce its reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum BackendError {
    /// No domain declares an operation with this name.
    #[error("unknown operation: {0}")]
    UnknownOp(String),
    /// The caller does not hold the scope this operation needs.
    #[error("forbidden: needs {scope}")]
    Forbidden { scope: Scope },
    /// The params did not decode as the operation.
    #[error("bad params: {0}")]
    BadParams(String),
    /// The crab is still booting: the agent service is not up yet.
    #[error("the backend is not ready yet")]
    NotReady,
    /// The crab cannot take this now; the caller may retry.
    #[error("the backend is busy")]
    Busy,
    /// The operation ran and failed; carries the reason the surface shows.
    #[error("{0}")]
    Failed(String),
}

impl BackendError {
    /// The reason as the window has always shown it.
    ///
    /// A failed operation carries the exact string the code it replaced
    /// returned, so a surface that used to print `Err(String)` prints the same
    /// text through this.
    pub fn into_message(self) -> String {
        match self {
            BackendError::Failed(text) => text,
            other => other.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_survives_the_wire() {
        let all = vec![
            BackendError::UnknownOp("x.y".into()),
            BackendError::Forbidden {
                scope: Scope::Admin,
            },
            BackendError::BadParams("missing id".into()),
            BackendError::NotReady,
            BackendError::Busy,
            BackendError::Failed("db connect: nope".into()),
        ];
        for err in all {
            let json = serde_json::to_string(&err).expect("encode");
            let back: BackendError = serde_json::from_str(&json).expect("decode");
            assert_eq!(back, err, "{json}");
        }
    }

    #[test]
    fn a_failure_keeps_its_text_verbatim() {
        assert_eq!(
            BackendError::Failed("rename thread dropped its result".into()).into_message(),
            "rename thread dropped its result"
        );
    }
}
