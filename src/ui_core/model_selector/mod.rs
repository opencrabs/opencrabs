//! Provider discovery and model selection shared by every frontend.

mod catalogue;
mod order;

pub use catalogue::load_default_models;
pub use order::sort_newest_first;
