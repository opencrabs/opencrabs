//! Provider discovery and model selection shared by every frontend.

mod catalogue;
mod discovery;
mod fetch;
mod order;
mod state;

pub use catalogue::load_default_models;
pub use discovery::discovery_requests;
pub use fetch::{fetch, fetch_provider_models};
#[cfg(test)]
pub(crate) use fetch::{merge_minimax_baseline, opencode_models_url, xiaomi_baseline_models};
pub use order::sort_newest_first;
pub use state::{
    CUSTOM_INSTANCES_START, CUSTOM_PROVIDER_IDX, CodexDeviceFlowStatus, EXISTING_KEY_SENTINEL,
    FetchLifecycle, FetchOutcome, FetchRequest, PROVIDERS, ProviderInfo, ProviderSelectorState,
    index_of_provider, single_model_choice,
};
