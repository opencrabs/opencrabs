//! Service Layer
//!
//! This module contains the business logic services that orchestrate
//! operations between the database layer and the application layer.

pub(crate) mod context;
pub mod file;
pub mod force_default;
pub mod message;
pub mod project;
pub mod project_match;
pub mod session;
pub mod surface;

pub use context::{ServiceContext, ServiceManager};
pub use file::FileService;
pub use message::MessageService;
pub use project::ProjectService;
pub use session::SessionService;
