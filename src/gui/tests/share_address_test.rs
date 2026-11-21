//! Tests for where the sharing card sends the other crab.
//!
//! Kept apart from `instances_wiring_test` because the rule under test is not
//! the card's shape: it is which address wins when the machine publishes more
//! than one, and that rule was wrong in a way only a real machine showed.

use crate::dashboard_instances::share_address;
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
    let a2a = A2aConfig {
        advertise_url: Some("http://192.168.1.114:18790".to_string()),
        ..loopback()
    };
    let (url, warning, snippet) = share_address(&a2a, None);

    assert_eq!(url, "http://192.168.1.114:18790");
    assert!(warning.is_none(), "the owner already said how to reach it");
    assert!(snippet.is_none());
}
