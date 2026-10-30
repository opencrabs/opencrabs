//! Usage Dashboard
//!
//! Full-screen usage analytics dashboard with 6 cards:
//! Summary Bar, Daily Activity, By Project, By Model, Core Tools, By Activity.
//!
//! Terminal-free logic (data, pricing, categorizer, view model, key
//! decisions) lives in `crate::ui_core::usage` and is re-exported here so
//! existing callers keep their paths. `keymap` is the crossterm edge that
//! translates terminal keys into the neutral `UiKey` those decisions read.

pub mod cards;
pub mod dashboard;
pub mod keymap;
pub mod layout;

pub use crate::ui_core::usage::{categorizer, data, pricing};
