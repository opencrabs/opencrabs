//! Tests for the instance strip's wiring (`crate::dashboard_instances`).
//!
//! The network paths are not exercised here: what is asserted is the shape of
//! the state the window holds, because that is what the paint reads. The
//! client and the store carry their own tests next to the code that parses.

use crate::dashboard_instances::{
    PairingForm, RemoteState, RemoteView, ShareTunnel, ShareView, apply_tunnel, pair_link,
    parse_pair_link,
};
use opencrabs::config::A2aConfig;
use opencrabs::ui_core::text_field::TextField;

#[test]
fn a_busy_form_refuses_to_look_idle() {
    // `..Default::default()` on purpose: the form grows fields as the keyboard
    // work lands, and a literal here breaks the test build every time it does.
    let form = PairingForm {
        url: TextField::from_text("http://a:7878"),
        code: TextField::from_text("123456"),
        ..Default::default()
    }
    .busy();
    assert!(form.busy);
    assert_eq!(form.code.text(), "123456");
}

#[test]
fn a_failed_view_carries_the_error_text() {
    let view = RemoteView::failed("a:1".into(), "http://a:1".into(), "timed out".into());
    assert_eq!(view.host_id, "a:1");
    assert!(matches!(view.state, RemoteState::Failed(ref e) if e == "timed out"));
}

#[test]
fn a_loading_view_is_not_ready() {
    let view = RemoteView::loading("a:1".into());
    assert!(matches!(view.state, RemoteState::Loading));
}

#[test]
fn a_pair_link_survives_the_round_trip() {
    // The link is the wire format between two machines: it is built here and
    // read there, so the only thing worth asserting is that what goes in comes
    // back out. The address carries a scheme, a port and a host, all of which
    // need escaping, and the label is the owner's own words.
    use crate::dashboard_instances::{parse_pair_link, percent_encode};
    let url = "http://192.168.1.114:18790";
    let code = "K7F2MQ9P";
    let label = "portátil do norte";
    let link = format!(
        "opencrabs://pair?url={}&code={}&label={}",
        percent_encode(url),
        percent_encode(code),
        percent_encode(label),
    );
    let (got_url, got_code, got_label) = parse_pair_link(&link).expect("the link parses");
    assert_eq!(got_url, url);
    assert_eq!(got_code, code);
    assert_eq!(got_label.as_deref(), Some(label));
}

#[test]
fn a_link_without_a_code_is_not_a_link() {
    // Half a link is worse than none: it would fill the form with an address
    // and leave the code empty, and the submit would fail with a message about
    // the code rather than about what was pasted.
    use crate::dashboard_instances::parse_pair_link;
    assert!(parse_pair_link("opencrabs://pair?url=http%3A%2F%2Fa").is_none());
    assert!(parse_pair_link("https://example.com").is_none());
    assert!(parse_pair_link("").is_none());
}

#[test]
fn the_card_prints_the_steps_the_other_crab_actually_has() {
    // The card's whole job is to tell the owner what to do on the far side. The
    // steps are pinned here because a menu label that changes on one side and
    // not on the other turns the card into a wrong instruction, which is worse
    // than an empty card: the owner follows it and fails.
    use crate::dashboard_instances::share_steps;
    let steps = share_steps();
    assert_eq!(
        steps.len(),
        3,
        "three steps, one per action on the far side"
    );
    // Step one names the menu row the other crab's mark really offers.
    assert!(
        steps[0].contains("emparelhar um crab"),
        "step 1 must name the menu row: {}",
        steps[0]
    );
    assert!(
        steps[0].contains("botão direito"),
        "step 1 must say how the menu opens: {}",
        steps[0]
    );
    // Step two is the paste, which is what makes the code usable without typing.
    assert!(
        steps[1].contains("cola o link"),
        "step 2 must be the paste: {}",
        steps[1]
    );
    // Step three ends on the button that submits the form.
    assert!(
        steps[2].contains("emparelhar"),
        "step 3 must end on the submit: {}",
        steps[2]
    );
}

#[test]
fn the_card_says_what_it_cannot_do() {
    // Two expectations the card must kill before they are formed: that the QR
    // is scannable by the other desktop (it is not, there is no reader), and
    // that a crab on the same network shows up on its own (it does not, the
    // discovery pass does not exist yet). Both are one line here and an hour of
    // confusion in the owner's head.
    use crate::dashboard_instances::SHARE_FOOTNOTE;
    assert!(
        SHARE_FOOTNOTE.contains("link colado"),
        "the footnote must point at the link as the desktop path"
    );
    assert!(
        SHARE_FOOTNOTE.contains("descoberta"),
        "the footnote must say discovery does not exist yet"
    );
}

#[test]
fn a_working_config_gets_no_snippet() {
    // A card on a machine that is already reachable must not carry a config
    // block: showing one would suggest something is broken when nothing is, and
    // the owner would go edit a file that is already right.
    use crate::dashboard_instances::share_config_snippet;
    let a2a = A2aConfig {
        enabled: true,
        bind: "0.0.0.0".to_string(),
        port: 18790,
        allowed_origins: vec![],
        advertise_url: Some("https://crab.example".to_string()),
        api_key: Some("uma-chave-longa".to_string()),
    };
    assert_eq!(share_config_snippet(&a2a), None);
}

#[test]
fn a_loopback_crab_gets_the_block_that_opens_it() {
    // The common case on this machine: enabled, bound to loopback, no key. The
    // snippet has to carry all three fixes, because fixing one and not the
    // others leaves the card warning about the next one and the owner looping.
    use crate::dashboard_instances::share_config_snippet;
    let a2a = A2aConfig {
        enabled: false,
        bind: "127.0.0.1".to_string(),
        port: 18790,
        allowed_origins: vec![],
        advertise_url: None,
        api_key: None,
    };
    let snippet = share_config_snippet(&a2a).expect("a loopback crab needs the block");
    assert!(
        snippet.starts_with("[a2a]"),
        "section header first: {snippet}"
    );
    assert!(
        snippet.contains("enabled = true"),
        "turn the gateway on: {snippet}"
    );
    assert!(
        snippet.contains("bind = \"0.0.0.0\""),
        "open the bind: {snippet}"
    );
    assert!(
        snippet.contains("advertise_url = \"http://"),
        "name the address: {snippet}"
    );
    assert!(
        snippet.contains(":18790"),
        "the port comes from the config: {snippet}"
    );
    assert!(
        snippet.contains("api_key ="),
        "no key means no auth: {snippet}"
    );
}

#[test]
fn the_snippet_leaves_what_is_already_right_alone() {
    // A machine that is open to the network but has no key needs one line, not
    // a rewritten block. Rewriting keys the owner already set is how a snippet
    // becomes a footgun: pasting it would silently reset their address.
    use crate::dashboard_instances::share_config_snippet;
    let a2a = A2aConfig {
        enabled: true,
        bind: "0.0.0.0".to_string(),
        port: 9000,
        allowed_origins: vec![],
        advertise_url: Some("https://crab.example".to_string()),
        api_key: None,
    };
    let snippet = share_config_snippet(&a2a).expect("no key is still a problem");
    assert!(!snippet.contains("enabled = true"), "already on: {snippet}");
    assert!(!snippet.contains("bind ="), "already open: {snippet}");
    assert!(
        !snippet.contains("advertise_url"),
        "already named: {snippet}"
    );
    assert!(
        snippet.contains("api_key ="),
        "the one missing key: {snippet}"
    );
}

/// GPUI only offers `overflow_y_scroll` on `StatefulInteractiveElement`, which
/// is `Stateful<Div>`: a scroll container without an id does not compile at
/// all. That cost a clippy cycle on 2026-11-21, and the same trap is already
/// documented in `topic_tabs.rs`. Scanning the source catches the class, not
/// just the one line, because the next card is where it comes back.
#[test]
fn every_scroll_container_in_the_wiring_carries_an_id() {
    // Both panels: the dashboard cards and the remote transcript, which is the
    // one that shipped clipped because nothing scanned it.
    for (name, source) in [
        (
            "dashboard_instances.rs",
            include_str!("../dashboard_instances.rs"),
        ),
        ("remote_panel.rs", include_str!("../remote_panel.rs")),
    ] {
        check_scroll_ids(name, source);
    }
}

fn check_scroll_ids(name: &str, source: &str) {
    let lines: Vec<&str> = source.lines().collect();
    let mut checked = 0;
    for (i, line) in lines.iter().enumerate() {
        if !line.contains(".overflow_y_scroll()") && !line.contains(".overflow_scroll()") {
            continue;
        }
        // The id sits above in the same builder chain. Look back far enough to
        // cover a comment block between the two, which is how this one is
        // written: the reason is documented right where it is needed.
        let window = &lines[i.saturating_sub(12)..i];
        assert!(
            window.iter().any(|l| l.contains(".id(")),
            "{name}: scroll container at line {} carries no id, so the method \
             cannot resolve: {}",
            i + 1,
            line.trim()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "{name}: the scan found no scroll container to check"
    );
}

/// A card as it comes out on a machine whose gateway only listens on loopback:
/// a code, a link, and the two blocks that explain why the address is useless
/// from anywhere else.
fn card_on_loopback() -> ShareView {
    let code = "K7QP2M4X".to_string();
    ShareView {
        url: "http://127.0.0.1:18790".into(),
        link: pair_link("http://127.0.0.1:18790", &code, "este pc"),
        code,
        qr: Vec::new(),
        width: 0,
        warning: Some("esta máquina só aceita ligações locais".into()),
        snippet: Some("[a2a]\nbind = \"0.0.0.0\"".into()),
        tunnel: ShareTunnel::Idle,
        ttl_secs: 300,
    }
}

#[test]
fn a_tunnel_repoints_the_card_at_the_public_url() {
    let mut card = card_on_loopback();
    apply_tunnel(&mut card, "https://x.trycloudflare.com".into(), "este pc");

    assert_eq!(card.url, "https://x.trycloudflare.com");
    assert!(matches!(card.tunnel, ShareTunnel::Up(ref u) if u == "https://x.trycloudflare.com"));
    // The warning and the TOML block described an address that is no longer
    // the one being handed out. Leaving them up would be two lies on one card.
    assert!(card.warning.is_none());
    assert!(card.snippet.is_none());
    assert!(card.width > 0, "the QR is rebuilt around the new link");

    let parsed = parse_pair_link(&card.link).expect("the rebuilt link parses");
    assert_eq!(parsed.0, "https://x.trycloudflare.com");
    assert_eq!(parsed.1, "K7QP2M4X");
    assert_eq!(parsed.2.as_deref(), Some("este pc"));
}

#[test]
fn a_failed_tunnel_keeps_the_bind_address_on_screen() {
    // A tunnel that could not open must not blank the card: the bind address
    // and its warning are still the only true thing it has.
    let mut card = card_on_loopback();
    card.tunnel = ShareTunnel::Failed("cloudflared download failed".into());
    assert_eq!(card.url, "http://127.0.0.1:18790");
    assert!(card.warning.is_some());
}

/// The panel's "desligar" must close the view, not the pairing.
///
/// A code is minted on the far machine and typed in on this one. Losing it to a
/// button that reads like "close this" is the kind of loss the user only
/// notices when the other crab is off and cannot mint another. So the two acts
/// have two controls, and this pins which one the panel's button reaches.
#[test]
fn disconnecting_closes_the_view_and_keeps_the_pairing() {
    let source = include_str!("../dashboard_instances.rs");

    let close = fn_body(source, "pub(crate) fn instance_disconnect")
        .expect("instance_disconnect is where the panel's close lands");
    assert!(
        !close.contains("InstanceStore") && !close.contains(".remove("),
        "desligar must not touch the store: {close}"
    );
    assert!(
        close.contains("instance_remote = None"),
        "it must close the remote view: {close}"
    );

    // The button's listener is wired to it, and nothing else.
    // The listener that reaches the button: from its own `let close` to the
    // call that hands it to the panel.
    let before = source
        .split("remote_panel::view(")
        .next()
        .expect("the panel is drawn somewhere");
    let head = before
        .rsplit("let close = cx.listener")
        .next()
        .unwrap_or(before);
    assert!(
        head.contains("this.instance_disconnect(cx)"),
        "the close button must reach instance_disconnect: {head}"
    );
    assert!(
        !head.contains("this.instance_remove("),
        "the close button must not forget the crab: {head}"
    );
}

/// Forgetting a crab is a right-click on its badge, and the menu says its name.
#[test]
fn a_badge_right_click_opens_a_menu_that_names_the_crab() {
    let strip = include_str!("../instance_strip.rs");
    assert!(
        strip.contains("instance_badge_menu("),
        "the badge must open its own menu"
    );
    assert!(
        strip.contains(".on_mouse_down(MouseButton::Right, ask)"),
        "a right press is what opens it"
    );

    let dash = include_str!("../dashboard_instances.rs");
    let items = fn_body(dash, "fn instance_badge_menu_items")
        .expect("the badge's rows are built somewhere");
    assert!(
        items.contains("esquecer {label}"),
        "the destructive row must carry the crab's name: {items}"
    );
    assert!(
        items.contains("instance_remove(&forget_host"),
        "and it must be the row that forgets: {items}"
    );
    assert!(
        items.contains("menu_item(\"abrir\")"),
        "with a harmless row above it, so the pointer does not land on the \
         destructive one: {items}"
    );
}

/// The body of one `fn`, from its signature to the next item at the same
/// indent. Enough to assert on what a function does without a parser.
fn fn_body<'a>(source: &'a str, signature: &str) -> Option<&'a str> {
    let start = source.find(signature)?;
    let rest = &source[start..];
    let end = rest[1..]
        .find("\n    fn ")
        .or_else(|| rest[1..].find("\n    pub(crate) fn "))
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    Some(&rest[..end])
}
