//! A guest chat resolves its session from the database like a regular chat
//! does (#2159), under a guest suffix that never matches the owner's chat.

#[test]
fn guest_sessions_resolve_from_the_database_by_a_guest_suffix() {
    let src = include_str!("../channels/telegram/guest.rs");
    assert!(
        src.contains("resolve_or_create_channel_session("),
        "a guest session must be resolved from the database, like a regular chat"
    );
    assert!(
        src.contains("[guest:{chat}]"),
        "the guest suffix must be distinct from the owner's [chat:<id>]"
    );
    assert!(
        !src.contains("sessions: Mutex<HashMap"),
        "the guest session map must not live in memory"
    );
}
