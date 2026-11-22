//! Gateways identify callers by header. OpenCode Zen asked for a `User-Agent`
//! (ours arrived as "Unknown client", because reqwest sends none by default)
//! and a stable `X-Opencode-Session`, warning that requests without it may be
//! rejected. The same anonymity applied to every other gateway we talk to.

use crate::brain::provider::identity::{Vendor, headers_for, user_agent};
use uuid::Uuid;

const ZEN: &str = "https://opencode.ai/zen/go/v1";
const ZAI: &str = "https://api.z.ai/api/coding/paas/v4";
const MODELSCOPE: &str = "https://api-inference.modelscope.ai/v1";
const KIMI: &str = "https://api.kimi.com/coding/v1";
const DEEPSEEK: &str = "https://api.deepseek.com";
/// The endpoint we own. It is the one host whose gate depends on the answer
/// rather than merely preferring it (#3138).
const OPENCRABS: &str = "https://inference.opencrabs.com/v1";

fn get<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
    h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

// ── host resolution ──────────────────────────────────────────────────

#[test]
fn each_gateway_resolves_from_its_host() {
    assert_eq!(Vendor::from_base_url(ZEN), Some(Vendor::OpenCode));
    assert_eq!(Vendor::from_base_url(ZAI), Some(Vendor::Zai));
    assert_eq!(Vendor::from_base_url(MODELSCOPE), Some(Vendor::ModelScope));
    assert_eq!(Vendor::from_base_url(KIMI), Some(Vendor::Kimi));
    assert_eq!(Vendor::from_base_url(DEEPSEEK), Some(Vendor::DeepSeek));
    assert_eq!(Vendor::from_base_url(OPENCRABS), Some(Vendor::OpenCrabs));
}

/// Our own host is matched by parsed host, never by substring: a URL that
/// merely carries the name, or a lookalike domain, must not resolve to it.
/// The proxy trusts this answer to decide who is calling (#3138).
#[test]
fn our_endpoint_is_matched_by_host_and_never_by_substring() {
    for url in [
        "https://inference.opencrabs.com",
        "https://inference.opencrabs.com/",
        "https://inference.opencrabs.com:443/v1",
        "inference.opencrabs.com/v1",
        "  https://inference.opencrabs.com/v1  ",
    ] {
        assert_eq!(
            Vendor::from_base_url(url),
            Some(Vendor::OpenCrabs),
            "should resolve: {url}"
        );
    }
    for url in [
        "https://inference.opencrabs.com.attacker.example/v1",
        "https://notinference.opencrabs.com/v1",
        "https://evil.example/?x=inference.opencrabs.com",
        "https://proxy.example/inference.opencrabs.com",
        "https://inference.opencrabs.org/v1",
    ] {
        assert_eq!(Vendor::from_base_url(url), None, "must not resolve: {url}");
    }
}

/// The whole point of #3138: before this, a request to our own endpoint went
/// out with no `User-Agent` at all, so the proxy could not tell an OpenCrabs
/// install from any other HTTP client.
#[test]
fn our_endpoint_receives_the_opencrabs_user_agent() {
    let h = headers_for(OPENCRABS, Some(Uuid::new_v4()));
    assert_eq!(
        get(&h, "User-Agent"),
        Some(user_agent().as_str()),
        "our own endpoint saw no identity before #3138"
    );
    // Nothing else is added: the proxy asks for the User-Agent and no more,
    // and a header it does not expect is a fingerprint difference.
    assert_eq!(h.len(), 1, "unexpected extra headers: {h:?}");
}

/// The documented ModelScope endpoint is `.cn`; this deployment is configured
/// against `.ai`. Both are the same vendor.
#[test]
fn both_modelscope_hosts_resolve() {
    assert_eq!(
        Vendor::from_base_url("https://api-inference.modelscope.cn/v1"),
        Some(Vendor::ModelScope)
    );
    assert_eq!(
        Vendor::from_base_url("https://api-inference.modelscope.ai/v1"),
        Some(Vendor::ModelScope)
    );
}

#[test]
fn moonshot_and_bigmodel_aliases_resolve() {
    assert_eq!(
        Vendor::from_base_url("https://api.moonshot.ai/v1"),
        Some(Vendor::Kimi)
    );
    assert_eq!(
        Vendor::from_base_url("https://open.bigmodel.cn/api/paas/v4"),
        Some(Vendor::Zai)
    );
}

#[test]
fn port_case_and_path_variations_still_resolve() {
    assert_eq!(
        Vendor::from_base_url("HTTPS://OpenCode.AI/zen/go/v1"),
        Some(Vendor::OpenCode)
    );
    assert_eq!(
        Vendor::from_base_url("https://opencode.ai:443/zen/go/v1"),
        Some(Vendor::OpenCode)
    );
    assert_eq!(
        Vendor::from_base_url("https://opencode.ai/zen/go/v1/chat/completions"),
        Some(Vendor::OpenCode)
    );
}

/// Suffix-matched on a dot-prefixed domain, so a look-alike host or a URL that
/// merely mentions the name never picks up another vendor's identity.
#[test]
fn look_alike_hosts_never_match() {
    for url in [
        "https://evil-opencode.ai.example.com/v1",
        "https://example.com/opencode.ai/v1",
        "https://notopencode.ai/v1",
        "https://deepseek.com.evil.example/v1",
        "https://openrouter.ai/api/v1",
        "http://localhost:8888/v1",
    ] {
        assert_eq!(Vendor::from_base_url(url), None, "should not match: {url}");
    }
}

/// A `deepseek-*` model served by an aggregator must not draw DeepSeek
/// identity: identity follows the host, never the model id.
#[test]
fn a_deepseek_model_on_another_host_gets_no_deepseek_identity() {
    for aggregator in [
        "https://openrouter.ai/api/v1",
        "https://integrate.api.nvidia.com/v1",
    ] {
        assert!(headers_for(aggregator, Some(Uuid::new_v4())).is_empty());
    }
}

// ── what each vendor receives ────────────────────────────────────────

#[test]
fn every_identified_gateway_gets_a_user_agent() {
    assert!(user_agent().starts_with("OpenCrabs/"));
    for url in [ZEN, ZAI, MODELSCOPE, KIMI, DEEPSEEK, OPENCRABS] {
        assert_eq!(
            get(&headers_for(url, Some(Uuid::new_v4())), "User-Agent"),
            Some(user_agent().as_str()),
            "requests to {url} arrived as 'Unknown client' without this"
        );
    }
}

#[test]
fn opencode_carries_the_session_and_the_identity_pair() {
    let session = Uuid::new_v4();
    let h = headers_for(ZEN, Some(session));
    assert_eq!(
        get(&h, "X-Opencode-Session"),
        Some(session.to_string().as_str())
    );
    assert_eq!(get(&h, "X-Title"), Some("OpenCrabs"));
    assert_eq!(get(&h, "HTTP-Referer"), Some("https://opencrabs.com"));
}

/// z.ai documents `Accept-Language` in every request example.
#[test]
fn zai_carries_accept_language() {
    assert_eq!(
        get(&headers_for(ZAI, None), "Accept-Language"),
        Some("en-US,en")
    );
}

/// The other three document only Authorization and Content-Type, so the
/// User-Agent is the whole contract. Sending more risks the stricter
/// fingerprint bucket documented in provider/qwen.rs.
#[test]
fn gateways_without_a_session_contract_get_only_a_user_agent() {
    for url in [MODELSCOPE, KIMI, DEEPSEEK] {
        let h = headers_for(url, Some(Uuid::new_v4()));
        assert_eq!(h.len(), 1, "unexpected extra headers for {url}: {h:?}");
        assert_eq!(get(&h, "User-Agent"), Some(user_agent().as_str()));
    }
    assert!(get(&headers_for(KIMI, None), "X-Opencode-Session").is_none());
}

// ── session behaviour ────────────────────────────────────────────────

#[test]
fn the_session_id_is_stable_across_requests_in_one_conversation() {
    let session = Uuid::new_v4();
    let first = headers_for(ZEN, Some(session));
    let second = headers_for(ZEN, Some(session));
    assert_eq!(
        get(&first, "X-Opencode-Session"),
        get(&second, "X-Opencode-Session")
    );

    let other = headers_for(ZEN, Some(Uuid::new_v4()));
    assert_ne!(
        get(&first, "X-Opencode-Session"),
        get(&other, "X-Opencode-Session"),
        "two conversations must not share one session id"
    );
}

/// Calls made before any conversation exists (the catalogue fetch) still have
/// to carry a well-formed id, and stay in one bucket across the process.
#[test]
fn a_sessionless_call_still_sends_a_stable_well_formed_id() {
    let first = headers_for(ZEN, None);
    let id = get(&first, "X-Opencode-Session").expect("session header must always be present");
    assert!(Uuid::parse_str(id).is_ok(), "id must be well formed: {id}");
    assert_eq!(
        id,
        get(&headers_for(ZEN, None), "X-Opencode-Session").unwrap()
    );
}

// ── wire safety ──────────────────────────────────────────────────────

/// A malformed name or value is dropped at the header builder, which would
/// silently ship a request without the identity the gateway requires.
#[test]
fn every_emitted_header_is_valid_on_the_wire() {
    for url in [ZEN, ZAI, MODELSCOPE, KIMI, DEEPSEEK, OPENCRABS] {
        for (k, v) in headers_for(url, Some(Uuid::new_v4())) {
            assert!(
                k.parse::<reqwest::header::HeaderName>().is_ok(),
                "invalid header name for {url}: {k}"
            );
            assert!(
                v.parse::<reqwest::header::HeaderValue>().is_ok(),
                "invalid header value for {url} {k}: {v}"
            );
        }
    }
}

#[test]
fn unknown_hosts_receive_nothing() {
    for url in [
        "https://api.openai.com/v1",
        "https://inference.hetzner.com/api/v1",
        "https://dashscope.aliyuncs.com/compatible-mode/v1",
    ] {
        assert!(headers_for(url, Some(Uuid::new_v4())).is_empty());
    }
}
