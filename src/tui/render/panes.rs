//! Split pane rendering — draws pane borders, labels, and delegates chat rendering.

use super::theme::{self, Role};
use super::tools::render_tool_group;
use super::utils::wrap_line_with_padding;
use crate::tui::app::{App, DisplayMessage};
use crate::tui::pane::PaneId;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
};

/// Render a single inactive (non-focused) pane.
/// Shows the session's cached messages as a read-only chat view.
pub(super) fn render_inactive_pane(f: &mut Frame, app: &App, pane_id: PaneId, area: Rect) {
    let pane = match app.pane_manager.get(pane_id) {
        Some(p) => p,
        None => return,
    };

    let session_label = pane
        .session_id
        .and_then(|sid| {
            app.sessions.iter().find(|s| s.id == sid).map(|s| {
                s.title
                    .clone()
                    .unwrap_or_else(|| format!("Session {}", &s.id.to_string()[..8]))
            })
        })
        .unwrap_or_else(|| "No session".to_string());

    let is_processing = pane
        .session_id
        .map(|sid| app.processing_sessions.contains(&sid))
        .unwrap_or(false);

    let status = if is_processing {
        " [processing...]"
    } else {
        ""
    };

    // Background live state for this session — populated by the
    // routing helper from any TuiEvent that arrived while this
    // session wasn't the focused pane. Drives the live tool /
    // thinking / streaming preview rows appended below the
    // cached message snapshot. `None` when the session has no
    // sidecar entry (either it's idle or never had a turn while
    // off-screen).
    // Only a session with a turn still in flight renders live rows. A
    // cancelled session's sidecar entry is cleared on abort, but guarding here
    // too means a stale entry from any other path cannot be drawn as a live
    // turn, which is what left "is thinking" running forever in split view
    // after a cancel (#1342).
    let live = pane
        .session_id
        .filter(|sid| app.is_session_processing(*sid))
        .and_then(|sid| app.background_sessions.get(&sid));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::role(Role::GrayDim)))
        .title(Span::styled(
            format!(" {}{} ", session_label, status),
            Style::default()
                .fg(theme::role(Role::GrayDim))
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Render cached messages if available
    let cached = pane
        .session_id
        .and_then(|sid| app.pane_message_cache.get(&sid));

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Folded the way the focused pane is by default (#2075), so a tool block
    // keeps its header and preview when this pane is not focused.
    if let Some(messages) = cached {
        lines.extend(super::chat::folded_pane_lines(
            messages,
            inner.width as usize,
        ));
    }

    // Pending-message delta — flushed assistant text, tool-group
    // bullets, and queued user messages that arrived while this
    // session was off-screen. Rendered with the same simplified
    // shape as cached messages so the user sees a coherent feed
    // even when the inactive pane has accumulated several rounds.
    if let Some(bg) = live {
        lines.extend(super::chat::folded_pane_lines(
            &bg.pending_messages,
            inner.width as usize,
        ));
    }

    // Live in-flight rows. These show what's happening in this
    // session RIGHT NOW even though it isn't the focused pane —
    // before this code existed the inactive pane was frozen at
    // whatever was last DB-loaded and the user saw stale state
    // until they tabbed back.
    if let Some(bg) = live {
        // The in-flight group draws exactly as the focused pane draws it (#2075).
        if let Some(ref group) = bg.active_tool_group {
            render_tool_group(
                &mut lines,
                group,
                true,
                app.animation_frame,
                inner.width as usize,
            );
        }
        if bg.streaming_reasoning.is_some() {
            lines.push(Line::from(Span::styled(
                "  ▸ Thinking",
                Style::default()
                    .fg(theme::role(Role::GrayDim))
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        if let Some(ref text) = bg.streaming_response {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                // Show a single-line preview of the in-flight
                // streaming response. The active pane gets the
                // full rendered chat; here we only signal "text is
                // streaming in this session" so the user knows
                // there's progress to tab back to.
                let preview: String = trimmed
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .collect();
                lines.push(Line::from(Span::styled(
                    preview,
                    Style::default().fg(theme::role(Role::TextPrimary)),
                )));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Tab to switch focus",
            Style::default().fg(theme::role(Role::GrayDim)),
        )));
    }

    // Show last N lines that fit — no wrapping, so 1 Line = 1 row (guaranteed).
    let visible = inner.height as usize;
    let skip = lines.len().saturating_sub(visible);
    let visible_lines: Vec<Line> = lines.into_iter().skip(skip).collect();
    let para = Paragraph::new(visible_lines);
    f.render_widget(para, inner);
}

/// Render a single message in simplified form for inactive panes.
pub(crate) fn render_simple_message(
    lines: &mut Vec<Line<'static>>,
    msg: &DisplayMessage,
    width: usize,
) {
    // Skip system messages
    if msg.role == "system" || msg.role == "history_marker" {
        return;
    }

    // Tool groups: single collapsed line matching focused pane style
    if msg.role == "tool_group" {
        if let Some(ref group) = msg.tool_group {
            let n = group.calls.len();
            let all_done = group.calls.iter().all(|c| c.completed);
            let any_failed = group.calls.iter().any(|c| c.completed && !c.success);
            let (icon, color) = if !all_done {
                ("⚙", theme::role(Role::Warning))
            } else if any_failed {
                ("●", theme::role(Role::Error))
            } else {
                ("●", theme::role(Role::GrayDim))
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "  {} {} tool call{}",
                    icon,
                    n,
                    if n == 1 { "" } else { "s" }
                ),
                Style::default().fg(color),
            )));
        }
        return;
    }

    let is_assistant = msg.role == "assistant";

    // Strip reasoning/tool-marker blocks from content
    let mut content = msg.content.clone();
    // Remove <!-- reasoning -->...<!-- /reasoning --> blocks
    while let Some(start) = content.find("<!-- reasoning -->") {
        if let Some(end) = content.find("<!-- /reasoning -->") {
            content = format!(
                "{}{}",
                &content[..start],
                &content[end + "<!-- /reasoning -->".len()..]
            );
        } else {
            content = content[..start].to_string();
        }
    }
    // Remove <!-- tools-v2: [JSON] --> markers. Delegate to the shared
    // stripper so rustc `--> src/foo.rs:10` arrows embedded in tool
    // output don't truncate the match early and leak JSON into the
    // preview (see strip_html_comments doc for screenshot reference).
    if content.contains("<!-- tools-v2:") {
        content = crate::brain::agent::AgentService::strip_html_comments(&content);
    }
    let content = content.trim().to_string();

    // Show thinking indicator if assistant has reasoning details
    if is_assistant && msg.details.is_some() {
        lines.push(Line::from(Span::styled(
            "  ▸ Thinking",
            Style::default()
                .fg(theme::role(Role::GrayDim))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // No length cap here, on purpose. An earlier `content.len() > 500` cut
    // stored messages before the markdown parser ever ran: the pane dropped
    // the tail of the answer (where a turn's conclusion lives) while rows
    // sat blank below it, PT-PT accents burned two bytes each of an
    // ASCII-sized budget, and the focused pane showed the same message
    // whole (#1509). How much fits is decided once, below, by the tail line
    // trim that keeps whole parsed lines from the end.
    if content.is_empty() {
        return;
    }

    // User input is shown verbatim (wrapped) so literal `*` or `_` chars in a
    // prompt aren't mangled by the markdown parser. Assistant (and any other)
    // content goes through the same markdown pipeline as the focused pane, then
    // gets wrapped to the pane width. Without this the inactive pane dumped raw
    // text: `**bold**` markers leaked through as literal asterisks and long
    // lines were hard-truncated at the border instead of wrapping (#180).
    if msg.role == "user" {
        for (i, raw) in content.lines().enumerate() {
            let prefix = if i == 0 { "> " } else { "" };
            let line = Line::from(Span::styled(
                format!("{}{}", prefix, raw),
                Style::default().fg(theme::role(Role::AccentTeal)),
            ));
            for wrapped in wrap_line_with_padding(line, width, "  ") {
                lines.push(wrapped);
            }
        }
    } else {
        for line in crate::tui::markdown::parse_markdown(&content, width) {
            for wrapped in wrap_line_with_padding(line, width, "") {
                lines.push(wrapped);
            }
        }
    }
    lines.push(Line::from(""));
}

/// Render the focused pane's border decoration.
/// Returns the inner area (content area inside the border) for the caller to render chat into.
pub(super) fn focused_pane_border(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let pane = match app.pane_manager.focused_pane() {
        Some(p) => p,
        None => return area,
    };

    let session_label = pane
        .session_id
        .and_then(|sid| {
            app.sessions.iter().find(|s| s.id == sid).map(|s| {
                s.title
                    .clone()
                    .unwrap_or_else(|| format!("Session {}", &s.id.to_string()[..8]))
            })
        })
        .unwrap_or_else(|| "No session".to_string());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::role(Role::Success)))
        .title(Span::styled(
            format!(" {} ", session_label),
            Style::default()
                .fg(theme::role(Role::Success))
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(0));

    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}
