//! Provider discovery and model selection shared by every frontend.

mod catalogue;
mod discovery;
mod fetch;
mod order;
mod state;

pub use catalogue::load_default_models;
pub use discovery::discovery_requests;
pub use fetch::fetch_provider_models;
#[cfg(test)]
pub(crate) use fetch::{merge_minimax_baseline, opencode_models_url, xiaomi_baseline_models};
pub use order::sort_newest_first;
pub use state::{
    CUSTOM_INSTANCES_START, CUSTOM_PROVIDER_IDX, CodexDeviceFlowStatus, EXISTING_KEY_SENTINEL,
    FetchLifecycle, FetchOutcome, FetchRequest, PROVIDERS, ProviderInfo, ProviderSelectorState,
    index_of_provider, single_model_choice,
};

/// Execute one immutable request and preserve its identity for stale checks.
pub async fn fetch(request: FetchRequest) -> FetchOutcome {
    let provider_index = request.provider_index;
    let models = fetch_provider_models(
        request.provider_index,
        request.api_key.as_deref(),
        request.zhipu_endpoint.as_deref(),
        request.xiaomi_endpoint.as_deref(),
        request.moonshot_endpoint.as_deref(),
        request.base_url.as_deref(),
        request.opencode_endpoint.as_deref(),
    )
    .await;
    let error = models.is_empty().then(|| {
        "the provider returned no live models; check the key and endpoint, then retry".to_string()
    });
    FetchOutcome {
        provider_index,
        provider_id: request.provider_id,
        provider_name: request.provider_name,
        models,
        error,
    }
}
