//! The answer a `suggest_options` surface halts on (Discord, trace mode).
//!
//! The halting iteration streams its answer as `IntermediateText`, calls
//! `suggest_options`, and the turn ends on the buttons. With trace on, that
//! text is folded into the tool group as a clipped note and the buttons post
//! straight away, so the answer ends up collapsed and under the buttons.
//!
//! The answer is the text emitted immediately before the options tool. Any
//! other tool call in between means the text belonged to an earlier step.
//! [`OptionsHost`] keeps that adjacency, and [`post_options_host`] posts the
//! answer as its own message before the buttons, matching Telegram's
//! `pop_trailing_folded_texts` (#1226 K).

use serenity::model::id::{ChannelId, MessageId};

use super::state::DiscordState;

/// The tool whose `SuggestedOptions` surface halts the turn.
pub(crate) const OPTIONS_TOOL: &str = "suggest_options";

/// Tracks the text emitted since the last tool call that is not the options
/// tool. Fed from the progress callback in event order.
#[derive(Default)]
pub(crate) struct OptionsHost {
    adjacent: Option<String>,
}

impl OptionsHost {
    /// A text block arrived. Several in one iteration join in order.
    pub(crate) fn on_text(&mut self, text: &str) {
        match &mut self.adjacent {
            Some(prev) => {
                prev.push_str("\n\n");
                prev.push_str(text);
            }
            None => self.adjacent = Some(text.to_string()),
        }
    }

    /// A tool started. Only the options tool keeps the text before it.
    pub(crate) fn on_tool_started(&mut self, tool_name: &str) {
        if tool_name != OPTIONS_TOOL {
            self.adjacent = None;
        }
    }

    /// The options surface fired. Hands over the answer once.
    pub(crate) fn take(&mut self) -> Option<String> {
        self.adjacent.take()
    }
}

/// Post the answer an options surface halted on, as a message, before the
/// buttons. Its clipped note comes out of the tool group so the answer is
/// not shown twice. The text is recorded as posted so the final path does
/// not repeat it when the sign-off matches.
pub(crate) async fn post_options_host(
    state: &DiscordState,
    channel: ChannelId,
    http: &serenity::http::Http,
    sent: &tokio::sync::Mutex<Vec<String>>,
    group_mid: Option<MessageId>,
    host: &str,
) {
    let host_head = host
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_lowercase();
    if let Some(mid) = group_mid
        && let Some(group) = state
            .drop_note_if(mid.get(), |n| note_belongs_to(n, &host_head))
            .await
    {
        let edit = serenity::builder::EditMessage::new()
            .content(super::tool_group::render_content(&group))
            .embeds(super::tool_group::render_embeds(&group))
            .components(super::tool_group::render_components(&group, mid.get()));
        if let Some(Err(e)) =
            super::governor::edit_chrome(&state.governor, channel, http, mid, edit).await
        {
            tracing::debug!("Discord: options host note drop edit failed: {e}");
        }
    }
    sent.lock().await.push(host.to_string());
    for chunk in super::resume::resume_delivery_chunks(host) {
        if let Err(e) = super::governor::say(
            &state.governor,
            channel,
            http,
            super::governor::Surface::Send,
            &chunk,
        )
        .await
        {
            tracing::warn!(error = %e, "Discord: options host post failed");
        }
    }
}

/// True when a folded note is the clipped first line of `host_head`.
/// Clipped notes end in an ellipsis, which is not part of the match.
pub(crate) fn note_belongs_to(note: &str, host_head: &str) -> bool {
    let core = note.trim_end_matches('…').to_lowercase();
    !core.is_empty() && host_head.starts_with(&core)
}
