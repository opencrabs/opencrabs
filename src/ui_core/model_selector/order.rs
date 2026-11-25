//! Compatibility path for the newest-first comparator.
//!
//! The implementation lives in [`crate::utils::model_order`] so the provider
//! layer can order the live Gemini catalogue without reaching into the
//! frontend core. Every existing import path keeps resolving here.

pub use crate::utils::model_order::sort_newest_first;
