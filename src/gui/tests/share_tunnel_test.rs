//! Tests for the share card's public tunnel (`crate::instance_strip`).
//!
//! The transitions are asserted on values: the button's state machine and
//! what `apply_tunnel` does to the card. Where the rule is about where the
//! work happens (only behind a click, off the UI thread, through the one
//! path `opencrabs connect` takes, stopped with the card), the source is
//! pinned, the way the other window guards pin theirs.

use std::path::{Path, PathBuf};

use crate::instance_strip::{
    SHARE_FOOTNOTE, ShareTunnel, ShareView, apply_tunnel, parse_pair_link, qr_modules, share_steps,
};

const STRIP: &str = include_str!("../instance_strip.rs");
const WIRING: &str = include_str!("../dashboard_instances.rs");
const RENDER: &str = include_str!("../dashboard_render.rs");
const APP: &str = include_str!("../app.rs");
const TUNNEL: &str = include_str!("../../remote/tunnel.rs");
const CONNECT: &str = include_str!("../../cli/connect.rs");

/// The text of `fn name` in `src`, from its signature to the next function
/// at method or module level.
fn fn_body(src: &str, name: &str) -> String {
    let head = format!("fn {name}(");
    let (_, rest) = src
        .split_once(head.as_str())
        .unwrap_or_else(|| panic!("no fn {name} in the source"));
    let mut body = String::new();
    for line in rest.lines() {
        let next_fn = line.starts_with("    pub(crate) fn ")
            || line.starts_with("    pub fn ")
            || line.starts_with("    fn ")
            || line.starts_with("pub(crate) fn ")
            || line.starts_with("pub fn ")
            || line.starts_with("fn ");
        if !body.is_empty() && next_fn {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    body
}

fn card() -> ShareView {
    ShareView::new(
        "http://127.0.0.1:18790".to_string(),
        "ABCD2345".to_string(),
        Some("lenovo"),
        Some("esta máquina só aceita ligações locais".to_string()),
    )
    .with_snippet(Some("[a2a]\nbind = \"0.0.0.0\"".to_string()))
}

#[test]
fn the_tunnel_button_walks_idle_starting_up_and_failed() {
    let mut state = ShareTunnel::Idle;
    assert!(state.click(), "a click on an idle card opens a tunnel");
    assert_eq!(state, ShareTunnel::Starting);
    assert!(
        !state.click(),
        "a second click while starting must not open a second tunnel"
    );
    assert_eq!(state, ShareTunnel::Starting);

    let mut share = card();
    share.tunnel = state;
    apply_tunnel(
        &mut share,
        "https://x.trycloudflare.com".to_string(),
        Some("lenovo"),
    );
    assert_eq!(
        share.tunnel,
        ShareTunnel::Up("https://x.trycloudflare.com".to_string())
    );
    assert!(!share.tunnel.click(), "an up tunnel is not opened again");

    let mut failed = ShareTunnel::Failed("cloudflared download failed".to_string());
    assert!(failed.click(), "a failure is retried by the same button");
    assert_eq!(failed, ShareTunnel::Starting);
}

#[test]
fn an_up_tunnel_repoints_the_link_and_the_qr_and_clears_the_bind_warnings() {
    let mut share = card();
    let code = share.code.clone();
    apply_tunnel(
        &mut share,
        "https://x.trycloudflare.com".to_string(),
        Some("lenovo"),
    );

    assert_eq!(share.url, "https://x.trycloudflare.com");
    assert_eq!(share.code, code, "only the address changes, never the code");
    let (url, parsed_code, label) = parse_pair_link(&share.link).expect("the link parses");
    assert_eq!(url, "https://x.trycloudflare.com");
    assert_eq!(parsed_code, code);
    assert_eq!(label.as_deref(), Some("lenovo"));
    assert_eq!(qr_modules(&share.link), (share.qr.clone(), share.width));
    assert!(share.width > 0, "a tunnel link still fits a QR");
    assert!(
        share.warning.is_none(),
        "a card that hands out a public URL may not say the machine is local-only"
    );
    assert!(
        share.snippet.is_none(),
        "the TOML block fixed a bind that no longer matters"
    );
}

#[test]
fn a_failed_tunnel_shows_its_error_and_keeps_the_direct_address() {
    let mut share = card();
    let before = share.clone();
    share.tunnel = ShareTunnel::Failed("no cloudflared asset pinned".to_string());
    assert_eq!(share.url, before.url);
    assert_eq!(share.link, before.link);

    let block = fn_body(STRIP, "tunnel_block");
    assert!(
        block.contains("ShareTunnel::Failed(why) => block"),
        "{block}"
    );
    assert!(block.contains("format!(\"túnel: {why}\")"), "{block}");
    assert!(block.contains("\"instance-share-tunnel-retry\""), "{block}");
    // The error is the tunnel's own message, whole.
    let open = fn_body(STRIP, "open_public_tunnel");
    assert!(open.contains("Err(e) => Err(format!(\"{e:#}\"))"), "{open}");
    let landed = fn_body(STRIP, "tunnel_landed");
    assert!(
        landed.contains("share.tunnel = ShareTunnel::Failed(why);"),
        "{landed}"
    );
}

#[test]
fn the_card_never_opens_a_tunnel_without_a_click() {
    // A fresh card starts idle: minting a code publishes nothing.
    assert_eq!(card().tunnel, ShareTunnel::Idle);
    let mint = fn_body(STRIP, "mint_share");
    for door in ["open_public_tunnel", "tunnel::open(", "tunnel::start("] {
        assert!(!mint.contains(door), "mint_share reaches {door}");
    }
    let share = fn_body(STRIP, "open_share");
    assert!(!share.contains("open_tunnel"), "{share}");

    // The tunnel opens in exactly one function, and that function is
    // reached from exactly one place: the click listener on the card.
    assert_eq!(STRIP.matches("tunnel::open(").count(), 1);
    assert!(fn_body(STRIP, "open_public_tunnel").contains("tunnel::open("));
    assert_eq!(STRIP.matches("open_public_tunnel(&a2a)").count(), 1);
    assert!(fn_body(STRIP, "open_tunnel").contains("open_public_tunnel(&a2a)"));
    assert_eq!(STRIP.matches("this.open_tunnel(cx)").count(), 1);
    assert!(STRIP.contains(
        "let open = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.open_tunnel(cx));"
    ));
    assert!(!STRIP.contains("tunnel::start("));
    assert!(!WIRING.contains("open_tunnel"));
}

#[test]
fn the_tunnel_opens_off_the_ui_thread() {
    let open = fn_body(STRIP, "open_tunnel");
    assert!(open.contains(".background_executor()"), "{open}");
    assert!(
        open.contains(".spawn(async move { open_public_tunnel(&a2a) })"),
        "{open}"
    );
    // No config read on the click: the gateway the card was minted against
    // is held from the mint, which already ran off the UI thread.
    assert!(!open.contains("Config::load"), "{open}");
    assert!(open.contains("self.share_gateway.clone()"), "{open}");
}

#[test]
fn the_tunnel_start_shares_one_path_with_connect() {
    // Both doors go through `tunnel::open`, which gates the gateway and
    // reuses a running tunnel before it starts one.
    assert!(CONNECT.contains("tunnel::open(&config.a2a, &spec)"));
    assert!(!CONNECT.contains("tunnel::start("));
    let open = fn_body(TUNNEL, "open");
    assert!(open.contains("gateway_gate(a2a)?;"), "{open}");
    assert!(
        open.contains("reusable(&spec.state_dir, spec.port)?"),
        "{open}"
    );
    assert!(open.contains("start(spec).map(Opened::Started)"), "{open}");

    // And cloudflared is spawned in exactly one place in the whole crate.
    // The flag is assembled so this file does not count as a site.
    let flag = ["\"--no-", "autoupdate\""].concat();
    let starts = ["tunnel", "::start("].concat();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut spawns = Vec::new();
    let mut start_callers = Vec::new();
    for file in rust_files(&src) {
        let rel = file
            .strip_prefix(&src)
            .map(|p| p.display().to_string().replace('\\', "/"))
            .unwrap_or_default();
        if is_test_file(&rel) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let code = text.split("#[cfg(test)]").next().unwrap_or("");
        if code.contains(&flag) {
            spawns.push(rel.clone());
        }
        if code.contains(&starts) {
            start_callers.push(rel);
        }
    }
    assert_eq!(spawns, vec!["remote/tunnel.rs".to_string()], "{spawns:?}");
    assert!(
        start_callers.is_empty(),
        "tunnel::start is reached only through tunnel::open: {start_callers:?}"
    );
    let start = fn_body(TUNNEL, "start");
    assert!(start.contains(&flag), "{start}");
}

fn is_test_file(rel: &str) -> bool {
    rel.starts_with("tests/")
        || rel.contains("/tests/")
        || rel.ends_with("_test.rs")
        || rel.ends_with("_tests.rs")
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}

#[test]
fn the_tunnel_the_card_started_is_stopped_with_the_card_and_the_app() {
    // Any card other than the share card (or its re-mint) releases it, and
    // closing is `set_card(None, ..)`.
    let set = fn_body(STRIP, "set_card");
    assert!(
        set.contains(
            "if !matches!(card, Some(StripCard::Share(_) | StripCard::Minting)) {\n            self.release_tunnel(cx);"
        ),
        "{set}"
    );
    assert!(fn_body(STRIP, "close_card").contains("self.set_card(None, cx);"));
    // The stop runs on the background executor, never on the UI thread.
    let release = fn_body(STRIP, "release_tunnel");
    assert!(
        release.contains("Self::stop_off_thread(owned, cx);"),
        "{release}"
    );
    let off = fn_body(STRIP, "stop_off_thread");
    assert!(off.contains(".background_executor()"), "{off}");
    assert!(off.contains("drop(owned)"), "{off}");
    // A tunnel that lands after its card closed is stopped, not adopted.
    let landed = fn_body(STRIP, "tunnel_landed");
    assert!(
        landed.contains("Self::stop_off_thread(owned, cx);"),
        "{landed}"
    );
    // Quitting stops it, and the guard's own drop is the backstop when the
    // strip is torn down with the window.
    let new = fn_body(STRIP, "new");
    assert!(new.contains("cx.on_app_quit("), "{new}");
    assert!(new.contains("drop(strip.owned_tunnel.take());"), "{new}");
    assert!(APP.contains("instance_strip::InstanceStrip::new(strip_owner, instance_card, cx)"));
    assert!(TUNNEL.contains("impl Drop for OwnedTunnel {"));
    let drop_body = fn_body(TUNNEL, "drop");
    assert!(
        drop_body.contains("stop_owned(&self.state_dir, self.pid)"),
        "{drop_body}"
    );
    // Only a tunnel this window started is owned: one left by `connect` is
    // reused and never stopped from here.
    let open = fn_body(STRIP, "open_public_tunnel");
    assert!(
        open.contains(
            "Ok(tunnel::Opened::Reused(state)) => Ok(PublicTunnel {\n            url: state.url,\n            owned: None,"
        ),
        "{open}"
    );
}

#[test]
fn nothing_renders_with_the_instance_rail_off() {
    // The card layer is mounted only behind the flag, and switching the flag
    // off closes the card, which releases the tunnel with it.
    assert!(RENDER.contains(
        "if self.instance_rail {\n            root = root.child(self.instance_strip_element(cx));"
    ));
    let set = fn_body(WIRING, "set_instance_rail");
    assert!(set.contains("strip.close_card(cx)"), "{set}");
    // The share card, and so its tunnel button, is reached only from the
    // strip's own cards, which exist only behind that mount.
    assert!(!WIRING.contains("open_share"));
    assert_eq!(
        STRIP
            .matches("cx.listener(|this, _ev: &ClickEvent, _window, cx| this.open_share(cx))")
            .count(),
        STRIP.matches("this.open_share(cx)").count()
    );
}

#[test]
fn the_card_prints_three_steps_naming_the_controls_on_the_other_crab() {
    let steps = share_steps();
    assert!(steps[0].contains("logótipo"), "{}", steps[0]);
    assert!(steps[0].contains("«emparelhar um crab»"), "{}", steps[0]);
    assert!(steps[0].contains("instance_rail"), "{}", steps[0]);
    assert!(steps[1].contains("link"), "{}", steps[1]);
    assert!(steps[2].contains("«emparelhar»"), "{}", steps[2]);
    // The names the steps promise are the names the other crab shows.
    let rail = include_str!("../rail_view.rs");
    assert!(rail.contains("SharedString::from(\"emparelhar um crab\")"));
    assert!(STRIP.contains("\"emparelhar\""));
    let guide = fn_body(STRIP, "share_guide");
    assert!(guide.contains("share_steps()"), "{guide}");
    assert!(guide.contains("SHARE_FOOTNOTE"), "{guide}");
    assert!(fn_body(STRIP, "share_body").contains("body.push(Self::share_guide());"));
}

#[test]
fn the_footnote_puts_the_direct_address_first_and_explains_the_tunnel() {
    assert!(
        SHARE_FOOTNOTE.contains("endereço direto"),
        "{SHARE_FOOTNOTE}"
    );
    assert!(
        SHARE_FOOTNOTE.contains("redes diferentes"),
        "{SHARE_FOOTNOTE}"
    );
    assert!(SHARE_FOOTNOTE.contains("muda"), "{SHARE_FOOTNOTE}");
    assert!(SHARE_FOOTNOTE.contains("host_id"), "{SHARE_FOOTNOTE}");
    assert!(SHARE_FOOTNOTE.contains("link colado"), "{SHARE_FOOTNOTE}");
    let direct = SHARE_FOOTNOTE.find("direto").unwrap_or(usize::MAX);
    let tunnel = SHARE_FOOTNOTE.find("túnel").unwrap_or(0);
    assert!(direct < tunnel, "direct first, tunnel second");
}
