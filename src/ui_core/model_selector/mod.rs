//! Provider discovery and model selection shared by every frontend.

mod catalogue;
mod order;
mod state;

pub use catalogue::load_default_models;
pub use order::sort_newest_first;
pub use state::{
    CUSTOM_INSTANCES_START, CUSTOM_PROVIDER_IDX, CodexDeviceFlowStatus, EXISTING_KEY_SENTINEL,
    PROVIDERS, ProviderInfo, ProviderSelectorState, index_of_provider,
};
