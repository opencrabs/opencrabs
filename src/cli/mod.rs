//! CLI Module
//!
//! Command-line interface for OpenCrabs using Clap v4.

mod args;
pub(crate) mod commands;
pub(crate) mod connect;
pub(crate) mod crash_recovery;
mod cron;
pub(crate) mod daemon_health;
pub(crate) mod doctor_fix;
pub(crate) mod headless_callbacks;
pub(crate) mod migrate;
pub(crate) mod resume_delivery;
pub(crate) mod session_notify;
pub(crate) mod session_resolve;
pub(crate) mod session_set_model;
pub(crate) mod tool_setup;
pub(crate) mod ui;

pub use args::*;
