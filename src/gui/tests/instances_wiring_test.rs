//! Tests for the instance strip's wiring (`crate::dashboard_instances`).
//!
//! The network paths are not exercised here: what is asserted is the shape of
//! the state the window holds, because that is what the paint reads. The
//! client and the store carry their own tests next to the code that parses.

use crate::dashboard_instances::{PairingForm, RemoteState, RemoteView};
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
