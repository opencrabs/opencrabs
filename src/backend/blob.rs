//! Files an operation points at instead of carrying.
//!
//! An icon, an attachment, a plan file or a spoken reply lives on the crab's
//! disk. An operation answers with a [`BlobRef`]; a local surface opens the
//! path, a remote one fetches the bytes once and caches them. The fetch
//! operations (`BlobGet`, `BlobPut`) land here when the wire does.

use serde::{Deserialize, Serialize};

/// A file on the crab, named so either side can find it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobRef {
    /// Where the file lives on the crab.
    pub path: String,
    /// Its size in bytes, when the crab knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl BlobRef {
    /// A reference to `path` with no size known.
    pub fn at(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            size: None,
        }
    }
}

ops! {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blob_ref_survives_the_wire_without_a_size() {
        let blob = BlobRef::at("/home/crab/icon.png");
        let json = serde_json::to_string(&blob).expect("encode");
        assert_eq!(json, r#"{"path":"/home/crab/icon.png"}"#);
        let back: BlobRef = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, blob);
    }
}
