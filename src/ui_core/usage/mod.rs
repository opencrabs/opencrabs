//! Usage dashboard logic: data loading, pricing tables, activity categorization.
//!
//! Terminal-free by construction: no ratatui or crossterm imports. The render
//! shell that draws this data lives in `crate::usage` (TUI today, GUI later).

pub mod categorizer;
pub mod data;
pub mod input;
pub mod loader;
pub mod pricing;
pub mod view_model;
