//! Configuration Module
//!
//! Handles application configuration loading, validation, and management.

pub(crate) mod alias_merge;
mod current;
#[cfg(unix)]
pub(crate) mod flock;
pub mod guard;
pub mod health;
pub(crate) mod live_home_guard;
pub mod owner;
pub mod profile;
pub mod provider_registry;
pub mod registry_client;
pub mod repair;
pub mod secrets;
pub mod sections;
pub mod seed;
pub mod startup_checks;
pub mod stored_key;
pub mod timeout;
pub(crate) mod types;
pub mod update;
pub mod voice_flag_flips;
#[cfg(windows)]
pub(crate) mod winlock;
pub mod wire_dialect;

pub use provider_registry::{ProviderRegistry, ProviderRegistryConfig};
pub use registry_client::{Model, Provider, RegistryClient};
pub use secrets::SecretString;
pub use types::*;
pub use update::{ProviderUpdater, UpdateResult};
pub use wire_dialect::WireDialect;

// `merge_provider_keys` is internal to the crate but must be reachable
// from the regression tests in `src/tests/merge_provider_keys_test.rs`.
#[cfg(test)]
pub(crate) use types::merge_provider_keys;
