//! Discord-side rendering for the OPTIONAL `suggest_options` tool (#598).
//!
//! Non-blocking option surface: the agent surfaces
//! `ProgressEvent::SuggestedOptions`, and we post one Secondary button per
//! suggestion under the finished response. Tapping a button injects that
//! suggestion as the user's next message (a fresh turn) via
//! `interactions::route_followup_turn` (#1852). Typing your own message always
//! works; there is no oneshot or timeout.
//!
//! Reuses the existing TTL-bounded select-map (`register_select`/`take_select`)
//! to stash `idx -> suggestion text`, since the text can exceed Discord's
//! 100-char `custom_id` limit.

use std::sync::Arc;

use serenity::builder::{CreateActionRow, CreateButton, CreateMessage};
use serenity::model::application::ButtonStyle;
use serenity::model::id::ChannelId;
use uuid::Uuid;

/// Callback-id prefix for a tapped follow-up suggestion: `followup:<id>:<idx>`.
pub(crate) const FOLLOWUP_PREFIX: &str = "followup:";

/// Post the follow-up suggestion buttons under the response and stash the
/// option list under a fresh id so the interaction handler resolves
/// `idx -> text`. No-op on empty options or a missing channel/bot.
pub(crate) async fn render_suggestions(
    http: &Arc<serenity::http::Http>,
    state: &Arc<super::DiscordState>,
    session_id: Uuid,
    options: Vec<String>,
) {
    if options.is_empty() {
        return;
    }

    // Session→owner fallback via shared helper (#764 R5, silent twin).
    let Some(channel_id) = crate::channels::question_common::resolve_channel_or_silent(
        state.session_channel(session_id),
        state.owner_channel_id(),
    )
    .await
    else {
        return;
    };

    let followup_id = Uuid::new_v4().to_string();

    // Up to 5 buttons per ActionRow; suggest_options caps at 4, so one row.
    let rows: Vec<CreateActionRow> = options
        .iter()
        .enumerate()
        .collect::<Vec<_>>()
        .chunks(5)
        .map(|chunk| {
            CreateActionRow::Buttons(
                chunk
                    .iter()
                    .map(|(idx, opt)| {
                        CreateButton::new(format!("{FOLLOWUP_PREFIX}{followup_id}:{idx}"))
                            .label(crate::channels::question_common::truncate_label(
                                opt,
                                crate::channels::question_common::DISCORD_LABEL_BUDGET,
                            ))
                            .style(ButtonStyle::Secondary)
                    })
                    .collect(),
            )
        })
        .collect();

    // Registered only after the message actually lands (#2193): registering
    // first left a permanently orphaned entry every time the send failed, and
    // a button that was never delivered can never be clicked.
    match super::governor::send_content(
        &state.governor,
        ChannelId::new(channel_id),
        http,
        super::governor::Surface::Send,
        CreateMessage::new()
            .content("\u{1f4a1} Suggested next:")
            .components(rows),
    )
    .await
    {
        Ok(_) => state.register_select(followup_id, options).await,
        Err(e) => tracing::warn!("Discord suggest_options: send failed: {e}"),
    }
}
