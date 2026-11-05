//! GUI transport adapter for the shared model selector.
//!
//! Provider request construction, endpoint behavior, response normalization,
//! and stale-result identity live in `ui_core::model_selector`.

pub use opencrabs::ui_core::model_selector::{FetchOutcome, FetchRequest};

pub async fn fetch(request: FetchRequest) -> FetchOutcome {
    opencrabs::ui_core::model_selector::fetch(request).await
}

/// Fetch off the UI thread. GPUI has no Tokio context for reqwest.
pub fn spawn(
    generation: u64,
    request: FetchRequest,
) -> std::sync::mpsc::Receiver<(u64, FetchOutcome)> {
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
            let _ = sender.send((generation, outcome));
        });
    if let Err(error) = spawned {
        let _ = failed.send((
            generation,
            FetchOutcome {
                provider_index: failed_provider.0,
                provider_id: failed_provider.1,
                provider_name: failed_provider.2,
                models: Vec::new(),
                error: Some(format!("could not start the model fetch worker: {error}")),
            },
        ));
    }
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::ui_core::model_selector::CUSTOM_PROVIDER_IDX;
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
