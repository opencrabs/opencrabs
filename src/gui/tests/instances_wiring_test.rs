//! Tests for the instance rail's wiring (`crate::dashboard_instances`,
//! `crate::instance_strip`, `Backend::remote`).
//!
//! What a pick does is asserted on the values the window holds: the backend
//! it builds, the frame the strip paints. Where the rule is about where the
//! work runs (off the UI thread, on the remote client's runtime), the source
//! is pinned, the way the other window guards pin theirs.

use std::path::PathBuf;

use opencrabs::backend::hello::Hello;
use opencrabs::backend::{BackendError, Op, cap};
use opencrabs::remote::instance_store::{InstanceFile, KnownInstance};

use opencrabs::config::A2aConfig;
use opencrabs::ui_core::text_field::TextField;

use crate::backend_glue::Backend;
use crate::instance_strip::{
    PairingForm, ShareView, StripFrame, pair_link, parse_pair_link, qr_modules, share_target,
};
use crate::instances::{Health, Instances};

const WIRING: &str = include_str!("../dashboard_instances.rs");
const STRIP: &str = include_str!("../instance_strip.rs");
const APP: &str = include_str!("../app.rs");
const RENDER: &str = include_str!("../dashboard_render.rs");
const RAIL: &str = include_str!("../dashboard_rail.rs");
const PALETTE: &str = include_str!("../command_palette.rs");
const KEYS: &str = include_str!("../dashboard_keys.rs");
const CONFIG_RELOAD: &str = include_str!("../dashboard_config.rs");
const SETTINGS: &str = include_str!("../dashboard_settings.rs");

fn known(id: &str, label: &str, url: &str) -> KnownInstance {
    KnownInstance {
        host_id: id.to_string(),
        label: label.to_string(),
        url: url.to_string(),
        token: "ocpair_test".to_string(),
        added_at: 100,
        last_ok: 0,
        glyph: None,
        routes: Vec::new(),
    }
}

fn two() -> Instances {
    Instances::from_file(&InstanceFile {
        version: 1,
        instances: vec![
            known("a:1", "alpha", "http://a:7878"),
            known("b:2", "beta", "http://b:7878"),
        ],
    })
}

/// The text of `fn name` in `src`, from its signature to the next method of
/// the same impl. Enough to pin what one function calls. Split on lines, not
/// sliced at byte offsets.
fn fn_body(src: &str, name: &str) -> String {
    let head = format!("fn {name}(");
    let (_, rest) = src
        .split_once(head.as_str())
        .unwrap_or_else(|| panic!("no fn {name} in the source"));
    let mut body = String::new();
    for line in rest.lines() {
        let next_method = line.starts_with("    pub(crate) fn ") || line.starts_with("    fn ");
        if !body.is_empty() && next_method {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    body
}

/// The window's own ledger a remote backend carries.
fn window_ledger(backend: &Backend) -> PathBuf {
    match backend {
        Backend::Remote(crab) => crab.window_ledger().to_path_buf(),
        Backend::Local(_) => panic!("a local backend has no window ledger of its own"),
    }
}

#[test]
fn a_remote_backend_keeps_the_window_ledger_of_home() {
    let ledger = PathBuf::from("/tmp/window-ledger.db");
    let home = Backend::local(ledger.clone());
    let remote = Backend::remote(&known("a:1", "alpha", "http://a:7878"), &home);
    assert!(matches!(remote, Backend::Remote(_)));
    // What belongs to the window (dismissed hints) stays on this machine's
    // ledger whichever crab the window looks at, and a second hop keeps it.
    assert_eq!(window_ledger(&remote), ledger);
    let again = Backend::remote(&known("b:2", "beta", "http://b:7878"), &remote);
    assert_eq!(window_ledger(&again), ledger);
}

#[test]
fn a_picked_crab_that_is_not_there_answers_the_handshake_with_its_name() {
    // Port 9 on loopback: nothing listens, the connect is refused. The pick
    // must not hang the window on it, and the notice names the crab.
    let home = Backend::local(PathBuf::from("unused.db"));
    let remote = Backend::remote(&known("g:9", "gone", "http://127.0.0.1:9"), &home);
    let err = futures::executor::block_on(remote.call(Hello {})).expect_err("nothing listens");
    assert!(matches!(err, BackendError::Failed(_)), "{err:?}");
    let notice = cap::notice(<Hello as Op>::NAME, &err);
    assert!(notice.starts_with("gone: hello.handshake: "), "{notice}");
}

#[test]
fn the_frame_follows_the_model_in_store_order() {
    let mut set = two();
    set.record_probe("b:2", Ok(4), 10);
    assert!(set.select("b:2"));
    let frame = StripFrame::from_model(&set, 10, None);
    let ids: Vec<&str> = frame.badges.iter().map(|b| b.host_id.as_str()).collect();
    assert_eq!(ids, ["a:1", "b:2"]);
    assert_eq!(frame.selected.as_deref(), Some("b:2"));
    assert_eq!(frame.badges[1].health, Health::Up { latency_ms: 4 });
    assert!(frame.badges[1].title.contains("4 ms"));
    assert_eq!(frame.error, None);
}

#[test]
fn an_unchanged_model_hands_the_strip_an_equal_frame() {
    // The cached view replays on an equal frame: a keystroke elsewhere must
    // not rebuild the badges.
    let set = two();
    assert_eq!(
        StripFrame::from_model(&set, 10, None),
        StripFrame::from_model(&set, 10, None)
    );
    assert_ne!(
        StripFrame::from_model(&set, 10, None),
        StripFrame::from_model(&set, 10, Some("x".into()))
    );
}

#[test]
fn a_pick_swaps_the_backend_and_reloads_off_the_ui_thread() {
    let picked = fn_body(WIRING, "instance_picked");
    assert!(picked.contains("Backend::remote(&entry.known, &self.home_backend)"));
    assert!(picked.contains("self.instance_switch(backend, cx)"));
    assert!(
        picked.contains("self.instance_clear(cx)"),
        "a second click goes home"
    );

    let home = fn_body(WIRING, "instance_clear");
    assert!(home.contains("self.home_backend.clone()"));

    let switch = fn_body(WIRING, "instance_switch");
    assert!(switch.contains("self.backend = backend;"));
    assert!(switch.contains("self.hello = None;"));
    assert!(switch.contains("self.instance_epoch"));
    assert!(switch.contains("self.refresh_rail(cx);"));
    assert!(switch.contains("self.fetch_hello(cx);"));
    // The reload is the rail's own refetch, which awaits the backend in a
    // spawned task: nothing in the swap itself touches the network.
    assert!(!switch.contains(".call("));
    assert!(!switch.contains("block_on"));
    let refetch = fn_body(RAIL, "refresh_rail");
    assert!(refetch.contains("cx.spawn("));
    assert!(refetch.contains("self.backend.clone()"));
}

#[test]
fn the_handshake_runs_off_the_ui_thread_and_drops_a_stale_answer() {
    let hello = fn_body(WIRING, "fetch_hello");
    assert!(hello.contains("cx.spawn("));
    assert!(hello.contains("backend.call(Hello {}).await"));
    assert!(hello.contains("this.instance_epoch != epoch"));
    assert!(hello.contains("cap::notice("));
}

#[test]
fn the_probes_run_on_the_remote_runtime_in_the_background() {
    // gpui's executor is not a Tokio runtime and `reqwest` panics outside
    // one, so the probes go through the remote client's shared runtime, on a
    // background thread.
    let probe = fn_body(WIRING, "spawn_instance_probe");
    assert!(probe.contains("background_executor()"));
    assert!(probe.contains("instance_client::block_on("));
    assert!(probe.contains("instance_client::probe(&target)"));
}

#[test]
fn the_window_mounts_and_starts_the_strip() {
    assert!(RENDER.contains("self.instance_strip_element(cx)"));
    assert!(APP.contains("this.load_instances(cx);"));
    assert!(APP.contains("this.fetch_hello(cx);"));
    assert!(APP.contains("Dashboard::spawn_instance_probe(cx);"));
    assert!(APP.contains("home_backend: backend.clone(),"));
    assert!(STRIP.contains("dash.instance_picked("));
    assert!(STRIP.contains("dash.instance_clear(cx)"));
}

#[test]
fn the_rail_follows_the_backend_and_not_a_panel_of_its_own() {
    // The retired design drew a second rail and a panel for the picked crab.
    // The names are assembled so this guard does not count as a site itself.
    for gone in [
        ["remote", "_panel"].concat(),
        ["remote", "_rail_rows"].concat(),
        ["tasks", "/get"].concat(),
    ] {
        for (file, src) in [
            ("dashboard_instances.rs", WIRING),
            ("instance_strip.rs", STRIP),
        ] {
            assert!(!src.contains(&gone), "{file} names {gone}");
        }
    }
}

#[test]
fn the_menus_read_the_handshake_of_the_crab_the_window_holds() {
    assert!(PALETTE.contains("let printed = rows_gated(state, hello);"));
    assert!(RENDER.contains("hello: self.hello.as_ref(),"));
    let menu = fn_body(RAIL, "rail_menu_popover_for");
    assert!(menu.contains("kind.unavailable(hello)"));
    assert!(menu.contains("crate::menu::menu_item_gated("));
}

// ── The `+` card: pairing form and share QR ────────────────────────────────

#[test]
fn a_pairing_link_reads_back_what_it_carries() {
    let link = pair_link("http://10.0.0.2:18790", "ABCD2345", Some("servidor i7 ç"));
    assert!(link.starts_with("opencrabs://pair?url="), "{link}");
    let (url, code, label) = parse_pair_link(&link).expect("our own link parses");
    assert_eq!(url, "http://10.0.0.2:18790");
    assert_eq!(code, "ABCD2345");
    assert_eq!(label.as_deref(), Some("servidor i7 ç"));

    // No name: the link carries none, and the far side uses the agent card's.
    let bare = pair_link("http://10.0.0.2:18790", "ABCD2345", None);
    assert!(!bare.contains("label="), "{bare}");
    let (_, _, label) = parse_pair_link(&bare).expect("a link without a name parses");
    assert_eq!(label, None);
}

#[test]
fn a_pasted_link_fills_the_form_and_anything_else_does_not() {
    let mut form = PairingForm::default();
    assert!(parse_pair_link("http://10.0.0.2:18790").is_none());
    assert!(!form.absorb_link("http://10.0.0.2:18790"));
    assert!(form.url.is_empty() && form.code.is_empty());
    assert_eq!(form.field, 0);

    let link = pair_link("http://crab.lan:18790", "WXYZ6789", Some("lenovo"));
    assert!(form.absorb_link(&format!("  {link}\n")));
    assert_eq!(form.url.text(), "http://crab.lan:18790");
    assert_eq!(form.code.text(), "WXYZ6789");
    assert_eq!(form.label.text(), "lenovo");
    assert_eq!(form.field, 2, "the caret lands on the one field left");
}

#[test]
fn the_pairing_code_never_prints_in_debug() {
    let form = PairingForm {
        url: TextField::from_text("http://crab.lan:18790"),
        code: TextField::from_text("SECRET99"),
        ..PairingForm::default()
    };
    let printed = format!("{form:?}");
    assert!(!printed.contains("SECRET99"), "{printed}");
    assert!(printed.contains("<8 chars>"), "{printed}");

    let share = ShareView::new(
        "http://crab.lan:18790".to_string(),
        "SECRET99".to_string(),
        Some("lenovo"),
        None,
    );
    let printed = format!("{share:?}");
    assert!(!printed.contains("SECRET99"), "{printed}");
    assert!(
        share.link.contains("SECRET99"),
        "the link itself carries it"
    );
}

#[test]
fn the_share_card_draws_a_square_qr_of_its_link() {
    let share = ShareView::new(
        "http://crab.lan:18790".to_string(),
        "ABCD2345".to_string(),
        Some("lenovo"),
        None,
    );
    assert!(share.width > 0, "a pairing link fits a QR");
    assert_eq!(share.qr.len(), share.width * share.width);
    assert_eq!(qr_modules(&share.link), (share.qr.clone(), share.width));
}

#[test]
fn the_share_address_says_when_it_cannot_be_reached() {
    let off = A2aConfig {
        enabled: false,
        ..A2aConfig::default()
    };
    let (url, warning) = share_target(&off);
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert!(warning.is_some_and(|w| w.contains("enabled = true")));

    let advertised = A2aConfig {
        enabled: true,
        advertise_url: Some(" http://crab.lan:18790/ ".to_string()),
        ..A2aConfig::default()
    };
    assert_eq!(
        share_target(&advertised),
        ("http://crab.lan:18790".to_string(), None)
    );

    let loopback = A2aConfig {
        enabled: true,
        bind: "127.0.0.1".to_string(),
        advertise_url: None,
        ..A2aConfig::default()
    };
    let (url, warning) = share_target(&loopback);
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert!(warning.is_some_and(|w| w.contains("bind")));
}

#[test]
fn the_strip_is_drawn_only_behind_the_remote_crabs_flag() {
    // Off by default: a single-crab install must not spend a column on it.
    // The only way out of the function without a strip is the flag check.
    let element = fn_body(WIRING, "instance_strip_element");
    assert_eq!(element.matches("return None").count(), 1, "{element}");
    assert!(
        element.contains("if !self.remote_crabs {\n            return None;"),
        "{element}"
    );
    assert!(APP.contains("remote_crabs: Config::current().gui.remote_crabs,"));
    // While off no paired crab is probed: the loop's target list is empty.
    let probe = fn_body(WIRING, "spawn_instance_probe");
    assert!(
        probe.contains("if !this.remote_crabs {\n                        return Vec::new();"),
        "{probe}"
    );
    // Switching off while a remote crab is held goes home first, so a
    // hidden strip never leaves the window talking to a crab it cannot see.
    let set = fn_body(WIRING, "set_remote_crabs");
    assert!(set.contains("self.instance_clear(cx);"), "{set}");
    // The switch is this window's: it writes this machine's file, and a
    // reload of that file folds into the window whichever crab is held.
    let toggle = fn_body(WIRING, "toggle_remote_crabs");
    assert!(
        toggle.contains("Config::write_key(\"gui\", \"remote_crabs\""),
        "{toggle}"
    );
    assert!(CONFIG_RELOAD.contains("self.set_remote_crabs(config.gui.remote_crabs, cx);"));
    assert!(SETTINGS.contains("body.push(self.remote_crabs_row(cx));"));
    // Once on, the `+` that pairs the first crab is always there.
    assert!(STRIP.contains("children.push(self.add_mark(cx));"));
    assert!(STRIP.contains("this.open_pair(cx)"));
    // The card is painted over the window from the strip, not in its 56 px.
    let overlay = fn_body(STRIP, "card_overlay");
    assert!(overlay.contains("deferred("));
    assert!(overlay.contains("modal_shell::render("));
}

#[test]
fn pairing_and_minting_run_off_the_ui_thread() {
    let submit = fn_body(STRIP, "pair_submit");
    assert!(submit.contains("background_executor()"));
    assert!(submit.contains("pair_and_store(&url, &code, &label)"));
    assert!(submit.contains("dash.instance_paired(&file, &host_id, cx)"));
    assert!(STRIP.contains("instance_client::block_on(instance_store::pair(url, code, label))"));

    let share = fn_body(STRIP, "open_share");
    assert!(share.contains("background_executor()"));
    assert!(share.contains("mint_share()"));
    assert!(share.contains("strip.card_epoch != epoch"));

    let paired = fn_body(WIRING, "instance_paired");
    assert!(paired.contains("Instances::from_file(file)"));
    assert!(paired.contains("self.instance_picked(host_id, cx);"));
}

#[test]
fn the_card_owns_the_keyboard_before_the_seeker_and_the_composer() {
    let flat: String = KEYS.split_whitespace().collect::<Vec<_>>().join(" ");
    let card = flat
        .find("self.instance_card_key(&ev.keystroke, cx);")
        .expect("the card answers keys in on_key");
    let seeker = flat
        .find("self.finder_key(ks, cx)")
        .expect("the Seeker is in on_key");
    assert!(card < seeker, "the card reads keys first");
}

/// A paired crab must be removable from the window. The class pinned here is
/// "the strip can add a crab but never drop one": the right-click on a badge
/// opens the card, the card's forget rewrites the store off the UI thread, and
/// the window goes home first when the crab being forgotten is the open one.
#[test]
fn a_badge_right_click_forgets_its_crab_off_the_ui_thread() {
    let badge = fn_body(STRIP, "badge");
    assert!(
        badge.contains("on_mouse_down(MouseButton::Right, ask)"),
        "{badge}"
    );
    assert!(
        badge.contains("this.open_manage(&manage_badge, cx)"),
        "{badge}"
    );

    let card = fn_body(STRIP, "manage_body");
    assert!(
        card.contains("format!(\"esquecer {}\", view.label)"),
        "{card}"
    );
    assert!(card.contains("this.manage_open(cx)"), "{card}");
    assert!(card.contains("this.manage_forget(cx)"), "{card}");

    let forget = fn_body(STRIP, "manage_forget");
    assert!(forget.contains("background_executor()"), "{forget}");
    assert!(forget.contains("forget_instance(&forget)"), "{forget}");
    assert!(
        forget.contains("dash.instance_forgotten(&file, &host_id, cx)"),
        "{forget}"
    );
    assert!(STRIP.contains("store.remove(host_id)?;"));

    let forgotten = fn_body(WIRING, "instance_forgotten");
    let home = forgotten
        .find("self.instance_clear(cx);")
        .expect("forgetting the open crab goes home");
    let reload = forgotten
        .find("Instances::from_file(file)")
        .expect("the strip reloads from the store");
    assert!(
        home < reload,
        "home first, so no screen keeps the dropped token"
    );
}
