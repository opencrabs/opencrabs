//! Regression coverage for moving model selection out of the TUI.

use crate::tui::provider_selector::{self, CUSTOM_PROVIDER_IDX};
use crate::ui_core::model_selector::{
    self, FetchLifecycle, FetchOutcome, FetchRequest, ProviderSelectorState,
};

fn keyed_openai() -> ProviderSelectorState {
    ProviderSelectorState {
        selected_provider: provider_selector::index_of_provider("openai").unwrap(),
        api_key_input: "typed-test-key".into(),
        ..Default::default()
    }
}

fn response(provider_index: usize, models: &[&str]) -> FetchOutcome {
    FetchOutcome {
        provider_index,
        provider_id: "test".into(),
        provider_name: "Test".into(),
        models: models.iter().map(|model| (*model).to_string()).collect(),
        error: None,
    }
}

#[test]
fn moved_tui_state_filters_and_accepts_arbitrary_model_ids() {
    let mut selector = keyed_openai();
    selector.models = vec!["gpt-old".into(), "gpt-current".into()];
    selector.model_filter = "brand-new-model".into();
    assert_eq!(selector.selected_model_name(), "brand-new-model");

    // This is the same type and method the TUI compatibility export exposes.
    let tui_selector: provider_selector::ProviderSelectorState = selector;
    assert_eq!(tui_selector.selected_model_name(), "brand-new-model");
}

#[test]
fn moved_tui_order_compares_version_segments_numerically() {
    let mut models = vec!["glm-5.2", "glm-5.10", "glm-5-turbo", "glm-5.3s"];
    model_selector::sort_newest_first(&mut models);
    assert_eq!(models, ["glm-5.10", "glm-5.3s", "glm-5.2", "glm-5-turbo"]);
}

#[test]
fn catalogue_refresh_covers_every_provider_and_saved_custom_instance() {
    let selector = ProviderSelectorState {
        custom_names: vec!["local-one".into(), "local-two".into()],
        ..Default::default()
    };
    let order = selector.refresh_order("openai");
    assert_eq!(
        order[0],
        provider_selector::index_of_provider("openai").unwrap()
    );
    assert_eq!(order.len(), model_selector::PROVIDERS.len() + 2);
    assert!(order.contains(&model_selector::CUSTOM_INSTANCES_START));
    assert!(order.contains(&(model_selector::CUSTOM_INSTANCES_START + 1)));
}

#[test]
fn request_snapshots_unsaved_endpoint_and_key_edits() {
    let mut selector = ProviderSelectorState {
        selected_provider: CUSTOM_PROVIDER_IDX,
        api_key_input: "typed-test-key".into(),
        custom_name: "local-test".into(),
        base_url: "http://127.0.0.1:9999/v1".into(),
        ..Default::default()
    };
    let request = FetchRequest::from_selector(&selector).unwrap();
    assert_eq!(request.provider_id, "local-test");
    assert_eq!(
        request.base_url.as_deref(),
        Some("http://127.0.0.1:9999/v1")
    );
    assert_eq!(request.api_key.as_deref(), Some("typed-test-key"));

    selector.selected_provider = provider_selector::index_of_provider("moonshot").unwrap();
    selector.moonshot_endpoint_type = 1;
    selector.api_key_input = "typed-test-key".into();
    let request = FetchRequest::from_selector(&selector).unwrap();
    assert_eq!(request.moonshot_endpoint.as_deref(), Some("coding"));
}

#[test]
fn fresh_oauth_credentials_survive_before_the_config_write_lands() {
    let selector = ProviderSelectorState {
        selected_provider: provider_selector::index_of_provider("github").unwrap(),
        ..Default::default()
    };
    let request = FetchRequest::from_selector_and_api_key_for_provider(
        &selector,
        "github",
        Some("fresh-oauth-test-token".into()),
    )
    .unwrap();
    assert_eq!(request.provider_id, "github");
    assert_eq!(request.api_key.as_deref(), Some("fresh-oauth-test-token"));

    let other_provider = keyed_openai();
    assert!(
        FetchRequest::from_selector_and_api_key_for_provider(
            &other_provider,
            "github",
            Some("fresh-oauth-test-token".into()),
        )
        .is_none()
    );
}

#[test]
fn stale_generations_and_provider_responses_cannot_finish_a_request() {
    let mut lifecycle = FetchLifecycle::default();
    let first = FetchRequest {
        provider_index: 0,
        ..Default::default()
    };
    let first_generation = lifecycle.begin(&first);
    let second_generation = lifecycle.begin(&first);
    assert!(second_generation > first_generation);
    assert_eq!(
        lifecycle.finish(first_generation, 0, response(0, &["old"])),
        None
    );
    assert_eq!(
        lifecycle.finish(second_generation, 1, response(1, &["wrong"])),
        None
    );
    assert_eq!(
        lifecycle
            .finish(second_generation, 0, response(0, &["current"]))
            .unwrap()
            .models,
        ["current"]
    );
    assert!(!lifecycle.is_fetching());
}

#[test]
fn invalidating_a_provider_change_rejects_the_old_response() {
    let mut lifecycle = FetchLifecycle::default();
    let generation = lifecycle.begin(&FetchRequest {
        provider_index: 0,
        ..Default::default()
    });
    lifecycle.invalidate();
    assert_eq!(
        lifecycle.finish(generation, 0, response(0, &["stale"])),
        None
    );
}

#[tokio::test]
async fn shared_transport_reads_a_real_custom_models_endpoint() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request);
        let body = r#"{"data":[{"id":"custom-future-model"}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let selector = ProviderSelectorState {
        selected_provider: CUSTOM_PROVIDER_IDX,
        base_url: format!("http://{address}/v1"),
        custom_name: "test-custom".into(),
        ..Default::default()
    };
    let request = FetchRequest::from_selector(&selector).unwrap();
    let outcome = model_selector::fetch(request).await;
    assert_eq!(outcome.provider_id, "test-custom");
    assert_eq!(outcome.models, ["custom-future-model"]);
    assert_eq!(outcome.error, None);
}

#[test]
fn discovery_queue_snapshots_unsaved_credentials_before_saved_providers() {
    let dir = tempfile::tempdir().unwrap();
    crate::config::profile::with_home_override(dir.path().to_path_buf(), || {
        let catalogue = ProviderSelectorState::default();
        let edited = keyed_openai();
        let requests = model_selector::discovery_requests(&catalogue, "openai", Some(&edited));
        let first = requests.first().unwrap();
        assert_eq!(first.provider_id, "openai");
        assert_eq!(first.api_key.as_deref(), Some("typed-test-key"));
    });
}

#[test]
fn discovery_queue_does_not_resurrect_an_empty_edited_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    crate::config::profile::with_home_override(dir.path().to_path_buf(), || {
        let catalogue = ProviderSelectorState::default();
        let edited = ProviderSelectorState {
            selected_provider: CUSTOM_PROVIDER_IDX,
            custom_name: "unsaved-custom".into(),
            ..Default::default()
        };
        let requests =
            model_selector::discovery_requests(&catalogue, "unsaved-custom", Some(&edited));
        assert!(
            requests
                .iter()
                .all(|request| request.provider_id != "unsaved-custom")
        );
    });
}
