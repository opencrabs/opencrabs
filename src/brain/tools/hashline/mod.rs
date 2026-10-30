//! Hashline editing module.
//!
//! Provides hash-anchored file editing where lines are referenced by short
//! content hashes instead of reproduced text.

pub mod edit;
pub mod hash;
pub mod types;

pub use edit::HashlineEditTool;
