//! `/clear` starts the agent fresh at the current point without a
//! summariser call, keeping history and title (#1585).
//!
//! A failed manual `/compact` on a large context sends the whole snapshot
//! to every provider in the chain. `/clear` is one user row starting with
//! the compaction-marker prefix: the context loader cuts there, the TUI
//! hides the row on reload, and nothing is sent anywhere.

use std::sync::Arc;

use crate::brain::agent::service::AgentService;
use crate::brain::agent::service::clear::{
    CLEAR_HINT, CLEAR_MARKER_PREFIX, ClearReceipt, clear_marker,
};
use crate::channels::commands::ChannelCommand;
use crate::db::Database;
use crate::services::{MessageService, ServiceContext, SessionService};
use crate::tests::agent_service_mocks::MockProvider;

const LOADER_PREFIX: &str = "[CONTEXT COMPACTION";

#[test]
fn the_marker_is_what_the_loader_and_the_tui_key_on() {
    assert!(CLEAR_MARKER_PREFIX.starts_with(LOADER_PREFIX));
    let marker = clear_marker(Some("Router audit"), &[]);
    assert!(marker.starts_with(CLEAR_MARKER_PREFIX));
    // The TUI reload hides any row with this prefix; pin the check it uses.
    const TUI: &str = include_str!("../tui/app/messaging.rs");
    assert!(TUI.contains("if msg.content.starts_with(\"[CONTEXT COMPACTION\")"));
}

#[test]
fn the_body_names_the_session_and_the_search_tool() {
    let marker = clear_marker(Some("  Router audit  "), &[]);
    let body = marker.split_once("\n\n").expect("banner then body").1;
    assert!(body.contains("session_search"));
    assert!(body.contains("'Router audit'"));
    assert!(body.contains("'tail'") && body.contains("'search'"));
    assert!(
        !body.contains("[CONTEXT COMPACTION"),
        "the banner is not part of the body"
    );
}

#[test]
fn an_untitled_session_still_gets_a_usable_nudge() {
    let marker = clear_marker(None, &[]);
    assert!(marker.contains("'untitled'"));
    assert!(marker.contains("session_search"));
    assert_eq!(clear_marker(Some("   "), &[]), clear_marker(None, &[]));
}

#[test]
fn the_banner_is_stripped_and_the_body_kept_when_loaded_as_context() {
    let mut content = clear_marker(Some("Router audit"), &[]);
    AgentService::strip_compaction_banner(&mut content);
    assert!(!content.starts_with("["));
    assert!(content.starts_with("The user cleared this session's context"));
    assert!(content.contains("session_search"));
}

#[test]
fn the_receipt_line_reports_title_and_an_aborted_summariser() {
    let plain = ClearReceipt {
        session_title: Some("Router audit".to_string()),
        aborted_background_compaction: false,
    }
    .user_line();
    assert!(plain.contains("(title 'Router audit')"));
    assert!(plain.contains("No summariser call was made."));
    assert!(!plain.contains("cancelled"));

    let aborted = ClearReceipt {
        session_title: None,
        aborted_background_compaction: true,
    }
    .user_line();
    assert!(!aborted.contains("title"));
    assert!(aborted.contains("background compaction that was running has been cancelled"));
}

#[tokio::test]
async fn clearing_appends_the_marker_and_the_loader_cuts_there() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let agent = AgentService::new_for_test(Arc::new(MockProvider), context.clone()).await;
    let sessions = SessionService::new(context.clone());
    let messages = MessageService::new(context);
    let session = sessions
        .create_session(Some("Router audit".to_string()))
        .await
        .unwrap();
    messages
        .create_message(session.id, "user".to_string(), "first question".to_string())
        .await
        .unwrap();
    messages
        .create_message(
            session.id,
            "assistant".to_string(),
            "first answer".to_string(),
        )
        .await
        .unwrap();

    let receipt = agent
        .clear_context(session.id)
        .await
        .expect("clear succeeds");
    assert_eq!(receipt.session_title.as_deref(), Some("Router audit"));
    assert!(!receipt.aborted_background_compaction);

    let rows = messages
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3, "history is kept, one marker row appended");
    assert_eq!(rows[0].content, "first question");
    let marker = &rows[2];
    assert_eq!(marker.role, "user");
    assert!(marker.content.starts_with(CLEAR_MARKER_PREFIX));
    assert!(marker.content.contains("'Router audit'"));
    assert!(
        marker.content.contains("user: first question"),
        "the marker quotes the exchanges that were cleared away (#1905)"
    );
    assert!(marker.content.contains("assistant: first answer"));

    let loaded = AgentService::messages_from_last_compaction(rows);
    assert_eq!(loaded.len(), 1, "the next turn loads only the marker");
    assert!(loaded[0].content.starts_with(CLEAR_MARKER_PREFIX));

    // Clearing again is another cut at the new point, not an error.
    agent.clear_context(session.id).await.expect("second clear");
    let rows = messages
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 4);
    let second = &rows[3];
    assert!(
        second.content.matches(CLEAR_MARKER_PREFIX).count() == 1,
        "an older marker row is never quoted into the new one"
    );
    assert!(
        second.content.contains("first question"),
        "the live exchanges still carry into a second clear"
    );
}

#[test]
fn clear_aborts_a_background_summariser_before_writing_the_marker() {
    const SRC: &str = include_str!("../brain/agent/service/clear.rs");
    let abort = SRC
        .find("take_pending_compaction(session_id)")
        .expect("pending taken");
    let write = SRC.find("create_message(").expect("marker written");
    assert!(
        abort < write,
        "the in-flight summary is aborted before the cut"
    );
    assert!(SRC[abort..write].contains("pending.abort()"));
}

#[test]
fn channels_parse_clear_owner_only() {
    // The parser is exercised through handle_command in the channel tests;
    // here only the variant and its help entry are pinned.
    const SRC: &str = include_str!("../channels/commands.rs");
    let arm = SRC.find("\"/clear\" => {").expect("/clear parsed");
    assert!(SRC[arm..arm + 220].contains("Owner-only command"));
    assert!(SRC[arm..arm + 220].contains("ChannelCommand::ClearContext"));
    assert!(
        SRC.contains("(\n            \"/clear\","),
        "listed in /help"
    );
    let _ = ChannelCommand::ClearContext;
}

#[test]
fn every_surface_handles_clear_without_an_agent_turn() {
    const TELEGRAM: &str = include_str!("../channels/telegram/commands_tg.rs");
    const DISCORD: &str = include_str!("../channels/discord/handler.rs");
    const SLACK: &str = include_str!("../channels/slack/handler.rs");
    const WHATSAPP: &str = include_str!("../channels/whatsapp/handler.rs");
    const TUI: &str = include_str!("../tui/app/messaging.rs");
    const MENU: &str = include_str!("../channels/telegram/agent.rs");
    for (name, src) in [
        ("telegram", TELEGRAM),
        ("discord", DISCORD),
        ("slack", SLACK),
        ("whatsapp", WHATSAPP),
    ] {
        let arm = src
            .find("ChannelCommand::ClearContext => {")
            .unwrap_or_else(|| panic!("{name} handles ClearContext"));
        assert!(
            src[arm..arm + 400].contains("clear_context(session_id)"),
            "{name} clears through the agent service"
        );
    }
    let tui = TUI.find("\"/clear\" => {").expect("TUI command");
    // `get` returns None instead of panicking on a non-boundary index, so the
    // window never needs a byte-index slice (the ratchet counts those).
    let rest = TUI.get(tui..).expect("TUI command starts at a boundary");
    let window = rest.get(..2000).unwrap_or(rest);
    assert!(
        window.contains("is_processing"),
        "refused while a turn runs"
    );
    assert!(window.contains("prepare_clear(session_id)"));
    assert!(
        window.contains("resume_background_turn("),
        "the reply turn starts right after the clear"
    );
    assert!(
        window.contains("base_context_tokens()"),
        "footer reset to baseline"
    );
    assert!(
        MENU.contains("BotCommand::new(\"clear\""),
        "in the Telegram menu"
    );
}

#[test]
fn a_failed_manual_compaction_points_at_clear() {
    const TOOL_LOOP: &str = include_str!("../brain/agent/service/tool_loop.rs");
    let failure = TOOL_LOOP
        .find("Manual compaction failed")
        .expect("manual failure branch");
    assert!(TOOL_LOOP[failure..failure + 900].contains("CLEAR_HINT"));
    assert!(CLEAR_HINT.contains("/clear"));
    const NOTICE: &str = include_str!("../brain/agent/service/compaction_notice.rs");
    assert!(NOTICE.contains("self.verbose && matches!(step, CompactionStep::Failed { .. })"));
}

#[test]
fn the_marker_carries_only_the_last_six_pairs() {
    let recent: Vec<(String, String)> = (1..=16)
        .map(|i| {
            let role = if i % 2 == 1 { "user" } else { "assistant" };
            (role.to_string(), format!("exchange {i:02}"))
        })
        .collect();
    let marker = clear_marker(Some("Router audit"), &recent);
    assert!(marker.contains("The last exchanges before the clear, quoted as context only"));
    assert!(marker.contains("user: exchange 05"), "oldest carried pair");
    assert!(
        marker.contains("assistant: exchange 16"),
        "newest carried pair"
    );
    assert!(
        !marker.contains("exchange 04"),
        "older than six pairs is dropped"
    );
    assert!(
        !marker.contains("Nothing said or done before it is in your context"),
        "the marker must not deny the pairs it quotes"
    );
}

#[test]
fn an_empty_history_clears_without_a_pairs_section() {
    let marker = clear_marker(Some("Router audit"), &[]);
    assert!(!marker.contains("The last exchanges before the clear"));
    assert!(marker.contains("Nothing said or done before it is in your context"));
}

#[test]
fn a_long_carried_message_is_capped_on_a_char_boundary() {
    let long = "ç".repeat(600);
    let marker = clear_marker(Some("Router audit"), &[("user".to_string(), long)]);
    let line = marker
        .lines()
        .find(|l| l.starts_with("user: "))
        .expect("the carried message is quoted");
    assert!(line.ends_with("[...]"));
    let kept = line
        .strip_prefix("user: ")
        .and_then(|l| l.strip_suffix(" [...]"))
        .unwrap();
    assert_eq!(kept.chars().count(), 500);
    assert!(kept.chars().all(|c| c == 'ç'), "no codepoint is split");
}

#[tokio::test]
async fn a_long_thread_carries_only_the_tail_through_the_real_path() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let agent = AgentService::new_for_test(Arc::new(MockProvider), context.clone()).await;
    let sessions = SessionService::new(context.clone());
    let messages = MessageService::new(context);
    let session = sessions.create_session(None).await.unwrap();
    for i in 1..=14 {
        let role = if i % 2 == 1 { "user" } else { "assistant" };
        messages
            .create_message(session.id, role.to_string(), format!("thread {i:02}"))
            .await
            .unwrap();
    }

    agent.clear_context(session.id).await.unwrap();

    let rows = messages
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 15, "fourteen messages plus the marker");
    let marker = &rows[14];
    assert!(marker.content.contains("user: thread 03"), "oldest carried");
    assert!(
        marker.content.contains("assistant: thread 14"),
        "newest carried"
    );
    assert!(
        !marker.content.contains("thread 02"),
        "beyond six pairs is dropped"
    );
    let loaded = AgentService::messages_from_last_compaction(rows);
    assert_eq!(
        loaded.len(),
        1,
        "still one context row: the tail rides inside the marker"
    );
    assert!(loaded[0].content.contains("thread 14"));
}
