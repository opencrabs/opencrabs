//! Tests for the instance strip's wiring (`crate::dashboard_instances`).
//!
//! The network paths are not exercised here: what is asserted is the shape of
//! the state the window holds, because that is what the paint reads. The
//! client and the store carry their own tests next to the code that parses.

use crate::dashboard_instances::{PairingForm, RemoteState, RemoteView};

#[test]
fn a_busy_form_refuses_to_look_idle() {
    // `..Default::default()` on purpose: the form grows fields as the keyboard
    // work lands, and a literal here breaks the test build every time it does.
    let form = PairingForm {
        url: "http://a:7878".into(),
        code: "123456".into(),
        ..Default::default()
    }
    .busy();
    assert!(form.busy);
    assert_eq!(form.code, "123456");
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
