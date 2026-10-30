//! Tests for Discord handler `split_message`.

use crate::channels::discord::handler::split_message;

#[test]
fn split_short_message() {
    let chunks = split_message("hello", 2000);
    assert_eq!(chunks, vec!["hello"]);
}

#[test]
fn split_long_message() {
    let text = "a\n".repeat(1500);
    let chunks = split_message(&text, 2000);
    assert!(chunks.len() >= 2);
    for chunk in &chunks {
        assert!(chunk.len() <= 2000);
    }
    let joined: String = chunks.into_iter().collect();
    assert_eq!(joined, text);
}

// ── #2190: menu truncation is stated, never silent ───────────────────

/// Discord allows 5 action rows of 5 buttons, so 25 entries is a hard platform
/// ceiling. The menus used to `.take(5)` rows and say nothing, so a user with
/// 30 providers saw 25 buttons and had no way to know five were missing.
#[test]
fn a_menu_that_fits_is_unchanged() {
    use crate::channels::discord::handler::{MENU_BUTTON_CAP, menu_body};

    let text = "Pick a provider";
    assert_eq!(menu_body(text, 0), text, "an empty list adds no notice");
    assert_eq!(menu_body(text, 1), text);
    assert_eq!(
        menu_body(text, MENU_BUTTON_CAP),
        text,
        "exactly at the cap nothing is hidden, so there is nothing to report"
    );
}

#[test]
fn a_truncated_menu_says_how_many_were_hidden() {
    use crate::channels::discord::handler::{MENU_BUTTON_CAP, menu_body};

    let body = menu_body("Pick a provider", MENU_BUTTON_CAP + 5);
    assert!(
        body.contains("Showing 1-25 of 30"),
        "the notice must name the range and the total, got: {body}"
    );
    assert!(
        body.contains("Pick a provider"),
        "the notice must not replace the menu text"
    );

    // The count comes from the caller's real list, so an absurd length must
    // still report honestly rather than clamp the total too.
    let big = menu_body("Sessions", 1000);
    assert!(big.contains("of 1000"), "got: {big}");
}

/// The notice is worthless unless both menus actually call it.
#[test]
fn both_menus_route_their_body_through_the_notice() {
    const SRC: &str = include_str!("../channels/discord/handler.rs");
    assert!(
        SRC.contains("menu_body(&resp.text, resp.providers.len())"),
        "the provider menu must report its truncation"
    );
    assert!(
        SRC.contains("menu_body(&resp.text, resp.sessions.len())"),
        "the session menu must report its truncation"
    );
}
