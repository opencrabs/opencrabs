//! Database Layer
//!
//! Provides database connection management, models, and repositories.

pub(crate) mod database;
pub(crate) mod migration_heal;
pub(crate) mod migration_lock;
pub(crate) mod migration_snapshot;
pub mod models;
pub mod repository;
pub mod retry;

pub use database::{
    Database, Pool, PoolExt, db_integrity_failed, db_integrity_failed_now, global_pool,
    interact_err,
};
pub use models::*;
pub use repository::*;
pub use retry::{DbRetryConfig, retry_db_anyhow, retry_db_operation, retry_db_rusqlite};
