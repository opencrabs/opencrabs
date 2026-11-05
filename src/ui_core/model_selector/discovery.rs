//! Build one active-first discovery queue for every model picker.

use crate::ui_core::model_selector::{FetchRequest, ProviderSelectorState};

/// Snapshot every usable provider request. An edited wizard selector supplies
/// the current provider's unsaved key and endpoint; all other providers use
/// their saved configuration. The edited provider always leads the queue.
pub fn discovery_requests(
    catalogue: &ProviderSelectorState,
    active_provider: &str,
    edited: Option<&ProviderSelectorState>,
) -> Vec<FetchRequest> {
    let mut order = edited.unwrap_or(catalogue).refresh_order(active_provider);
    if let Some(selector) = edited {
        order.retain(|index| *index != selector.selected_provider);
        order.insert(0, selector.selected_provider);
    }
    order
        .into_iter()
        .filter_map(|index| {
            if let Some(selector) = edited.filter(|selector| selector.selected_provider == index) {
                FetchRequest::from_selector(selector)
            } else {
                FetchRequest::for_provider(index)
            }
        })
        .collect()
}
