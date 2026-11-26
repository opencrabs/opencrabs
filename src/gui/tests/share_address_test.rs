//! Tests for where the sharing card sends the other crab.
//!
//! Kept apart from `instances_wiring_test` because the rule under test is not
//! the card's shape: it is which address wins when the machine publishes more
//! than one, and that rule was wrong in a way only a real machine showed.

use crate::instance_strip::share_address;
use opencrabs::config::A2aConfig;
use opencrabs::remote::tunnel::TunnelState;

fn loopback() -> A2aConfig {
    A2aConfig {
        enabled: true,
        bind: "127.0.0.1".to_string(),
        port: 18790,
        ..Default::default()
    }
}

fn live(url: &str) -> TunnelState {
    TunnelState {
        pid: 4242,
        url: url.to_string(),
        port: 18790,
        origin_host: "127.0.0.1".to_string(),
        binary: "/tmp/cloudflared".to_string(),
        started_at: 1_700_000_000,
    }
}

#[test]
fn a_running_tunnel_beats_the_bind_address() {
    // Measured on the lenovo, 21 Nov: cloudflared was publishing 18790 on a
    // public hostname while the card insisted the machine was local-only and
    // handed out 127.0.0.1. A tunnel that is up is the only address another
    // crab can dial, so it wins, and the two paragraphs that exist to explain
    // an unreachable address must go with it.
    let tunnel = live("https://x.trycloudflare.com");
    let (url, warning, snippet) = share_address(&loopback(), Some(&tunnel));

    assert_eq!(url, "https://x.trycloudflare.com");
    assert!(
        warning.is_none(),
        "a reachable address may not carry a warning: {warning:?}"
    );
    assert!(
        snippet.is_none(),
        "the TOML block tells the owner to change a bind that no longer matters"
    );
}

#[test]
fn without_a_tunnel_the_bind_address_decides() {
    let (url, warning, snippet) = share_address(&loopback(), None);

    assert_eq!(url, "http://127.0.0.1:18790");
    assert!(
        warning.is_some(),
        "loopback is unreachable from another machine and the card must say so"
    );
    assert!(
        snippet.is_some(),
        "the warning names keys; the block is the thing the owner copies"
    );
}

#[test]
fn an_advertised_address_needs_no_tunnel_and_no_warning() {
    // A machine already published on the LAN (or through tailscale) has nothing
    // left to warn about: the owner said how to reach it, and the bind agrees.
    let a2a = A2aConfig {
        bind: "0.0.0.0".to_string(),
        advertise_url: Some("http://192.168.1.114:18790".to_string()),
        ..loopback()
    };
    let (url, warning, snippet) = share_address(&a2a, None);

    assert_eq!(url, "http://192.168.1.114:18790");
    assert!(warning.is_none(), "the owner already said how to reach it");
    // A block may still be there for an unrelated missing key; what must not be
    // there is a bind instruction for a bind that is already open.
    if let Some(snippet) = snippet {
        assert!(
            !snippet.contains("bind ="),
            "the bind is already 0.0.0.0: {snippet}"
        );
    }
}

#[test]
fn an_advertised_address_over_a_loopback_bind_is_still_a_lie() {
    // The one case that matters on a real machine: `advertise_url` written
    // before `bind` was changed. The card hands the address out, but the
    // listener is still on 127.0.0.1, so the block that fixes it must stay.
    let a2a = A2aConfig {
        advertise_url: Some("http://192.168.1.114:18790".to_string()),
        ..loopback()
    };
    let (url, _warning, snippet) = share_address(&a2a, None);

    assert_eq!(url, "http://192.168.1.114:18790");
    let snippet = snippet.expect("the bind still has to be opened");
    assert!(
        snippet.contains("bind ="),
        "the block is the thing that names the key: {snippet}"
    );
}
