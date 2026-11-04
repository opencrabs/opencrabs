//! End-to-end smoke of the shared streaming edit loop in its RESUME shape
//! (`react_target = None` — the #1086 seam 5 unification).
//!
//! Drives the real `spawn_edit_loop` against a mock Telegram Bot API and pins
//! the three behaviors that matter after the resume twin adopted the
//! handler's loop:
//!   1. **#261**: a `<<react:…>>` marker in an intermediate is stripped and
//!      never fires `setMessageReaction` — a resumed turn has no inbound
//!      message to react to, and the raw marker must never reach a body.
//!   2. A tool status flip (⚙️ → ✅) re-renders the open flow **in place**
//!      via `editMessageText` — the dirty-tool edit pass resume gained in the
//!      unification; no new `sendMessage` may land for the update.
//!   3. Cancellation completes the `JoinHandle` cleanly — both call sites
//!      await it after cancel to close the duplicate-send race.
//!
//! The Bot API is faked with mockito (expectation-driven asserts): every
//! method answers a canned success, so the loop runs its real code paths
//! (flow chrome, tool groups, typing) against localhost.
//!
//! One exception, honestly stated: the rich-API client hardcodes
//! `api.telegram.org` (rich/api.rs), so the first flow-open attempts
//! `sendRichMessage` against the real endpoint with an invalid token. It
//! fails fast (404 online, a connection error offline) and the code falls
//! back to the HTML path — the same fallback production uses when rich
//! rendering is unavailable. The outcome is deterministic either way.

use std::sync::Arc;
use std::time::Duration;

use teloxide::prelude::*;
use teloxide::types::ChatId;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::channels::telegram::flow::{DisplayItem, StreamingState, ToolMsg};
use crate::channels::telegram::state::TelegramState;
use crate::channels::telegram::stream_loop::spawn_edit_loop;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;

/// A minimal but valid Bot API `Message` envelope for sendMessage/editMessageText.
const MESSAGE_JSON: &str =
    r#"{"ok":true,"result":{"message_id":9001,"date":1,"chat":{"id":12345,"type":"private"}}}"#;
/// Envelope for methods whose result type is `True` (reactions, chat actions…).
const TRUE_JSON: &str = r#"{"ok":true,"result":true}"#;

#[tokio::test]
async fn resume_shape_loop_edits_tools_in_place_and_never_reacts() {
    // CI isolation (#199): `edit_admission`'s top fast-path drops every
    // Clock/Intermediary edit while the PROCESS-WIDE 429 cooldown
    // (`rate_limit::GLOBAL_COOLDOWN`) is live, and `gate_now()` is wall-clock
    // based in test builds, so a sibling test that arms a cooldown holds a
    // real edit-suppression window for the whole of its body. `cargo test`
    // runs the suite as parallel threads in ONE process, so without this
    // guard a cooldown armed elsewhere silently swallows this test's edits —
    // which is exactly how the open flow stopped being edited in place.
    // Every other test that touches the gates takes the same guard.
    let _guard = crate::channels::telegram::governor::test_support::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    crate::channels::telegram::governor::test_support::reset(0);
    crate::channels::telegram::rate_limit::reset_global_cooldown();

    // Runtime evidence: with no subscriber, every warn!/telemetry line in the
    // loop is a no-op in tests. try_init keeps this idempotent.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(true)
        .try_init();

    // In-memory agent service (house pattern: active_skill_tracking_test).
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let agent = Arc::new(AgentService::new_for_test(provider, context).await);

    // Mock Bot API. Mockito matches in CREATION ORDER: the first created
    // mock whose matchers fit a request wins. So the body sentinels go
    // first (highest priority), then the per-method pins, and the generic
    // catch-all LAST (lowest priority) so it only answers methods nothing
    // else covers. (An earlier revision had this backwards; the catch-all
    // hijacked every sendMessage and the pins starved.)
    let mut server = mockito::Server::new_async().await;
    let bot = Bot::new("test-token").set_api_url(server.url().parse().unwrap());

    // Sentinels: if the raw `<<react:` marker ever reaches a message body,
    // these catch it before the per-method pins below can count the leak
    // as a legitimate send/edit.
    let leak_on_edit = server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"(?i)bot.*/editmessagetext".into()),
        )
        .match_body(mockito::Matcher::Regex(r"<<react:".into()))
        .expect(0)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(MESSAGE_JSON)
        .create_async()
        .await;
    let leak_on_send = server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"(?i)bot.*/sendmessage".into()),
        )
        .match_body(mockito::Matcher::Regex(r"<<react:".into()))
        .expect(0)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(MESSAGE_JSON)
        .create_async()
        .await;

    // #261: a resumed turn has no inbound message — reactions never fire.
    let react = server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"(?i)bot.*/setmessagereaction".into()),
        )
        .expect(0)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(TRUE_JSON)
        .create_async()
        .await;

    // The whole scenario opens exactly ONE message surface (the flow/tool
    // block). The status flip below must EDIT it, not send another — that is
    // the edit-in-place contract resume gained from the unification.
    let send = server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"(?i)bot.*/sendmessage".into()),
        )
        .expect(1)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(MESSAGE_JSON)
        .create_async()
        .await;

    let edit = server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"(?i)bot.*/editmessagetext".into()),
        )
        .expect_at_least(1)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(MESSAGE_JSON)
        .create_async()
        .await;

    // Fallback (lowest priority): any unmocked method still succeeds with
    // `True` — typing indicators and friends.
    let _catch_all = server
        .mock("POST", mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(TRUE_JSON)
        .create_async()
        .await;

    // Streaming state in the resume shape: one running tool plus a queued
    // intermediate carrying a react marker.
    let chat = ChatId(12345);
    let tg = Arc::new(TelegramState::new());
    let streaming = Arc::new(std::sync::Mutex::new(StreamingState {
        is_dm: true,
        pending_suggestions: None,
        pending_trailer: None,
        delivered_files: Vec::new(),
        media_intermediates: Vec::new(),
        msg_id: None,
        thinking: String::new(),
        tool_msgs: vec![ToolMsg {
            msg_id: None,
            name: "bash".into(),
            context: String::new(),
            raw_context: String::new(),
            completed: None,
            dirty: true,
            task_id: None,
        }],
        display_queue: vec![
            DisplayItem::NewTool(0),
            DisplayItem::Intermediate("<<react:🔥>> checking the logs".into()),
        ],
        open_group_msg_id: None,
        rich_transport_failures: 0,
        flow_entries: Vec::new(),
        flow_status: None,
        flow_rich: false,
        response: String::new(),
        dirty: false,
        recreate: false,
        header_preview: None,
        compacting: false,
        sections: Default::default(),
        retained_goal: None,
        tool_round_count: 0,
        tools_started_at: Some(std::time::Instant::now()),
        turn_started_at: std::time::Instant::now(),
        flow_outcome: None,
        bg_indicator: None,
        bg_count: None,
        subagent_counts: Default::default(),
        sent_intermediates: Vec::new(),
        intermediate_bubbles: Vec::new(),
        voice_msg_ids: Vec::new(),
        applied_plan_kb: Default::default(),
        processing: true,
        final_bubble: None,
        is_cli: false,
    }));

    let cancel = CancellationToken::new();
    let handle = spawn_edit_loop(
        &bot,
        chat,
        // Resume shape: no inbound message → nothing to react to (#261).
        None,
        None,
        true,
        &streaming,
        &cancel,
        &tg,
        &agent,
        Uuid::new_v4(),
    );

    // Tick 1 (~1.5s): the tool group opens and the react-marked intermediate
    // folds into the flow.
    tokio::time::sleep(Duration::from_millis(1900)).await;

    // Status flip ⚙️ → ✅: must re-render the open flow in place, not send a
    // new message for the update.
    {
        let mut s = streaming.lock().unwrap();
        s.tool_msgs[0].completed = Some(true);
        s.tool_msgs[0].dirty = true;
    }

    // Tick 2 processes the flip.
    tokio::time::sleep(Duration::from_millis(1900)).await;

    // Clean-cancel contract: both call sites await the handle after cancel to
    // prevent the duplicate-send race, so it must complete promptly.
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("edit loop JoinHandle must complete after cancel")
        .expect("edit loop task panicked");

    // Expectation asserts (loop fully drained).
    send.assert_async().await;
    edit.assert_async().await;
    react.assert_async().await;
    leak_on_send.assert_async().await;
    leak_on_edit.assert_async().await;
}
