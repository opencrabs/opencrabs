//! Live provider-model fetching for the window.
//!
//! The terminal owns the provider-specific request logic. This module only
//! carries its `ProviderSelectorState` inputs across a worker thread and
//! brings the result back to the GUI poll. No second `/v1/models` client.

use opencrabs::tui::onboarding::fetch_provider_models;
use opencrabs::tui::provider_selector::ProviderSelectorState;

/// Everything the shared fetcher needs, captured before the worker starts.
#[derive(Clone, Debug, Default)]
pub struct FetchRequest {
    pub provider_index: usize,
    /// Switch spelling and display label, including existing custom providers.
    pub provider_id: String,
    pub provider_name: String,
    pub api_key: Option<String>,
    pub zhipu_endpoint: Option<String>,
    pub xiaomi_endpoint: Option<String>,
    pub moonshot_endpoint: Option<String>,
    pub base_url: Option<String>,
    pub opencode_endpoint: Option<String>,
}

impl FetchRequest {
    /// Capture the selected provider. `None` means the terminal would not
    /// fetch either: unsupported provider, or a key-required provider with no
    /// typed or saved credential.
    pub fn from_selector(ps: &ProviderSelectorState) -> Option<Self> {
        if !ps.supports_model_fetch() {
            return None;
        }
        let api_key = ps.effective_api_key();
        if !ps.is_custom() && !ps.is_keyless() && api_key.is_none() {
            return None;
        }
        let base_url = (!ps.base_url.trim().is_empty()).then(|| ps.base_url.clone());
        let opencode_endpoint = (ps.provider_id() == "opencode").then(|| {
            if ps.opencode_endpoint_type == 1 {
                "zen".to_string()
            } else {
                "go".to_string()
            }
        });
        let info = ps.current_provider();
        let (provider_id, provider_name) = if ps.is_custom() {
            (ps.custom_name.clone(), ps.custom_name.clone())
        } else {
            (info.id.to_string(), info.name.to_string())
        };
        Some(Self {
            provider_index: ps.selected_provider,
            provider_id,
            provider_name,
            api_key,
            zhipu_endpoint: ps.zhipu_endpoint_str(),
            xiaomi_endpoint: ps.xiaomi_endpoint_str(),
            moonshot_endpoint: ps.is_moonshot().then(|| {
                if ps.moonshot_endpoint_type == 1 {
                    "coding".to_string()
                } else {
                    "api".to_string()
                }
            }),
            base_url,
            opencode_endpoint,
        })
    }

    /// Resolve both built-in ids and existing custom provider names to the
    /// selector index used for stale-result checks.
    pub fn provider_index(provider: &str) -> Option<usize> {
        if let Some(index) = opencrabs::tui::onboarding::PROVIDERS
            .iter()
            .position(|info| info.id == provider)
        {
            return Some(index);
        }
        let mut ps = ProviderSelectorState::default();
        ps.load_custom_names();
        ps.custom_names
            .iter()
            .position(|name| name == provider)
            .map(|index| opencrabs::tui::provider_selector::CUSTOM_INSTANCES_START + index)
    }

    /// Capture a configured provider for the standalone `/models` screen.
    pub fn for_provider(index: usize) -> Option<Self> {
        let mut ps = ProviderSelectorState {
            selected_provider: index,
            ..Default::default()
        };
        ps.load_custom_names();
        ps.detect_existing_key();
        ps.reload_config_models();
        if ps.is_custom() {
            ps.load_custom_fields();
        }
        Self::from_selector(&ps)
    }
}

/// A completed fetch. The shared fetcher deliberately returns an empty list
/// on network/HTTP/parse failure, so the GUI surfaces that outcome as an
/// error while keeping the saved catalogue available as the fallback.
#[derive(Clone, Debug)]
pub struct FetchOutcome {
    pub provider_index: usize,
    pub provider_id: String,
    pub provider_name: String,
    pub models: Vec<String>,
    pub error: Option<String>,
}

/// Run the terminal's own fetcher. Kept separate from `spawn` so tests can
/// exercise a real local `/v1/models` endpoint without opening a window.
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

/// Fetch off the UI thread. reqwest needs a Tokio context, which gpui's
/// executor does not provide, so the worker owns a small current-thread
/// runtime exactly like the other GUI bridge workers.
pub fn spawn(request: FetchRequest) -> std::sync::mpsc::Receiver<FetchOutcome> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let failed = sender.clone();
    let failed_provider = (
        request.provider_index,
        request.provider_id.clone(),
        request.provider_name.clone(),
    );
    let spawned = std::thread::Builder::new()
        .name("gui-model-fetch".into())
        .spawn(move || {
            let outcome = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(fetch(request)),
                Err(error) => FetchOutcome {
                    provider_index: request.provider_index,
                    provider_id: request.provider_id.clone(),
                    provider_name: request.provider_name.clone(),
                    models: Vec::new(),
                    error: Some(format!("could not start the model fetch: {error}")),
                },
            };
            let _ = sender.send(outcome);
        });
    if let Err(error) = spawned {
        let _ = failed.send(FetchOutcome {
            provider_index: failed_provider.0,
            provider_id: failed_provider.1,
            provider_name: failed_provider.2,
            models: Vec::new(),
            error: Some(format!("could not start the model fetch worker: {error}")),
        });
    }
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::tui::provider_selector::CUSTOM_PROVIDER_IDX;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn server(status: &'static str, body: &'static str, replies: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local test listener");
        let address = listener.local_addr().expect("listener address");
        std::thread::spawn(move || {
            for _ in 0..replies {
                let (mut stream, _) = listener.accept().expect("test connection");
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("test response");
            }
        });
        format!("http://{address}/v1")
    }

    fn custom_request(base_url: String) -> FetchRequest {
        FetchRequest {
            provider_index: CUSTOM_PROVIDER_IDX,
            base_url: Some(base_url),
            ..Default::default()
        }
    }

    #[test]
    fn fetch_models_live_endpoint_populates_real_results() {
        let base = server(
            "200 OK",
            r#"{"data":[{"id":"live-two","created":2},{"id":"live-one","created":1}]}"#,
            1,
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let outcome = runtime.block_on(fetch(custom_request(base)));
        assert_eq!(outcome.models, ["live-two", "live-one"]);
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn fetch_models_failed_endpoint_becomes_an_error_state() {
        // The shared fetcher tries OpenAI's /v1/models, then Ollama's
        // /api/tags. Both fail here, which must not look like a live empty
        // catalogue in the GUI.
        let base = server("500 Internal Server Error", "{}", 2);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let outcome = runtime.block_on(fetch(custom_request(base)));
        assert!(outcome.models.is_empty());
        assert!(outcome.error.is_some());
    }
}
