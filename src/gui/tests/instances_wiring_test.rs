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
    // The dashboard's own cards.
    check_scroll_ids(
        "dashboard_instances.rs",
        include_str!("../dashboard_instances.rs"),
    );

    // The remote panel has no scroll container of its own any more: its body is
    // `transcript::view`, the window's own painter, and that painter owns the
    // scrolling column. What the panel must keep is the handle it hands over,
    // because a handle that stops travelling is the original bug coming back:
    // a 40-row session painted into a box nothing can move. Scanning for
    // `.overflow_y_scroll()` here would now only find nothing and fail, which
    // is what this guard did the moment the panel started delegating
    // (2026-11-21).
    let panel = include_str!("../remote_panel.rs");
    let view = fn_body(panel, "pub(crate) fn view(").expect("the panel has a view");
    assert!(
        view.contains("&ScrollHandle"),
        "the remote panel must take the transcript's scroll handle: {view}"
    );
    assert!(
        view.contains("transcript::view("),
        "the body must be the window's own transcript painter: {view}"
    );
    assert!(
        view.contains("scroll,"),
        "the handle must reach the painter, or the body cannot move: {view}"
    );

    // And the body both views share has to stay scrollable with the id the
    // method needs, since that is the container the remote panel relies on now.
    let body = include_str!("../transcript.rs");
    assert!(
        body.contains(".overflow_y_scroll()"),
        "the shared transcript body must scroll"
    );
    assert!(
        body.contains(".id(\"transcript\")"),
        "and carry the id `overflow_y_scroll` needs to resolve"
    );
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
#[test]
fn the_transcript_is_built_off_the_ui_thread() {
    // Building a session is a markdown parse per row, and a real one is 216 KB
    // with rows over 10 KB. That build used to run inside `this.update`, on the
    // UI thread, right before the repaint: the window froze for the whole parse
    // and the switch looked like it was waiting on the network, when the
    // network was answering in 17-55 ms (measured 21 Nov, P4 in
    // perf-switch.md). The build must sit between the executor spawn and the
    // closure that only installs the finished state.
    let source = include_str!("../dashboard_instances.rs");
    let body = fn_body(source, "pub(crate) fn open_remote_session")
        .expect("open_remote_session is where a switch fetches and builds");
    let spawn_at = body
        .find(".background_executor()")
        .expect("the fetch runs on the background executor");
    let build_at = body
        .find("remote_panel::state_for")
        .expect("the turns are built somewhere in this path");
    let update_at = body
        .find("this.update(cx")
        .expect("the finished state is installed on the window");
    assert!(
        spawn_at < build_at && build_at < update_at,
        "state_for must run between the executor spawn and the update closure, \
         or the parse lands on the UI thread: spawn {spawn_at}, build {build_at}, \
         update {update_at}"
    );
}

fn fn_body<'a>(source: &'a str, signature: &str) -> Option<&'a str> {
    let start = source.find(signature)?;
    // `get` instead of `&source[start..]`: a byte index is only safe while it
    // happens to sit on a char boundary, and this helper is used by every test
    // in the file, so a panic here would read as a broken test rather than a
    // broken helper (2026-11-21).
    let rest = source.get(start..)?;
    // The NEAREST following item ends the body, whichever way it is written.
    // This used to try `\n    fn ` first and only fall back to `pub(crate) fn`
    // when that missed, so a private fn anywhere later in the file widened the
    // window: `instance_disconnect` came back with `instance_glyph_begin` and
    // more glued on, and an assert that the close button never touches the
    // store failed on an `InstanceStore` from three functions away. A helper
    // that silently returns a superset makes every test built on it lie
    // (2026-11-21).
    let end = ["\n    fn ", "\n    pub(crate) fn ", "\n    pub fn "]
        .iter()
        .filter_map(|marker| rest.get(1..)?.find(marker))
        .min()
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    rest.get(..end)
}
#[test]
fn the_workspace_mark_is_the_way_home() {
    // The owner's report: clicking the mark did nothing, and the only way back
    // to his own crab was toggling a badge off. The mark carried a right-click
    // menu and no left click at all, so the one control that names "my crab"
    // was the one that could not select it (#3133). A source test is the right
    // shape here: the wiring is a gpui listener, and what can regress is the
    // listener going away while the element still paints.
    let source = include_str!("../instance_strip.rs");
    let body =
        fn_body(source, "fn workspace_mark").expect("workspace_mark is the mark the owner clicks");
    assert!(
        body.contains(".on_click(home)"),
        "the mark must answer a left click: it is the only way home from a \
         remote crab, and a right-click menu cannot be discovered"
    );
    assert!(
        body.contains("this.clear(cx)"),
        "the click handler must clear the remote, which is what selects this \
         machine's own crab again"
    );
}

#[test]
fn clearing_the_remote_keeps_the_pairing() {
    // Two different acts share a neighbourhood here and must never share a
    // control: `clear` closes the view and returns home, `instance_remove`
    // forgets the crab (and costs a fresh code to get back). The mark is the
    // first one only.
    let source = include_str!("../instance_strip.rs");
    let body = fn_body(source, "fn clear").expect("clear is the mark's handler");
    assert!(
        body.contains("instance_clear"),
        "closing the view must go through instance_clear, which keeps the \
         pairing: forgetting lives in the badge's right-click menu"
    );
    assert!(
        !body.contains("instance_remove"),
        "the way home must never forget the crab: a code is a fresh round trip \
         to the other machine"
    );
}

#[test]
fn the_mark_and_the_badges_are_rounded_squares() {
    // A circle in a column of squares read as a different widget from the rail
    // it sits in. The badge's own box is the square; the health dot and the
    // selection bar inside it stay round, because they are a dot and a bar.
    let source = include_str!("../instance_strip.rs");
    let mark = fn_body(source, "fn workspace_mark").expect("workspace_mark exists");
    assert!(
        mark.contains(".rounded_md()") && !mark.contains("rounded_full"),
        "the mark is a rounded square, the shape of the session marks and the \
         topic tabs"
    );
    let badge = fn_body(source, "fn badge").expect("badge exists");
    let square_at = badge
        .find(".rounded_md()")
        .expect("the badge's box is a rounded square");
    let first_round = badge.find("rounded_full()").unwrap_or(usize::MAX);
    assert!(
        square_at < first_round,
        "the badge's box must take its radius before the dot inside it does, \
         or the box itself is being drawn as a circle"
    );
}

#[test]
fn a_remote_send_carries_the_conversation_on_screen() {
    // `send_params` is already pinned next to the client, and it is only half
    // the story: the wire helper can carry a context that the window never
    // passes it. That is exactly the bug the owner hit, because a send without
    // a context still returns a task id and still looks like it worked while
    // the host opens a brand new session (#3133).
    let source = include_str!("../dashboard_instances.rs");
    let body =
        fn_body(source, "pub(crate) fn send_remote").expect("send_remote is the window's send");
    assert!(
        body.contains(".context_id.clone()"),
        "the send must read the context off the transcript it is showing"
    );
    assert!(
        body.contains("instance_client::send(&target, &body, ctx.as_deref())"),
        "the context must reach the client as its third argument, not be \
         dropped on the way"
    );
}

#[test]
fn the_remote_composer_takes_the_local_ones_measurements() {
    // The owner's report was not "make it prettier", it was that the remote
    // field did not look like the one in the chat: 13 px of text in a 32 px
    // box against the local 14 px on a 22 px line with a 44 px floor (#3133).
    // Both numbers live in two files that nothing forces together, so a change
    // to the local composer would silently leave the remote one behind. The
    // guard reads both and compares them.
    let panel = include_str!("../remote_panel.rs");
    let composer = fn_body(panel, "pub(crate) fn view(").expect("the panel has a view");
    let editor = include_str!("../editor.rs");

    for measurement in [
        ".line_height(px(22.))",
        ".text_size(px(14.))",
        ".min_h(px(44.))",
        "transcript::card_inset()",
    ] {
        assert!(
            composer.contains(measurement),
            "the remote composer must carry {measurement}, which is what the \
             local field uses; without it the two fields are different sizes"
        );
        assert!(
            editor.contains(measurement),
            "{measurement} is no longer the local composer's, so this guard is \
             comparing the remote field against nothing: update both together"
        );
    }

    // The placeholder is the local field's own constant. A second copy of the
    // string is how the two drift into saying different things.
    assert!(
        composer.contains("crate::editor::PLACEHOLDER"),
        "the remote placeholder must be the local constant, not a literal"
    );
}

#[test]
fn every_pairing_field_answers_the_mouse() {
    // Reported twice: the form prints three boxes that look editable and only
    // Tab could move between them (2026-11-21, Daniel: "no ecra de adicionar
    // crab nao consigo carregar com o rato noutros inputs"). A source test is
    // the right shape because the wiring is a gpui listener: what regresses is
    // the handler going away while the box still paints.
    let source = include_str!("../dashboard_instances.rs");
    let body = fn_body(source, "pub(crate) fn instance_pair_modal")
        .expect("instance_pair_modal is the form the owner fills in");
    assert!(
        body.contains("instance-pair-field-{index}"),
        "each row needs its own id, or the click target is the whole card"
    );
    assert!(
        body.contains(".on_click(focus_here)"),
        "the row must answer a left click: the mouse is how a field is found"
    );
    assert!(
        body.contains("form.field = index"),
        "the click must move the caret to the row that was clicked, the same \
         thing Tab does"
    );
}
