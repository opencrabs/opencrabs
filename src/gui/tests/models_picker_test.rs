//! The standalone model picker reads the crab the window holds, never this
//! machine: its catalogue is `ModelCatalogue` and its live discovery is
//! `ModelDiscover`, both answered by the crab.
//!
//! A remote crab is a `RemoteCrab` on a port nothing listens on, so a
//! discovery that reached it comes back as the transport's refusal, named
//! after the crab, and one fetched here would not.
//!
//! The catalogue is asked without waiting: a crab that is slow to answer
//! never holds the window while the picker opens.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use opencrabs::backend::models::CatalogueProvider;
use opencrabs::backend::{BackendError, Scope};
use opencrabs::ui_core::model_selector::CUSTOM_PROVIDER_IDX;

use crate::backend_glue::{Backend, RemoteCrab};
use crate::dashboard_models::ask_catalogue;
use crate::model_fetch::{self, FetchRequest, LOCAL_FETCHES};
use crate::models;

fn remote() -> Backend {
    Backend::Remote(Arc::new(RemoteCrab::new(
        "elsewhere",
        "http://127.0.0.1:9",
        None,
        PathBuf::from("unused.db"),
    )))
}

fn entry(provider: &str, name: &str, models: &[&str]) -> CatalogueProvider {
    CatalogueProvider {
        provider: provider.into(),
        provider_name: name.into(),
        models: models.iter().map(|model| model.to_string()).collect(),
    }
}

fn local_fetches() -> usize {
    LOCAL_FETCHES.with(|count| count.get())
}

#[test]
fn a_remote_crab_is_asked_and_nothing_is_fetched_here() {
    let catalogue = vec![
        entry("anthropic", "Anthropic", &["claude-x"]),
        entry("lab", "lab", &[]),
    ];
    let queue = models::discovery_queue(&catalogue, "lab");
    assert_eq!(queue[0].provider_id, "lab", "the active provider leads");
    assert!(
        queue.iter().all(|request| request.api_key.is_none()),
        "the window's queue names providers, it carries no key"
    );

    let before = local_fetches();
    let receiver = model_fetch::discover(&remote(), 7, queue[0].clone());
    let (generation, outcome) = receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("the crab's door answers");
    assert_eq!(
        local_fetches(),
        before,
        "the picker fetched from this machine"
    );
    assert_eq!(generation, 7);
    assert_eq!(outcome.provider_index, queue[0].provider_index);
    assert_eq!(outcome.provider_id, "lab");
    let error = outcome.error.expect("nothing listens on the crab's port");
    assert!(error.contains("elsewhere"), "{error}");
    assert!(error.contains("models.discover"), "{error}");
}

#[test]
fn the_wizard_door_is_the_one_that_fetches_here() {
    // The counter the test above reads is real: the wizard's door moves it.
    let before = local_fetches();
    let receiver = model_fetch::spawn(
        3,
        FetchRequest {
            provider_index: CUSTOM_PROVIDER_IDX,
            base_url: Some("http://127.0.0.1:9/v1".into()),
            ..Default::default()
        },
    );
    assert_eq!(local_fetches(), before + 1);
    let (generation, _) = receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("the local fetch answers");
    assert_eq!(generation, 3);
}

#[test]
fn a_refused_discovery_is_a_log_line_and_a_failed_one_is_shown() {
    let notice = model_fetch::discover_refusal(&BackendError::Forbidden {
        scope: Scope::Admin,
    })
    .expect("a missing scope is about access");
    assert!(notice.contains("a2a:admin"), "{notice}");
    assert_eq!(
        model_fetch::discover_refusal(&BackendError::Failed("rate limited".into())),
        None
    );
}

#[test]
fn the_crab_catalogue_becomes_rows_with_the_live_pair_marked() {
    let catalogue = vec![
        entry("anthropic", "Anthropic", &["claude-x", "claude-y"]),
        entry("lab", "lab", &["lab-one"]),
    ];
    let rows = models::rows_from_catalogue(&catalogue, Some(("lab", "lab-one")));
    let pairs: Vec<String> = rows.iter().map(models::Row::pair).collect();
    assert_eq!(
        pairs,
        ["anthropic/claude-x", "anthropic/claude-y", "lab/lab-one"]
    );
    let current: Vec<&str> = rows
        .iter()
        .filter(|row| row.current)
        .map(|row| row.model.as_str())
        .collect();
    assert_eq!(current, ["lab-one"]);
}

#[test]
fn a_provider_with_no_list_endpoint_is_never_queued() {
    // `qwen` has no models endpoint; a custom provider's endpoint is the
    // crab's to know, so it is always asked.
    let catalogue = vec![
        entry("qwen", "Qwen", &["qwen-x"]),
        entry("openai", "OpenAI", &["gpt-x"]),
        entry("lab", "lab", &[]),
    ];
    let queue: Vec<String> = models::discovery_queue(&catalogue, "")
        .into_iter()
        .map(|request| request.provider_id)
        .collect();
    assert_eq!(queue, ["openai", "lab"]);
}

#[test]
fn opening_the_picker_does_not_wait_for_a_slow_crab() {
    // A crab that takes the connection and never answers: a read waits on
    // it up to the transport's deadline. `open_models` asks through this
    // door, so it returns as soon as the ask is out.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
    let port = listener.local_addr().expect("the port's address").port();
    let slow = Backend::Remote(Arc::new(RemoteCrab::new(
        "slow",
        format!("http://127.0.0.1:{port}"),
        None,
        PathBuf::from("unused.db"),
    )));

    let asked = Instant::now();
    let mut reply = ask_catalogue(&slow);
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "the ask waited {:?} for the crab",
        asked.elapsed()
    );
    assert!(
        matches!(reply.try_recv(), Ok(None)),
        "the slow crab has not answered yet"
    );

    // Closing the port ends the read; its answer still arrives, as a failure
    // named after the crab.
    drop(listener);
    let answer = futures::executor::block_on(reply).expect("the worker answers");
    let error = answer
        .expect_err("nothing answered the read")
        .into_message();
    assert!(error.contains("slow"), "{error}");
}
