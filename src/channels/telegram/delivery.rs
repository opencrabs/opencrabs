//! Final-response delivery: the ONE path every Telegram turn ends on
//! (#471 phases 2-4). Live turns (handle_message) and crash-recovery
//! resumes both call deliver_final_response; the post-loop display drain
//! lives here with it.

use super::TelegramState;
use super::flow::{
    DisplayItem, StreamingState, append_intermediate_to_flow, append_system_to_flow,
    append_tool_group, folded_duplicates_final, last_folded_text, settle_options_reclaim,
    take_folded_final,
};
use super::handler::{fire_reaction, map_to_allowed_reaction};
use super::intermediates::send_html_or_plain;
use super::markdown::{markdown_to_telegram_html, split_message};
use super::send::{
    best_effort_delete, document_in_thread, message_in_thread, photo_in_thread, voice_in_thread,
};
use crate::brain::agent::AgentService;
use crate::db::ChannelMessageRepository;
use crate::db::models::ChannelMessage as DbChannelMessage;
use crate::utils::sanitize::redact_secrets;
use std::sync::Arc;
use teloxide::payloads::SendDocumentSetters;
use teloxide::prelude::*;
use teloxide::types::{InputFile, MessageId, ParseMode};
use uuid::Uuid;

/// Whether a turn carrying a `<<react:…>>` directive is react-ONLY, given the
/// text that remains once the directive is stripped.
///
/// The prompt teaches the model two shapes (see `handler.rs`, "Reaction
/// directive"): output ONLY the directive to react-only, or put the directive
/// at the START and answer after it to react AND respond. Emptiness of the
/// remaining text is therefore the whole question, and the answer is on the
/// wire rather than inferred.
///
/// Deliberately takes no other input. #928 also consulted whether the turn ran
/// tools and suppressed the text when it had not, which destroyed completions
/// that simply needed no tools (#1009). Tool state does not tell you what the
/// content channel holds, so it is not a parameter here.
pub(crate) fn is_react_only(text_after_directive: &str) -> bool {
    text_after_directive.trim().is_empty()
}

/// Whether the turn is genuinely react-only, as opposed to merely text-empty.
///
/// Emptiness alone does not answer it. A final suppressed by dedup (#1152) is
/// also text-empty at this point, but it was already delivered as intermediate
/// bubbles, so treating it as react-only fires the early return and the #546
/// incomplete-turn notice on top of work the user already received.
///
/// Named rather than inlined at the call site so the distinction has one
/// definition and a test can exercise it: asserting the expression by
/// rebuilding it in a test body proves nothing about the delivery path.
pub(crate) fn is_react_only_turn(suppressed_final: bool, text_after_directive: &str) -> bool {
    !suppressed_final && is_react_only(text_after_directive)
}

/// Whether the turn ends on a suggest_options surface (#1226 K): the
/// progress handler stashes the options mid-turn (progress.rs), so by
/// delivery time a Some here means an option surface is armed and the
/// flow block ends on the suggest_options Tool entry — the reclaim calls
/// below must then look BEFORE that trailing tool for the answer.
fn options_pending(streaming: &Arc<std::sync::Mutex<StreamingState>>) -> bool {
    streaming
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_suggestions
        .is_some()
}

/// Which intermediate bubbles does the final answer already deliver (#2156)?
///
/// Pure over the pairs so a test can assert on ids: the reported turn must
/// yield the narration bubble and never the final bubble.
///
/// `final_id` is excluded from the candidates by construction, not by text
/// comparison: the final bubble carries the answer verbatim, so a text-only
/// predicate would flag it as a duplicate of itself and delete the very bubble
/// the turn exists to deliver.
pub(crate) fn duplicate_intermediate_ids(
    final_text: &str,
    final_id: MessageId,
    bubbles: &[(MessageId, String)],
) -> Vec<MessageId> {
    bubbles
        .iter()
        .filter(|(id, _)| *id != final_id)
        .filter(|(_, text)| super::flow::answer_already_delivers(final_text, text))
        .map(|(id, _)| *id)
        .collect()
}

/// Delete the intermediate bubbles the final answer already delivers (#2156).
///
/// The dedup ladder above suppresses the FINAL when an intermediate equals it.
/// A prefix never matched that comparison, so a 1962-char narration bubble
/// survived above a 3761-char answer that opened with the same words. The
/// answer is the complete copy, so the narration bubble is the duplicate and it
/// is the one that goes.
///
/// Media-bearing intermediates are never touched: their bubble is the only copy
/// of the documents it carries (#1939).
///
/// Returns the ids deleted, so a caller (and a test) can assert on addresses
/// rather than on a count.
pub(crate) async fn prune_delivered_intermediates(
    bot: &Bot,
    chat_id: ChatId,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    final_text: &str,
    final_id: MessageId,
) -> Vec<MessageId> {
    let doomed: Vec<MessageId> = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        duplicate_intermediate_ids(final_text, final_id, &s.intermediate_bubbles)
    };
    for mid in &doomed {
        best_effort_delete(bot, chat_id, *mid, "duplicate intermediate").await;
    }
    if !doomed.is_empty() {
        tracing::info!(
            "Telegram: deleted {} intermediate bubble(s) the final already delivers (#2156)",
            doomed.len()
        );
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        s.intermediate_bubbles
            .retain(|(id, _)| !doomed.contains(id));
    }
    doomed
}

/// True when a rich-send failure means Telegram could not fetch the embedded
/// media (the mermaid.ink diagram) — a transient renderer or network window
/// that a single re-send can sail (#tg-mermaid-delivery-hardening). Our
/// resolve runs seconds before Telegram's own server-side refetch, so
/// `RICH_MESSAGE_PHOTO_NO_MEDIA_FOUND` is a race with a flaky renderer, not a
/// structural rejection. Structural 400s (schema, content) are never retried.
pub(crate) fn is_no_media_found(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("RICH_MESSAGE_PHOTO_NO_MEDIA_FOUND")
}

/// Drain all pending intermediate texts from the streaming state's display
/// queue and send them immediately. Called by the follow-up-question callback
/// BEFORE posting the question message, so the user sees contextual text
/// above the buttons instead of below (race reported in issue #142).
///
/// Applies the same sanitize/redact/dedup/split/send chain as the edit loop.
/// Deliver the final agent response for a live inbound turn: marker
/// extraction, sanitize, react directive, dedup, reaction-only handling,
/// folded-final reclaim, footer, rich-first send with HTML fallback,
/// chunked sends, history recording, TTS. Extracted VERBATIM from
/// handle_message (#471 phase 2) — the only edit is early
/// `return Ok(())` becoming `return Ok(false)` so the caller can
/// preserve handle_message's original control flow exactly.
/// Background-task and sub-agent counting for the settled flow moved to the
/// shared [`crate::channels::background_work`] module (#1985); re-exported so
/// the handler's `pub(crate) use super::delivery::{..}` and resume's
/// `super::delivery::` paths keep working unchanged.
pub(crate) use crate::channels::background_work::{bg_indicator_for, subagent_counts_for};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn deliver_final_response(
    bot: &Bot,
    chat_id: ChatId,
    // The inbound message this turn answers: reaction target and reply
    // anchor. None on the crash-recovery resume path, where the original
    // message id is lost across restarts — reactions strip without firing.
    inbound: Option<&Message>,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    session_id: Uuid,
    agent: &Arc<AgentService>,
    telegram_state: &Arc<TelegramState>,
    channel_msg_repo: &ChannelMessageRepository,
    voice_config: &crate::config::VoiceConfig,
    is_voice: bool,
    is_dm: bool,
    chat_title: &str,
    mut streaming_msg_id: Option<MessageId>,
    result: Result<crate::brain::agent::AgentResponse, crate::brain::agent::AgentError>,
) -> ResponseResult<bool> {
    match result {
        Ok(response) => {
            // Merge candidate captured below: whichever bubble carried the final
            // response (classic HTML edit/send, or table-free rich message).
            // render_suggestions attaches its keyboard to THIS bubble when Some.
            let mut final_bubble: Option<super::state::MergeBubble> = None;
            // Extract <<IMG:path>> markers — send each as a Telegram photo.
            let (text_only, img_paths) = crate::utils::extract_img_markers(&response.content);
            // Strip LLM-hallucinated artifacts (<!-- tools-v2 -->, XML tool blocks)
            let text_only = crate::utils::sanitize::strip_llm_artifacts(&text_only);
            let text_only = redact_secrets(&text_only);
            // #1916: after secret redaction, scan the reply for markdown
            // links to local files. The session working directory is the
            // base a relative target resolves against; scan-cleared text is
            // adopted via partial move so the failure list stays available
            // for the notice below.
            let session_cwd = agent.get_working_directory_for_session(session_id);
            let mut file_scan =
                crate::utils::image::extract_local_files(&text_only, Some(session_cwd.as_path()));
            let text_only = std::mem::take(&mut file_scan.text);

            // Drop an echoed plan title (#837). The reminder shows the model
            // the title every turn and it opens by repeating it, directly
            // under the card that already renders it.
            //
            // Covers Editing as well as Active: #621 folded title and prose
            // into the card in BOTH states, so limiting this to Active left
            // the duplicate visible for every plan still being drafted.
            let text_only = match crate::utils::plan_files::load_plan(session_id).await {
                Some(plan) if !plan.title.trim().is_empty() => {
                    strip_echoed_plan_title(&text_only, &plan.title)
                }
                _ => text_only,
            };

            // Extract <<react:emoji>> directive — the LLM outputs this to
            // signal a reaction-only response (no text bubble). If the
            // response is ONLY a reaction, the emoji is sent as a Telegram
            // reaction on the user's message and text delivery is skipped.
            let (text_only, react_emoji) = crate::utils::extract_react_marker(&text_only);

            // Dedup: strip text that was already sent as intermediate messages
            // to avoid duplicating content on Telegram. An intermediate chunk
            // that already carries the final answer (e.g. "Done. Uploaded to
            // Drive: https://…") will otherwise be repeated when the
            // streaming placeholder is edited with the final response.
            // Intermediates stay visible as-is; only the streaming
            // placeholder's final text is pruned.
            let sent = {
                let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                s.sent_intermediates.clone()
            };
            tracing::info!(
                "Telegram dedup: response.content len={}, sent_intermediates count={}",
                text_only.len(),
                sent.len(),
            );
            let pre_dedup_text = text_only.clone();
            // Normalize whitespace for comparison — collapse runs of
            // whitespace (including newlines) to single spaces so that
            // minor formatting differences between the streamed
            // intermediate and the final response don't bypass dedup.
            let norm = |s: &str| -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") };
            let mut suppressed_final = false;
            let text_only = if !sent.is_empty() {
                let norm_final = norm(&text_only);
                if sent.iter().any(|i| norm(i) == norm_final) {
                    tracing::info!(
                        "Telegram dedup: match found among {} intermediates (normalized) — suppressing final response",
                        sent.len()
                    );
                    suppressed_final = true;
                    String::new()
                } else {
                    text_only
                }
            } else {
                text_only
            };

            // Reclaim a folded answer BEFORE the reaction decision (#478):
            // the completion can stream mid-turn as IntermediateText and
            // fold into the flow block. When the final text is empty, the
            // trailing folded run IS the answer — pulling it out here means
            // a closing react directive becomes a reaction ON TOP of the
            // delivered completion, instead of the reaction-only skip
            // imprisoning the answer inside the Processing log block.
            //
            // UNLESS the final was just suppressed by dedup (#1152): the
            // trailing folded run then belongs to the SAME answer, which
            // already went out as intermediate bubbles. Reclaiming it here
            // re-ships an orphan duplicate fragment (62-char tail under the
            // full answer). `folded_duplicates_final` cannot arbitrate this:
            // streaming may leave a SUFFIX chunk folded in the block, and
            // that predicate deliberately only matches prefix overlap. So
            // when suppression happened, strip the folded copy from the
            // block silently — it is already delivered.
            //
            // #31: tracks whether THIS block already reclaimed the answer, so
            // the main reclaim below can tell "nothing left to reclaim" from
            // "the reclaim found nothing that was ever there" in its K warn.
            let mut pre_reclaimed = false;
            let text_only = if text_only.trim().is_empty() {
                if suppressed_final {
                    let (discarded, discarded_trailer) =
                        take_folded_final(bot, chat_id, streaming, options_pending(streaming))
                            .await;
                    if discarded.is_some() || discarded_trailer.is_some() {
                        tracing::info!(
                            "Telegram: final suppressed by dedup — dropping {}(+{}) folded \
                             chars already delivered as intermediates (#1152)",
                            discarded.as_ref().map(|t| t.len()).unwrap_or(0),
                            discarded_trailer.as_ref().map(|t| t.len()).unwrap_or(0),
                        );
                    }
                    text_only
                } else {
                    let (reclaimed, trailer) =
                        take_folded_final(bot, chat_id, streaming, options_pending(streaming))
                            .await;
                    if let Some(t) = trailer {
                        let trailer_len = t.len();
                        // #31: the post-halt sign-off rides AFTER the buttons —
                        // stash it for render_suggestions (keep-never-discard).
                        streaming
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .pending_trailer = Some(t);
                        tracing::info!(
                            "Telegram: stashed {} trailer chars reclaimed before the react \
                             decision (#31)",
                            trailer_len
                        );
                    }
                    match reclaimed {
                        Some(reclaimed) => {
                            pre_reclaimed = true;
                            tracing::info!(
                                "Telegram: reclaimed folded final ({} chars) before react \
                                 decision (#478)",
                                reclaimed.len()
                            );
                            reclaimed
                        }
                        None => text_only,
                    }
                }
            } else {
                // #2156: the final is non-empty, so it ships as its own bubble.
                // A trailing folded run that the final already delivers must not
                // stay in the block, or the turn ends with the answer twice: the
                // resticked block above and the fresh bubble below.
                let (redundant, trailer) = super::flow::take_folded_final_if_redundant(
                    bot,
                    chat_id,
                    streaming,
                    options_pending(streaming),
                    &text_only,
                )
                .await;
                if let Some(t) = trailer {
                    let trailer_len = t.len();
                    streaming
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pending_trailer = Some(t);
                    tracing::info!(
                        "Telegram: stashed {trailer_len} trailer chars reclaimed from the \
                         block before delivery (#2156)"
                    );
                }
                if let Some(r) = redundant {
                    tracing::info!(
                        "Telegram: block trailing run ({} chars) is already delivered by \
                         the final answer, reclaimed from the block (#2156)",
                        r.len()
                    );
                }
                text_only
            };

            // Reaction directive: if the LLM included <<react:emoji>>, send
            // a reaction on the user's message. For reaction-only responses
            // (empty text after stripping the directive), skip all text/TTS
            // delivery and just react — but ONLY when the turn did no tool
            // work (#439): a turn that executed tools and ended with a bare
            // reaction dropped its whole completion (issues were closed and
            // commented, the user saw only 🔥). For a work turn, empty final
            // text is a failure mode, never a deliberate ack.
            let turn_ran_tools = {
                let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                !s.tool_msgs.is_empty()
            };
            if let Some(ref emoji) = react_emoji {
                let mapped = map_to_allowed_reaction(emoji);
                let reaction = teloxide::types::ReactionType::Emoji {
                    emoji: mapped.clone(),
                };
                super::telemetry::log_request(
                    "turn",
                    "delivery reaction",
                    &session_id.to_string(),
                    "reaction",
                    "setMessageReaction",
                    chat_id.0,
                    None,
                    inbound.map(|m| i64::from(m.id.0)),
                );
                let react_result = match inbound {
                    Some(m) => bot
                        .set_message_reaction(chat_id, m.id)
                        .reaction(vec![reaction])
                        .is_big(false)
                        .await
                        .map(|_| ()),
                    // Resume path: the original message is gone, nothing to
                    // react to — treat as delivered so no fallback fires.
                    None => Ok(()),
                };
                if let Err(ref e) = react_result {
                    tracing::warn!("Telegram: failed to set reaction ({mapped}): {}", e);
                }
                // `!suppressed_final` (#1152): a final suppressed by dedup was
                // not dropped — it shipped early as intermediate bubbles. That
                // is delivery, not the failure mode #439 guards against, so no
                // synthetic "Done — X/Y tool calls" summary on top of it.
                if text_only.trim().is_empty() && turn_ran_tools && !suppressed_final {
                    // Work turn with no completion text (#439): the model
                    // replaced its summary with a reaction. Deliver a
                    // fallback completion so the work is reported — the
                    // reaction already landed above.
                    tracing::warn!(
                        "Telegram: turn executed tools but produced no completion text — \
                         delivering fallback summary instead of reaction-only skip (#439)"
                    );
                    let fallback = {
                        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                        let done = s
                            .tool_msgs
                            .iter()
                            .filter(|t| t.completed == Some(true))
                            .count();
                        format!(
                            "Done — {done}/{} tool calls completed. (The model ended the turn \
                             without a summary; see the log above for what ran.)",
                            s.tool_msgs.len()
                        )
                    };
                    if let Err(e) = message_in_thread(bot, chat_id, thread_id, &fallback).await {
                        tracing::error!("Telegram: fallback completion send failed: {}", e);
                    }
                    if let Some(mid) = streaming_msg_id {
                        best_effort_delete(bot, chat_id, mid, "fallback send cleanup").await;
                    }
                    return Ok(false);
                }
                // React-only means exactly what the prompt says it means: ONLY
                // the directive, nothing after it. The contract taught to the
                // model (handler.rs, "Reaction directive") states both shapes:
                //
                //   react-only        -> output ONLY the directive
                //   react AND respond -> directive at the START, then the text
                //
                // so text after the directive IS the documented second shape,
                // and delivering it is the contract being honoured rather than
                // violated.
                //
                // #928 suppressed that text whenever the turn ran no tools, on
                // the theory that a react turn has no answer in it and anything
                // present must be reasoning spilled into the content channel.
                // The tool state was standing in for "is this reasoning", and
                // it does not answer that question: a turn that answers from
                // analysis alone runs no tools, which is the ordinary shape of
                // an explanation or a follow-up. #1009 is four consecutive
                // completions destroyed that way, none of which contained any
                // reasoning at all.
                //
                // Reasoning that reaches the content channel is a provider-side
                // defect (#760) and belongs where it originates. Delivery must
                // not delete a completion on suspicion, because that failure is
                // silent and total while a leak is merely ugly.
                //
                // `!suppressed_final` (#1152, fork #293): a final suppressed by
                // dedup was already delivered as intermediate bubbles. That is
                // delivery, not a react-only turn — do not trigger the
                // react-only early return or the spurious #546 incomplete-turn
                // notice on top of delivered work.
                if is_react_only_turn(suppressed_final, &text_only) {
                    // Never-silent guard (#353): a reaction-only turn whose
                    // reaction FAILED must degrade to text, not to nothing.
                    if react_result.is_err() {
                        tracing::warn!(
                            "Telegram: reaction-only turn with failed reaction — \
                             delivering the emoji as text instead"
                        );
                        if let Err(e) =
                            message_in_thread(bot, chat_id, thread_id, emoji.as_str()).await
                        {
                            tracing::error!("Telegram: emoji text fallback also failed: {}", e);
                        }
                    } else {
                        // A genuine ack (praise, "got it") completes in seconds.
                        // A react-only turn that instead took MINUTES engaged
                        // with real work, ran no tools, and then bailed with
                        // only a reaction — a dropped request, not an ack (#546:
                        // "executing a task for 5m, it reacts and just stops").
                        // #439 only guards the tools-ran case; this catches the
                        // reasoned-but-no-tools case. Delivery can't re-run the
                        // model, so surface it as an incomplete turn instead of
                        // a silent drop, so the user knows to re-send.
                        let elapsed = {
                            let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                            s.turn_started_at.elapsed()
                        };
                        // This branch is now reached only with an empty content
                        // channel, so the turn produced a reaction and nothing
                        // else. #928's carve-out for prose landing here is gone
                        // with the suppression it guarded (#1009): prose no
                        // longer routes into this branch at all, it is
                        // delivered.
                        if elapsed >= std::time::Duration::from_secs(60) {
                            tracing::warn!(
                                "Telegram: react-only after {}s with no tools and no text — \
                                 surfacing as an incomplete turn, not a silent drop (#546)",
                                elapsed.as_secs()
                            );
                            let notice = "⚠️ I ended that turn with only a reaction and did not \
                                          complete the request. Please re-send it if you needed \
                                          me to act.";
                            if let Err(e) = message_in_thread(bot, chat_id, thread_id, notice).await
                            {
                                tracing::error!(
                                    "Telegram: #546 incomplete-turn notice failed: {}",
                                    e
                                );
                            }
                        } else {
                            tracing::info!(
                                "Telegram: reaction-only response ({}), skipping text delivery",
                                mapped
                            );
                        }
                    }
                    // A react-only turn ran no tools (the tools-with-no-text
                    // case returned above, #439), so any open processing-log
                    // block is header-only — the model's thinking preview plus
                    // persistent plan chrome. Left up, it reads as a "Processing
                    // log" bubble with no answer, i.e. a dropped request (#544).
                    // Remove it and clear its state so ONLY the reaction remains;
                    // the plan state persists independently (/show-plan still
                    // works).
                    let flow_mid = {
                        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                        s.flow_entries.clear();
                        s.open_group_msg_id.take()
                    };
                    if let Some(mid) = flow_mid {
                        best_effort_delete(bot, chat_id, mid, "flow teardown").await;
                        // #1377: the card is gone — drop the fold handle so a
                        // later completion falls back to the bubble lane
                        // instead of editing a deleted message.
                        telegram_state.clear_flow_state(session_id).await;
                    }
                    if let Some(mid) = streaming_msg_id {
                        best_effort_delete(bot, chat_id, mid, "streaming teardown").await;
                    }
                    return Ok(false);
                }
            }

            // Context budget footer is display-only chrome: it rides the
            // settled flow message as a section, never the final answer, and
            // is NOT stored in the session/messages table or used for TTS.
            let ctx_max = agent.context_limit_for_session(session_id);
            let footer = crate::utils::format_ctx_footer(
                response.context_tokens,
                ctx_max,
                response.tokens_per_second,
            );
            // Quiet ❕ while the #909 pressure hint is active (#29): the
            // settled footer mirrors the nudge that's in the prompt. Cleared
            // on compaction success; re-arms below the 55% floor.
            let footer = if !footer.is_empty() && agent.pressure_warning_active(session_id) {
                format!("{footer} ❕")
            } else {
                footer
            };
            {
                let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                s.sections.ctx = (!footer.is_empty()).then(|| footer.clone());
            }

            for img_path in img_paths {
                match tokio::fs::read(&img_path).await {
                    Ok(bytes) => {
                        if let Err(e) =
                            photo_in_thread(bot, chat_id, thread_id, InputFile::memory(bytes)).await
                        {
                            tracing::error!("Telegram: failed to send generated image: {}", e);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Telegram: failed to read image {}: {}", img_path, e);
                    }
                }
            }

            // #1916: ship every scanned local-file link as a document bubble.
            // Read or send failures come back on the SAME link rather than as
            // a log-only error, so the model learns the reference produced
            // nothing. Refusals land in `text_only` as a short notice: raw
            // link + reason, one per line, no markdown, because it rides the
            // same channel as the reply itself.
            // #1918: a file an INTERMEDIATE already shipped is in the chat;
            // the final leg must not put a second copy there. Consume those
            // entries from the scan before the floor runs: their markers
            // stay for the reader (linking them to the intermediate's bubble
            // is #1939's leg), and the failure notice is not theirs to
            // join, because their delivery already succeeded.
            {
                let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                if !s.delivered_files.is_empty() {
                    file_scan
                        .attachments
                        .retain(|f| !s.delivered_files.iter().any(|d| d.path == f.path));
                }
            }
            let (delivered, send_failures) = send_local_files(
                session_id,
                bot,
                chat_id,
                thread_id,
                &std::mem::take(&mut file_scan.attachments),
            )
            .await;
            file_scan.failures.extend(send_failures);
            let text_only =
                crate::utils::image::append_file_failure_notice(&text_only, &file_scan.failures);

            // #1918: a marker for a file that DID land becomes a link to its
            // bubble. The inbound message decides whether this chat has a
            // link form at all; a chat without one (private, basic group)
            // gets an empty list and keeps the plain markers. `unwrap_or_default`
            // is the no-inbound leg: nothing here invented a chat, so nothing
            // here can guess a link into existence.
            let mut file_links: Vec<(std::path::PathBuf, String)> = match inbound {
                Some(inbound) => delivered
                    .iter()
                    .filter_map(|delivered_file| {
                        let link = file_message_link(
                            &inbound.chat.kind,
                            chat_id.0,
                            thread_id,
                            delivered_file.message_id,
                        )?;
                        Some((delivered_file.path.clone(), link))
                    })
                    .collect(),
                None => Vec::new(),
            };
            // #1939: a document an INTERMEDIATE delivered is in the chat but
            // absent from `delivered` above: the final leg sent nothing for
            // it, so there is no id here to build a link from. Its bubble id
            // is recorded next to the path, so the one splice below links
            // both origins by one rule. A path the final leg DID ship keeps
            // its own (fresher) id rather than being overwritten.
            if let Some(inbound) = inbound {
                let from_intermediates = {
                    let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                    s.delivered_files.clone()
                };
                for file in from_intermediates {
                    if file_links.iter().any(|(p, _)| p == &file.path) {
                        continue;
                    }
                    if let Some(link) =
                        file_message_link(&inbound.chat.kind, chat_id.0, thread_id, file.message_id)
                    {
                        file_links.push((file.path, link));
                    }
                }
            }
            let text_only = link_file_markers(&text_only, &file_scan, &file_links);

            // Rich fallback: when all content was sent as HTML intermediates
            // during streaming, the dedup step strips text_only to empty. If
            // the original response had rich structure (tables, headings,
            // lists), replace the HTML intermediates with a single native rich
            // message so Telegram renders proper tables and blocks.
            // #46: this arm decides on `pre_dedup_text` BEFORE the options
            // reclaim below can restore the host, so it must consult
            // options_pending itself — same gate as the final-answer site
            // (#45) — or a fully-deduped buttons turn stays plain.
            // #1939: does a media-bearing intermediate already carry this
            // exact body? Its bubble is not in `intermediate_bubbles`, so
            // the cleanup below cannot replace it: the arm would only
            // re-send the body the reader already has, and the copy's
            // markers would point at a bubble that holds nothing. Skipping
            // the arm loses nothing: `text_only` is empty here, and the
            // body itself already lives in that bubble.
            let fallback_dup = {
                let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                super::intermediates::fallback_would_duplicate(
                    &s.media_intermediates,
                    &pre_dedup_text,
                )
            };
            let text_only = if text_only.is_empty()
                && !sent.is_empty()
                && !fallback_dup
                && super::rich::should_send_native_rich_for(
                    &pre_dedup_text,
                    options_pending(streaming),
                ) {
                let rich_md = link_file_markers(&pre_dedup_text, &file_scan, &file_links);
                match super::rich::send_rich_with_mermaid_id(
                    bot.api_url().as_str(),
                    bot.token(),
                    chat_id.0,
                    thread_id,
                    &rich_md,
                    None,
                    "turn",
                    "-",
                )
                .await
                {
                    Ok(rich_msg_id) => {
                        // Delete the HTML intermediates now that rich message succeeded
                        let intermediate_ids: Vec<MessageId> = {
                            let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                            s.intermediate_bubbles.iter().map(|(id, _)| *id).collect()
                        };
                        for mid in &intermediate_ids {
                            best_effort_delete(bot, chat_id, *mid, "intermediate cleanup").await;
                        }
                        tracing::info!(
                            "Telegram: rich fallback delivered ({} chars), deleted {} HTML intermediates",
                            rich_md.len(),
                            intermediate_ids.len()
                        );
                        // Merge candidate (#tg-suggest-merge): every rich
                        // bubble can carry the suggestion controls now —
                        // table-bearing answers ride the markdown plane,
                        // whose server-side render keeps tables intact
                        // (#79 piece 4; the html plane flattened them,
                        // #679, which is why they used to be excluded).
                        final_bubble = Some(super::state::MergeBubble {
                            message_id: teloxide::types::MessageId(rich_msg_id),
                            body: super::state::BubbleBody::Markdown(pre_dedup_text.clone()),
                        });
                        // Store bot reply in channel_messages even though
                        // text_only is empty (dedup stripped it). The rich
                        // fallback already sent pre_dedup_text, so the next
                        // turn's recent() query sees the bot's side of the
                        // conversation. Without this, the agent "talks to
                        // itself in the dark" after every rich fallback.
                        if !is_dm {
                            let bot_display_name = telegram_state
                                .bot_username()
                                .await
                                .map(|u| format!("@{}", u))
                                .unwrap_or_else(|| "OpenCrabs".to_string());
                            let thread_id_str = thread_id.map(|t| t.0.to_string());
                            let cm = DbChannelMessage::new(
                                "telegram".to_string(),
                                chat_id.0.to_string(),
                                Some(chat_title.to_string()),
                                "bot:opencrabs".to_string(),
                                bot_display_name,
                                pre_dedup_text.clone(),
                                "text".to_string(),
                                Some(rich_msg_id.to_string()),
                            )
                            .with_thread(thread_id_str, None);
                            if let Err(e) = channel_msg_repo.insert(&cm).await {
                                tracing::warn!(
                                    "Telegram: rich fallback: failed to record bot reply: {}",
                                    e
                                );
                            }
                        }
                        text_only
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Telegram: rich fallback failed, keeping HTML intermediates: {e}"
                        );
                        text_only
                    }
                }
            } else {
                text_only
            };

            // #300 follow-up: ALWAYS check if the trailing folded text matches the
            // final answer and remove it to prevent duplication. For CLI providers,
            // the final answer arrives as a trailing IntermediateText folded into
            // the collapsed block while response.content comes back empty, so we
            // reclaim it. For other providers (or CLI turns where the answer stayed
            // in content), if the same text ended up both folded and in the final
            // response, remove the folded copy to avoid showing it twice.
            //
            // #31: with options pending the reclaim returns BOTH runs — the
            // answer as host, the post-halt sign-off run as trailer — and
            // `settle_options_reclaim` arbitrates. The options arm runs FIRST:
            // the stock trailing-duplicate strip used to pop the trailer run
            // and drop the host with it (the smoke-v4 abandonment).
            let (text_only, reclaimed_trailer) = if text_only.trim().is_empty() {
                // CLI provider case: no separate answer, reclaim the folded final
                let (host, trailer) =
                    take_folded_final(bot, chat_id, streaming, options_pending(streaming)).await;
                settle_options_reclaim(text_only, host, trailer)
            } else if options_pending(streaming) {
                // #1226 flow-fold (K) + #31 trailer: the turn halted on the
                // suggest_options surface — the substantive pre-options answer
                // is the Text run BEFORE the Tool entry, the sign-off ack the
                // run AFTER it. Reclaim both; the answer becomes the final
                // bubble, the ack rides after the buttons.
                let (host, trailer) = take_folded_final(bot, chat_id, streaming, true).await;
                if host.is_none() && trailer.is_none() && !pre_reclaimed {
                    tracing::warn!(
                        "Telegram: options pending but flow reclaim returned nothing — \
                         answer stuck in flow block (#1226 K)"
                    );
                }
                let (text, trailer) = settle_options_reclaim(text_only, host, trailer);
                if let Some(t) = &trailer {
                    tracing::info!(
                        "Telegram: options reclaim settled — {} trailer chars ride after \
                         the buttons (#31)",
                        t.len()
                    );
                }
                (text, trailer)
            } else {
                // Non-CLI case: check if the trailing folded text matches the final
                // answer and remove it to prevent duplication
                let trailing_matches = {
                    let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                    // Past any chrome appended after the answer (#1253): a
                    // provider switch landing late must not hide the folded
                    // copy and let it render twice.
                    match last_folded_text(&s.flow_entries) {
                        Some(folded) => folded_duplicates_final(folded, &text_only),
                        None => false,
                    }
                };
                if trailing_matches {
                    // Remove the duplicate from the block
                    take_folded_final(bot, chat_id, streaming, options_pending(streaming)).await;
                    (text_only, None)
                } else {
                    (text_only, None)
                }
            };

            // #31: hand the trailer to render_suggestions — rich merges it as
            // a paragraph after the in-body button rows, classic delivers it
            // as its own bubble. Stashed here so both render sites (handler
            // turn end + resume) pick it up identically.
            if let Some(trailer) = reclaimed_trailer {
                streaming
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending_trailer = Some(trailer);
            }

            // #690: re-expand any table the model collapsed onto one line so it
            // renders (native rich or <pre> grid) instead of raw pipes. Applied
            // once here before the rich-vs-HTML branching, so both the
            // should_send_native_rich detection below and the HTML render see the
            // reflowed table. Idempotent on well-formed tables.
            let text_only = super::rich::reflow_collapsed_tables(&text_only);

            // Deliver final response — prefer editing the streaming message in-place
            // to avoid the delete+send race that causes duplicates.
            let html = markdown_to_telegram_html(&text_only);
            // Final answers stay clean prose: the ctx footer lives on the
            // settled flow message, not here.
            let display_html = html.clone();
            tracing::info!(
                "Telegram deliver: html.len={}, ctx footer on flow='{}'",
                html.len(),
                footer
            );
            // Telegram message_id of the FINAL reply bubble. Captured across the
            // delivery paths (rich send, in-place edit, chunked send) so we can
            // persist it and later recover the EXACT message a user replies to,
            // instead of guessing "the most recent bot message" (#234 follow-up).
            let mut sent_reply_id: Option<i32> = None;
            // Merge candidate (#tg-suggest-merge): id + exact HTML of the bubble
            // the classic HTML path delivered the final response in. Captured in
            // every success arm below; handed to StreamingState after delivery so
            // suggest_options can attach its keyboard to THIS bubble instead of
            // posting a separate "Suggested next" message. Rich and voice paths
            // deliberately leave it None — their bubbles are not re-editable as
            // plain HTML without breaking their rendering.
            if !display_html.is_empty() {
                // Rich-first delivery: a structured reply (tables / headings /
                // lists / math) is delivered as a native Telegram rich message
                // regardless of length — Telegram renders the raw markdown into
                // real tables and blocks. Edit the streamed placeholder in place
                // if we have one, otherwise send a fresh message. The ctx footer
                // is plain text, appended as-is. On ANY failure we fall through
                // to the HTML chunking path below, so the streaming path is
                // never regressed. Plain prose skips rich entirely so Telegram's
                // parser never reinterprets incidental characters — EXCEPT prose
                // that ends on a suggest_options surface (#45): the tap rewrite
                // preserves the host plane, so button-bearing prose rides rich
                // too and the pick record edits back in rendered form.
                // #679: for TABLE messages, skip only the doomed native-BLOCKS
                // attempt — Telegram's InputRichBlock rejects our header/rows/align
                // shape (its schema wants cells/size), so a table always 400s the
                // block send and wastes a round-trip. But the rich-MARKDOWN send
                // renders tables correctly, so route tables straight to it instead
                // of skipping the whole rich branch (which #651 did, sending tables
                // to the HTML path where they showed as bare markup). Non-table
                // rich content still tries blocks first (clean fences) then falls
                // back to markdown.
                let mut delivered_rich = super::rich::should_send_native_rich_for(
                    &text_only,
                    // #45: `options_pending` is true when the turn stashed a
                    // suggest_options set mid-turn (#1226 K helper) — force the
                    // rich plane for prose so buttons never live on a plain host.
                    options_pending(streaming),
                ) && {
                    let rich_md = text_only.clone();
                    // Send a FRESH rich message rather than editing the streamed
                    // placeholder into rich. Editing a normal message into a rich
                    // one glitches the client render — overlap during the
                    // transition, and a stale pre-edit (HTML) version after a
                    // refresh / chat switch. A fresh sendRichMessage renders clean.
                    //
                    // Delete the placeholder FIRST so the fresh rich message is
                    // the LAST thing added to the chat — deleting it AFTER the
                    // send pulls the content up and leaves the view mid-chat
                    // instead of scrolling to the bottom on completion. `.take()`
                    // clears the id so the HTML fallback below sends a fresh
                    // message (not an edit of a deleted one) if the rich send fails.
                    if let Some(mid) = streaming_msg_id.take() {
                        best_effort_delete(bot, chat_id, mid, "pre-rich-fallback cleanup").await;
                    }
                    // Native BLOCKS first (#476 path B) for NON-table content: the
                    // block value is sent as-is, so code fences render natively with
                    // no server-side parser to mangle them into <code> artifacts. A
                    // table would only 400 here (schema mismatch), so skip blocks
                    // entirely when one is present and let the markdown send below
                    // render it. On any block rejection we also fall through to
                    // markdown, so worst case is exactly the rich-markdown render.
                    // Straight to rich-markdown (#871). The native-blocks
                    // attempt ran 68 times across two days and returned 400
                    // (RICH_MESSAGE_CONTENT_REQUIRED) all 68 times, never once
                    // succeeding, so every rich message already arrived via the
                    // markdown fallback below. Keeping it cost a guaranteed
                    // round-trip and a guaranteed error before every delivery.
                    //
                    // Markdown is also the only mode that renders tables: the
                    // rich HTML input mode returns 200 and then flattens a table
                    // into a run-on paragraph, which is why telegram_send now
                    // uses this same call rather than its own.
                    {
                        // Rich MARKDOWN renders tables correctly; mermaid fences
                        // are routed to the rich-HTML image path inside the sender
                        // (#1044), everything else stays on markdown.
                        // #tg-mermaid-delivery-hardening: retry once when
                        // Telegram's server-side refetch of the embedded media
                        // (mermaid.ink) died — `RICH_MESSAGE_PHOTO_NO_MEDIA_FOUND`
                        // is a race against a flaky renderer (our resolve ran
                        // seconds earlier), not a structural rejection. The sender
                        // re-resolves media on every call, so the retry naturally
                        // re-fetches from the renderer. Structural 400s are never
                        // retried.
                        let mut rich_send = super::rich::send_rich_with_mermaid_id(
                            bot.api_url().as_str(),
                            bot.token(),
                            chat_id.0,
                            thread_id,
                            &rich_md,
                            None,
                            "turn",
                            "-",
                        )
                        .await;
                        if rich_send.is_err() && is_no_media_found(rich_send.as_ref().unwrap_err())
                        {
                            tracing::warn!(
                                "Telegram: rich send hit NO_MEDIA_FOUND (renderer flake?) — retrying once"
                            );
                            rich_send = super::rich::send_rich_with_mermaid_id(
                                bot.api_url().as_str(),
                                bot.token(),
                                chat_id.0,
                                thread_id,
                                &rich_md,
                                None,
                                "turn",
                                "-",
                            )
                            .await;
                        }
                        match rich_send {
                            Ok(id) => {
                                // Success was silent, which is why an
                                // unformatted table could not be traced (#860).
                                tracing::info!(
                                    "Telegram: rich markdown delivered as msg {id} ({} chars)",
                                    rich_md.len()
                                );
                                sent_reply_id = Some(id);
                                // Merge candidate (#tg-suggest-merge): the
                                // controls can ride this bubble — table-bearing
                                // answers included: the merge edit goes back out
                                // on the markdown plane, whose server-side
                                // render keeps tables intact (#79 piece 4; the
                                // old html-plane merge flattened them, #679,
                                // which is why tables used to be excluded).
                                final_bubble = Some(super::state::MergeBubble {
                                    message_id: teloxide::types::MessageId(id),
                                    body: super::state::BubbleBody::Markdown(rich_md.clone()),
                                });
                                true
                            }
                            Err(e) => {
                                tracing::warn!("Telegram: rich delivery failed, using HTML: {e}");
                                false
                            }
                        }
                    }
                };

                if !delivered_rich {
                    // #tg-mermaid-delivery-hardening: last-chance mermaid render
                    // before degrading to chunks — the classic chunked HTML path
                    // cannot embed `<img>`, so when the source carried a diagram
                    // try the rich HTML dialect once more (it supports images).
                    // If the renderer recovered in the meantime the diagram still
                    // lands inline instead of raw fence text; if it is still down
                    // the resolve yields a legible failure block (renderer note +
                    // source) rather than a bare code dump.
                    if super::rich::mermaid::should_render_mermaid(&text_only) {
                        let fallback_html =
                            super::rich::markdown_to_html_mermaid_p(&text_only).await;
                        match super::rich::api::send_rich_html_id(
                            bot.api_url().as_str(),
                            bot.token(),
                            chat_id.0,
                            thread_id,
                            &fallback_html,
                            None,
                            "turn",
                            "-",
                        )
                        .await
                        {
                            Ok(id) => {
                                tracing::info!(
                                    "Telegram: rich-html mermaid fallback delivered as msg {id}"
                                );
                                sent_reply_id = Some(id);
                                delivered_rich = true;
                            }
                            Err(e2) => {
                                tracing::warn!(
                                    "Telegram: rich-html mermaid fallback failed ({e2}); degrading to chunks"
                                );
                            }
                        }
                    }
                }
                if !delivered_rich {
                    let chunks: Vec<String> = split_message(&display_html, 4096)
                        .into_iter()
                        .map(|s| s.to_string())
                        .collect();

                    // If single chunk and we have a streaming message, edit it in-place
                    if chunks.len() == 1
                        && let Some(mid) = streaming_msg_id
                    {
                        match bot
                            .edit_message_text(chat_id, mid, &chunks[0])
                            .parse_mode(ParseMode::Html)
                            .await
                        {
                            Ok(_) => {
                                // Edited in place — the reply bubble keeps `mid`.
                                // The final visible text is what a duplicate or
                                // chatty-agent investigation needs: one line
                                // closes "what actually landed in the mutated
                                // message" (#1085 post-review). Failure arms
                                // already log via the fallback sends' telemetry.
                                super::telemetry::log_send_success(
                                    "turn",
                                    "-",
                                    &session_id.to_string(),
                                    "stream_edit_final",
                                    "in_place_edit",
                                    chat_id.0,
                                    thread_id.map(|t| t.0.0),
                                    mid.0,
                                    chunks[0].len(),
                                    &super::telemetry::content_hash8(&chunks[0]),
                                );
                                sent_reply_id = Some(mid.0);
                                // Merge candidate (#tg-suggest-merge): the
                                // answer bubble suggest_options can ride on.
                                final_bubble = Some(super::state::MergeBubble {
                                    message_id: mid,
                                    body: super::state::BubbleBody::Html(chunks[0].clone()),
                                });
                            }
                            Err(teloxide::RequestError::RetryAfter(secs)) => {
                                super::rate_limit::wait_out(
                                    "edit",
                                    secs.duration(),
                                    "",
                                    Some(chat_id.0),
                                )
                                .await;
                                match bot
                                    .edit_message_text(chat_id, mid, &chunks[0])
                                    .parse_mode(ParseMode::Html)
                                    .await
                                {
                                    Ok(_) => {
                                        sent_reply_id = Some(mid.0);
                                        final_bubble = Some(super::state::MergeBubble {
                                            message_id: mid,
                                            body: super::state::BubbleBody::Html(chunks[0].clone()),
                                        });
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Telegram: edit retry failed ({e}), falling back to delete+send"
                                        );
                                        best_effort_delete(
                                            bot,
                                            chat_id,
                                            mid,
                                            "edit-retry fallback",
                                        )
                                        .await;
                                        // Never silent (#1019): this is the LAST fallback.
                                        // The edit already failed and was logged; if the
                                        // resend fails too the message is gone entirely,
                                        // so the recovery path must not be the quiet one.
                                        if let Ok(sent) = send_html_or_plain(
                                            bot, chat_id, thread_id, &chunks[0], "turn", None,
                                        )
                                        .await
                                        {
                                            sent_reply_id = Some(sent.0);
                                            final_bubble = Some(super::state::MergeBubble {
                                                message_id: sent,
                                                body: super::state::BubbleBody::Html(
                                                    chunks[0].clone(),
                                                ),
                                            });
                                        } else {
                                            tracing::error!(
                                                "Telegram: delete+send fallback failed in chat {chat_id}, \
                                                 the reply was lost"
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Telegram: edit final failed ({e}), falling back to delete+send"
                                );
                                best_effort_delete(bot, chat_id, mid, "edit-final fallback").await;
                                if let Ok(sent) = send_html_or_plain(
                                    bot, chat_id, thread_id, &chunks[0], "turn", None,
                                )
                                .await
                                {
                                    sent_reply_id = Some(sent.0);
                                    final_bubble = Some(super::state::MergeBubble {
                                        message_id: sent,
                                        body: super::state::BubbleBody::Html(chunks[0].clone()),
                                    });
                                }
                            }
                        }
                    } else {
                        // Multi-chunk or no streaming message — delete old, send new
                        if let Some(mid) = streaming_msg_id {
                            best_effort_delete(bot, chat_id, mid, "multi-chunk swap").await;
                        }
                        for chunk in &chunks {
                            // Last chunk wins — that's the bubble a user replies to.
                            if let Ok(sent) =
                                send_html_or_plain(bot, chat_id, thread_id, chunk, "turn", None)
                                    .await
                            {
                                sent_reply_id = Some(sent.0);
                                final_bubble = Some(super::state::MergeBubble {
                                    message_id: sent,
                                    body: super::state::BubbleBody::Html(chunk.clone()),
                                });
                            }
                        }
                    }
                }
            } else if let Some(mid) = streaming_msg_id {
                // Empty final text: all content was already delivered as
                // intermediate messages. The ctx budget rides the settled
                // flow message now, so just remove the now-empty streaming
                // placeholder.
                best_effort_delete(bot, chat_id, mid, "empty-final placeholder").await;
            }

            // #2156: the answer is in its own bubble now. Remove the
            // intermediate bubbles whose text the answer already delivers, so
            // the reader sees one answer instead of a narration bubble
            // repeating it above. Deleting AFTER a successful send keeps the
            // narration intact if delivery failed and had to degrade.
            if let Some(final_mid) = sent_reply_id {
                prune_delivered_intermediates(
                    bot,
                    chat_id,
                    streaming,
                    &text_only,
                    MessageId(final_mid),
                )
                .await;
            }

            // Hand the merge candidate to the turn state (#tg-suggest-merge):
            // handler.rs reads it immediately after this returns and passes it
            // into render_suggestions. None (rich / voice / suppressed paths)
            // means suggestions fall back to their standalone block as before.
            if final_bubble.is_some() {
                streaming
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .final_bubble = final_bubble;
            }

            // Record the bot's text reply into channel_messages.
            //
            // Groups: needed so the recent() query that builds conversation
            // context on the NEXT turn sees both sides — without it the bot
            // loads a one-sided transcript and talks to itself in the dark.
            //
            // Both group AND DM persist the Telegram message_id (when captured)
            // so a later reply to THIS bubble can be recovered EXACTLY by id —
            // Telegram delivers rich bot messages with empty text, so the reply
            // handler can't read the quoted content from the update and must
            // look it up by id (#234 follow-up). DMs are stored only when we
            // have an id (lookup-only; DM conversation context still comes from
            // the session messages table, not channel_messages).
            let pmid = sent_reply_id.map(|i| i.to_string());
            if !text_only.trim().is_empty() && (!is_dm || pmid.is_some()) {
                let bot_display_name = telegram_state
                    .bot_username()
                    .await
                    .map(|u| format!("@{}", u))
                    .unwrap_or_else(|| "OpenCrabs".to_string());
                let thread_id = thread_id.map(|t| t.0.to_string());
                let cm = DbChannelMessage::new(
                    "telegram".to_string(),
                    chat_id.0.to_string(),
                    Some(chat_title.to_string()),
                    "bot:opencrabs".to_string(),
                    bot_display_name,
                    text_only.clone(),
                    "text".to_string(),
                    pmid.clone(),
                )
                .with_thread(thread_id, None);
                if let Err(e) = channel_msg_repo.insert(&cm).await {
                    tracing::warn!(
                        "Telegram: failed to record bot reply in channel_messages: {}",
                        e
                    );
                }
            }

            // If input was voice AND TTS is enabled, also send voice note after text
            if is_voice && voice_config.tts_enabled {
                tracing::info!(
                    "Telegram: TTS requested — synthesizing response text (len={})",
                    response.content.len()
                );
                match crate::channels::voice::synthesize(&response.content, voice_config).await {
                    Ok(audio_bytes) => {
                        tracing::info!(
                            "Telegram: TTS succeeded — {} bytes of audio, sending to chat {}",
                            audio_bytes.len(),
                            chat_id
                        );
                        match voice_in_thread(
                            bot,
                            chat_id,
                            thread_id,
                            InputFile::memory(audio_bytes),
                        )
                        .await
                        {
                            Ok(m) => {
                                tracing::info!(
                                    "Telegram: voice message delivered (msg_id={})",
                                    m.id
                                );
                                // Record the delivered voice message ID in
                                // the isolated voice_msg_ids list. Cleanup
                                // paths do not touch this list. See the
                                // field doc on StreamingState.
                                let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                                s.voice_msg_ids.push(m.id);
                            }
                            Err(e) => {
                                tracing::error!("Telegram: send_voice failed — {}: {:?}", e, e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Telegram: TTS synthesis failed: {:#}", e);
                    }
                }
            }
        }
        Err(ref e) if matches!(e, crate::brain::agent::AgentError::Cancelled) => {
            tracing::info!("Telegram: agent call cancelled for session {}", session_id);
            // Silently clean up — user already received "Operation cancelled." from /stop
            if let Some(mid) = streaming_msg_id {
                best_effort_delete(bot, chat_id, mid, "cancel cleanup").await;
            }
        }
        Err(e) => {
            tracing::error!("Telegram: agent error: {}", e);
            // Translate via the shared helper so the message tells the
            // user WHAT self-heal already tried + what to do next,
            // instead of leaking the raw `API error (502)` shape that
            // confused users into thinking the agent silently dropped
            // their request. See `brain::agent::format_user_error` for
            // the pattern matchers (5xx exhausted / 429 / context too
            // large / stream broken / repetition loop / etc.).
            let user_msg = format!("❌ Error\n\n{}", crate::brain::agent::format_user_error(&e));
            if let Some(mid) = streaming_msg_id {
                if let Err(e) = bot.edit_message_text(chat_id, mid, user_msg).await {
                    tracing::warn!(
                        target: "telegram::send",
                        chat_id = chat_id.0,
                        message_id = mid.0,
                        error = %e,
                        "final-error edit failed"
                    );
                }
            } else {
                message_in_thread(bot, chat_id, thread_id, user_msg).await?;
            }
        }
    }
    Ok(true)
}

/// One delivered file together with the id of the message bubble it landed
/// in (#1918). The path travels with the id because the caller looks the
/// file up BY PATH: the id alone would not say which file it carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveredFile {
    /// The file that was sent.
    pub path: std::path::PathBuf,
    /// Telegram's id for the message the send produced.
    pub message_id: i32,
}

/// The `t.me` message link for a delivered file's bubble, when its chat kind
/// has one (#1918).
///
/// A message link exists only for a public supergroup or a channel.
/// `t.me/c/...` addresses a chat by its INTERNAL id, the chat id with the
/// `-100` marker stripped, and gains a middle segment with the topic id
/// when the message sits in a forum topic, so the reader lands on the bubble
/// inside the right topic rather than at the top of a thread they then have
/// to search.
///
/// A private chat and a basic group have no message-link form at all, so
/// they get `None` and the caller leaves the marker as plain text: a link
/// that goes nowhere is worse than no link, and the marker's job of pointing
/// at the file is done by the label either way.
pub(crate) fn file_message_link(
    kind: &teloxide::types::ChatKind,
    chat_id: i64,
    thread_id: Option<teloxide::types::ThreadId>,
    message_id: i32,
) -> Option<String> {
    let has_message_link = match kind {
        teloxide::types::ChatKind::Public(public) => matches!(
            public.kind,
            teloxide::types::PublicChatKind::Supergroup { .. }
                | teloxide::types::PublicChatKind::Channel { .. }
        ),
        teloxide::types::ChatKind::Private { .. } => false,
    };
    if !has_message_link {
        return None;
    }
    // `t.me/c/` takes the id WITHOUT the `-100` marker that identifies a
    // supergroup or channel. A private or basic-group id never reaches here,
    // so an id without the prefix keeps its digits rather than losing three.
    let chat = chat_id.to_string();
    let internal = chat.strip_prefix("-100").unwrap_or(chat.as_str());
    Some(match thread_id {
        Some(thread) => format!("https://t.me/c/{internal}/{}/{message_id}", thread.0.0),
        None => format!("https://t.me/c/{internal}/{message_id}"),
    })
}

/// Rewrite each file's `<label>` marker into a markdown link to the bubble
/// that carries that file (#1918).
///
/// `links` names the files that were BOTH delivered and reachable by a link
/// as `(path, url)`, so a file the channel refused, a file an intermediate
/// already sent, or one whose chat kind has no message-link form is simply
/// absent and keeps the plain marker. The marker never points at a bubble
/// that does not exist, and it never disappears: the label is the reader's
/// anchor either way.
///
/// `text` is the buffer being rewritten, which STARTS as a copy of `scan.text`.
/// The artifact strip and the secret redaction have already run by the time the
/// scan does, so what they left is what the recorded spans index; the rewrites
/// that can move a marker are the ones AFTER it — `strip_echoed_plan_title`
/// drops an echoed leading line and shifts every following span left (#837),
/// `extract_react_marker` cuts a trailing directive, and the dedup ladder and
/// the folded reclaim each rewrite the text again. The recorded span is
/// therefore a record of WHAT the scanner wrote, never of WHERE it sits: it is
/// tried first, and a marker that moved is then found by its own bytes. A
/// marker that vanished, or that the model also wrote itself (#2001), keeps the
/// plain form rather than splicing a link over unrelated words.
///
/// Spans are cut from the LAST to the FIRST, so a link's extra bytes can never
/// invalidate a span that has not been cut yet.
pub(crate) fn link_file_markers(
    text: &str,
    scan: &crate::utils::image::LocalFileScan,
    links: &[(std::path::PathBuf, String)],
) -> String {
    if links.is_empty() {
        return text.to_string();
    }
    let mut spans: Vec<(usize, usize, &str, &str)> = Vec::new();
    for file in &scan.attachments {
        let Some(span) = file.marker_span.as_ref() else {
            // Not from a scan: nothing to cut.
            continue;
        };
        // The span indexes `scan.text`, the scanner's own output, so it still
        // names the marker the scanner wrote there. Read the marker FROM it
        // rather than from `text`, whose bytes have since moved.
        let Some(marker) = scan.text.get(span.start..span.end) else {
            continue;
        };
        let Some(start) = locate_marker(text, marker, Some(span.clone())) else {
            continue;
        };
        let Some((_, link)) = links
            .iter()
            .find(|(path, _)| path.as_path() == file.path.as_path())
        else {
            continue;
        };
        spans.push((start, start + marker.len(), marker, link.as_str()));
    }
    if spans.is_empty() {
        return text.to_string();
    }
    // The scanner emits markers in order, so this only guards a caller that
    // reordered them: cutting back-to-front is what keeps the earlier spans
    // valid, and that needs the order to be known.
    spans.sort_by_key(|(start, _, _, _)| *start);
    // Two attachments can resolve to the SAME offset. A file named twice leaves
    // two identical markers; when the text has moved past both recorded spans,
    // `locate_marker` falls back to the marker's own bytes, which may now exist
    // only once, and both attachments land on that one occurrence. Cutting the
    // range twice splices the second link into the first one's bytes and, when
    // the marker's last character is multi-byte, ends inside it: a
    // `replace_range` panic (#2125). One occurrence carries one link, so the
    // first span wins and the second attachment keeps its plain marker.
    spans.dedup_by_key(|(start, _, _, _)| *start);
    let mut linked = text.to_string();
    for (start, end, marker, link) in spans.iter().rev() {
        linked.replace_range(*start..*end, &format!("[{marker}]({link})"));
    }
    linked
}

/// Find where `marker` sits in `text`.
///
/// The recorded `span` is tried FIRST. It indexes `scan.text`, and when the
/// bytes it names still equal `marker` at that same offset in `text` the buffer
/// has not moved, so that is unambiguously the occurrence the scanner wrote —
/// even when the model wrote the same marker again elsewhere. A file named
/// twice in one reply produces two attachments, two identical markers and two
/// valid spans, and each must link at its own offset.
///
/// When the span is stale the marker's own bytes are searched for, and the
/// result is accepted only on EXACTLY one occurrence: two are ambiguous, since
/// the scanner wrote one and the model may have written the other itself
/// (#2001), and a stale span no longer says which. Zero means the text was
/// rewritten beyond recognition. Both fall back to the plain marker.
fn locate_marker(text: &str, marker: &str, span: Option<std::ops::Range<usize>>) -> Option<usize> {
    if marker.is_empty() {
        return None;
    }
    if let Some(span) = span {
        let start = span.start;
        if text.get(span) == Some(marker) {
            return Some(start);
        }
    }
    let first = text.find(marker)?;
    if text[first + marker.len()..].contains(marker) {
        return None;
    }
    Some(first)
}

/// The filename Telegram should display for a document read from `path`.
///
/// `InputFile::memory` carries no name, and teloxide's own fallback returns an
/// empty string for a `Bytes` payload, so Telegram labels the document `file`
/// and drops the MIME: the extension never travelled. The multipart part's
/// filename is where the name travels from (#1918).
pub(crate) fn document_part_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Send each resolved local-file link as its own document bubble (#1916).
///
/// Telegram's clients render no document preview, so there is no
/// photo/document ceiling to route by: every file ships through
/// `sendDocument`, and the one size gate that matters lives upstream in
/// [`crate::utils::image::validate_local_file`], where exceeding it can be
/// reported to the MODEL as a rejection reason before delivery is attempted
/// instead of surfacing here as a refusal the model never saw coming.
/// Returns the files that landed, each with the message id the send
/// produced, and the failures the channel itself refused: the two halves
/// are set in different arms, and inferring one from the other would couple
/// two independent facts.
///
/// A file that cannot be READ reports `Unreadable`: the reference named
/// something this host cannot produce, which the model can repair by
/// rewriting the link. A file the Telegram API REFUSES reports
/// `DeliveryFailed`: the reference was fine, the channel said no, and no
/// rewrite fixes that. The distinction is what routes the correction.
pub(crate) async fn send_local_files(
    session_id: uuid::Uuid,
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    files: &[crate::utils::image::LocalFile],
) -> (
    Vec<DeliveredFile>,
    Vec<crate::utils::image::LocalImageFailure>,
) {
    let mut delivered: Vec<DeliveredFile> = Vec::new();
    let mut failures: Vec<crate::utils::image::LocalImageFailure> = Vec::new();

    for file in files {
        let path = &file.path;
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!("Telegram: failed to read file {}: {}", path.display(), e);
                failures.push(crate::utils::image::LocalImageFailure {
                    raw: path.display().to_string(),
                    resolved: Some(path.clone()),
                    reason: crate::utils::image::LocalImageFailureReason::Unreadable,
                });
                continue;
            }
        };
        let len = bytes.len();
        // The document's own name rides the multipart part's filename
        // (#1937): InputFile::memory carries none, teloxide guesses empty,
        // and Telegram labels an unnamed document "file" with no MIME. The
        // extension is also what teloxide derives the MIME type from, so
        // naming the part is what makes a .pdf arrive as application/pdf.
        let mut req = document_in_thread(
            bot,
            chat_id,
            thread_id,
            InputFile::memory(bytes).file_name(document_part_name(path)),
        );
        // Upstream `document_in_thread` carries no caption parameter, so the
        // link label chains onto the request as the document caption.
        if let Some(caption) = file.caption.as_deref() {
            req = req.caption(caption.to_string());
        }
        match req.await {
            Ok(msg) => {
                delivered.push(DeliveredFile {
                    path: path.clone(),
                    message_id: msg.id.0,
                });
                // Match the outbox media receipt: len is sent bytes and hash8
                // identifies the path, so one audit predicate covers every leg.
                super::telemetry::log_send_success(
                    "turn",
                    "-",
                    &session_id.to_string(),
                    "delivery_media",
                    "file_document",
                    chat_id.0,
                    thread_id.map(|t| t.0.0),
                    msg.id.0,
                    len,
                    &super::telemetry::content_hash8(&path.display().to_string()),
                );
            }
            Err(e) => {
                tracing::error!(
                    "Telegram: failed to send file {} as document: {}",
                    path.display(),
                    e
                );
                failures.push(crate::utils::image::LocalImageFailure {
                    raw: path.display().to_string(),
                    resolved: Some(path.clone()),
                    reason: crate::utils::image::LocalImageFailureReason::DeliveryFailed,
                });
            }
        }
    }

    (delivered, failures)
}

/// Drain the display items left queued after the edit loop stopped,
/// folding them into the processing-log flow. ONE shared implementation for
/// handle_message and resume_session (#470 / #462 item 1: the drains were
/// copy-pasted and could drift apart — parity is now structural).
/// `react_target` is the inbound message a folded `<<react:>>` directive
/// acknowledges; resume has none (the original message id is lost across
/// restarts), so the directive strips without firing (#261).
pub(crate) async fn drain_remaining_display(
    bot: &Bot,
    chat: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    remaining: Vec<DisplayItem>,
    react_target: Option<MessageId>,
) {
    let mut tool_buffer: Vec<usize> = Vec::new();
    for item in remaining {
        match item {
            DisplayItem::NewTool(idx) => {
                tool_buffer.push(idx);
            }
            DisplayItem::Intermediate(text) => {
                // Fold into the open processing-log flow instead of sending
                // a standalone message (#300); sanitize exactly like the
                // live edit-loop path.
                append_tool_group(bot, chat, thread_id, streaming, &tool_buffer).await;
                tool_buffer.clear();
                let text = crate::utils::sanitize::strip_llm_artifacts(&text);
                let text = redact_secrets(&text);
                let (text, _img_paths) = crate::utils::extract_img_markers(&text);
                let (text, react_emoji) = crate::utils::extract_react_marker(&text);
                if let (Some(target), Some(emoji)) = (react_target, react_emoji.as_deref()) {
                    fire_reaction(bot, chat, target, emoji).await;
                }
                append_intermediate_to_flow(bot, chat, thread_id, streaming, &text).await;
            }
            DisplayItem::System(text) => {
                // Chrome folds into the same block, in order, but skips the
                // model pipeline: it is our own text, so there is no artifact
                // to strip, no image marker, and no react directive to fire.
                // Secrets are still scrubbed — a self-healing alert embeds a
                // raw upstream error string (#1253).
                append_tool_group(bot, chat, thread_id, streaming, &tool_buffer).await;
                tool_buffer.clear();
                let text = redact_secrets(&text);
                append_system_to_flow(bot, chat, thread_id, streaming, &text).await;
            }
        }
    }
    // Flush any remaining tools into the open group (merges the final batch
    // into the running collapsible block instead of opening a new message).
    append_tool_group(bot, chat, thread_id, streaming, &tool_buffer).await;
}

/// Strip an echoed plan title from the start of the agent's response (#837).
///
/// The `[ACTIVE PLAN REMINDER]` shows the model `📋 Plan: "{title}"` every
/// turn, and the model opens its reply by repeating it. The plan card already
/// carries the title, so the text repeats what is rendered directly above it.
///
/// Pure, taking the title rather than loading it, so the matching is testable
/// without a session on disk — the first version compared an exact string and
/// could only be exercised end to end.
pub(crate) fn strip_echoed_plan_title(text: &str, plan_title: &str) -> String {
    let title = normalize_title_line(plan_title);
    if title.is_empty() {
        return text.to_string();
    }
    let trimmed = text.trim_start();
    let Some(first_line) = trimmed.lines().next() else {
        return text.to_string();
    };
    if normalize_title_line(first_line) != title {
        return text.to_string();
    }
    // Drop the line and the blank line that usually follows a heading.
    let rest = trimmed[first_line.len()..].trim_start_matches('\n');
    rest.to_string()
}

/// Reduce a line to the bare title for comparison.
///
/// Handles the shapes the model actually produces, which are the shapes the
/// reminder itself shows it: markdown headings and emphasis, the `📋` the
/// reminder and the card both use, a `Plan:` label, and surrounding quotes.
/// The earlier version trimmed only `#`, `*` and `~`, so an echo of the
/// reminder's own formatting — the most likely echo of all — never matched.
fn normalize_title_line(line: &str) -> String {
    let mut s = line.trim();
    // Leading blockquote / list / heading markers.
    s = s.trim_start_matches(['#', '>', '-', '*', '_', '~', ' ']);
    // Any leading non-alphanumeric run: emoji such as 📋, bullets, symbols.
    s = s.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '"');
    // The reminder labels it `Plan: "…"`; the model copies the label too.
    for label in ["Plan:", "plan:", "PLAN:"] {
        if let Some(rest) = s.strip_prefix(label) {
            s = rest.trim_start();
            break;
        }
    }
    s = s.trim_matches(['"', '\'', '*', '~', '_', ' ']);
    s.trim().to_string()
}
