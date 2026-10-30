//! Streaming progress-event state machine for the Telegram handler
//! (#1086 seam 3). Owns the placeholder-edit lifecycle driven by agent
//! progress events; extracted verbatim from `handle_message`.

use std::sync::Arc;

use teloxide::Bot;
use teloxide::types::{ChatAction, ChatId, ThreadId};

use super::flow::{
    COMPACTING_HEADER_TEXT, DisplayItem, StreamingState, ToolMsg, compacted_flow_line,
    compacting_flow_line, detach_flow_for_followup,
};
use super::handler::tool_context;
use super::send::fire_chat_action;
use crate::brain::agent::{ProgressCallback, ProgressEvent};
use crate::config::Config;

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_progress_cb(
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
) -> ProgressCallback {
    let st = streaming.clone();
    let bot_typing = bot.clone();
    let chat_typing = chat_id;
    Arc::new(move |_sid, event| {
        // The typing pinger upstream refreshes on a timer; a compaction entry
        // fires an immediate one so the indicator visibly resets the moment
        // compaction starts rather than looking stalled through the silent
        // window (#29). It is the only chat-side effect in this mapping, so it
        // stays in the Telegram callback: a caller with no chat to ping (a
        // guest reply, where the bot is not a member of the chat and cannot
        // type at all) still wants the state below and nothing else.
        if matches!(event, ProgressEvent::Compacting { .. }) {
            let bot = bot_typing.clone();
            let chat = chat_typing;
            tokio::spawn(async move {
                fire_chat_action(
                    &bot,
                    chat,
                    thread_id,
                    ChatAction::Typing,
                    "compacting typing refresh",
                )
                .await;
            });
        }
        apply_progress_event(&st, event);
    })
}

/// Fold one progress event into a turn's state.
///
/// Every surface that shows a turn reads the same [`StreamingState`], so the
/// event-to-state mapping lives here once. A caller that renders the state on
/// a different surface (a guest reply edited through `inline_message_id`)
/// gets the same lines, counters and clocks instead of a second copy of this
/// match that drifts from it.
pub(crate) fn apply_progress_event(
    st: &Arc<std::sync::Mutex<StreamingState>>,
    event: ProgressEvent,
) {
    match event {
        // Auto-compaction produces zero streaming chunks for 10-60s.
        // The 4s typing pinger upstream stays alive, but fire an
        // immediate refresh on entry so the indicator visibly resets
        // the moment compaction starts (#29). The start line also lands
        // in the flow body so the silent window has a visible
        // explanation: a fill level, not a progress bar.
        ProgressEvent::Compacting {
            usage_pct,
            predicted,
        } => {
            if let Ok(mut s) = st.lock() {
                // The whole compaction surface is opt-in chrome (#1686),
                // header pin included. Leaving the pin ungated was
                // justified by a staleness worry that does not exist:
                // tick_flow_header advances flow_status from
                // turn_started_at outside the `compacting` branch, so the
                // clock keeps counting through the silent window and the
                // header keeps its last real preview. With the flag off
                // the pin is never set, so a compaction leaves no trace on
                // the block at all. The CompactionSummary arm's lift stays
                // unconditional, so a flag flip mid-window cannot strand
                // a pin.
                if Config::current().agent.compaction_notice {
                    s.compacting = true;
                    s.header_preview = Some(COMPACTING_HEADER_TEXT.to_string());
                    s.display_queue
                        .push(DisplayItem::Intermediate(compacting_flow_line(
                            usage_pct, predicted,
                        )));
                }
            }
        }
        ProgressEvent::ReasoningChunk { text } => {
            if let Ok(mut s) = st.lock() {
                s.thinking.push_str(&text);
                s.dirty = true;
            }
        }
        ProgressEvent::StreamingChunk { text } => {
            if let Ok(mut s) = st.lock() {
                if !s.thinking.is_empty() {
                    s.thinking.clear();
                }
                s.response.push_str(&text);
                s.dirty = true;
                s.processing = false; // first real text = stop rolling messages
            }
        }
        ProgressEvent::ToolStarted {
            tool_name,
            tool_input,
            ..
        } => {
            if let Ok(mut s) = st.lock() {
                s.thinking.clear();
                if s.tools_started_at.is_none() {
                    s.tools_started_at = Some(std::time::Instant::now());
                }
                let ctx = tool_context(&tool_name, &tool_input);
                let raw_ctx = crate::utils::tool_status_source(&tool_name, &tool_input);
                let idx = s.tool_msgs.len();
                s.tool_msgs.push(ToolMsg {
                    msg_id: None,
                    name: tool_name,
                    context: ctx,
                    raw_context: raw_ctx,
                    completed: None,
                    dirty: true,
                    task_id: None,
                });
                s.display_queue.push(DisplayItem::NewTool(idx));
            }
        }
        ProgressEvent::ToolCompleted {
            tool_name, success, ..
        } => {
            if let Ok(mut s) = st.lock() {
                s.tool_round_count += 1;
                if let Some(tool) = s
                    .tool_msgs
                    .iter_mut()
                    .rev()
                    .find(|t| t.name == tool_name && t.completed.is_none())
                {
                    tool.completed = Some(success);
                    tool.dirty = true;
                }
                // No recreate here (#299): a completion only edits the
                // open group block in place — nothing new lands below
                // the placeholder. The re-post happens where a message
                // is actually SENT (fresh group in append_tool_group,
                // and the IntermediateText arm below).
            }
        }
        // Detached background task (#2147): draw it as ONE row inside the
        // turn's existing tool block, so a task that outlives its turn is
        // visible while it runs instead of appearing only as a receipt later.
        ProgressEvent::DetachedTaskStarted { task_id, label } => {
            if let Ok(mut s) = st.lock() {
                s.thinking.clear();
                if s.tools_started_at.is_none() {
                    s.tools_started_at = Some(std::time::Instant::now());
                }
                let idx = s.tool_msgs.len();
                s.tool_msgs.push(ToolMsg {
                    msg_id: None,
                    name: label,
                    context: String::new(),
                    raw_context: String::new(),
                    completed: None,
                    dirty: true,
                    task_id: Some(task_id),
                });
                s.display_queue.push(DisplayItem::NewTool(idx));
            }
        }
        // The finish only settles the row it started. It deliberately pushes
        // NOTHING onto `display_queue`: a display item is what causes a new
        // message, and the whole point (#2147) is that start and finish share
        // one message id. Setting `dirty` is what the edit loop picks up to
        // rewrite the existing block in place.
        //
        // A finish with no matching row is not an error: the process may have
        // restarted, or the turn ended and the block settled. Since #2218 the
        // receipt push is the fallback for a task the manager holds no
        // callback for, so this arm must not assume a push will follow: it
        // stays silent rather than opening a duplicate block.
        ProgressEvent::DetachedTaskFinished {
            task_id, success, ..
        } => {
            if let Ok(mut s) = st.lock()
                && let Some(tool) = s
                    .tool_msgs
                    .iter_mut()
                    .find(|t| t.task_id.as_deref() == Some(task_id.as_str()))
            {
                tool.completed = Some(success);
                tool.dirty = true;
            }
        }
        ProgressEvent::QueuedUserMessage { .. } => {
            // The user's own message is already visible in the chat;
            // the block just has to stop growing above it (#404).
            detach_flow_for_followup(st);
        }
        ProgressEvent::IntermediateText { text, reasoning: _ } => {
            if let Ok(mut s) = st.lock() {
                s.thinking.clear();
                // Clear accumulated streaming response — it's now captured
                // as an intermediate message. Without this, text from
                // consecutive tool rounds gets concatenated without spacing.
                s.response.clear();
                // Delete the streaming message so stale text doesn't linger
                if s.msg_id.is_some() {
                    s.recreate = true;
                }
                // Never push reasoning as a standalone intermediate — it
                // belongs in the streaming response's 💭 thinking block.
                // Using reasoning as a fallback here causes duplicate
                // messages on Telegram (reasoning intermediate + final
                // response that doesn't contain the reasoning text, so
                // dedup can't strip it).
                if !text.is_empty() {
                    s.display_queue.push(DisplayItem::Intermediate(text));
                }
            }
        }
        ProgressEvent::SelfHealingAlert { message } => {
            if let Ok(mut s) = st.lock() {
                s.display_queue.push(DisplayItem::System(format!(
                    "🔧 {}",
                    crate::utils::sanitize::normalize_dashes(&message)
                )));
            }
        }
        ProgressEvent::RetryAttempt {
            attempt,
            max,
            reason,
        } => {
            if let Ok(mut s) = st.lock() {
                s.display_queue.push(DisplayItem::System(format!(
                    "⏳ Retry {}/{} — {}",
                    attempt, max, reason
                )));
            }
        }
        ProgressEvent::ProviderSwitched {
            to_name, to_model, ..
        } => {
            if let Ok(mut s) = st.lock() {
                s.display_queue.push(DisplayItem::System(format!(
                    "🔄 Now using {}/{}",
                    to_name, to_model
                )));
            }
        }
        // Optional follow-up suggestions (#597): post tap-to-send
        // buttons under the response. Non-blocking — spawned like the
        // other async arms; a tap injects the suggestion as a new turn.
        ProgressEvent::SuggestedOptions(options) => {
            // Buffer the options and render AFTER the final delivery so the
            // buttons are always the last thing in the chat, and the stash
            // is set fresh at turn end (#724 / #723). Only the latest set
            // is kept if the tool fires more than once.
            if let Ok(mut s) = st.lock() {
                s.pending_suggestions = Some(options);
            }
        }
        // Compaction finished — the ✅ line is the definitive "silent
        // window over" signal (#29). Arrival means success: a failed
        // compaction never emits this event, so no false receipts.
        ProgressEvent::CompactionSummary {
            before_pct,
            after_pct,
            elapsed,
            ..
        } => {
            if let Ok(mut s) = st.lock() {
                // Lift the header pin FIRST so the next tick recomputes
                // from live data; the ✅ line leads the body until newer
                // activity arrives (#29).
                s.compacting = false;
                s.header_preview = None;
                // Lifting the pin stays unconditional while setting it is
                // gated (#1686): set-gated / lift-ungated means a flag flip
                // inside the compaction window can never strand a pin that
                // nothing will clear. The lift is what lets the next tick
                // recompute the header from live data.
                if Config::current().agent.compaction_notice {
                    s.display_queue
                        .push(DisplayItem::Intermediate(compacted_flow_line(
                            before_pct, after_pct, elapsed,
                        )));
                }
            }
        }
        _ => {}
    }
}
