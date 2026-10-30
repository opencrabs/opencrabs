//! The streaming edit loop, extracted from `handler.rs` (#1086 seam 5).
//!
//! Spawned once per turn: every 1.5s it snapshots the shared `StreamingState`
//! under lock, folds queued display items (tools, intermediates) into the
//! chat in chronological order, re-sticks the open flow block when newer
//! chatter buries it (#451), refreshes tool-group messages whose status
//! changed, and edits the response message in place. It exits when the turn
//! cancels the token.

use std::sync::Arc;

use teloxide::prelude::*;
use teloxide::types::{ChatAction, ChatId, MessageId, ParseMode, ThreadId};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::flow::{
    DisplayItem, StreamingState, append_intermediate_to_flow, append_system_to_flow,
    append_tool_group, restick_flow_if_buried, row_display_name,
};
use super::handler::{fire_reaction, thinking_status_excerpt};
use super::markdown::markdown_to_telegram_html;
use super::send::{best_effort_delete, fire_chat_action, message_in_thread};
use super::state::TelegramState;
use crate::brain::AgentService;

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_edit_loop(
    bot: &Bot,
    chat: ChatId,
    react_target: Option<MessageId>,
    thread_id: Option<ThreadId>,
    is_dm: bool,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    edit_cancel: &CancellationToken,
    telegram_state: &Arc<TelegramState>,
    agent: &Arc<AgentService>,
    session_id: Uuid,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn({
        let bot = bot.clone();
        let st = streaming.clone();
        let cancel = edit_cancel.clone();
        let tg = telegram_state.clone();
        let agent = agent.clone();
        let sid = session_id;
        async move {
            // #2010: the bubble surface this turn was last seen streaming
            // into, so the pending row is rewritten only when it moves.
            // Tuple is (surface tag, message id); see `CarriedBubble`.
            let mut last_surface: Option<(&'static str, i32)> = None;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(1500)) => {
                        // ── Snapshot state under lock, then release immediately ──
                        struct Snapshot {
                            dirty: bool,
                            recreate: bool,
                            response_text: String,
                            msg_id: Option<MessageId>,
                            tool_round_count: usize,
                            /// Ordered display items (tools + intermediates in chronological order)
                            display_items: Vec<DisplayItem>,
                            /// Dirty tools that already have messages (need editing, not new sends)
                            tool_edits: Vec<(usize, String, Option<bool>, MessageId)>,
                            has_active_tools: bool,
                            processing: bool,
                            /// Short excerpt of the latest reasoning chunk used as
                            /// a context-aware status line during the pre-tool
                            /// phase. Falls back to a fun-quip rotation when
                            /// reasoning hasn't started yet.
                            thinking_excerpt: Option<String>,
                        }

                        let mut settle_flow = false;
                        let snap = {
                            let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                            let has_display = !s.display_queue.is_empty();
                            let any_tools_dirty = s.tool_msgs.iter().any(|t| t.dirty);
                            let has_active_tools = s.tool_msgs.iter().any(|t| t.completed.is_none());

                            let processing = s.processing;

                            if !s.dirty && !s.recreate && !any_tools_dirty && !has_display && !has_active_tools && !processing { continue; }

                            // Drain the ordered display queue
                            let display_items: Vec<DisplayItem> = std::mem::take(&mut s.display_queue);

                            // Collect dirty tools that already have messages (for editing)
                            let tool_edits: Vec<_> = s.tool_msgs.iter().enumerate()
                                .filter(|(_, t)| t.dirty && t.msg_id.is_some())
                                .map(|(i, t)| {
                                    let label = format!(
                                        "**{}**{}",
                                        row_display_name(
                                            &t.name,
                                            t.task_id.as_deref(),
                                            t.completed
                                        ),
                                        t.context
                                    );
                                    (i, label, t.completed, t.msg_id.unwrap())
                                })
                                .collect();

                            // Mark tools as not dirty
                            for t in s.tool_msgs.iter_mut().filter(|t| t.dirty) {
                                t.dirty = false;
                            }

                            // Snapshot response
                            let response_text = if s.dirty || s.recreate {
                                s.render()
                            } else {
                                String::new()
                            };

                            let snap = Snapshot {
                                dirty: s.dirty,
                                recreate: s.recreate,
                                response_text,
                                msg_id: s.msg_id,
                                tool_round_count: s.tool_round_count,
                                display_items,
                                tool_edits,
                                has_active_tools,
                                processing,
                                thinking_excerpt: thinking_status_excerpt(&s.thinking),
                            };

                            // Pre-clear state that will be handled
                            if s.recreate {
                                s.recreate = false;
                            }
                            if s.dirty {
                                s.dirty = false;
                            }
                            // Clear status tracking only when final response arrives (#313)
                            // Don't clear on intermediates — keep the status message alive and
                            // edit it in place throughout multi-tool sequences, so we get one
                            // updating message instead of N+1 separate messages.
                            if snap.dirty && !snap.response_text.is_empty() {
                                s.tools_started_at = None;
                                s.tool_round_count = 0;
                                // Header settles to the plain "N tool calls"
                                // via an immediate refresh below (#360).
                                if s.flow_status.take().is_some() && s.open_group_msg_id.is_some()
                                {
                                    settle_flow = true;
                                }
                            }

                            snap
                        };
                        // Lock is now released

                        // #2010: keep the pending row pointed at the bubble this
                        // turn is streaming into. A SIGKILL takes this task's
                        // memory with it, and the row is the only state that
                        // survives to the boot resume, so without this the
                        // resumed turn has nothing to edit and opens a second
                        // bubble beside the partial answer left on screen. The
                        // flow block wins when both exist, because that is the
                        // bubble a reader sees growing. Checked against the last
                        // written value so the row is touched only when the
                        // surface moves (placeholder opens, the flow takes over,
                        // a relocation replaces it). Fire-and-forget: the edit
                        // tick must never wait on the database, and a failed
                        // write only costs the adoption, which then falls back
                        // to the pre-#2010 behaviour of a fresh bubble.
                        {
                            let surface = {
                                let s = st.lock().unwrap_or_else(|e| e.into_inner());
                                match s.open_group_msg_id {
                                    Some(m) => Some((
                                        if s.flow_rich { "flow" } else { "flow_plain" },
                                        m.0,
                                    )),
                                    None => snap.msg_id.map(|m| ("answer", m.0)),
                                }
                            };
                            if surface != last_surface {
                                last_surface = surface;
                                if let Some((tag, id)) = surface {
                                    let carried = format!("{tag}:{id}");
                                    let pool = agent.context().pool();
                                    tokio::spawn(async move {
                                        if let Err(e) =
                                            crate::db::PendingRequestRepository::new(pool)
                                                .set_channel_message_id_for_session(sid, &carried)
                                                .await
                                        {
                                            tracing::debug!(
                                                "[telegram] #2010 bubble id persist failed, this turn keeps no adoptable bubble: {e:#}"
                                            );
                                        }
                                    });
                                }
                            }
                        }

                        // ── Ordered display: tools and intermediates in chronological order ──
                        // Buffer consecutive tool calls to group them into collapsible blocks
                        let mut tool_buffer: Vec<usize> = Vec::new();

                        for item in &snap.display_items {
                            match item {
                                DisplayItem::NewTool(idx) => {
                                    // Buffer this tool call
                                    tool_buffer.push(*idx);
                                }
                                DisplayItem::Intermediate(text) => {
                                    // Flush buffered tools into the open flow,
                                    // then fold this intermediate into the SAME
                                    // in-place processing-log message. It no
                                    // longer lands as its own message, so only
                                    // the final response stays clean at the
                                    // bottom (#300).
                                    append_tool_group(&bot, chat, thread_id, &st, &tool_buffer)
                                        .await;
                                    tool_buffer.clear();

                                    // Sanitize exactly as before folding:
                                    // strip LLM artifacts, redact secrets, strip
                                    // <<IMG:>> markers (the final-response
                                    // handler sends the image), and extract +
                                    // fire <<react:>> now so a mid-turn reaction
                                    // acknowledges the user immediately (#261).
                                    let text = crate::utils::sanitize::strip_llm_artifacts(text);
                                    let text = crate::utils::redact_secrets_scoped(&text, is_dm);
                                    let (text, _img_paths) =
                                        crate::utils::extract_img_markers(&text);
                                    let (text, react_emoji) =
                                        crate::utils::extract_react_marker(&text);
                                    // A resumed turn has no inbound message to
                                    // react to: the marker is stripped above but
                                    // nothing fires (#261).
                                    if let Some(ref emoji) = react_emoji
                                        && let Some(target) = react_target
                                    {
                                        fire_reaction(&bot, chat, target, emoji).await;
                                    }

                                    // A substantial rich report (a table) the
                                    // model emits before a tool call would be
                                    // buried in the collapsed log — surface it as
                                    // its own rich message instead (#582). Thin
                                    // narration keeps folding: folded intermediates
                                    // are NOT recorded in sent_intermediates, so
                                    // the final-response dedup does not suppress the
                                    // visible answer just because it also appears in
                                    // the collapsed trace.
                                    if super::intermediates::is_deliverable_rich_report(&text) {
                                        let session_cwd =
                                            agent.get_working_directory_for_session(sid);
                                        super::intermediates::deliver_intermediate_message(
                                            &bot,
                                            chat,
                                            thread_id,
                                            &st,
                                            &tg,
                                            sid,
                                            session_cwd.as_path(),
                                            &text,
                                        )
                                        .await;
                                    } else {
                                        append_intermediate_to_flow(
                                            &bot, chat, thread_id, &st, &text,
                                        )
                                        .await;
                                    }
                                }
                                DisplayItem::System(text) => {
                                    // Chrome folds into the same block, in
                                    // order, but skips the model pipeline: it
                                    // is our own text, so there is no artifact
                                    // to strip, no rich report to promote, and
                                    // no react directive to fire. Secrets are
                                    // still scrubbed — a self-healing alert
                                    // embeds a raw upstream error string
                                    // (#1253).
                                    append_tool_group(&bot, chat, thread_id, &st, &tool_buffer)
                                        .await;
                                    tool_buffer.clear();
                                    let text =
                                        crate::utils::redact_secrets_scoped(text, is_dm);
                                    append_system_to_flow(&bot, chat, thread_id, &st, &text)
                                        .await;
                                }
                            }
                        }

                        // Flush any remaining buffered tools into the open group.
                        // No close here: the run may continue on the next tick, in
                        // which case those tools append to this same message.
                        append_tool_group(&bot, chat, thread_id, &st, &tool_buffer).await;

                        // ── Re-stick the open block to the bottom if buried (#451) ──
                        // A new round landed this tick (tools/intermediates were in
                        // the display queue). If newer chatter has pushed the block
                        // above the newest message, relocate it to the bottom. Gated
                        // on real appends, never plain status ticks, so an idle chat
                        // sees no churn.
                        if !snap.display_items.is_empty() {
                            let newest = tg.newest_incoming_msg_id(chat.0);
                            if restick_flow_if_buried(&bot, chat, thread_id, &st, &tg, newest).await
                            {
                                // #1150: the relocated block landed at the absolute bottom,
                                // which can sit BELOW an active plan card — burying exactly
                                // the surface that owns Approve/Discard. One coordinated
                                // remove+refresh under the SAME budget draw restores
                                // [flow block][plan card]. The per-session card backoff
                                // (#814) still wins: when suppressed, the next settle
                                // fixes the order instead.
                                if !tg.plan_card_suppressed(sid).await {
                                    super::plan_card::remove_plan_card(&bot, chat, &tg, sid).await;
                                    let plan_kb = {
                                        st.lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .sections
                                            .plan_kb
                                    };
                                    super::plan_card::refresh_plan_card(
                                        &bot, chat, thread_id, &tg, &agent, sid, plan_kb,
                                    )
                                    .await;
                                }
                            }
                        }

                        // ── Update tool-group messages for tools that changed status ──
                        // A completed tool shares its group's message with its
                        // siblings, so re-render the whole group (never a single
                        // tool line, which would overwrite the block). Refresh each
                        // distinct group once.
                        // A tool status flip (⚙️ → ✅/❌) re-renders the whole
                        // processing-log flow (tools + folded intermediates) in
                        // its single message.
                        // Show progress when: tools are active, OR tools ran but no
                        // response yet, OR still processing (initial wait).
                        let show_status = snap.has_active_tools
                            || (snap.tool_round_count > 0 && snap.response_text.is_empty())
                            || snap.processing;

                        // ── Single progress surface: the flow message ──
                        // The live status (thinking / Working-on / activity
                        // preview), wall-clock duration, and plan/goal/ctx
                        // sections all ride the flow header (#360, #480,
                        // #509). While no flow is open and the turn is still
                        // working, the shared tick opens it header-only on
                        // this activity tick; the legacy pre-block status
                        // bubble is gone.
                        let turn_done = snap.dirty && !snap.response_text.is_empty();
                        // Only show the thinking excerpt as a status preview.
                        // The user's message is NOT what the bot is "working on"
                        // it's just the input request, so showing it as
                        // "Working on: <user message>" is confusing. The goal
                        // section (from GoalManager) already shows what the bot
                        // is actually working on when a plan task is active.
                        let preview = snap
                            .thinking_excerpt
                            .as_deref()
                            .map(|t| format!("🧠 {t}"));
                        let flow_needs_refresh = !snap.tool_edits.is_empty() || settle_flow;
                        super::flow_chrome::tick_flow_header(
                            &bot,
                            chat,
                            thread_id,
                            &st,
                            &agent,
                            sid,
                            show_status,
                            turn_done,
                            preview,
                            flow_needs_refresh,
                        )
                        .await;

                        // Update the persistent plan card in place (#580): the
                        // checklist lives on its own card now, not the flow
                        // block, so it advances here as tasks complete.
                        let plan_kb = {
                            st.lock().unwrap_or_else(|e| e.into_inner()).sections.plan_kb
                        };
                        super::plan_card::refresh_plan_card(
                            &bot, chat, thread_id, &tg, &agent, sid, plan_kb,
                        )
                        .await;

                        // ── Response message (thinking + response, always at bottom) ──
                        // Stale-placeholder cleanup runs unconditionally: a bubble
                        // opened before the first tool call must still be removed
                        // once a block opens.
                        if snap.recreate
                            && let Some(old_mid) = snap.msg_id
                        {
                            best_effort_delete(&bot, chat, old_mid, "recreate swap").await;
                            let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                            s.msg_id = None;
                        }
                        // While a processing-log block is open, mid-round narration
                        // folds into that block (append_intermediate_to_flow) and the
                        // final answer is delivered by deliver_final_response at turn
                        // end. Opening a standalone streaming bubble here leaks the
                        // intermediate text as its own message beneath the folded
                        // block (#490), so only stream the placeholder when NO
                        // processing-log block is open. Re-read the id: the
                        // header tick above may have just opened the flow.
                        let open_block = {
                            let s = st.lock().unwrap_or_else(|e| e.into_inner());
                            s.open_group_msg_id
                        };
                        if (snap.dirty || snap.recreate)
                            && open_block.is_none()
                            && !snap.response_text.is_empty()
                        {
                            let current_msg_id = {
                                let s = st.lock().unwrap_or_else(|e| e.into_inner());
                                s.msg_id
                            };
                            if current_msg_id.is_none() {
                                // Success-silent until #1085: the twin in
                                // resume.rs logged both outcomes while this
                                // one dropped the error, so a failing
                                // placeholder send was invisible on the
                                // handler path.
                                match message_in_thread(&bot, chat, thread_id, "\u{258b}").await {
                                    Ok(m) => {
                                        super::telemetry::log_send_success(
                                            "turn",
                                            "-",
                                            "-",
                                            "placeholder",
                                            "new",
                                            chat.0,
                                            thread_id.map(|t| t.0.0),
                                            m.id.0,
                                            "\u{258b}".len(),
                                            &super::telemetry::content_hash8("\u{258b}"),
                                        );
                                        let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                                        s.msg_id = Some(m.id);
                                    }
                                    Err(e) => {
                                        super::telemetry::log_send_failure(
                                            "turn",
                                            "-",
                                            "-",
                                            "placeholder",
                                            "new",
                                            chat.0,
                                            thread_id.map(|t| t.0.0),
                                            "\u{258b}".len(),
                                            &super::telemetry::content_hash8("\u{258b}"),
                                            &e.to_string(),
                                        );
                                    }
                                }
                            }
                            let msg_id = {
                                let s = st.lock().unwrap_or_else(|e| e.into_inner());
                                s.msg_id
                            };
                            if let Some(mid) = msg_id {
                                // Strip any complete <<react:emoji>>
                                // directive from the streaming snapshot so
                                // the raw marker never flashes in the
                                // placeholder (#261). The reaction itself
                                // fires from the intermediate/final paths.
                                let (clean, _) =
                                    crate::utils::extract_react_marker(&snap.response_text);
                                let html = markdown_to_telegram_html(&clean);
                                let display = format!("{}\u{258b}", html); // ▋ cursor
                                // G2 flood governor (#1211): streaming preview churn is the
                                // brain-preview ladder class — droppable when the per-forum
                                // edit bucket is empty; the next chunk's render self-heals.
                                let admitted = super::governor::edit_admission(
                                    &bot,
                                    chat,
                                    mid,
                                    super::governor::EditClass::BrainPreview,
                                    display.clone(),
                                    false,
                                )
                                .await;
                                if admitted
                                    && let Err(e) = bot
                                        .edit_message_text(chat, mid, &display)
                                        .parse_mode(ParseMode::Html)
                                        .await
                                {
                                    // Review F10: placeholder edits were fully
                                    // silent; a failing edit stream is now visible.
                                    tracing::warn!(
                                        "Telegram: streaming placeholder edit failed (chat={} msg={}): {}",
                                        chat.0,
                                        mid.0,
                                        e
                                    );
                                }
                            }
                        }

                        // Re-send typing indicator after any bot message
                        fire_chat_action(&bot, chat, thread_id, ChatAction::Typing, "post-message typing").await;
                    }
                }
            }
        }
    })
}
