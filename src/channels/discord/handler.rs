//! Discord Message Handler
//!
//! Processes incoming Discord messages: text + image attachments, allowlist enforcement,
//! session routing (owner shares TUI session, others get per-user sessions).

use super::DiscordState;
use crate::brain::agent::AgentService;
use crate::channels::background_work::{
    FlowOutcome, bg_indicator_for, outcome_for_error, subagent_counts_for, waiting_verb,
};
use crate::channels::group_history;
use crate::config::{Config, RespondTo};
use crate::db::ChannelMessageRepository;
use crate::db::models::ChannelMessage as DbChannelMessage;
use crate::services::SessionService;
use crate::utils::sanitize::redact_secrets;
use crate::utils::truncate_str;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use uuid::Uuid;

use serenity::builder::{CreateAttachment, CreateMessage};
use serenity::http::Http;
use serenity::model::channel::{Message, MessageFlags, MessageSnapshot};
use serenity::model::id::ChannelId;
use serenity::prelude::*;

/// Split a message into chunks that fit Discord's 2000 char limit.
/// Whether a chunk ending here leaves no markup open (#876).
///
/// Chunks are sent as separate messages and parsed independently, so an
/// unclosed `<code>`/`<b>` or an odd number of backticks makes that chunk
/// invalid on its own. Counts backtick runs and unclosed HTML start tags.
///
/// Conservative by design: it only ever moves a break EARLIER, never past the
/// length limit, so a false negative costs a slightly shorter chunk and a false
/// positive is the behaviour that already shipped.
fn splits_cleanly(prefix: &str) -> bool {
    if !prefix.matches('`').count().is_multiple_of(2) {
        return false;
    }
    let mut depth: i32 = 0;
    let mut rest = prefix;
    while let Some(open) = rest.find('<') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            return false; // a start tag left dangling mid-chunk
        };
        let tag = &after[..close];
        if !tag.starts_with('!') {
            if tag.starts_with('/') {
                depth -= 1;
            } else if !tag.ends_with('/') {
                depth += 1;
            }
        }
        rest = &after[close + 1..];
    }
    depth == 0
}

pub fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    // Opener prepended to the next chunk when this one had to close a fence
    // early (4 bytes: "```\n"). Budgeted against max_len below.
    let mut reopen = String::new();
    while start < text.len() {
        let budget = max_len.saturating_sub(reopen.len());
        let mut end = (start + budget).min(text.len());
        // Ensure end falls on a char boundary (back up if inside a multi-byte char)
        while end < text.len() && !text.is_char_boundary(end) {
            end -= 1;
        }
        let break_at = if end < text.len() {
            // Prefer a newline near the limit, as before. Then, among the
            // newlines in the window, prefer one that does NOT sit inside an
            // open code span or HTML tag (#876): breaking there leaves an
            // unclosed `<code>` or a lone backtick, which Telegram rejects when
            // it parses each chunk independently.
            //
            // Strictly an improvement: when no markup spans the boundary the
            // first candidate is already safe and the result is byte-identical
            // to the old behaviour. Only the previously-broken case moves.
            let window = &text[start..end];
            let floor = (end - start).saturating_sub(200);
            // When a fence is already re-opened at the top of this chunk, a
            // prefix is "clean" only if it CLOSES that fence (odd backtick
            // count on its own).
            let want_even = reopen.is_empty();
            let mut chosen = None;
            for (pos, _) in window.char_indices().rev().filter(|(_, c)| *c == '\n') {
                if pos <= floor {
                    break;
                }
                if splits_cleanly(&window[..pos]) == want_even {
                    chosen = Some(start + pos + 1);
                    break;
                }
            }
            chosen
                .or_else(|| {
                    // Nothing safe near the limit. Widen the scan over the whole
                    // remaining span before falling back: a fenced block (a table
                    // grid) longer than the preference window should travel whole
                    // into the next chunk instead of being cut open here.
                    for (pos, _) in window.char_indices().rev().filter(|(_, c)| *c == '\n') {
                        if splits_cleanly(&window[..pos]) == want_even {
                            return Some(start + pos + 1);
                        }
                    }
                    None
                })
                .unwrap_or_else(|| {
                    window
                        .rfind('\n')
                        .filter(|&pos| pos > floor)
                        .map(|pos| start + pos + 1)
                        .unwrap_or(end)
                })
        } else {
            end
        };
        let piece = &text[start..break_at];
        let mut chunk = String::with_capacity(reopen.len() + piece.len() + 8);
        chunk.push_str(&reopen);
        // Balance check spans the reopened fence: an opener contributes 3
        // backticks, so with one active the piece must be odd to close it.
        let piece_bt = piece.matches('`').count();
        let fence_open = !(piece_bt + if reopen.is_empty() { 0 } else { 3 }).is_multiple_of(2);
        if !fence_open || break_at >= text.len() {
            chunk.push_str(piece);
            reopen.clear();
        } else if start + piece.len() < text.len() {
            // The break landed inside open markup — realistically a fenced
            // block the fallbacks couldn't dodge. Close the fence here and
            // re-open it at the top of the next chunk so every chunk parses
            // on its own (#876 family).
            chunk.push_str(piece);
            chunk.push_str("\n```");
            reopen = String::from("```\n");
        } else {
            chunk.push_str(piece);
            reopen.clear();
        }
        chunks.push(chunk);
        start = break_at;
    }
    chunks
}

/// Flow-line re-render interval (#1843): one edit per tick, deliberately
/// slower than Telegram's 1500 ms. Discord's current docs do not publish a
/// fixed per-route edit budget and explicitly forbid hardcoding one
/// ("rate limits should not be hard coded into your app... parse response
/// headers... and respond accordingly"); serenity 0.12 ships a built-in
/// per-bucket ratelimiter (src/http/ratelimiting.rs) that pre-emptively
/// queues requests and honors retry_after, so safety comes from the
/// limiter, not from a magic number. 4 s matches the Slack ticker (#1807)
/// for cross-channel parity.
const FLOW_TICKER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(4);
/// Hard stop for orphaned ticks (crashed turn): no immortal tasks.
const FLOW_TICKER_CAP: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Re-render the turn's flow group on an interval (#1843): the Discord twin
/// of Slack's `spawn_flow_ticker` and Telegram's `spawn_edit_loop`. Without
/// it the clock freezes between tool events. Waits for the group message to
/// be born (first tool call), exits on settle/prune/cap, and re-snapshots
/// after each edit so the settled line keeps the last word.
pub(super) fn spawn_flow_ticker(
    http: Arc<serenity::http::Http>,
    channel: serenity::model::id::ChannelId,
    group_mid: Arc<Mutex<Option<serenity::model::id::MessageId>>>,
    dstate: Arc<super::state::DiscordState>,
) {
    tokio::spawn(async move {
        use serenity::builder::EditMessage;
        let born = std::time::Instant::now();
        loop {
            tokio::time::sleep(FLOW_TICKER_INTERVAL).await;
            if born.elapsed() > FLOW_TICKER_CAP {
                break;
            }
            let Some(mid) = *group_mid.lock().await else {
                // Group not born yet: the turn has not reached its first
                // tool call. Keep waiting.
                continue;
            };
            let Some(group) = dstate.tool_group_snapshot(mid.get()).await else {
                break; // pruned by retention mid-turn
            };
            if group.settled.is_some() {
                break; // settle already posted the final line
            }
            // #1910: chrome yields to content. This paint carries nothing a
            // later one will not restate, so a refusal is a free skip: the
            // budget and the spacing window stay available for the content
            // that has to land at settle. A channel parked by a 429 is refused
            // by the same call, which is what stops the ticker fighting a
            // window the API already named.
            if !dstate.governor.chrome_admits(channel.get()) {
                tracing::debug!(
                    "Discord: flow ticker edit dropped by governor (mid={})",
                    mid.get()
                );
                continue;
            }
            let edit = EditMessage::new()
                .content(super::tool_group::render_content(&group))
                .embeds(super::tool_group::render_embeds(&group))
                .components(super::tool_group::render_components(&group, mid.get()));
            // The ticker is the one chrome writer that pays for its paint up
            // front, because a refusal here means skipping the rest of the
            // tick, not just the API call. That is also why the edit itself
            // goes out directly instead of through `edit_chrome`: the check
            // above already charged this paint, and the helper would charge
            // it a second time.
            if let Err(e) = channel.edit_message(&http, mid, edit).await {
                super::governor::note_if_429(&dstate.governor, channel.get(), Some(&e));
                tracing::warn!("Discord: flow ticker edit failed (mid={}): {e}", mid.get());
            }
            // #1912: the plan card rides this same tick and this same
            // per-channel budget instead of getting a timer of its own. An
            // unchanged checklist paints nothing at all, so sharing the cadence
            // costs a disk read, not an edit.
            super::plan_card::refresh_for_channel(&dstate, &http, channel).await;
            // Race guard: settle may have stamped and posted while this
            // tick's edit was in flight. The settled line must be last,
            // so if the group settled behind us, re-render its content once.
            match dstate.tool_group_snapshot(mid.get()).await {
                Some(re) if re.settled.is_some() => {
                    // The settled line is the one chrome write that must not be
                    // lost to pacing: it is the last thing this bubble will ever
                    // say. It goes through the content path, which holds instead
                    // of dropping, and still yields to the park.
                    dstate
                        .governor
                        .gate(channel.get(), super::governor::Surface::Final)
                        .await;
                    let edit = EditMessage::new()
                        .content(super::tool_group::render_content(&re))
                        .embeds(super::tool_group::render_embeds(&re))
                        .components(super::tool_group::render_components(&re, mid.get()));
                    // Same shape as the chrome tick: the `gate` above is what
                    // paid for this write, so the edit goes out directly and
                    // only the 429 learning is shared.
                    if let Err(e) = channel.edit_message(&http, mid, edit).await {
                        super::governor::note_if_429(&dstate.governor, channel.get(), Some(&e));
                        tracing::warn!("Discord: flow ticker settle fixup failed: {e}");
                    }
                    break;
                }
                Some(_) => {}  // still live: keep ticking
                None => break, // pruned mid-tick
            }
        }
    });
}

/// Fold forwarded payloads into display text (#1891).
///
/// Discord message forwards never touch `Message::content` or the top-level
/// attachments; the payload lives in `message_snapshots`, which this handler
/// used to ignore entirely, so a pure forward was invisible both to the agent
/// turn and to the channel history. Images keep the vision-first
/// `<<IMG:url>>` marker format used for regular attachments; other files carry
/// their name and CDN URL so the agent can fetch them on demand.
pub(crate) fn forwarded_snapshot_text(snapshots: &[MessageSnapshot]) -> String {
    let mut out = String::new();
    for snap in snapshots {
        let text = snap.content.trim();
        if !text.is_empty() {
            out.push_str(&format!("\n\n[forwarded message]: {text}"));
        }
        for att in &snap.attachments {
            let mime = att.content_type.as_deref().unwrap_or("");
            if mime.starts_with("image/") {
                out.push_str(&format!(" <<IMG:{}>>", att.url));
            } else {
                out.push_str(&format!(
                    "\n[forwarded attachment]: {} {}",
                    att.filename, att.url
                ));
            }
        }
    }
    out
}

/// Combine a message's own text with its forwarded payloads (#1891 shape),
/// reused for replied-to messages so a bare mention reading a forwarded
/// original sees the payload too (#1890).
pub(crate) fn folded_message_text(content: &str, snapshots: &[MessageSnapshot]) -> String {
    let fwd = forwarded_snapshot_text(snapshots);
    if fwd.is_empty() {
        content.to_string()
    } else if content.trim().is_empty() {
        fwd.trim_start().to_string()
    } else {
        format!("{content}{fwd}")
    }
}

/// What to do with a message that is empty of text and attachments after
/// stripping. Pure so the branch contract from #1890 is testable without a
/// live Context.
pub(crate) enum EmptyContentDecision {
    /// The message qualified (mention mode) despite coming up empty:
    /// dispatch with this visible context instead of vanishing.
    DispatchWith(String),
    /// Nothing qualified; drop it, but leave a reason the caller can log.
    Drop(&'static str),
}

pub(crate) fn decide_empty_content(
    in_mention_mode: bool,
    replied_folded: Option<&str>,
) -> EmptyContentDecision {
    if in_mention_mode {
        EmptyContentDecision::DispatchWith(bare_mention_content(replied_folded))
    } else {
        EmptyContentDecision::Drop("no content, no attachments, and no qualifying mention")
    }
}

/// Content for a bare @mention whose tag was just stripped (#1890). When the
/// mention rode a reply, the replied-to text is the payload the user meant to
/// send; without it, the ping itself is still a dispatchable turn.
pub(crate) fn bare_mention_content(replied_folded: Option<&str>) -> String {
    match replied_folded.map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => format!("The user mentioned you in reply to this message:\n\n{text}"),
        None => "The user mentioned you with no other content.".to_string(),
    }
}

/// Fetch the message a bare mention replied to and fold its forwards in.
/// `None` when there is no reply reference or the fetch fails — the caller
/// still dispatches the ping, just without extra context.
async fn resolve_replied_folded(ctx: &Context, msg: &Message) -> Option<String> {
    let reference = msg.message_reference.as_ref()?;
    let referenced = reference.message_id?;
    let channel = reference.channel_id;
    match ctx.http.get_message(channel, referenced).await {
        Ok(replied) => Some(folded_message_text(
            &replied.content,
            &replied.message_snapshots,
        )),
        Err(e) => {
            tracing::warn!(error = %e, "Discord: could not resolve reply target of a bare mention (#1890)");
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_message(
    ctx: &Context,
    msg: &Message,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    shared_session: Arc<Mutex<Option<Uuid>>>,
    discord_state: Arc<DiscordState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
) {
    // Read latest config from watch channel — single source of truth
    let cfg = config_rx.borrow().clone();
    let dc_cfg = &cfg.channels.discord;
    let allowed: HashSet<i64> = dc_cfg
        .allowed_users
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let allowed_channels: HashSet<String> = dc_cfg.allowed_channels.iter().cloned().collect();
    let idle_timeout_hours = dc_cfg.session_idle_hours;
    let voice_config = cfg.voice_config();

    let user_id = msg.author.id.get() as i64;

    // Forwarded payloads belong in history too, not just in the agent's turn
    // (#1891) — the raw `msg.content` of a pure forward is empty.
    let forwarded_history = forwarded_snapshot_text(&msg.message_snapshots);

    // Helper: passively capture a channel message for history
    let store_channel_msg = |text: String| {
        let repo = channel_msg_repo.clone();
        let fwd = forwarded_history.clone();
        let channel_chat_id = msg.channel_id.get().to_string();
        let guild_name = msg
            .guild_id
            .map(|g| g.get().to_string())
            .unwrap_or_else(|| "DM".to_string());
        let sender_id = msg.author.id.get().to_string();
        let sender_name = msg.author.name.clone();
        let msg_id = msg.id.get().to_string();
        async move {
            let mut text = text;
            if !fwd.is_empty() {
                if text.is_empty() {
                    text = fwd.trim_start().to_string();
                } else {
                    text.push_str(&fwd);
                }
            }
            if text.is_empty() {
                return;
            }
            let cm = DbChannelMessage::new(
                "discord".into(),
                channel_chat_id,
                Some(guild_name),
                sender_id,
                sender_name,
                text,
                "text".into(),
                Some(msg_id),
            );
            if let Err(e) = repo.insert(&cm).await {
                tracing::warn!("Failed to store Discord channel message: {e}");
            }
        }
    };

    // Channel settings (#2014). A thread or forum post resolves through its
    // parent channel (#384); the lookup runs only when the operator set up
    // something a parent can carry, so a default install costs nothing.
    let is_dm = msg.guild_id.is_none();
    let channel_str = msg.channel_id.get().to_string();
    let needs_parent = !is_dm && (!allowed_channels.is_empty() || !dc_cfg.channels.is_empty());
    let parent_id: Option<String> = if needs_parent {
        super::commands::parent_channel_id(&ctx.http, msg.channel_id).await
    } else {
        None
    };
    let open_here = !is_dm && dc_cfg.channel_open(&channel_str, parent_id.as_deref());
    let respond_to_here = dc_cfg.respond_to_for(&channel_str, parent_id.as_deref());
    let respond_to = &respond_to_here;

    // Deny-by-default allowlist (OC-02). An empty allowlist used to accept
    // everyone, unlike Telegram, which denies an unconfigured channel. Now a
    // channel with no allowed_users, no allowed_roles, and no bot_owner denies;
    // a configured one admits only allowlisted users, holders of an allowed
    // role (#387, evaluated per message), or the owner. Roles are evaluated
    // even when allowed_users is empty, and a DM is never treated as
    // role-granted (guild roles do not apply to a DM).
    let is_owner = crate::config::owner::is_owner(
        &dc_cfg.allowed_users,
        &dc_cfg.bot_owner,
        &user_id.to_string(),
    );
    let in_allowlist = allowed.contains(&user_id);
    let role_granted = msg.guild_id.is_some()
        && !dc_cfg.allowed_roles.is_empty()
        && msg.member.as_ref().is_some_and(|m| {
            m.roles.iter().any(|r| {
                dc_cfg
                    .allowed_roles
                    .iter()
                    .any(|ar| ar == &r.get().to_string())
            })
        });
    let unconfigured =
        allowed.is_empty() && dc_cfg.allowed_roles.is_empty() && dc_cfg.bot_owner.is_empty();
    if unconfigured || !(is_owner || in_allowlist || role_granted || open_here) {
        tracing::debug!(
            "Discord: ignoring message from non-allowed user {} (deny-by-default, OC-02)",
            user_id
        );
        return;
    }

    // The owner's /cowork and /respond_to bypass the allowed_channels scope and
    // the dm_only policy below: they are how the owner opens or fixes the channel.
    let owner_scope = !is_dm && super::commands::owner_scope_text(&msg.content, is_owner);
    // respond_to / allowed_channels filtering — DMs always pass
    if !is_dm && !owner_scope {
        // Check allowed_channels (empty = all channels allowed). Threads and
        // forum posts (#384) carry their own channel id, so a miss falls back
        // to the PARENT channel: allow-listing a forum allows every post in
        // it, each post keeping its own per-thread session.
        if !allowed_channels.is_empty()
            && !allowed_channels.contains(&channel_str)
            && !dc_cfg.has_channel_block(&channel_str, parent_id.as_deref())
        {
            let parent_allowed = parent_id
                .as_deref()
                .is_some_and(|p| allowed_channels.contains(p));
            if !parent_allowed {
                tracing::debug!(
                    "Discord: ignoring message in non-allowed channel {} (parent not allowed either)",
                    channel_str
                );
                store_channel_msg(msg.content.clone()).await;
                return;
            }
        }

        match respond_to {
            RespondTo::DmOnly if !owner_scope => {
                tracing::debug!("Discord: respond_to=dm_only, ignoring channel message");
                store_channel_msg(msg.content.clone()).await;
                return;
            }
            RespondTo::DmOnly => {} // owner scope command in a dm_only channel
            RespondTo::Mention => {
                let bot_id = discord_state.bot_user_id().await;
                let mentioned =
                    bot_id.is_some_and(|bid| msg.mentions.iter().any(|u| u.id.get() == bid));
                // The owner's own /respond_to is solicited, so it passes the gate
                // unmentioned (#2016). Everything else stays dropped.
                let owner_scope_command = is_owner
                    && (crate::channels::respond_to_scope::is_respond_to_command(&msg.content)
                        || super::cowork::is_cowork_command(&msg.content));
                if !mentioned && !owner_scope_command {
                    tracing::debug!("Discord: respond_to=mention, bot not mentioned — ignoring");
                    store_channel_msg(msg.content.clone()).await;
                    return;
                }
            }
            RespondTo::All => {} // pass through
            RespondTo::Auto => {
                // Active sender tracking not implemented for Discord yet;
                // fall back to mention-only behaviour (#244).
                let bot_id = discord_state.bot_user_id().await;
                let mentioned =
                    bot_id.is_some_and(|bid| msg.mentions.iter().any(|u| u.id.get() == bid));
                if !mentioned {
                    tracing::debug!("Discord: respond_to=auto, bot not mentioned — ignoring");
                    store_channel_msg(msg.content.clone()).await;
                    return;
                }
            }
        }
    }

    // Store the directed message for history. DMs too (#2025): channel_search
    // must see a DM conversation, and the session table alone is not searchable.
    store_channel_msg(msg.content.clone()).await;

    // Check for audio attachments → STT
    let audio_attachment = msg.attachments.iter().find(|a| {
        a.content_type
            .as_ref()
            .is_some_and(|ct| ct.starts_with("audio/"))
    });

    let mut is_voice = false;
    let mut content = msg.content.clone();

    // Bang-thread (opt-in): "!question" anchors a thread to this message and
    // routes the whole turn into it. `target` is the display channel for
    // everything downstream (tool bubble, intermediates, answer, gallery).
    // DMs have no threads — fall through untouched.
    let mut target = msg.channel_id;
    if dc_cfg.bang_new_thread && msg.guild_id.is_some() && content.starts_with('!') {
        let stripped = content[1..].trim_start().to_string();
        if !stripped.is_empty() {
            content = stripped;
            let title = thread_title(&content);
            let body = serde_json::json!({ "name": title });
            match ctx
                .http
                .create_thread_from_message(msg.channel_id, msg.id, &body, None)
                .await
            {
                Ok(thread) => target = thread.id,
                Err(e) => {
                    tracing::warn!("Discord: bang-thread creation failed, replying inline: {e}")
                }
            }
        }
    }

    // Show typing immediately when processing voice
    if audio_attachment.is_some()
        && voice_config.stt_enabled
        && let Err(e) = msg.channel_id.broadcast_typing(&ctx.http).await
    {
        tracing::warn!(error = %e, "failed to broadcast Discord typing");
    }

    if let Some(audio) = audio_attachment
        && voice_config.stt_enabled
        && let Ok(resp) = reqwest::get(&audio.url).await
        && let Ok(bytes) = resp.bytes().await
    {
        match crate::channels::voice::transcribe(bytes.to_vec(), &voice_config).await {
            Ok(transcript) => {
                tracing::info!(
                    "Discord: transcribed voice: {}",
                    truncate_str(&transcript, 80)
                );
                content = transcript;
                is_voice = true;
            }
            Err(e) => tracing::error!("Discord: STT error: {e}"),
        }
    }

    // Strip bot @mention from content when responding to a mention
    if !is_dm
        && respond_to == &RespondTo::Mention
        && let Some(bot_id) = discord_state.bot_user_id().await
    {
        let mention_tag = format!("<@{}>", bot_id);
        content = content.replace(&mention_tag, "").trim().to_string();
    }
    // Surface forwarded payloads before the emptiness guard (#1891): a
    // mention + pure forward has empty content and no top-level attachments,
    // and used to be dropped here as noise before ever reaching the agent.
    content = folded_message_text(&content, &msg.message_snapshots);
    if content.is_empty() && msg.attachments.is_empty() {
        // The strip above can empty a message that DID qualify at the gate:
        // a bare @mention, usually a reply to a missed message (#1890).
        // Resolve what it replied to and dispatch anyway; everything else
        // keeps dropping, but never silently.
        let in_mention_mode = !is_dm && respond_to == &RespondTo::Mention;
        let replied_folded = if in_mention_mode {
            resolve_replied_folded(ctx, msg).await
        } else {
            None
        };
        match decide_empty_content(in_mention_mode, replied_folded.as_deref()) {
            EmptyContentDecision::DispatchWith(text) => {
                tracing::debug!(
                    "Discord: bare mention after tag-strip resolved to a dispatch (#1890)"
                );
                content = text;
            }
            EmptyContentDecision::Drop(reason) => {
                tracing::debug!("Discord: dropping empty message: {reason}");
                return;
            }
        }
    }

    // Handle attachments — vision-first pipeline
    if !is_voice {
        use crate::utils::{inject_file_content, process_file_with_vision};
        for attachment in &msg.attachments {
            let mime = attachment.content_type.as_deref().unwrap_or("");
            let fname = &attachment.filename;

            if mime.starts_with("audio/") {
                continue;
            }
            let is_image = mime.starts_with("image/");
            if is_image {
                if content.is_empty() {
                    content = "Describe this image.".to_string();
                }
                content.push_str(&format!(" <<IMG:{}>>", attachment.url));
            }

            let bytes = match reqwest::get(attachment.url.as_str()).await {
                Ok(resp) => match resp.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("Discord: failed to read attachment {fname}: {e}");
                        continue;
                    }
                },
                Err(e) => {
                    tracing::warn!("Discord: failed to download attachment {fname}: {e}");
                    continue;
                }
            };

            // Durable copy in the channel-attachment store, as Telegram and WhatsApp keep (#1729).
            if let Some(path) =
                crate::channels::persist_channel_attachment("discord", fname, &bytes)
            {
                content.push_str(&format!("\n\n[file: {}]", path.display()));
            }

            if !is_image {
                let cfg = config_rx.borrow();
                let fc = process_file_with_vision(&bytes, mime, fname, &cfg);
                let injected = inject_file_content(&fc).0;
                if !injected.is_empty() {
                    content.push_str(&format!("\n\n{injected}"));
                }
            }
        }
    }

    if content.is_empty() {
        return;
    }

    let text_preview = truncate_str(&content, 50);
    tracing::info!(
        "Discord: message from {} ({}): {}",
        msg.author.name,
        user_id,
        text_preview
    );

    // Track owner's channel for proactive messaging
    let is_owner = dc_cfg.is_owner(&user_id.to_string());

    if is_owner {
        discord_state.set_owner_channel(msg.channel_id.get()).await;
    }

    // Track guild ID for guild-scoped actions (kick, ban, roles, list_channels)
    if let Some(guild_id) = msg.guild_id {
        discord_state.set_guild_id(guild_id.get()).await;
    }

    // Sessions are ALWAYS isolated per chat — owner DMs no longer share the
    // TUI session. DMs keyed by author user_id; guild channels by channel_id.
    // Title carries a stable `[chat:discord-…]` suffix so auto-rename rewrites
    // the visible label but `find_session_by_title_suffix` still resolves the
    // same row (issue #121, pre-fix every renamed session was orphaned).
    let session_id = {
        use crate::channels::session_resolve;
        let (id_str, legacy_title) = if is_dm {
            (
                format!("discord-dm-{}", msg.author.id.get()),
                format!("Discord: DM {} ({})", msg.author.name, msg.author.id.get()),
            )
        } else {
            (
                format!("discord-{}", msg.channel_id.get()),
                format!("Discord: #{}", msg.channel_id.get()),
            )
        };
        let suffix = session_resolve::chat_id_suffix(&id_str);
        let session_title = format!("{legacy_title} {suffix}");

        match session_resolve::resolve_or_create_channel_session(
            &session_svc,
            &suffix,
            &legacy_title,
            &session_title,
            idle_timeout_hours,
            "Discord",
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Discord: failed to resolve session: {e:#} (#442)");
                if let Err(send_err) = super::governor::say(
                    &discord_state.governor,
                    msg.channel_id,
                    &ctx.http,
                    super::governor::Surface::Send,
                    format!(
                        "⚠️ Could not load this chat's session ({e}). Your history is \
                             intact and this message was NOT processed. Try again, or send \
                             /new if you deliberately want a fresh session."
                    ),
                )
                .await
                {
                    tracing::warn!(error = %send_err, "failed to send Discord session error message");
                }
                return;
            }
        }
    };

    // Session gate (#1051, ADR-003): mark group sessions so memory_search
    // keeps external index content out of them by default.
    if !is_dm {
        crate::memory::mark_session_shared(session_id);
    }

    // Fast-cancel: any recognised stop intent, in any supported language (#965).
    //
    // Cancellation is scoped to explicit stop requests. A follow-up message
    // never cancels: it joins the live turn's queue through the turn slot
    // (#1990). Channel commands like /models,
    // /help, /usage, /new must NEVER abort an in-flight task: switching models
    // applies to the next run, it does not drop current work (#266).
    if crate::utils::stop_intent::is_stop_command_or_intent(&msg.content) {
        discord_state.cancel_session(session_id).await;
        if let Err(e) = super::governor::say(
            &discord_state.governor,
            msg.channel_id,
            &ctx.http,
            super::governor::Surface::Send,
            "Operation cancelled.",
        )
        .await
        {
            tracing::warn!(error = %e, "failed to send Discord message");
        }
        return;
    }

    // Restore session's own provider (each session keeps its provider independently)
    let session_meta = session_svc.get_session(session_id).await.ok().flatten();
    crate::channels::commands::sync_provider_for_session(
        &agent,
        session_id,
        session_meta
            .as_ref()
            .and_then(|s| s.provider_name.as_deref()),
        session_meta.as_ref().and_then(|s| s.model.as_deref()),
    )
    .await;

    // `/respond_to` and `/cowork` are answered here, not by the shared parser:
    // `/respond_to` would write Telegram's section with no chat id (#2013), and
    // `/cowork` is Telegram's group flow (#2015).
    let cowork_name = if !is_dm && super::cowork::is_cowork_command(&content) {
        super::cowork::channel_name(&ctx.http, msg.channel_id).await
    } else {
        None
    };
    let scope_reply = super::cowork::cowork_discord_channel(
        &content,
        is_owner,
        !is_dm,
        &channel_str,
        cowork_name.as_deref(),
        super::cowork::write_channel_open,
    )
    .or_else(|| {
        crate::channels::respond_to_scope::respond_to_discord_channel(
            &content,
            is_owner,
            &channel_str,
            &respond_to_here,
            super::commands::write_channel_respond_to,
        )
    });
    if let Some(reply) = scope_reply {
        if let Err(e) = msg.channel_id.say(&ctx.http, reply).await {
            tracing::warn!(error = %e, "failed to send Discord message");
        }
        return;
    }

    // ── Channel commands (/help, /usage, /models) ──────────────────────────
    {
        use crate::channels::commands::{self, ChannelCommand};
        let cmd =
            commands::handle_command(&content, session_id, &agent, &session_svc, is_owner, None)
                .await;

        // Handle simple text-response commands (Help, Usage, Evolve, Doctor, etc.)
        if let Some(reply) = commands::try_execute_text_command(&cmd).await {
            if let Err(e) = super::governor::say(
                &discord_state.governor,
                msg.channel_id,
                &ctx.http,
                super::governor::Surface::Send,
                &reply,
            )
            .await
            {
                tracing::warn!(error = %e, "failed to send Discord message");
            }
            return;
        }

        match cmd {
            ChannelCommand::Models(resp) => {
                use serenity::builder::{CreateActionRow, CreateButton, CreateMessage};
                use serenity::model::application::ButtonStyle;
                // Show provider buttons (step 1 of two-step flow)
                let rows: Vec<CreateActionRow> = resp
                    .providers
                    .chunks(5)
                    .take(5)
                    .map(|chunk| {
                        CreateActionRow::Buttons(
                            chunk
                                .iter()
                                .map(|(name, label, configured)| {
                                    let marker = crate::channels::commands::provider_marker(
                                        name,
                                        &resp.current_provider,
                                        *configured,
                                    );
                                    let display = match marker {
                                        "🔒" => format!("🔒 {} (setup)", label),
                                        "✓" => format!("✓ {}", label),
                                        _ => label.clone(),
                                    };
                                    let display = if display.len() > 80 {
                                        format!("{}…", display.chars().take(79).collect::<String>())
                                    } else {
                                        display
                                    };
                                    let cb = if *configured {
                                        format!("provider:{}", name)
                                    } else {
                                        format!("setup:{}", name)
                                    };
                                    CreateButton::new(cb)
                                        .label(display)
                                        .style(ButtonStyle::Secondary)
                                })
                                .collect(),
                        )
                    })
                    .collect();
                let builder = CreateMessage::new().content(&resp.text).components(rows);
                // Never silent (#1019): this IS the reply. A swallowed failure
                // here is indistinguishable from the agent choosing not to
                // answer, and the user has no way to tell or report it.
                if let Err(e) = super::governor::send_content(
                    &discord_state.governor,
                    msg.channel_id,
                    &ctx.http,
                    super::governor::Surface::Send,
                    builder,
                )
                .await
                {
                    tracing::error!(
                        "Discord: reply with components failed in channel {}: {e}",
                        msg.channel_id
                    );
                }
                return;
            }
            ChannelCommand::NewSession => {
                // MUST match the per-message resolver format above —
                // DM titles include the author id so /new and the next
                // typed message land on the same row (issue #89).
                let session_title = if is_dm {
                    format!("Discord: DM {} ({})", msg.author.name, msg.author.id.get())
                } else {
                    format!("Discord: #{}", msg.channel_id.get())
                };
                // The new session inherits its working directory from the
                // session that received this /new (same chat), not the global
                // most-recent session (#263).
                let prior_session = session_svc
                    .find_session_by_title(&session_title)
                    .await
                    .unwrap_or_else(|e| {
                        // /new means a fresh session IS the intent; the
                        // failure is logged, never silent (#442).
                        tracing::error!("Discord: /new prior-session lookup failed: {e:#}");
                        None
                    });
                // Archive the previous session on /new, except for the owner —
                // owner sessions stay non-archived so they remain visible in
                // /sessions for history review. Guest sessions get archived
                // so the next title lookup resolves cleanly to the new row.
                if !is_owner
                    && let Some(old) = prior_session.as_ref()
                    && let Err(e) = session_svc.archive_session(old.id).await
                {
                    tracing::error!("Discord: failed to archive old session {}: {}", old.id, e);
                }
                match crate::channels::session_init::create_channel_session(
                    &session_svc,
                    Some(session_title),
                    prior_session.as_ref(),
                )
                .await
                {
                    Ok(new_session) => {
                        if is_owner && is_dm {
                            *shared_session.lock().await = Some(new_session.id);
                        }
                        discord_state
                            .register_session_channel(new_session.id, msg.channel_id.get())
                            .await;
                        // Sync provider for the new session so baseline is accurate
                        let new_meta = session_svc.get_session(new_session.id).await.ok().flatten();
                        crate::channels::commands::sync_provider_for_session(
                            &agent,
                            new_session.id,
                            new_meta.as_ref().and_then(|s| s.provider_name.as_deref()),
                            new_meta.as_ref().and_then(|s| s.model.as_deref()),
                        )
                        .await;
                        let baseline = agent.base_context_tokens();
                        let ctx_max = agent.context_limit_for_session(new_session.id);
                        let footer = crate::utils::format_ctx_footer(baseline, ctx_max, None);
                        let msg_text = format!("✅ New session started.\n\n{footer}");
                        if let Err(e) = super::governor::say(
                            &discord_state.governor,
                            msg.channel_id,
                            &ctx.http,
                            super::governor::Surface::Send,
                            &msg_text,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "failed to send Discord message");
                        }
                        tracing::info!(
                            "Discord /new: sent ctx footer='{}' (baseline={}, ctx_max={})",
                            footer,
                            baseline,
                            ctx_max,
                        );
                    }
                    Err(e) => {
                        tracing::error!("Discord: failed to create session: {}", e);
                        if let Err(send_err) = super::governor::say(
                            &discord_state.governor,
                            msg.channel_id,
                            &ctx.http,
                            super::governor::Surface::Send,
                            "Failed to create session.",
                        )
                        .await
                        {
                            tracing::warn!(error = %send_err, "failed to send Discord session creation error");
                        }
                    }
                }
                return;
            }
            ChannelCommand::Sessions(resp) => {
                use serenity::builder::{CreateActionRow, CreateButton, CreateMessage};
                use serenity::model::application::ButtonStyle;
                let rows: Vec<CreateActionRow> = resp
                    .sessions
                    .chunks(5)
                    .take(5)
                    .map(|chunk| {
                        CreateActionRow::Buttons(
                            chunk
                                .iter()
                                .map(|(id, label)| {
                                    let display = if *id == resp.current_session_id {
                                        format!("▸ {} ← current", label)
                                    } else {
                                        label.clone()
                                    };
                                    let display = if display.len() > 80 {
                                        format!("{}…", display.chars().take(79).collect::<String>())
                                    } else {
                                        display
                                    };
                                    CreateButton::new(format!("session:{}", id))
                                        .label(display)
                                        .style(ButtonStyle::Secondary)
                                })
                                .collect(),
                        )
                    })
                    .collect();
                let builder = CreateMessage::new().content(&resp.text).components(rows);
                // Never silent (#1019): this IS the reply. A swallowed failure
                // here is indistinguishable from the agent choosing not to
                // answer, and the user has no way to tell or report it.
                if let Err(e) = super::governor::send_content(
                    &discord_state.governor,
                    msg.channel_id,
                    &ctx.http,
                    super::governor::Surface::Send,
                    builder,
                )
                .await
                {
                    tracing::error!(
                        "Discord: reply with components failed in channel {}: {e}",
                        msg.channel_id
                    );
                }
                return;
            }
            ChannelCommand::Stop => {
                let cancelled = discord_state.cancel_session(session_id).await;
                let reply = if cancelled {
                    "Operation cancelled."
                } else {
                    "No operation in progress."
                };
                if let Err(e) = super::governor::say(
                    &discord_state.governor,
                    msg.channel_id,
                    &ctx.http,
                    super::governor::Surface::Send,
                    reply,
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to send Discord message");
                }
                return;
            }
            ChannelCommand::Compact => {
                if let Err(e) = super::governor::say(
                    &discord_state.governor,
                    msg.channel_id,
                    &ctx.http,
                    super::governor::Surface::Send,
                    "⏳ Compacting context...",
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to send Discord compact notification");
                }
                content =
                    "[SYSTEM: Compact context now. Summarize this conversation for continuity.]"
                        .to_string();
            }
            ChannelCommand::ClearContext => {
                // No agent turn: the marker row is the whole operation (#1585).
                let reply = match agent.clear_context(session_id).await {
                    Ok(receipt) => receipt.user_line(),
                    Err(e) => {
                        tracing::error!("/clear failed: {e}");
                        format!("/clear did nothing, the context is unchanged: {e}")
                    }
                };
                if let Err(e) = super::governor::say(
                    &discord_state.governor,
                    msg.channel_id,
                    &ctx.http,
                    super::governor::Surface::Send,
                    reply,
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to send Discord clear receipt");
                }
                return;
            }
            ChannelCommand::UserPrompt(prompt) => {
                content = prompt;
                // fall through to agent with the prompt as the message
            }
            ChannelCommand::NotACommand => {}
            // Help, Usage, Evolve, Doctor, UserSystem handled by try_execute_text_command above
            ChannelCommand::Profiles(resp) => {
                if let Err(e) = super::governor::say(
                    &discord_state.governor,
                    msg.channel_id,
                    &ctx.http,
                    super::governor::Surface::Send,
                    &resp.text,
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to send Discord message");
                }
                return;
            }
            _ => {}
        }
    }

    // Extract replied-to message context so the agent knows what the user is referencing.
    let reply_context = msg.referenced_message.as_ref().and_then(|reply| {
        let reply_text = crate::utils::strip_ctx_footer(reply.content.trim());
        if reply_text.is_empty() {
            return None;
        }
        let reply_sender = if reply.author.bot {
            "assistant".to_string()
        } else {
            reply.author.name.clone()
        };
        Some(format!("[Replying to {reply_sender}: \"{reply_text}\"]"))
    });

    // Build the human-readable display text (used for DB persistence + TUI).
    // Owner DMs show the bare text; everything else gets a `Sender: text`
    // prefix so multi-user channels stay readable in OpenCrabs without
    // surfacing the LLM-only metadata brackets.
    let display_text = if is_owner && msg.guild_id.is_none() {
        content.clone()
    } else {
        format!("{}: {}", msg.author.name, content)
    };

    // Name the current sender. In a guild channel this always runs — even for
    // the owner — because the history block below carries other members' names,
    // and without the label the model addresses the sender by one of those
    // (#682). DMs keep the old shape: nobody else's name is in play there.
    let agent_input = if msg.guild_id.is_some() {
        let name = &msg.author.name;
        let uid = msg.author.id.get();
        let channel = msg.channel_id.get().to_string();
        let role = if is_owner { "owner" } else { "user" };
        format!(
            "{}\n{content}",
            group_history::current_sender_label(
                "Discord channel",
                &channel,
                name,
                &format!(" (ID {uid})"),
                role,
            )
        )
    } else if !is_owner {
        let name = &msg.author.name;
        let uid = msg.author.id.get();
        format!("[Discord DM from {name} (ID {uid})]\n{content}")
    } else {
        content
    };

    // Prepend reply context if the user is replying to a specific message.
    let agent_input = if let Some(ref ctx) = reply_context {
        format!("{ctx}\n{agent_input}")
    } else {
        agent_input
    };

    // Inject recent channel history so the agent has full conversation context.
    // Deduped against the live session window: after a compaction the model
    // still holds those turns, so re-sending all 30 every turn was pure waste
    // (#1619, the Discord half of #133).
    let agent_input = if msg.guild_id.is_some() {
        let chat_id_str = msg.channel_id.get().to_string();
        let fetched = channel_msg_repo
            .recent(Some("discord"), &chat_id_str, 30, None, None)
            .await
            .unwrap_or_default();
        match group_history::build_preamble(
            session_svc.pool(),
            session_id,
            fetched,
            "channel",
            "Discord",
        )
        .await
        {
            Some(preamble) => format!("{preamble}\n{agent_input}"),
            None => agent_input,
        }
    } else {
        agent_input
    };

    // Tell the LLM its text response is automatically delivered to the chat,
    // so it should NOT use discord_send for simple text replies. Surface the
    // channel_id so the agent can target THIS channel for cron reports /
    // cross-surface sends without guessing (#533, mirror of upstream #510).
    let channel_id = msg.channel_id.get();
    let agent_input = format!(
        "[Channel: Discord (channel_id: {channel_id}) — your text response is automatically sent to this channel. \
         Do NOT call discord_send to deliver your answer. Only use discord_send for: \
         sending to a different channel, embeds, reactions, threads, files, or moderation.]\n{agent_input}"
    );

    // Register channel for approval routing, then send with approval callback
    discord_state
        .register_session_channel(session_id, msg.channel_id.get())
        .await;

    // Mid-turn follow-up claim (#1990): if a turn already owns this session,
    // this message must NOT fork a second concurrent loop. Queue it, ack with
    // 👀, and let the live loop inject it between rounds (the queue callback
    // wired in manager.rs) or this turn's end-of-turn flush pick it up. The
    // claim precedes `store_cancel_token` below on purpose: a lost claim must
    // never replace the live turn's cancel token, or /stop would reach a turn
    // that is not the one running.
    let Some(_turn_guard) = discord_state.try_begin_turn(session_id) else {
        tracing::info!("Discord: mid-turn follow-up queued for session {session_id} (#1990)");
        discord_state.enqueue_followup(
            session_id,
            crate::brain::agent::QueuedUserMessage {
                context_text: agent_input.clone(),
                display_text: display_text.clone(),
                origin: crate::brain::agent::PushOrigin::Ingress,
                bg_meta: None,
            },
        );
        use serenity::model::channel::ReactionType;
        if let Err(e) = msg
            .react(&ctx.http, ReactionType::Unicode("👀".to_string()))
            .await
        {
            tracing::debug!("Discord: 👀 ack on queued follow-up failed: {e}");
        }
        return;
    };

    // Claim this session's background-task completions for Discord: a completion
    // must be delivered by the surface that OWNS the session, not by whichever
    // service happened to run the command (#940).
    crate::brain::agent::service::session_routes::claim_for_channel(
        session_id,
        agent.message_enqueue_callback(),
    );
    let approval_cb = make_approval_callback(discord_state.clone());

    let cancel_token = tokio_util::sync::CancellationToken::new();
    discord_state
        .store_cancel_token(session_id, cancel_token.clone())
        .await;

    // Sustained typing for the turn, continuing while the session has detached
    // work (#812). Discord had no turn-long pinger at all, so an ordinary turn
    // showed the dots briefly and a background command showed nothing: spawning
    // one ENDS the turn. Its own token, not `cancel_token`, which only fires on
    // abort — this must stop when the turn FINISHES, however it finishes.
    let typing_cancel = tokio_util::sync::CancellationToken::new();
    super::typing::spawn_typing(
        ctx.http.clone(),
        msg.channel_id,
        typing_cancel.clone(),
        agent.background_manager(),
        agent.subagent_manager(),
        session_id,
    );
    let _typing_guard = super::typing::TypingGuard(typing_cancel);

    // Per-turn record of intermediate post bodies. The body feeds the
    // final-response dedup: tool_loop emits the last iteration's text BOTH
    // as IntermediateText (so the TUI persists it) AND as response.content,
    // so without coordination every tool turn that ends in text was posted
    // twice. Per-TURN scope: a cross-turn window suppressed legitimate
    // repeated answers on Slack. (#1842: no message ids or chunk text are
    // recorded anymore — the ctx footer never rides on any message.)
    /// One intermediate already posted: its post-sanitized body.
    type SentIntermediate = String;
    let sent_intermediates: Arc<Mutex<Vec<SentIntermediate>>> = Arc::new(Mutex::new(Vec::new()));

    // Track every IntermediateText spawn handle so the final-response path can
    // await ALL of them before reading sent_intermediates. Without this, the
    // spawn-then-push race posts the intermediate hundreds of ms later, after
    // the final path already found no match — the exact duplicate class Slack
    // fixed in #456/#459/#943/#951. std::sync::Mutex because the progress
    // callback closure is synchronous and we only ever drain it.
    let intermediate_handles: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let intermediate_handles_cb = intermediate_handles.clone();
    let intermediate_handles_final = intermediate_handles.clone();
    let sent_intermediates_final = sent_intermediates.clone();

    // Trace mode posts no intermediate, it folds each one into the bubble
    // as a clipped note. Keep the last folded body so the final path can
    // deliver it when the provider returned an empty final (#1942).
    let last_trace_body: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    let last_trace_body_cb = last_trace_body.clone();
    // The text a suggest_options surface halted on (trace mode posts it
    // before the buttons, see options_host.rs).
    let options_host = Arc::new(std::sync::Mutex::new(
        super::options_host::OptionsHost::default(),
    ));
    let options_host_cb = options_host.clone();
    let sent_intermediates_cb = sent_intermediates.clone();

    use serenity::model::id::MessageId;

    // Turn bubble id, hoisted OUT of the progress-callback block so the
    // final-response path can find the bubble: trace mode drops the trailing
    // narration note that mirrors the answer, and auto-thread anchors the
    // thread to the bubble.
    let turn_group_mid: Arc<Mutex<Option<MessageId>>> = Arc::new(Mutex::new(None));

    // Build progress callback — sends tool call status as Discord messages
    let progress_cb: crate::brain::agent::ProgressCallback = {
        use crate::brain::agent::ProgressEvent;
        use serenity::builder::EditMessage;

        use super::tool_group::{GroupEntry, GroupState};

        let tools: Arc<Mutex<Vec<GroupEntry>>> = Arc::new(Mutex::new(Vec::new()));
        let group_msg_id = turn_group_mid.clone();
        let trace_narration = dc_cfg.trace_narration;
        let group_state_cb = discord_state.clone();
        let http = ctx.http.clone();
        let channel = target;

        Arc::new(move |session_id, event| {
            let tools = tools.clone();
            let http = http.clone();

            match event {
                // Auto-compaction produces zero streaming chunks for
                // 10-60s and Discord has no continuous typing pinger
                // like Telegram. Ping broadcast_typing every 8s for up
                // to 90s so the channel shows the "is typing" dots
                // through the silent window. No text — just the native
                // indicator. The loop self-terminates after 90s; if
                // compaction finishes earlier, real streaming chunks
                // resume the indicator naturally.
                ProgressEvent::Compacting { .. } => {
                    let http = http.clone();
                    tokio::spawn(async move {
                        for _ in 0..12 {
                            if let Err(e) = channel.broadcast_typing(&http).await {
                                tracing::warn!(error = %e, "failed to broadcast Discord typing");
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                        }
                    });
                }
                ProgressEvent::ToolStarted {
                    tool_name,
                    tool_input,
                    tool_call_id,
                } => {
                    if let Ok(mut host) = options_host_cb.lock() {
                        host.on_tool_started(&tool_name);
                    }
                    let ctx_hint = crate::utils::tool_context_hint(&tool_name, &tool_input);
                    let gmid = group_msg_id.clone();
                    let dstate = group_state_cb.clone();
                    tokio::spawn(async move {
                        // One grouped message per turn (#380), collapsed by
                        // default with an Expand toggle; edited in place.
                        let entries = {
                            let mut t = tools.lock().await;
                            t.push(GroupEntry {
                                name: tool_name,
                                context: ctx_hint,
                                call_id: tool_call_id.clone(),
                                status: None,
                            });
                            t.clone()
                        };
                        let mut mid_guard = gmid.lock().await;
                        match *mid_guard {
                            Some(mid) => {
                                let group = dstate
                                    .upsert_tool_group(
                                        mid.get(),
                                        GroupState {
                                            page: 0,
                                            entries,
                                            notes: Vec::new(),
                                            expanded: false,
                                            started_at: Instant::now(),
                                            settled: None,
                                        },
                                    )
                                    .await;
                                let edit = EditMessage::new()
                                    .content(super::tool_group::render_content(&group))
                                    .embeds(super::tool_group::render_embeds(&group))
                                    .components(super::tool_group::render_components(
                                        &group,
                                        mid.get(),
                                    ));
                                if let Some(Err(e)) = super::governor::edit_chrome(
                                    &dstate.governor,
                                    channel,
                                    &http,
                                    mid,
                                    edit,
                                )
                                .await
                                {
                                    tracing::warn!("Discord: tool group edit failed (append): {e}");
                                }
                            }
                            None => {
                                let group = GroupState {
                                    page: 0,
                                    entries,
                                    notes: Vec::new(),
                                    expanded: false,
                                    started_at: Instant::now(),
                                    settled: None,
                                };
                                let content = super::tool_group::render_content(&group);
                                match super::governor::say(
                                    &dstate.governor,
                                    channel,
                                    &http,
                                    super::governor::Surface::Send,
                                    &content,
                                )
                                .await
                                {
                                    Ok(sent) => {
                                        let comps = super::tool_group::render_components(
                                            &group,
                                            sent.id.get(),
                                        );
                                        if !comps.is_empty()
                                            && let Some(Err(e)) = super::governor::edit_chrome(
                                                &dstate.governor,
                                                channel,
                                                &http,
                                                sent.id,
                                                EditMessage::new()
                                                    .embeds(super::tool_group::render_embeds(
                                                        &group,
                                                    ))
                                                    .components(comps),
                                            )
                                            .await
                                        {
                                            tracing::warn!(
                                                "Discord: tool group component fixup failed: {e}"
                                            );
                                        }
                                        dstate.upsert_tool_group(sent.id.get(), group).await;
                                        *mid_guard = Some(sent.id);
                                    }
                                    Err(e) => tracing::warn!(
                                        "Discord: failed to post tool group message: {e}"
                                    ),
                                }
                            }
                        }
                    });
                }
                ProgressEvent::ToolCompleted {
                    tool_name,
                    success,
                    tool_call_id,
                    ..
                } => {
                    let gmid = group_msg_id.clone();
                    let dstate = group_state_cb.clone();
                    tokio::spawn(async move {
                        let entries = {
                            let mut t = tools.lock().await;
                            if let Some(entry) = t.iter_mut().rev().find(|e| {
                                e.status.is_none()
                                    && super::tool_card::entry_matches_completion(
                                        e,
                                        &tool_name,
                                        &tool_call_id,
                                    )
                            }) {
                                entry.status = Some(success);
                            }
                            t.clone()
                        };
                        if let Some(mid) = *gmid.lock().await {
                            let group = dstate
                                .upsert_tool_group(
                                    mid.get(),
                                    GroupState {
                                        page: 0,
                                        entries,
                                        notes: Vec::new(),
                                        expanded: false,
                                        started_at: Instant::now(),
                                        settled: None,
                                    },
                                )
                                .await;
                            let edit = EditMessage::new()
                                .content(super::tool_group::render_content(&group))
                                .embeds(super::tool_group::render_embeds(&group))
                                .components(super::tool_group::render_components(
                                    &group,
                                    mid.get(),
                                ));
                            if let Some(Err(e)) = super::governor::edit_chrome(
                                &dstate.governor,
                                channel,
                                &http,
                                mid,
                                edit,
                            )
                            .await
                            {
                                tracing::warn!("Discord: tool group edit failed (status): {e}");
                            }
                        }
                    });
                }
                ProgressEvent::SelfHealingAlert { message } => {
                    let dstate = group_state_cb.clone();
                    tokio::spawn(async move {
                        let text =
                            format!("🔧 {}", crate::utils::sanitize::normalize_dashes(&message));
                        if let Err(e) = super::governor::say(
                            &dstate.governor,
                            channel,
                            &http,
                            super::governor::Surface::Send,
                            &text,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "failed to send Discord message");
                        }
                    });
                }
                ProgressEvent::IntermediateText { text, .. } => {
                    // Strip LLM artifacts, secrets, and media markers
                    // the same way the final-response path does.
                    let clean = crate::utils::sanitize::strip_llm_artifacts(&text);
                    let clean = redact_secrets(&clean);
                    let (clean, _) = crate::utils::extract_img_markers(&clean);
                    let (clean, _) = crate::utils::extract_vid_markers(&clean);
                    // Same table conversion as the final path — keys must
                    // match for the dedup below.
                    let clean = super::table_convert::tables_to_discord(&clean);
                    if clean.trim().is_empty() {
                        return;
                    }
                    if let Ok(mut host) = options_host_cb.lock() {
                        host.on_text(&clean);
                    }
                    // Trace mode: fold the narration into the turn's bubble
                    // as a dim subtext note instead of posting it. Notes
                    // before the first tool are dropped — the bubble appears
                    // with the first tool anyway, and a notes-only bubble
                    // would be a message we then have to clean up. Also note
                    // this path must NOT touch sent_intermediates: the final
                    // dedup would then see a matching key with no id and
                    // skip the real answer entirely.
                    if trace_narration {
                        if let Ok(mut last) = last_trace_body_cb.lock() {
                            *last = Some(clean.clone());
                        }
                        let gmid = group_msg_id.clone();
                        let dstate = group_state_cb.clone();
                        let http = http.clone();
                        let channel = channel;
                        let handles = intermediate_handles_cb.clone();
                        let note = super::tool_group::clip_note(&clean);
                        let handle = tokio::spawn(async move {
                            let Some(mid) = *gmid.lock().await else {
                                return;
                            };
                            let Some(group) = dstate.append_note(mid.get(), note).await else {
                                return;
                            };
                            let edit = EditMessage::new()
                                .content(super::tool_group::render_content(&group))
                                .embeds(super::tool_group::render_embeds(&group))
                                .components(super::tool_group::render_components(
                                    &group,
                                    mid.get(),
                                ));
                            if let Some(Err(e)) = super::governor::edit_chrome(
                                &dstate.governor,
                                channel,
                                &http,
                                mid,
                                edit,
                            )
                            .await
                            {
                                tracing::debug!("Discord: trace note edit failed: {e}");
                            }
                        });
                        if let Ok(mut g) = handles.lock() {
                            g.push(handle);
                        }
                        return;
                    }
                    let sent = sent_intermediates.clone();
                    let handles = intermediate_handles_cb.clone();
                    let http = http.clone();
                    let channel = channel;
                    let dstate = group_state_cb.clone();
                    let handle = tokio::spawn(async move {
                        // Pre-send dedup: skip if this exact body was
                        // already posted this turn.
                        {
                            let mut prev = sent.lock().await;
                            if prev.iter().any(|b| b == &clean) {
                                return;
                            }
                            prev.push(clean.clone());
                        }
                        for chunk in split_message(&clean, 2000) {
                            if let Err(e) = super::governor::say(
                                &dstate.governor,
                                channel,
                                &http,
                                super::governor::Surface::Send,
                                &chunk,
                            )
                            .await
                            {
                                tracing::debug!("Discord: intermediate text send failed: {}", e)
                            }
                        }
                    });
                    if let Ok(mut g) = handles.lock() {
                        g.push(handle);
                    }
                }
                ProgressEvent::RetryAttempt {
                    attempt,
                    max,
                    reason,
                } => {
                    let http = http.clone();
                    let channel = channel;
                    let dstate = group_state_cb.clone();
                    tokio::spawn(async move {
                        let text = format!("⏳ Retry {}/{} — {}", attempt, max, reason);
                        if let Err(e) = super::governor::say(
                            &dstate.governor,
                            channel,
                            &http,
                            super::governor::Surface::Send,
                            &text,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "failed to send Discord message");
                        }
                    });
                }
                ProgressEvent::ProviderSwitched {
                    to_name, to_model, ..
                } => {
                    let http = http.clone();
                    let channel = channel;
                    let dstate = group_state_cb.clone();
                    tokio::spawn(async move {
                        let text = format!("🔄 Now using {}/{}", to_name, to_model);
                        if let Err(e) = super::governor::say(
                            &dstate.governor,
                            channel,
                            &http,
                            super::governor::Surface::Send,
                            &text,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "failed to send Discord message");
                        }
                    });
                }
                // Optional follow-up suggestions (#598): post tap-to-send
                // buttons under the response. A tap injects the suggestion as a
                // new turn via route_followup_turn (#1852).
                ProgressEvent::SuggestedOptions(options) => {
                    // Trace mode folded the answer into the bubble: post it
                    // before the buttons. Trace off already posted it as a
                    // message when it arrived.
                    let host = if trace_narration {
                        options_host_cb.lock().ok().and_then(|mut h| h.take())
                    } else {
                        None
                    };
                    if host.is_some()
                        && let Ok(mut g) = last_trace_body_cb.lock()
                    {
                        *g = None;
                    }
                    let http = http.clone();
                    let state = group_state_cb.clone();
                    let sent = sent_intermediates_cb.clone();
                    let gmid = group_msg_id.clone();
                    let raw_options: Vec<String> =
                        options.into_iter().map(|item| item.label).collect();
                    tokio::spawn(async move {
                        if let Some(host) = host {
                            let mid = *gmid.lock().await;
                            super::options_host::post_options_host(
                                &state, channel, &http, &sent, mid, &host,
                            )
                            .await;
                        }
                        super::suggest_options::render_suggestions(
                            &http,
                            &state,
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

    // Turn-start group shell (#1845, the #1808 Slack parity): the bubble
    // posts NOW, before the turn dispatches, so the clock covers the
    // thinking window. Starts as `✅ **0 tool calls** · 🕒 0:00` and is
    // edited in place on the first tool event (the Some(mid) upsert path
    // preserves this started_at); the settle stamp is the last word. On a
    // post failure the mid stays None and creation falls back to the first
    // tool call, the pre-#1845 behavior.
    let turn_shell = super::tool_group::GroupState {
        page: 0,
        entries: Vec::new(),
        notes: Vec::new(),
        expanded: false,
        started_at: Instant::now(),
        settled: None,
    };
    match super::governor::say(
        &discord_state.governor,
        target,
        &ctx.http,
        super::governor::Surface::Send,
        &super::tool_group::render_content(&turn_shell),
    )
    .await
    {
        Ok(sent) => {
            discord_state
                .upsert_tool_group(sent.id.get(), turn_shell)
                .await;
            *turn_group_mid.lock().await = Some(sent.id);
        }
        Err(e) => tracing::warn!("Discord: turn-start group shell post failed: {e}"),
    }

    // Flow ticker (#1843): re-renders the bubble's clock every 4 s so the
    // timer does not freeze between tool events. Spawns after the turn-start
    // shell (#1845) so the group already exists; stops itself on
    // settle/prune/cap.
    spawn_flow_ticker(
        ctx.http.clone(),
        target,
        turn_group_mid.clone(),
        discord_state.clone(),
    );

    let discord_chat_id = msg.channel_id.get().to_string();
    let result = agent
        .send_message_with_tools_and_display(
            session_id,
            agent_input,
            Some(display_text),
            None,
            Some(cancel_token),
            Some(approval_cb),
            Some(progress_cb),
            "discord",
            Some(&discord_chat_id),
            None, // Discord threads: not tracked yet
        )
        .await;

    discord_state.remove_cancel_token(session_id).await;

    match result {
        Ok(response) => {
            // Extract <<IMG:path>> markers — send each as a Discord file attachment.
            let (response_content, react_emoji) =
                crate::utils::extract_react_marker(&response.content);
            // React-back (#381): fire the reaction on the user's message
            // instead of leaking the marker into Discord text.
            if let Some(ref em) = react_emoji {
                use serenity::model::channel::ReactionType;
                let em = em.trim().to_string();
                if let Err(e) = msg
                    .react(&ctx.http, ReactionType::Unicode(em.clone()))
                    .await
                {
                    tracing::warn!("Discord: react-back {em} failed: {e}");
                }
            }
            let (text_only, img_paths) = crate::utils::extract_img_markers(&response_content);
            let text_only = crate::utils::sanitize::strip_llm_artifacts(&text_only);
            let text_only = redact_secrets(&text_only);
            // Discord has no table markup — convert before dedup so both
            // copies of a text (intermediate + final) normalize identically.
            let text_only = super::table_convert::tables_to_discord(&text_only);

            // Settled-line ctx source (#1842): the context budget lives ONLY
            // on the flow group's settled chrome, never appended to an
            // answer message. Built here, consumed by settle_tool_group.
            let ctx_max = agent.context_limit_for_session(session_id);
            let ctx_line = crate::utils::format_ctx_footer(
                response.context_tokens,
                ctx_max,
                response.tokens_per_second,
            );

            // --- Intermediate vs final dedup (port of Slack's fix for
            // #456/#459/#943/#951). tool_loop emits the last iteration's text
            // as IntermediateText (for TUI persistence) AND returns it as
            // response.content; without this block the channel posted both —
            // the answer appeared twice, footerless then footered.
            //
            // Await every in-flight intermediate spawn first: the spawn posts
            // + records the body hundreds of ms after the event fires, and
            // reading the list earlier classified in-flight intermediates as
            // not-yet-posted and duplicated them.
            let pending = {
                let mut g = intermediate_handles_final.lock().expect("poisoned");
                std::mem::take(&mut *g)
            };
            for h in pending {
                if let Err(e) = h.await {
                    tracing::warn!("Discord: intermediate post task panicked: {e}");
                }
            }
            // CLI providers return the answer only as intermediates, which
            // trace mode folded into notes: deliver the last one (#1942).
            let text_only = super::trace_answer::final_text_for_delivery(
                text_only,
                dc_cfg.trace_narration,
                last_trace_body.lock().ok().and_then(|mut g| g.take()),
            );
            let skip_final_post = {
                let posted = sent_intermediates_final.lock().await;
                if text_only.trim().is_empty() {
                    // Empty-final guard (#943/#951 class): the model's real
                    // answer already went out as intermediates and the final
                    // content is just a wrap-up. Keep them, never post a bare
                    // shell.
                    true
                } else {
                    let final_key = norm_key(&text_only);
                    posted.iter().any(|b| norm_key(b) == final_key)
                }
            };

            // Trace mode cleanup: tool_loop emits the final text as a
            // trailing IntermediateText too, and trace folded it into the
            // bubble as the last note. The full answer posts below, so drop
            // that mirror note — otherwise the bubble shows a clip of the
            // answer AND the channel gets the whole thing: the duplicate,
            // one level deeper. No-op when trace is off (notes stay empty).
            let answer_head = text_only
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_lowercase();
            if let Some(mid) = *turn_group_mid.lock().await
                && let Some(group) = discord_state
                    .drop_note_if(mid.get(), |n| answer_head.starts_with(&n.to_lowercase()))
                    .await
            {
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .embeds(super::tool_group::render_embeds(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Some(Err(e)) = super::governor::edit_chrome(
                    &discord_state.governor,
                    target,
                    &ctx.http,
                    mid,
                    edit,
                )
                .await
                {
                    tracing::debug!("Discord: trace mirror-note drop failed: {e}");
                }
            }

            // Settled status chrome (#1841): freeze the clock and stamp the
            // ctx budget into the flow group, the Discord twin of Telegram's
            // settled flow header. Runs on every delivery outcome so the
            // chrome ends as the last word regardless of the answer path.
            // #1987: a turn that ends with detached work still alive settles
            // to the shared waiting verb instead of a green check, folding
            // both registries (shell tasks + working sub-agents) through
            // `background_work`, the Discord twin of Telegram's override.
            let waiting = {
                let (_, bg_count) = bg_indicator_for(&agent, session_id);
                waiting_verb(bg_count, subagent_counts_for(&agent, session_id))
            };
            if let Some(mid) = *turn_group_mid.lock().await
                && let Some(group) = discord_state
                    .settle_tool_group(
                        mid.get(),
                        if ctx_line.is_empty() {
                            None
                        } else {
                            Some(ctx_line.clone())
                        },
                        waiting.clone(),
                        None,
                    )
                    .await
            {
                if waiting.is_some() {
                    // #1987: keep the mid so the background-completion path
                    // in resume.rs can find this ⏳ line after the turn is
                    // gone and flip it once both registries drain.
                    discord_state
                        .register_waiting_group(session_id, mid.get())
                        .await;
                }
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .embeds(super::tool_group::render_embeds(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Err(e) = super::governor::edit_content(
                    &discord_state.governor,
                    target,
                    &ctx.http,
                    mid,
                    super::governor::Surface::Final,
                    edit,
                )
                .await
                {
                    tracing::debug!("Discord: settled status stamp failed: {e}");
                }
                // #1912: the plan card's last word for this session. Same rule
                // as the stamp above it: FINAL, because nothing after this will
                // ever restate the checklist.
                super::plan_card::refresh_plan_card(
                    &discord_state,
                    &ctx.http,
                    target,
                    session_id,
                    true,
                )
                .await;
            }

            // Media gallery (#385): batch all generated files into ONE
            // multi-attachment message (Discord caps 10 per message; the
            // remainder rolls into follow-up batches) instead of one
            // message per file.
            let mut attachments: Vec<CreateAttachment> = Vec::new();
            for img_path in &img_paths {
                match tokio::fs::read(img_path).await {
                    Ok(bytes) => {
                        let fname = std::path::Path::new(img_path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("image.png")
                            .to_string();
                        attachments.push(CreateAttachment::bytes(bytes, fname));
                    }
                    Err(e) => {
                        tracing::error!("Discord: failed to read image {}: {}", img_path, e);
                    }
                }
            }
            for batch in attachments.chunks(10) {
                let mut message = CreateMessage::new();
                for file in batch {
                    message = message.add_file(file.clone());
                }
                if let Err(e) = super::governor::send_content(
                    &discord_state.governor,
                    target,
                    &ctx.http,
                    super::governor::Surface::Send,
                    message,
                )
                .await
                {
                    tracing::error!("Discord: failed to send media gallery batch: {}", e);
                }
            }

            // The answer's last chunk id, for the DM lookup record below (#2025).
            let mut reply_msg_id: Option<String> = None;
            if skip_final_post {
                // Answer already visible via the kept intermediate (#459's
                // keep-intermediate outcome): skip the duplicate post. The
                // settled flow group above carries the completion chrome.
            } else {
                let chunks: Vec<String> = split_message(&text_only, 2000);
                // Auto-thread (opt-in): long answers post a short teaser in
                // the channel and the full body in a thread anchored to the
                // turn's bubble (or the user's message). The channel stays
                // scannable; the deliverable stays whole.
                let auto_thread = dc_cfg.auto_thread_min_chars > 0
                    && text_only.chars().count() >= dc_cfg.auto_thread_min_chars;
                if auto_thread {
                    let anchor = (*turn_group_mid.lock().await).unwrap_or(msg.id);
                    let title = thread_title(&text_only);
                    let body = serde_json::json!({ "name": title });
                    match ctx
                        .http
                        .create_thread_from_message(target, anchor, &body, None)
                        .await
                    {
                        Ok(thread) => {
                            let truncated = text_only.chars().count() > 280;
                            let teaser: String = text_only.chars().take(280).collect();
                            let teaser = format!(
                                "{teaser}{}\n\n-# Full response in thread: <#{}>",
                                if truncated { "…" } else { "" },
                                thread.id
                            );
                            if let Err(e) = super::governor::say(
                                &discord_state.governor,
                                target,
                                &ctx.http,
                                super::governor::Surface::Send,
                                &teaser,
                            )
                            .await
                            {
                                tracing::error!("Discord: auto-thread teaser failed: {e}");
                            }
                            for chunk in &chunks {
                                if let Err(e) = super::governor::say(
                                    &discord_state.governor,
                                    thread.id,
                                    &ctx.http,
                                    super::governor::Surface::Send,
                                    chunk,
                                )
                                .await
                                {
                                    tracing::error!("Discord: auto-thread body failed: {e}");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Discord: auto-thread failed, posting inline: {e}");
                            for chunk in &chunks {
                                if let Err(e) = super::governor::say(
                                    &discord_state.governor,
                                    target,
                                    &ctx.http,
                                    super::governor::Surface::Send,
                                    chunk,
                                )
                                .await
                                {
                                    tracing::error!("Discord: failed to send reply: {}", e);
                                }
                            }
                        }
                    }
                } else {
                    for chunk in &chunks {
                        match super::governor::say(
                            &discord_state.governor,
                            target,
                            &ctx.http,
                            super::governor::Surface::Send,
                            chunk,
                        )
                        .await
                        {
                            Ok(m) => reply_msg_id = Some(m.id.get().to_string()),
                            Err(e) => tracing::error!("Discord: failed to send reply: {}", e),
                        }
                    }
                }
            }

            // Record the bot's reply in channel_messages so the recent() query
            // used for group context on the next guild turn sees both sides of
            // the conversation, and so channel_search can find it. A DM reply is
            // stored when it has a message id, as Telegram's DM replies are
            // (#2025); the session's messages table still carries full history.
            if (!is_dm || reply_msg_id.is_some()) && !text_only.trim().is_empty() {
                let bot_id = discord_state.bot_user_id().await;
                let bot_sender_id = bot_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "bot:opencrabs".to_string());
                let guild_name = msg
                    .guild_id
                    .map(|g| g.get().to_string())
                    .unwrap_or_else(|| "DM".to_string());
                let cm = DbChannelMessage::new(
                    "discord".into(),
                    target.get().to_string(),
                    Some(guild_name),
                    bot_sender_id,
                    "OpenCrabs".into(),
                    text_only.clone(),
                    "text".into(),
                    reply_msg_id.clone(),
                );
                if let Err(e) = channel_msg_repo.insert(&cm).await {
                    tracing::warn!(
                        "Discord: failed to record bot reply in channel_messages: {}",
                        e
                    );
                }
            }

            // TTS: send voice reply if input was audio and TTS is enabled
            if is_voice && voice_config.tts_enabled {
                match crate::channels::voice::synthesize(&response.content, &voice_config).await {
                    Ok(audio_bytes) => {
                        send_tts_voice(
                            &discord_state.governor,
                            &ctx.http,
                            msg.channel_id,
                            &audio_bytes,
                        )
                        .await;
                    }
                    Err(e) => tracing::error!("Discord: TTS error: {e}"),
                }
            }
        }
        Err(ref e) if matches!(e, crate::brain::agent::AgentError::Cancelled) => {
            tracing::info!("Discord: agent call cancelled for session {}", session_id);
            // #1987: an unsettled group keeps editing its 🕒 line until the
            // 30-minute orphan cap; stamp the cancelled outcome so the clock
            // ends with the cancelled turn.
            if let Some(mid) = *turn_group_mid.lock().await
                && let Some(group) = discord_state
                    .settle_tool_group(mid.get(), None, None, Some(FlowOutcome::Cancelled))
                    .await
            {
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .embeds(super::tool_group::render_embeds(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Err(e) = super::governor::edit_content(
                    &discord_state.governor,
                    target,
                    &ctx.http,
                    mid,
                    super::governor::Surface::Final,
                    edit,
                )
                .await
                {
                    tracing::debug!("Discord: cancelled settle stamp failed: {e}");
                }
            }
        }
        Err(e) => {
            tracing::error!("Discord: agent error: {}", e);
            // Shared helper translates the raw error into something
            // the user can act on (5xx exhausted, rate limit, context
            // too large, stream broken, repetition loop). Same wording
            // as the TUI + Telegram + Slack + WhatsApp paths.
            let error_msg = format!("❌ Error\n\n{}", crate::brain::agent::format_user_error(&e));
            if let Err(e) = super::governor::say(
                &discord_state.governor,
                target,
                &ctx.http,
                super::governor::Surface::Send,
                error_msg,
            )
            .await
            {
                tracing::warn!(error = %e, "failed to send Discord message");
            }
            // #1987: a failed turn must settle its group too, or the flow
            // ticker keeps editing 🕒 on a bubble whose turn is gone.
            // #1911: the settle carries the classified outcome, so a timeout
            // reads `⏱ Timed out` and anything else `❌ Failed`.
            if let Some(mid) = *turn_group_mid.lock().await
                && let Some(group) = discord_state
                    .settle_tool_group(mid.get(), None, None, Some(outcome_for_error(&e)))
                    .await
            {
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .embeds(super::tool_group::render_embeds(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Err(e) = super::governor::edit_content(
                    &discord_state.governor,
                    target,
                    &ctx.http,
                    mid,
                    super::governor::Surface::Final,
                    edit,
                )
                .await
                {
                    tracing::debug!("Discord: error settle stamp failed: {e}");
                }
            }
        }
    }

    // End-of-turn flush (#1990, Telegram's #201 rule): a message enqueued
    // after the loop's last between-rounds drain has no consumer left on
    // this path. Release the slot first so the flushed follow-up can claim
    // it as a fresh visible turn; if it loses that brand-new race, the
    // winner's queue takes the message back, so nothing is ever dropped.
    drop(_turn_guard);
    if let Some(joined) = discord_state.drain_followups(session_id) {
        let http = ctx.http.clone();
        let dstate = discord_state.clone();
        let agent = agent.clone();
        let channel = target;
        let text = joined.context_text.clone();
        let display = joined.display_text.clone();
        tokio::spawn(async move {
            if matches!(
                super::tracked_turn::run_tracked_resume_turn(
                    http,
                    channel,
                    session_id,
                    dstate.clone(),
                    agent,
                    super::tracked_turn::ResumeDispatch::Display {
                        text,
                        display_tag: Some(display),
                    },
                )
                .await,
                super::tracked_turn::ResumeTurnOutcome::Queued
            ) {
                dstate.enqueue_followup(session_id, joined);
            }
        });
    }
}

/// Filename the synthesized reply is uploaded under (#1849). The `ogg`
/// extension is load-bearing, not cosmetic: serenity derives the part's
/// `Content-Type` from the filename (`http/multipart.rs:10-12`) and mime_guess
/// 2.0.5 maps `ogg` to `audio/ogg` (`src/mime_types.rs:829`), which is the
/// `audio/` prefix the voice-message contract requires.
const TTS_ATTACHMENT_NAME: &str = "response.ogg";

/// The message that carries a synthesized TTS reply: a native Discord voice
/// bubble (#1849).
///
/// The bubble only renders when `IS_VOICE_MESSAGE` (`1 << 13`) is set on create.
/// It is one of the four flags a create request may set (`discord-api-docs`,
/// `developers/resources/message.mdx:1093`), and a voice message must carry a
/// single audio attachment and no content (same file, "Voice Messages",
/// `:433-438`). Both hold here: the text half of a TTS reply is sent before this
/// builder runs, and the opus file is the only attachment. serenity refuses to
/// `edit()` a flagged message (`model/channel/message.rs:400`), which matches the
/// documented "cannot be edited" property instead of fighting it.
///
/// Known gap, stated rather than hidden: the same docs list `duration_secs` and
/// `waveform` in the Attachment Request Structure (`:685-686`) as required for
/// voice messages, while serenity 0.12.5 `CreateAttachment` exposes only `bytes`
/// and `description`, so neither field can be sent from this crate version.
/// Whether a client draws a waveform from an upload that omits it is a live-bot
/// check tracked on #1849. [`plain_attachment_builder`] is the fallback if the
/// flagged send is rejected outright.
pub(crate) fn voice_reply_builder(audio: &[u8]) -> CreateMessage {
    CreateMessage::new()
        .add_file(CreateAttachment::bytes(audio, TTS_ATTACHMENT_NAME))
        .flags(MessageFlags::IS_VOICE_MESSAGE)
}

/// The shape this path had before #1849: the same audio as a plain file entry,
/// no voice flag. Kept as the fallback so a TTS reply cannot be lost.
pub(crate) fn plain_attachment_builder(audio: &[u8]) -> CreateMessage {
    CreateMessage::new().add_file(CreateAttachment::bytes(audio, TTS_ATTACHMENT_NAME))
}

/// Send a synthesized reply, preferring the native voice bubble (#1849).
///
/// A rejected flagged send is retried once as a plain attachment and logged at
/// warn with the original error: the flag's behaviour without an
/// uploader-supplied waveform is unverified against a live bot, and a TTS reply
/// that silently vanishes is worse than one that renders as a file.
pub(crate) async fn send_tts_voice(
    gov: &super::governor::Governor,
    http: &Http,
    channel: ChannelId,
    audio: &[u8],
) {
    if let Err(e) = super::governor::send_content(
        gov,
        channel,
        http,
        super::governor::Surface::Send,
        voice_reply_builder(audio),
    )
    .await
    {
        tracing::warn!(
            "Discord: voice-flagged TTS send rejected ({e}); retrying as a plain attachment (#1849)"
        );
        if let Err(e2) = super::governor::send_content(
            gov,
            channel,
            http,
            super::governor::Surface::Send,
            plain_attachment_builder(audio),
        )
        .await
        {
            tracing::error!("Discord: failed to send TTS voice: {e2}");
        }
    }
}

/// Build an `ApprovalCallback` that sends a Discord message with 3 buttons
/// (Yes / Always / No) and waits up to 5 min for a click.
pub(crate) fn make_approval_callback(
    state: Arc<super::DiscordState>,
) -> crate::brain::agent::ApprovalCallback {
    use crate::brain::agent::ToolApprovalInfo;
    use crate::utils::{check_approval_policy, persist_auto_session_policy};
    use serenity::builder::{CreateActionRow, CreateButton, CreateMessage, EditMessage};
    use serenity::model::application::ButtonStyle;
    use serenity::model::id::ChannelId;
    use tokio::sync::oneshot;

    Arc::new(move |info: ToolApprovalInfo| {
        let state = state.clone();
        Box::pin(async move {
            if let Some(result) = check_approval_policy() {
                return Ok(result);
            }

            let http = match state.http().await {
                Some(h) => h,
                None => {
                    tracing::warn!("Discord approval: bot not connected");
                    return Ok((false, false));
                }
            };

            let channel_id = match state.session_channel(info.session_id).await {
                Some(id) => id,
                None => match state.owner_channel_id().await {
                    Some(id) => id,
                    None => {
                        tracing::warn!(
                            "Discord approval: no channel_id for session {}",
                            info.session_id
                        );
                        return Ok((false, false));
                    }
                },
            };

            let approval_id = uuid::Uuid::new_v4().to_string();
            let safe_input = crate::utils::redact_tool_input(&info.tool_input);
            let input_pretty = serde_json::to_string_pretty(&safe_input)
                .unwrap_or_else(|_| safe_input.to_string());
            let text = format!(
                "🔐 **Tool Approval Required**\n\nTool: `{}`\nInput:\n```json\n{}\n```",
                info.tool_name,
                truncate_str(&input_pretty, 1800),
            );

            let row = CreateActionRow::Buttons(vec![
                CreateButton::new(format!("approve:{}", approval_id))
                    .label("✅ Yes")
                    .style(ButtonStyle::Success),
                CreateButton::new(format!("always:{}", approval_id))
                    .label("🔁 Always (session)")
                    .style(ButtonStyle::Primary),
                CreateButton::new(format!("yolo:{}", approval_id))
                    .label("🔥 YOLO")
                    .style(ButtonStyle::Secondary),
                CreateButton::new(format!("deny:{}", approval_id))
                    .label("❌ No")
                    .style(ButtonStyle::Danger),
            ]);

            // Register BEFORE sending to prevent race condition
            let (tx, rx) = oneshot::channel();
            state
                .register_pending_approval(approval_id.clone(), tx)
                .await;
            tracing::info!(
                "Discord approval: registered pending id={}, sending to channel={}",
                approval_id,
                channel_id
            );

            let mut sent_msg = match super::governor::send_content(
                &state.governor,
                ChannelId::new(channel_id),
                &http,
                super::governor::Surface::Final,
                CreateMessage::new().content(&text).components(vec![row]),
            )
            .await
            {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!("Discord approval: failed to send message: {}", e);
                    return Ok((false, false));
                }
            };

            tracing::info!(
                "Discord approval: message sent, waiting for response (id={})",
                approval_id
            );

            match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
                Ok(Ok((approved, always))) => {
                    tracing::info!(
                        "Discord approval: user responded id={}, approved={}, always={}",
                        approval_id,
                        approved,
                        always
                    );
                    if always {
                        persist_auto_session_policy();
                    }
                    let label = if always {
                        "🔁 Always approved (session)"
                    } else if approved {
                        "✅ Approved"
                    } else {
                        "❌ Denied"
                    };
                    if let Err(e) = sent_msg
                        .edit(&http, EditMessage::new().content(label).components(vec![]))
                        .await
                    {
                        tracing::warn!(error = %e, "failed to edit Discord approval button");
                    }
                    Ok((approved, always))
                }
                Ok(Err(_)) => {
                    tracing::warn!(
                        "Discord approval: oneshot channel closed (id={})",
                        approval_id
                    );
                    Ok((false, false))
                }
                Err(_) => {
                    tracing::warn!(
                        "Discord approval: 5-minute timeout — auto-denying (id={})",
                        approval_id
                    );
                    if let Err(e) = sent_msg
                        .edit(
                            &http,
                            EditMessage::new()
                                .content("⏱️ Approval timed out — denied")
                                .components(vec![]),
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "failed to edit Discord timeout message");
                    }
                    Ok((false, false))
                }
            }
        })
    })
}

/// Whitespace-normalized comparison key for intermediate-vs-final matching.
/// The intermediate path and the final path sanitize through slightly
/// different orders (markers extracted before vs after artifact stripping),
/// so the bodies can differ by trailing/running whitespace only. Collapsing
/// whitespace makes those equivalent without letting real content drift
/// through (every word must still match, in order).
pub(crate) fn norm_key(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Short, recognizable thread title: drops a leading bang, collapses
/// whitespace, cuts on a word boundary, and prefixes a marker so the thread
/// is easy to pick out of Discord's sidebar. Discord caps thread names at
/// 100 chars; marker + 64 + ellipsis stays well under it.
pub(crate) fn thread_title(raw: &str) -> String {
    const MAX: usize = 64;
    let cleaned = raw.trim_start_matches('!');
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    let mut truncated = false;
    for word in cleaned.split(' ') {
        let extra = word.chars().count() + usize::from(!out.is_empty());
        if !out.is_empty() && out.chars().count() + extra > MAX {
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.chars().count() > MAX {
        out = out.chars().take(MAX).collect();
        truncated = true;
    }
    if truncated {
        out.push('…');
    }
    format!("🧵 {out}")
}
