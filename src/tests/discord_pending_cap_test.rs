//! #2193: the pending select and form maps are bounded.
//!
//! Both maps accepted every registration and only shed entries when a user
//! clicked, so a long-lived daemon accumulated one entry per unclicked menu
//! forever. The comments claimed the maps were "bounded by lazy TTL expiry",
//! which was false: the TTL is consulted only for an id someone asks for, and
//! an unclicked id is never asked for.

use crate::channels::discord::DiscordState;
use crate::channels::discord::interactions::FormSpec;
use crate::channels::discord::pending_interactions::PENDING_INTERACTION_CAP;

/// Far more than the cap, so any map that is still unbounded keeps every
/// entry and the assertions below fail loudly.
const REGISTRATIONS: usize = 100;

#[tokio::test]
async fn a_hundred_unclicked_menus_leave_a_bounded_map() {
    let state = DiscordState::new();
    for i in 0..REGISTRATIONS {
        state
            .register_select(format!("select-{i}"), vec![format!("option-{i}")])
            .await;
    }

    // The oldest entries are the ones shed: registering past the cap must not
    // drop the menu the user is looking at right now.
    assert!(
        state.take_select("select-0", 24.0).await.is_none(),
        "the oldest unclicked menu must be evicted once the cap is passed"
    );
    let newest = format!("select-{}", REGISTRATIONS - 1);
    assert!(
        state.take_select(&newest, 24.0).await.is_some(),
        "the newest menu must survive the prune"
    );

    // The survivor at the cap boundary is still there too, so the prune is a
    // cap and not a flush.
    let boundary = format!("select-{}", REGISTRATIONS - PENDING_INTERACTION_CAP);
    assert!(
        state.take_select(&boundary, 24.0).await.is_some(),
        "the entry at the cap boundary must survive, got None for {boundary}"
    );
}

#[tokio::test]
async fn a_hundred_unclicked_forms_leave_a_bounded_map() {
    let state = DiscordState::new();
    for i in 0..REGISTRATIONS {
        state
            .register_form(
                format!("form-{i}"),
                FormSpec {
                    title: format!("Form {i}"),
                    fields: vec![("answer".to_string(), false)],
                },
            )
            .await;
    }

    assert!(
        state.get_form("form-0", 24.0).await.is_none(),
        "the oldest unclicked form must be evicted once the cap is passed"
    );
    let newest = format!("form-{}", REGISTRATIONS - 1);
    assert!(
        state.get_form(&newest, 24.0).await.is_some(),
        "the newest form must survive the prune"
    );
}

/// The prune runs on insert, so there is no timer to start and no path that
/// registers without pruning. A sweep-based fix would pass the tests above and
/// still leak whenever the sweep was not wired.
#[test]
fn both_registrations_prune_before_inserting() {
    const SRC: &str = include_str!("../channels/discord/pending_interactions.rs");

    for (name, anchor) in [
        ("register_select", "pub(crate) async fn register_select"),
        ("register_form", "pub(crate) async fn register_form"),
    ] {
        let at = SRC
            .find(anchor)
            .unwrap_or_else(|| panic!("{name} is gone from the module"));
        let body = &SRC[at..];
        let body = &body[..body.find("\n    }").unwrap_or(body.len())];
        assert!(
            body.contains("make_room(&mut map)"),
            "{name} must prune before inserting, or the map is unbounded again"
        );
    }

    // The false claim is what kept anyone from looking. It must not come back.
    assert!(
        !SRC.contains("Bounded by lazy TTL expiry"),
        "the module must not claim the TTL bounds the map"
    );
}

/// A failed send used to leave a registered select behind: the button was
/// never delivered, so nothing could ever consume the entry.
#[test]
fn the_followup_select_is_registered_only_after_a_successful_send() {
    const SRC: &str = include_str!("../channels/discord/suggest_options.rs");

    let send = SRC
        .find("super::governor::send_content(")
        .expect("the send is gone from suggest_options");
    let register = SRC
        .find("state.register_select(")
        .expect("the select registration is gone from suggest_options");
    assert!(
        send < register,
        "registration must follow the send, or a failed send orphans an entry"
    );
    assert!(
        SRC.contains("Ok(_) => state.register_select(followup_id, options).await"),
        "the registration must be the success arm of the send"
    );
}
