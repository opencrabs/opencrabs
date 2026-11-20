//! One canonical `channel_chat_id` per WhatsApp conversation (#2928).
//!
//! The bug: a contact addressed by PN one message and by LID the next got two
//! `channel_messages` threads, and a reply written into the thread the next
//! turn does not read is a reply the agent never sees. These tests pin the key
//! to the conversation rather than to the transport address.

use crate::channels::whatsapp::WhatsAppState;
use crate::channels::whatsapp::chat_key::{canonical_chat_id, pn_from_stanza};
use wacore_binary::jid::Jid;

const PN: &str = "6282361295343";
const LID: &str = "210122852823201";

fn jid(s: &str) -> Jid {
    s.parse().expect("test jid parses")
}

fn pn_jid() -> Jid {
    jid(&format!("{PN}@s.whatsapp.net"))
}

fn lid_jid() -> Jid {
    jid(&format!("{LID}@lid"))
}

#[test]
fn a_pn_chat_keys_on_its_own_number() {
    assert_eq!(
        canonical_chat_id(&pn_jid(), false, None),
        format!("{PN}@s.whatsapp.net")
    );
}

#[test]
fn a_lid_chat_with_a_known_twin_keys_on_the_pn() {
    assert_eq!(
        canonical_chat_id(&lid_jid(), false, Some(PN)),
        format!("{PN}@s.whatsapp.net")
    );
}

#[test]
fn a_lid_chat_with_no_twin_keeps_its_own_id() {
    assert_eq!(
        canonical_chat_id(&lid_jid(), false, None),
        format!("{LID}@lid")
    );
}

#[test]
fn an_empty_twin_is_treated_as_unknown() {
    assert_eq!(
        canonical_chat_id(&lid_jid(), false, Some("  ")),
        format!("{LID}@lid")
    );
}

#[test]
fn a_group_keeps_its_own_address() {
    let group = jid("120363000000000000@g.us");
    assert_eq!(
        canonical_chat_id(&group, true, Some(PN)),
        group.to_string(),
        "a group jid is not a lid/pn identity pair"
    );
}

#[test]
fn the_stanza_twin_is_read_from_whichever_family_arrived() {
    assert_eq!(pn_from_stanza(&pn_jid(), None).as_deref(), Some(PN));
    assert_eq!(pn_from_stanza(&lid_jid(), Some(PN)).as_deref(), Some(PN));
    assert_eq!(pn_from_stanza(&lid_jid(), None), None);
}

/// The case from the issue: the bot's reply went out under the PN JID while
/// the user's next message arrived on the LID, so the reply was invisible to
/// the turn that answered.
#[tokio::test]
async fn pn_outbound_and_lid_inbound_share_one_channel_chat_id() {
    let state = WhatsAppState::new();

    let outbound = state
        .canonical_chat_id(&pn_jid(), false, Some(LID))
        .await;
    let inbound = state.canonical_chat_id(&lid_jid(), false, Some(PN)).await;

    assert_eq!(outbound, inbound);
    assert_eq!(outbound, format!("{PN}@s.whatsapp.net"));
}

/// A stanza that names both ids teaches the mirror, so the next one that names
/// only the LID still lands in the same thread.
#[tokio::test]
async fn a_lid_only_stanza_uses_the_pair_learned_from_an_earlier_one() {
    let state = WhatsAppState::new();

    let with_twin = state.canonical_chat_id(&lid_jid(), false, Some(PN)).await;
    let without_twin = state.canonical_chat_id(&lid_jid(), false, None).await;

    assert_eq!(with_twin, format!("{PN}@s.whatsapp.net"));
    assert_eq!(without_twin, with_twin);
}

/// Pairs the Signal store already knows answer a LID-only stanza on the first
/// message after a restart.
#[tokio::test]
async fn the_persisted_mapping_answers_a_lid_only_stanza() {
    let state = WhatsAppState::new();
    state
        .seed_lid_pn_pairs(vec![(LID.to_string(), PN.to_string())])
        .await;

    assert_eq!(
        state.canonical_chat_id(&lid_jid(), false, None).await,
        format!("{PN}@s.whatsapp.net")
    );
}

/// Two contacts must not collide: a LID with no known twin keeps its own key.
#[tokio::test]
async fn an_unpaired_lid_does_not_borrow_another_contacts_key() {
    let state = WhatsAppState::new();
    state
        .seed_lid_pn_pairs(vec![(LID.to_string(), PN.to_string())])
        .await;

    let other = jid("999000111222333@lid");
    assert_eq!(
        state.canonical_chat_id(&other, false, None).await,
        other.to_string()
    );
}
