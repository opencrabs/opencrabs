//! Source-level boundary audit for the userbot.
//!
//! The userbot has two surfaces: the passive watch loop, and an explicit
//! operator download command. Both are inbound. Neither may grow an
//! outbound-as-user or agent-dispatch path, so the forbidden list below is
//! applied to every file in the directory, download.rs included.

/// Calls that would give the userbot an outbound-as-user or agent-dispatch
/// surface. `iter_download` is deliberately absent: pulling a document in is
/// the download command's whole job, and it mutates nothing.
const FORBIDDEN_SURFACE: [&str; 14] = [
    "send_reactions(",
    "send_message(",
    "edit_message(",
    "delete_messages(",
    "download_media(",
    "search_all_messages(",
    "handle_message(",
    "AgentService",
    "userbot/tools",
    "allow_dangerous",
    "outbound_allowlist",
    "chat_permissions",
    "wait_reply",
    "schedule_unix",
];

fn userbot_source(module: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/channels/telegram/userbot")
        .join(module);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

#[test]
fn userbot_source_has_no_mutation_or_agent_dispatch_surface() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/telegram/userbot");
    let mut source = String::new();
    for entry in std::fs::read_dir(root).expect("userbot dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            source.push_str(&std::fs::read_to_string(path).expect("source"));
        }
    }

    for forbidden in FORBIDDEN_SURFACE {
        assert!(
            !source.contains(forbidden),
            "userbot boundary contains forbidden surface: {forbidden}"
        );
    }
}

/// The download command is the one file that streams media, and it streams IN
/// only: it carries none of the forbidden outbound calls, and the transfer
/// goes through `iter_download` rather than an upload or a send.
#[test]
fn userbot_download_streams_inbound_only() {
    let source = userbot_source("download.rs");
    assert!(
        source.contains("iter_download"),
        "download command should stream with iter_download"
    );
    for forbidden in FORBIDDEN_SURFACE {
        assert!(
            !source.contains(forbidden),
            "download surface contains forbidden call: {forbidden}"
        );
    }
}

#[test]
fn watch_gate_precedes_conversion_and_storage() {
    let watch = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/channels/telegram/userbot/watch.rs"),
    )
    .expect("watch source");
    let gate = watch.find("chat_allowed(&allowed").expect("allowlist gate");
    let convert = watch
        .find("to_channel_message(&message)")
        .expect("conversion");
    let store = watch.find("messages.insert(&row)").expect("storage");
    assert!(gate < convert && convert < store);
}
