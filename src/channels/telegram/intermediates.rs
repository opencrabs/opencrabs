//! Intermediate-message delivery: standalone narration posts (rich-first
//! with HTML fallback), footer append to the last intermediate, the
//! rate-limit-retrying send wrapper and the HTML-or-plain send.
//!
//! Moved VERBATIM out of handler.rs (#471 phase 1, pure decomposition —
//! only visibility widened to pub(crate) so the handler glob re-export
//! keeps every existing call site and test import stable).

use super::handler::StreamingState;
use super::markdown::{markdown_to_telegram_html, split_message, strip_html_tags};
use super::send::message_in_thread;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{MessageId, ParseMode, ReplyParameters};

/// Send an HTML message, falling back to plain text if Telegram rejects the HTML.
/// Returns the resulting `MessageId` so callers that need to track or later delete
/// the message (e.g. intermediate cleanup on cancellation) can do so.
/// Build the edited message body for appending the ctx/tok-s footer to the
/// last intermediate message.
///
/// Used when a turn's final response text deduped to empty because all of
/// it was already delivered as intermediate messages (the common tool-
/// using case). Rather than drop the footer (which left the user never
/// seeing ctx budget on Telegram — 2026-06-06) or send a standalone
/// footer bubble (removed in 7a0ca1c9), we edit the last intermediate
/// message to carry the footer inline.
///
/// Reconstructs the last chunk exactly as it was originally sent
/// (`markdown_to_telegram_html` + `split_message(_, 4096)` then `.last()`),
/// appends the footer, and returns `None` when:
/// - the footer or intermediate text is empty, OR
/// - the combined result would exceed Telegram's 4096-char cap (never
///   truncate real content to make room for metadata).
///
/// Pure + free function so the fit/reconstruct logic is unit-testable
/// without a live bot.
// Channel-unused since the ctx footer moved onto the flow message (the
// intermediate-footer append path went with the pre-block status bubble);
// kept because the reconstruct-last-chunk logic is nontrivial and its tests
// pin the split/fit contract meanwhile.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_last_intermediate_with_footer(
    last_intermediate_text: &str,
    footer: &str,
) -> Option<String> {
    if footer.is_empty() || last_intermediate_text.is_empty() {
        return None;
    }
    let html = markdown_to_telegram_html(last_intermediate_text);
    let chunks = split_message(&html, 4096);
    let last_chunk = chunks.last()?;
    let combined = format!("{last_chunk}\n\n{footer}");
    if combined.chars().count() > 4096 {
        None
    } else {
        Some(combined)
    }
}

/// Send a structured intermediate segment as a native rich message, returning
/// its id for tracking. Mermaid-aware (#1044/#1202): fences resolve to a media
/// array; with no fence this is byte-identical to `send_rich_markdown_id`.
/// Returns `None` when the text carries no rich structure
/// or the rich API rejects it — the caller then falls back to the HTML path.
pub(crate) async fn try_send_intermediate_rich(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    text: &str,
    local_media: &[super::rich::mermaid::MediaEntry],
) -> Option<MessageId> {
    // A media array is its own reason to take the rich plane (#1918): a
    // plain report that carries documents has something only the array can
    // render, and the structure detector below knows nothing about files.
    if !super::rich::should_send_native_rich(text) && local_media.is_empty() {
        return None;
    }
    // Media-aware sender (#1044/#1202, #1918): resolves fences into the rich
    // markdown media array and merges the caller's document entries;
    // byte-identical to send_rich_markdown_id when neither is present, so
    // non-diagram, file-free reports are unaffected.
    match super::rich::send_rich_with_media_target_id(
        bot.api_url().as_str(),
        bot.token(),
        chat_id.0,
        thread_id,
        None,
        text,
        local_media,
        "turn",
        "-",
    )
    .await
    {
        Ok(id) => Some(MessageId(id)),
        Err(e) => {
            tracing::warn!("Telegram: intermediate rich send failed, using HTML: {e}");
            None
        }
    }
}

/// Does a media-bearing intermediate already carry `rich_text`, so the rich
/// fallback's re-send would be pure duplication (#1939)?
///
/// The fallback arm's premise is that it REPLACES the intermediates it
/// deletes. A bubble whose media array carried documents is not in
/// `intermediate_bubbles`, so nothing it holds can be deleted: the arm would
/// delete only the smaller text bubbles and still re-send the body, leaving
/// the reader with it twice. The normalization matches the dedup ladder in
/// `deliver_final_response`, so both agree on what "same body" means.
pub(crate) fn fallback_would_duplicate(
    media: &[(teloxide::types::MessageId, String)],
    rich_text: &str,
) -> bool {
    let norm = |s: &str| -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") };
    let norm_final = norm(rich_text);
    media.iter().any(|(_, text)| norm(text) == norm_final)
}

/// True when an intermediate message contains a substantial markdown status report
/// (e.g. status/progress/pipeline heading or substantial section) worth delivering
/// as its own message rather than burying in collapsible flow (#215).
pub(crate) fn is_deliverable_status_report(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    let total_chars = trimmed.chars().count();
    let line_count = text.lines().filter(|l| !l.trim().is_empty()).count();

    let mut in_fence = false;
    let mut fence_char = ' ';
    let mut fence_len = 0;
    let mut has_keyword_heading = false;
    let mut has_atx_heading = false;
    let mut has_status_callout = false;

    const STATUS_KEYWORDS: &[&str] = &[
        "status",
        "update",
        "progress",
        "pipeline",
        "summary",
        "verification",
        "verdict",
        "phase",
        "findings",
        "plan",
        "results",
    ];

    for line in text.lines() {
        let t = line.trim_start();
        let leading_spaces = line.len() - t.len();
        if leading_spaces < 4 {
            let fence_run: String = t.chars().take_while(|&c| c == '`' || c == '~').collect();
            if fence_run.len() >= 3 {
                let f_char = fence_run.chars().next().unwrap();
                if !in_fence {
                    in_fence = true;
                    fence_char = f_char;
                    fence_len = fence_run.len();
                    continue;
                } else if f_char == fence_char && fence_run.len() >= fence_len {
                    in_fence = false;
                    continue;
                }
            }
        }
        if in_fence {
            continue;
        }

        if super::rich::is_atx_heading(t) {
            has_atx_heading = true;
            let lower = t.to_lowercase();
            if STATUS_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
                has_keyword_heading = true;
            }
        }

        // Rule C — structured callout: non-empty line starting with `>` and `**` followed by status/update/progress
        let trimmed_line = line.trim();
        if let Some(after_gt) = trimmed_line.strip_prefix('>') {
            let after_gt = after_gt.trim_start();
            if let Some(after_stars) = after_gt.strip_prefix("**") {
                let lower_callout = after_stars.to_lowercase();
                if lower_callout.starts_with("status")
                    || lower_callout.starts_with("update")
                    || lower_callout.starts_with("progress")
                {
                    has_status_callout = true;
                }
            }
        }
    }

    // Rule A — keyword heading: line_count >= 2 and total_chars >= 50
    if has_keyword_heading && line_count >= 2 && total_chars >= 50 {
        return true;
    }

    // Rule B — substantial section: any ATX heading, line_count >= 3 and total_chars >= 150
    if has_atx_heading && line_count >= 3 && total_chars >= 150 {
        return true;
    }

    // Rule C — structured callout: line_count >= 2 and total_chars >= 60
    if has_status_callout && line_count >= 2 && total_chars >= 60 {
        return true;
    }

    false
}

/// True when a folded intermediate is a substantial rich report worth
/// delivering as its OWN message rather than burying in the collapsed
/// processing log (#582). Keyed on a real markdown table plus some length, so
/// thin narration (no table) keeps folding and only report-shaped content —
/// which the model may emit before a tool call (e.g. text + `plan complete` in
/// one step) — is surfaced.
pub(crate) fn is_deliverable_rich_report(text: &str) -> bool {
    // #690 follow-up (#980): a table collapsed onto ONE line is invisible to
    // contains_table (which needs the header and separator each on their own
    // line), so a collapsed report would fail this gate and get buried in the
    // folded log as raw pipes. Reflow first — the same recovery the final-
    // response and HTML-render paths already apply. Idempotent.
    let reflowed = super::rich::reflow_collapsed_tables(text);
    // A mermaid fence (tagged or content-classified, #1202) is report-shaped
    // on its own: folding buries the diagram behind a tap-to-expand tap AND
    // leaves raw fence text in the log, because neither the fold renderer nor
    // the pre-fix rich path resolved fences. Surfaced intermediates go
    // through deliver_intermediate_message, which now resolves them.
    if super::rich::mermaid::has_mermaid_fence(&reflowed) {
        return true;
    }
    if super::rich::contains_table(&reflowed) && text.trim().chars().count() >= 200 {
        return true;
    }
    is_deliverable_status_report(text)
}

/// Deliver `text` as its own message (rich-first, HTML fallback) and record it
/// in `sent_intermediates` so the final-response dedup will not resend it.
/// Returns true when something was delivered. Used to surface a rich report the
/// model emitted before a tool call, which folding would otherwise bury (#582).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn deliver_intermediate_message(
    bot: &Bot,
    chat: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    tg: &super::state::TelegramState,
    session_id: uuid::Uuid,
    base_dir: &std::path::Path,
    text: &str,
) -> bool {
    // #690 follow-up (#980): re-expand a collapsed table once, up front, so the
    // dedup record, the rich send and the HTML fallback all see the same
    // expanded shape. The HTML path reflows again internally but is idempotent.
    let expanded = super::rich::reflow_collapsed_tables(text);
    // The dedup record keeps the PRE-scan text: the scan rewrites links into
    // markers, and recording that form would make a repeated intermediate
    // (same raw text) miss the exact-match check and ship its files twice.
    // The BUBBLE carries the marker form; the dedup ledger carries what came
    // in, byte-identical to what the next attempt will compare against.
    let dedup_text = expanded.as_str();
    {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        if s.sent_intermediates.iter().any(|prev| prev == dedup_text) {
            return true;
        }
    }
    // #1918: scan the intermediate for local-file links. The scan comes
    // AFTER the dedup check: an already-sent intermediate delivered its
    // files the first time. The scan consumes each link and leaves a visible
    // marker, which is the honest shape for a plane the rich send declines.
    let mut file_scan = crate::utils::image::extract_local_files(dedup_text, Some(base_dir));
    let marked = std::mem::take(&mut file_scan.text);

    // Read every candidate's bytes BEFORE the rewrite (#1921). A
    // `tg://document` reference resolves only through its media-array entry,
    // so a file that cannot be read must not be referenced: its link is
    // consumed instead (the same treatment an already-delivered file gets),
    // the failure list names it, and no reference ever reaches the wire
    // without its entry riding the same request.
    let (readable, read_failures) =
        crate::utils::image::probe_document_bytes(&file_scan.attachments);
    file_scan.failures.extend(read_failures);
    let unreadable: Vec<std::path::PathBuf> = file_scan
        .attachments
        .iter()
        .map(|f| f.path.clone())
        .filter(|p| !readable.contains_key(p))
        .collect();

    // The rich plane can do better than a marker: the same rewrite the final
    // leg's rich arm uses replaces each resolvable link IN PLACE with a
    // `tg://document` reference, and the entries it returns ride the rich
    // bubble's media array, so the document renders AT its reference instead
    // of as a detached bubble. The rewrite runs on the pre-scan text, since
    // nothing has been delivered by this plane yet; the unreadable set is
    // consumed rather than referenced, so the array and the body can never
    // disagree.
    let fw = crate::utils::image::rewrite_local_files(
        dedup_text,
        Some(base_dir),
        crate::utils::DOC_ID_PREFIX,
        &unreadable,
        crate::config::Config::current()
            .channels
            .telegram
            .inline_markdown_documents,
    );
    let mut doc_media: Vec<super::rich::mermaid::MediaEntry> = Vec::new();
    let mut all_attached = true;
    for entry in &fw.entries {
        match readable.get(&entry.file.path) {
            Some(bytes) => doc_media.push(super::rich::mermaid::MediaEntry {
                id: entry.id.clone(),
                url: None,
                bytes: Some(bytes.clone()),
                kind: super::rich::mermaid::MediaKind::Document,
                name: Some(super::delivery::document_part_name(&entry.file.path)),
            }),
            None => {
                // Unreachable while the consume list above matches the probe
                // exactly, but the failure mode is a reference without an
                // entry, so the response is to abandon the rich form
                // entirely rather than ship one.
                all_attached = false;
            }
        }
    }
    let plain_text = crate::utils::image::append_file_failure_notice(&marked, &file_scan.failures);
    let rich_markdown = if doc_media.is_empty() || !all_attached {
        plain_text.clone()
    } else {
        crate::utils::image::append_file_failure_notice(&fw.rich, &file_scan.failures)
    };
    if let Some(id) =
        try_send_intermediate_rich(bot, chat, thread_id, &rich_markdown, &doc_media).await
    {
        // The documents ride the bubble's media array: they are in the chat,
        // so the final leg must not ship them again. The consume list is the
        // same one the bubble fallback below feeds.
        // The bubble is non-sticky burial evidence (#1150): the flow block must
        // restick below its own output on the next append.
        tg.note_bot_bubble(chat.0, id.0);
        {
            let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
            if doc_media.is_empty() {
                // Paired with its text so the final leg can delete exactly the
                // bubbles its own answer already delivers (#2156).
                s.intermediate_bubbles.push((id, dedup_text.to_string()));
            } else {
                // The documents ride THIS bubble's media array: it is the
                // only copy of them in the chat, so its id must never reach
                // a delete list (#1939). The pair is also the address the
                // final leg links the marker to.
                s.media_intermediates.push((id, dedup_text.to_string()));
                s.delivered_files.extend(fw.entries.iter().map(|entry| {
                    super::delivery::DeliveredFile {
                        path: entry.file.path.clone(),
                        message_id: id.0,
                    }
                }));
            }
            s.sent_intermediates.push(dedup_text.to_string());
        }
        return true;
    }
    // The rich send declined or failed: each file ships as its own document
    // bubble, and the marker text carries the reader's anchor. Shipping here
    // rather than only marking is what makes the marker honest: the final
    // response is deduped against the intermediates it repeats, so a marker
    // with no delivery behind it would point at a file that never reaches
    // the chat.
    if !file_scan.attachments.is_empty() {
        let (delivered, send_failures) = super::delivery::send_local_files(
            session_id,
            bot,
            chat,
            thread_id,
            &file_scan.attachments,
        )
        .await;
        if !delivered.is_empty() {
            let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
            // Each document shipped as its OWN bubble, so the pair points at
            // the document itself: the address the final leg links the
            // marker to (#1939). These ids stay out of
            // `intermediate_bubbles`, so the rich fallback deletes only the
            // text around them.
            s.delivered_files.extend(delivered.iter().cloned());
        }
        file_scan.failures.extend(send_failures);
    }
    let text = crate::utils::image::append_file_failure_notice(&marked, &file_scan.failures);

    // Resolve fences here too (#1142 parity): when the rich path rejected the
    // message, the HTML fallback must still render the diagram instead of
    // shipping raw fence text. Identical to markdown_to_telegram_html when
    // the feature is off or no fence is present.
    let html = super::rich::markdown_to_html_mermaid(&text).await;
    if html.is_empty() {
        return false;
    }
    let mut sent_pairs: Vec<(MessageId, String)> = Vec::new();
    for chunk in split_message(&html, 4096) {
        match send_html_or_plain(bot, chat, thread_id, chunk, "turn", None).await {
            Ok(id) => {
                tg.note_bot_bubble(chat.0, id.0);
                // The chunk text rides with its id: a chunked intermediate is
                // several bubbles, and only the pair says which words went
                // into which one (#2156).
                sent_pairs.push((id, chunk.to_string()));
            }
            Err(e) => {
                tracing::warn!("Telegram: rich-intermediate send failed ({e})");
                return false;
            }
        }
    }
    let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
    s.sent_intermediates.push(dedup_text.to_string());
    s.intermediate_bubbles.extend(sent_pairs);
    true
}

/// Threshold for treating a Telegram 429 as a "long rate-limit" (#1110).
///
/// When Telegram returns `Retry-After: N` where N > this threshold, the chat
/// is flood-banned for hours (28442s = 7.9 hours observed). Retrying the
/// send ladder burns 90 seconds (3 × 30s clamped wait) for no gain: the
/// window won't clear in that time. Instead, bail immediately and let the
/// caller surface the rate-limit to the user.
///
/// One hour is the boundary: typical flood windows (placeholder-edit churn,
/// command bursts) are seconds and stay under the inline cap. Anything over
/// an hour is a multi-hour ban, not a throttle.
const LONG_RATE_LIMIT_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(3600);

/// Run a Telegram send, waiting out `RetryAfter` (429) up to 3 attempts.
///
/// Command replies are programmatic: a per-chat rate limit (typically a
/// streaming turn editing its placeholder into the same chat) must DELAY
/// them, never drop them. The command branches used a bare `.await?`, so
/// the 429 propagated out of the handler and the reply vanished with a
/// single error log line — /models looked "stuck" while a turn streamed
/// and worked right after it completed (#297). Non-429 errors and
/// exhausted retries still propagate to the caller.
///
/// Long rate-limits (>1 hour) bail immediately without retrying (#1110).
pub(crate) async fn send_retrying_rate_limit<T, F, Fut>(
    what: &str,
    mut send: F,
) -> std::result::Result<T, teloxide::RequestError>
where
    F: FnMut() -> Fut,
    Fut: std::future::IntoFuture<Output = std::result::Result<T, teloxide::RequestError>>,
{
    const MAX_RETRIES: u32 = 3;
    let mut attempt = 0u32;
    loop {
        match send().await {
            Err(teloxide::RequestError::RetryAfter(secs)) => {
                let requested = secs.duration();
                // Long rate-limit (>1 hour): bail immediately, don't retry (#1110).
                // The chat is flood-banned for hours; retrying burns 90s for no gain.
                if requested > LONG_RATE_LIMIT_THRESHOLD {
                    tracing::error!(
                        "Telegram: {what} long rate-limit ({}s > {}s threshold) — bailing immediately, \
                         no retry ladder (#1110)",
                        requested.as_secs(),
                        LONG_RATE_LIMIT_THRESHOLD.as_secs()
                    );
                    return Err(teloxide::RequestError::RetryAfter(secs));
                }
                if attempt < MAX_RETRIES {
                    attempt += 1;
                    super::rate_limit::wait_out(
                        what,
                        requested,
                        &format!(" (attempt {attempt}/{MAX_RETRIES})"),
                        // No chat in scope: this ladder is generic over the send
                        // closure, and its ~20 callers pass only a label. The
                        // process-wide deadline is armed; the per-chat pause is
                        // armed by the callers that do know their chat (#635).
                        None,
                    )
                    .await;
                } else {
                    tracing::error!(
                        "Telegram: {what} still rate-limited after {MAX_RETRIES} retries ({}s) — giving up",
                        requested.as_secs()
                    );
                    return Err(teloxide::RequestError::RetryAfter(secs));
                }
            }
            // No success line here (review F1): the wrapper is generic and
            // has no correlation fields, so its line carried nothing the
            // chokepoint telemetry doesn't already say with full fields.
            other => return other,
        }
    }
}

pub(crate) async fn send_html_or_plain(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    html: &str,
    origin: &str,
    reply_to: Option<i32>,
) -> std::result::Result<MessageId, teloxide::RequestError> {
    // G3 send pacing (#1211): the universal outbox ladder funnels cron
    // deliveries, tool sends and chunked replies through here, so the
    // ~1/s + 18/min per-forum pacer applies at this one seam. DMs pass
    // through untouched; pacing delays, never drops (#297).
    super::governor::pace_send(chat_id).await;
    // Correlation telemetry (#1085 P1a, review F8): this is the chokepoint
    // carrying chunked final replies, command acks and error notices.
    // `origin` is threaded by the caller (turn | tool | cron | system) so
    // an outbox/cron send is never mislabeled "turn". Every exit logs;
    // metadata only, never content.
    let thread = thread_id.map(|t| t.0.0);
    let hash8 = super::telemetry::content_hash8(html);
    let len = html.len();
    let log_ok = |path: &str, m: &MessageId, len: usize, hash8: &str| {
        super::telemetry::log_send_success(
            origin,
            "-",
            "-",
            "html_or_plain",
            path,
            chat_id.0,
            thread,
            m.0,
            len,
            hash8,
        );
    };
    // HTML rides the shared retry ladder (#1085 P1b R1): up to 3 attempts
    // with `rate_limit::wait_out` between them (#297 delay-never-drop),
    // matching every other send path — previously this hand-rolled a single
    // retry. Only a final failure falls back to plain text, and the
    // fallback rides the same ladder so a 429 cannot drop it either.
    // `reply_to` (optional) attaches Telegram reply_parameters so the same
    // seam carries tool-reply targeting without a separate writer (#1230).
    match send_retrying_rate_limit("HTML send", || {
        let mut req = message_in_thread(bot, chat_id, thread_id, html);
        if let Some(mid) = reply_to {
            req = req.reply_parameters(ReplyParameters::new(MessageId(mid)));
        }
        req.parse_mode(ParseMode::Html)
    })
    .await
    {
        Ok(m) => {
            log_ok("html", &m.id, len, &hash8);
            Ok(m.id)
        }
        Err(e) => {
            tracing::warn!("Telegram: HTML send failed after retries ({e}), sending as plain text");
            let plain = strip_html_tags(html);
            // Review F2: hash and len must describe the text that actually
            // landed on the wire (the stripped plain text), not the HTML
            // source — a duplicate-correlation query must match payloads.
            let plain_hash8 = super::telemetry::content_hash8(&plain);
            let plain_len = plain.len();
            match send_retrying_rate_limit("plain fallback", || {
                let mut req = message_in_thread(bot, chat_id, thread_id, plain.as_str());
                if let Some(mid) = reply_to {
                    req = req.reply_parameters(ReplyParameters::new(MessageId(mid)));
                }
                req
            })
            .await
            {
                Ok(m) => {
                    log_ok("plain_fallback", &m.id, plain_len, &plain_hash8);
                    Ok(m.id)
                }
                Err(e2) => {
                    super::telemetry::log_send_failure(
                        origin,
                        "-",
                        "-",
                        "html_or_plain",
                        "plain_fallback",
                        chat_id.0,
                        thread,
                        plain_len,
                        &plain_hash8,
                        &e2.to_string(),
                    );
                    Err(e2)
                }
            }
        }
    }
}
