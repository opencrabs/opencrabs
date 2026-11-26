//! The catalogue fetch has to reach our own endpoint as an OpenCrabs install.
//!
//! Every other call to the gateway already carries the identity headers. The
//! model-catalogue fetch was the one path that went out with nothing but
//! `Authorization`, so a request aimed at our own host arrived anonymous and
//! the proxy had no way to tell it apart from any other HTTP client.

use crate::brain::provider::identity::user_agent;
use crate::brain::provider::model_fetch::{catalogue_request, catalogue_request_headers};

/// The endpoint we own. It is the one host whose gate depends on the answer
/// rather than merely preferring it.
const OPENCRABS: &str = "https://inference.opencrabs.com/v1";
/// A host with no identity contract: nothing may be added to it.
const OTHER: &str = "https://api.openai.com/v1";

fn get<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
    h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// The gap this guards: the catalogue fetch used to send only the bearer
/// token, so our own endpoint saw an anonymous caller.
#[test]
fn the_catalogue_fetch_carries_our_identity_to_our_own_endpoint() {
    let h = catalogue_request_headers(OPENCRABS, None);
    assert_eq!(
        get(&h, "User-Agent"),
        Some(user_agent().as_str()),
        "the catalogue fetch reached our endpoint anonymous"
    );
}

/// A configured base URL can carry the chat suffix the provider factory
/// appends. Identity follows the host, so the suffix must not hide it.
#[test]
fn identity_survives_a_base_url_carrying_the_chat_suffix() {
    let h = catalogue_request_headers("https://inference.opencrabs.com/v1/chat/completions", None);
    assert_eq!(get(&h, "User-Agent"), Some(user_agent().as_str()));
}

/// The bearer token and the identity header are separate things, and both have
/// to be there: the token says who pays, the User-Agent says who is calling.
#[test]
fn the_bearer_token_and_the_identity_header_travel_together() {
    let h = catalogue_request_headers(OPENCRABS, Some("sk-test"));
    assert_eq!(get(&h, "Authorization"), Some("Bearer sk-test"));
    assert_eq!(get(&h, "User-Agent"), Some(user_agent().as_str()));
}

/// A host without an identity contract gets nothing added to it. The token it
/// was configured with is all it ever had.
#[test]
fn a_host_without_an_identity_contract_gets_only_its_token() {
    assert!(catalogue_request_headers(OTHER, None).is_empty());
    let h = catalogue_request_headers(OTHER, Some("sk-test"));
    assert_eq!(h.len(), 1, "unexpected extra headers: {h:?}");
    assert_eq!(get(&h, "Authorization"), Some("Bearer sk-test"));
}

/// The same on the wire the request actually carries. Both attempts of the
/// catalogue fetch go through the same builder, so the OpenAI-compatible one
/// and the Ollama fallback cannot drift apart.
#[test]
fn the_request_built_for_our_endpoint_carries_the_user_agent() {
    let client = reqwest::Client::new();
    let req = catalogue_request(
        &client,
        "https://inference.opencrabs.com/v1/models",
        OPENCRABS,
        None,
    )
    .build()
    .expect("request builds");

    let ua = req
        .headers()
        .get("User-Agent")
        .and_then(|v| v.to_str().ok());
    assert_eq!(ua, Some(user_agent().as_str()));
}

/// The same request aimed at a foreign host must not claim to be us.
#[test]
fn the_request_built_for_another_host_carries_no_identity() {
    let client = reqwest::Client::new();
    let req = catalogue_request(&client, "https://api.openai.com/v1/models", OTHER, None)
        .build()
        .expect("request builds");

    assert!(
        req.headers().get("User-Agent").is_none(),
        "a foreign host must not see our User-Agent"
    );
}
