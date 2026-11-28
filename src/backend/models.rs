//! Model pairs, onboarding and reloading the agent.
//!
//! The picker's catalogue and its live discovery are the crab's: the names a
//! provider offers come from the crab's config, and a live fetch spends the
//! crab's keys. A key never crosses the wire in either direction: the
//! catalogue carries names only, and the fetch request is built here, on the
//! crab, from its own config.
//!
//! The setup wizard is not here. It edits this machine's config and keys, so
//! it stays in the window, on this machine only (`Cap::Setup`).

use serde::{Deserialize, Serialize};

use crate::ui_core::model_selector::{CUSTOM_PROVIDER_IDX, FetchRequest, ProviderSelectorState};

use super::{BackendError, Ctx, Op, Scope};

/// One provider of the catalogue, with the model names the crab's config
/// knows for it, in the order the picker shows them.
///
/// One entry per provider rather than one per pair, so a provider that knows
/// no names yet (a custom endpoint whose list only comes from its live fetch)
/// is still named, and the window can still ask to discover it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueProvider {
    /// The provider id a switch names, e.g. `anthropic`, or a custom
    /// provider's runtime name.
    pub provider: String,
    /// The provider's display name.
    pub provider_name: String,
    /// The model ids, live fetch not included.
    pub models: Vec<String>,
}

/// The saved state of the provider at `index`, as the picker reads it: its
/// key detected (never loaded), its config-persisted model list reloaded.
pub fn saved_provider(catalogue: &ProviderSelectorState, index: usize) -> ProviderSelectorState {
    let mut saved = catalogue.clone();
    saved.selected_provider = index;
    saved.detect_existing_key();
    saved.reload_config_models();
    saved
}

/// One provider's catalogue entry from a selector snapshot. A custom
/// provider is named by its runtime name, a built-in one by its id.
pub fn catalogue_entry(selector: &ProviderSelectorState) -> CatalogueProvider {
    let (provider, provider_name) = if selector.selected_provider >= CUSTOM_PROVIDER_IDX {
        (selector.custom_name.clone(), selector.custom_name.clone())
    } else {
        (
            selector.current_provider().id.to_string(),
            selector.current_provider().name.to_string(),
        )
    };
    CatalogueProvider {
        provider,
        provider_name,
        models: selector
            .all_model_names()
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

/// Every provider in display order, from the config on this crab. The empty
/// "new custom" sentinel is skipped: it names no provider until the wizard
/// fills it in, and the wizard is not here.
pub fn catalogue() -> Vec<CatalogueProvider> {
    let mut catalogue = ProviderSelectorState::default();
    catalogue.load_custom_names();
    catalogue
        .provider_display_order()
        .into_iter()
        .filter(|index| *index != CUSTOM_PROVIDER_IDX)
        .map(|index| catalogue_entry(&saved_provider(&catalogue, index)))
        .collect()
}

/// The model picker's catalogue: every provider the crab knows and the model
/// names its config lists for each.
///
/// Names only. Whether a provider has a key, and the key itself, never leave
/// the crab.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelCatalogue {}

impl Op for ModelCatalogue {
    const NAME: &'static str = "models.catalogue";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<CatalogueProvider>;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        Ok(catalogue())
    }
}

/// Ask one provider for its live model list, with the crab's own key.
///
/// The request is built here from the crab's config; the caller names the
/// provider and nothing else. A provider the crab cannot ask (unknown, no
/// key, no list endpoint) answers an empty list, the same as it was never
/// queued. A failed fetch is the `Err`, worded as the picker shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDiscover {
    /// The provider id, or a custom provider's runtime name.
    pub provider_id: String,
}

impl Op for ModelDiscover {
    const NAME: &'static str = "models.discover";
    const SCOPE: Scope = Scope::Admin;
    type Reply = Vec<String>;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let Some(request) =
            FetchRequest::provider_index(&self.provider_id).and_then(FetchRequest::for_provider)
        else {
            return Ok(Vec::new());
        };
        let outcome = crate::ui_core::model_selector::fetch(request).await;
        match outcome.error {
            Some(error) => Err(BackendError::Failed(error)),
            None => Ok(outcome.models),
        }
    }
}

ops! { ModelCatalogue, ModelDiscover }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    /// A crab whose config lists one custom provider and whose keys file
    /// holds a key for it and for a built-in provider.
    const SECRET: &str = "sk-parity-never-on-the-wire-7f3a";

    fn seed(gateway: &Gateway) {
        let home = gateway.home();
        std::fs::write(
            home.join("config.toml"),
            "[providers.anthropic]\nenabled = true\nmodels = [\"claude-x\"]\n\n\
             [providers.custom.lab]\nenabled = true\nbase_url = \"http://127.0.0.1:9/v1\"\n\
             models = [\"lab-one\"]\n",
        )
        .expect("config.toml");
        std::fs::write(
            home.join("keys.toml"),
            format!(
                "[providers.anthropic]\napi_key = \"{SECRET}\"\n\n\
                 [providers.custom.lab]\napi_key = \"{SECRET}\"\n"
            ),
        )
        .expect("keys.toml");
    }

    #[tokio::test]
    async fn model_catalogue_has_parity() {
        let gateway = Gateway::start().await;
        seed(&gateway);
        let answer = parity!(gateway, ModelCatalogue {});
        let providers = answer["Ok"].as_array().expect("a list of providers");
        assert!(
            providers
                .iter()
                .any(|p| p["provider"] == "anthropic" && p["models"][0] == "claude-x"),
            "{answer}"
        );
        assert!(
            providers
                .iter()
                .any(|p| p["provider"] == "lab" && p["models"][0] == "lab-one"),
            "{answer}"
        );
        assert!(
            providers.iter().all(|p| p["provider"] != ""),
            "the new-custom sentinel is not a provider: {answer}"
        );
        forbidden!(gateway, ModelCatalogue {});
    }

    #[tokio::test]
    async fn the_catalogue_never_carries_a_key() {
        let gateway = Gateway::start().await;
        seed(&gateway);
        let local = gateway.local(ModelCatalogue {}).await.expect("catalogue");
        let remote = gateway
            .remote(ModelCatalogue {}, &[Scope::Read])
            .await
            .expect("catalogue over the wire");
        for answer in [local, remote] {
            let wire = serde_json::to_string(&answer).expect("encode");
            assert!(!wire.contains(SECRET), "a key crossed the wire: {wire}");
        }
    }

    #[tokio::test]
    async fn model_discover_has_parity() {
        let gateway = Gateway::start().await;
        // No key and no endpoint on a fresh crab: nothing to ask, so an
        // empty list, and nothing leaves the process.
        let answer = parity!(
            gateway,
            ModelDiscover {
                provider_id: "anthropic".into(),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": [] }));
        let answer = parity!(
            gateway,
            ModelDiscover {
                provider_id: "no-such-provider".into(),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": [] }));
        forbidden!(
            gateway,
            ModelDiscover {
                provider_id: "anthropic".into(),
            }
        );
    }

    #[test]
    fn discovery_is_admin_and_the_catalogue_is_read() {
        assert_eq!(ModelCatalogue::SCOPE, Scope::Read);
        assert_eq!(ModelDiscover::SCOPE, Scope::Admin);
    }
}
