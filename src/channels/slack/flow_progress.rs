//! Live flow progress for a Slack turn (#1838, #1988).
//!
//! One `ProgressCallback` that folds tool steps, narration and notices into
//! the turn's tool-group message. The follow-up tap and the background
//! resume both drive their turn through it, so a resumed answer shows the
//! same live steps as a message the user typed.

use super::SlackState;
use super::governor::GatedWrites;
use super::handler::sync_step_group;
use super::options_host::{drain_note_tasks, post_halted_answer};
use super::tool_group::{GroupEntry, GroupState};
use crate::brain::agent::ProgressEvent;
use slack_morphism::prelude::*;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared state a turn's progress callback writes into.
pub(crate) struct FlowProgress {
    pub(crate) cb: crate::brain::agent::ProgressCallback,
    pub(crate) steps: Arc<Mutex<Vec<GroupEntry>>>,
    pub(crate) intermediate_handles: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    /// Answers posted before the buttons (see `options_host`).
    pub(crate) posted: Arc<Mutex<Vec<String>>>,
}

/// Build the progress callback for one turn.
///
/// `thread_ts` threads the group message into the conversation the turn
/// started in; `None` posts at channel root.
pub(crate) fn build_flow_progress(
    client: Arc<SlackHyperClient>,
    slack_state: Arc<SlackState>,
    channel: SlackChannelId,
    thread_ts: Option<SlackTs>,
    turn_group_ts: Arc<Mutex<Option<SlackTs>>>,
    bot_token: String,
) -> FlowProgress {
    let steps: Arc<Mutex<Vec<GroupEntry>>> = Arc::new(Mutex::new(Vec::new()));
    // Track IntermediateText spawns so the group is fully updated before the
    // answer posts (handle_message #943 ordering).
    let intermediate_handles: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let posted: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let cb: crate::brain::agent::ProgressCallback = {
        let tools = steps.clone();
        let group_ts = turn_group_ts.clone();
        let channel = channel.clone();
        let client = client.clone();
        let slack_state_grp = slack_state.clone();
        let thread = thread_ts.clone();
        let handles = intermediate_handles.clone();
        let posted = posted.clone();

        Arc::new(move |session_id, event| {
            let tools = tools.clone();
            let group_ts = group_ts.clone();
            let token = SlackApiToken::new(SlackApiTokenValue::from(bot_token.clone()));
            let channel = channel.clone();
            let client = client.clone();
            let slack_state_grp = slack_state_grp.clone();
            match event {
                ProgressEvent::ToolStarted {
                    tool_name,
                    tool_input,
                    ..
                } => {
                    let ctx = crate::utils::tool_context_hint(&tool_name, &tool_input);
                    let thread = thread.clone();
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let entries = {
                            let mut t = tools.lock().await;
                            t.push(GroupEntry::Tool {
                                name: tool_name,
                                context: ctx,
                                status: None,
                                task_id: None,
                            });
                            t.clone()
                        };
                        sync_step_group(
                            &session,
                            &slack_state_grp,
                            channel,
                            thread.clone(),
                            &group_ts,
                            entries,
                        )
                        .await;
                    });
                }
                ProgressEvent::ToolCompleted {
                    tool_name, success, ..
                } => {
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let entries = {
                            let mut t = tools.lock().await;
                            if let Some(GroupEntry::Tool { status, .. }) =
                                t.iter_mut().rev().find(|e| {
                                    matches!(
                                        e,
                                        GroupEntry::Tool { name, status: None, .. }
                                            if *name == tool_name
                                    )
                                })
                            {
                                *status = Some(success);
                            }
                            t.clone()
                        };
                        if let Some(ts) = group_ts.lock().await.as_ref() {
                            let group = slack_state_grp
                                .upsert_tool_group(
                                    ts.to_string(),
                                    GroupState::new(channel.clone(), entries),
                                )
                                .await;
                            let content = super::tool_group::render(&group, ts);
                            let upd = SlackApiChatUpdateRequest::new(channel, content, ts.clone());
                            if let Some(Err(e)) = session.update_chrome(&upd).await {
                                tracing::warn!(
                                    "Slack tap: chat_update failed (tool group status, ts={ts}): {e}"
                                );
                            }
                        }
                    });
                }
                ProgressEvent::IntermediateText { text, .. } => {
                    // Same cleaning as handle_message's fold path: artifacts
                    // and media markers must not leak into the note.
                    let text = crate::utils::sanitize::sanitize_outbound(&text);
                    let (text_clean, _img) = crate::utils::extract_img_markers(&text);
                    let (text_clean, _vid) = crate::utils::extract_vid_markers(&text_clean);
                    if text_clean.trim().is_empty() {
                        return;
                    }
                    let thread = thread.clone();
                    let handle = tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let text_fmt = crate::utils::slack_fmt::markdown_to_mrkdwn(&text_clean);
                        let entries = {
                            let mut t = tools.lock().await;
                            t.push(GroupEntry::Note(text_fmt));
                            t.clone()
                        };
                        sync_step_group(
                            &session,
                            &slack_state_grp,
                            channel,
                            thread.clone(),
                            &group_ts,
                            entries,
                        )
                        .await;
                    });
                    if let Ok(mut g) = handles.lock() {
                        g.push(handle);
                    }
                }
                ProgressEvent::DetachedTaskStarted { task_id, label } => {
                    // One row, updated in place (#2149). The start posts the
                    // block when the turn has none yet; the finish only ever
                    // rewrites, so a long task cannot leave two bubbles.
                    let thread = thread.clone();
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        apply_detached(
                            &session,
                            &slack_state_grp,
                            channel,
                            thread,
                            &group_ts,
                            &tools,
                            super::tool_group::DetachedEvent::Started {
                                task_id: &task_id,
                                label: &label,
                            },
                        )
                        .await;
                    });
                }
                ProgressEvent::DetachedTaskFinished {
                    task_id, success, ..
                } => {
                    let thread = thread.clone();
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        apply_detached(
                            &session,
                            &slack_state_grp,
                            channel,
                            thread,
                            &group_ts,
                            &tools,
                            super::tool_group::DetachedEvent::Finished {
                                task_id: &task_id,
                                success,
                            },
                        )
                        .await;
                    });
                }
                ProgressEvent::SelfHealingAlert { message } => {
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let text =
                            format!("🔧 {}", crate::utils::sanitize::normalize_dashes(&message));
                        let req = SlackApiChatPostMessageRequest::new(
                            channel,
                            SlackMessageContent::new().with_text(text),
                        );
                        if let Err(e) = session.post(&req).await {
                            tracing::warn!(error = %e, "Slack tap: self-healing alert post failed");
                        }
                    });
                }
                ProgressEvent::RetryAttempt {
                    attempt,
                    max,
                    reason,
                } => {
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let text = format!("⏳ Retry {attempt}/{max}: {reason}");
                        let req = SlackApiChatPostMessageRequest::new(
                            channel,
                            SlackMessageContent::new().with_text(text),
                        );
                        if let Err(e) = session.post(&req).await {
                            tracing::warn!(error = %e, "Slack tap: retry notice post failed");
                        }
                    });
                }
                ProgressEvent::ProviderSwitched {
                    to_name, to_model, ..
                } => {
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let text = format!("🔄 Now using {to_name}/{to_model}");
                        let req = SlackApiChatPostMessageRequest::new(
                            channel,
                            SlackMessageContent::new().with_text(text),
                        );
                        if let Err(e) = session.post(&req).await {
                            tracing::warn!(error = %e, "Slack tap: provider switch post failed");
                        }
                    });
                }
                // Chained suggestions (#599): buttons under the tap's answer
                // resolve their channel from the session route.
                ProgressEvent::SuggestedOptions(options) => {
                    let raw_options: Vec<String> =
                        options.into_iter().map(|item| item.label).collect();
                    let handles = handles.clone();
                    let posted = posted.clone();
                    let thread = thread.clone();
                    tokio::spawn(async move {
                        // The answer the surface halted on posts before the
                        // buttons, out of the group (#2029).
                        drain_note_tasks(&handles).await;
                        let session = client.open_session(&token);
                        post_halted_answer(
                            &session,
                            &slack_state_grp,
                            channel,
                            thread,
                            &group_ts,
                            &tools,
                            &posted,
                        )
                        .await;
                        super::suggest_options::render_suggestions(
                            &slack_state_grp,
                            session_id,
                            raw_options,
                        )
                        .await;
                    });
                }
                _ => {}
            }
        })
    };

    FlowProgress {
        cb,
        steps,
        intermediate_handles,
        posted,
    }
}

/// Apply one detached-task event to the turn's group message (#2149).
///
/// The decision comes from the pure [`super::tool_group::detached_render`], so
/// the "one message per task" contract is the same one the tests drive without
/// a bot. This function only performs what that decision says.
async fn apply_detached<'a>(
    session: &SlackClientSession<'a, slack_morphism::hyper_tokio::SlackClientHyperHttpsConnector>,
    slack_state: &Arc<SlackState>,
    channel: SlackChannelId,
    thread_ts: Option<SlackTs>,
    group_ts: &Arc<Mutex<Option<SlackTs>>>,
    tools: &Mutex<Vec<GroupEntry>>,
    event: super::tool_group::DetachedEvent<'_>,
) {
    let (action, entries) = {
        let mut t = tools.lock().await;
        let ts = group_ts.lock().await.as_ref().map(|ts| ts.to_string());
        let action = super::tool_group::detached_render(&mut t, ts.as_deref(), event);
        (action, t.clone())
    };
    match action {
        super::tool_group::DetachedRender::Post => {
            sync_step_group(session, slack_state, channel, thread_ts, group_ts, entries).await;
        }
        super::tool_group::DetachedRender::Edit(ts) => {
            let ts = SlackTs::new(ts);
            let group = slack_state
                .upsert_tool_group(ts.to_string(), GroupState::new(channel.clone(), entries))
                .await;
            let content = super::tool_group::render(&group, &ts);
            let upd = SlackApiChatUpdateRequest::new(channel, content, ts.clone());
            if let Some(Err(e)) = session.update_chrome(&upd).await {
                tracing::warn!("Slack: chat_update failed (detached row, ts={ts}): {e}");
            }
        }
        super::tool_group::DetachedRender::Skip => {}
    }
}
