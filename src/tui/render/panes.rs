//! Split pane rendering — draws pane borders, labels, and delegates chat rendering.

use super::theme::{self, Role};
use super::tools::render_tool_group;
use super::utils::wrap_line_with_padding;
use crate::brain::agent::QueuedUserMessage;
use crate::tui::app::{App, DisplayMessage};
use crate::tui::pane::PaneId;
use crate::tui::plan::{PlanDocument, PlanStatus, TaskStatus};
use crate::ui_core::split::{PANE_STRIP_MIN, PaneRows, pane_rows_with};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
};

/// Render a single inactive (non-focused) pane.
/// Shows the session's cached messages as a read-only chat view, plus the
/// pane's own plan strip, queued preview and stashed input rendered into
/// the slot rects the shared row budget carved out (#2494).
pub(super) fn render_inactive_pane(
    f: &mut Frame,
    app: &App,
    pane_id: PaneId,
    area: Rect,
    rows: PaneRows,
) {
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

    // Carve the pane's inner rows bottom-up: transcript body, plan strip,
    // queued preview, its own input box (#2494).
    let layout = pane_layout(inner, rows);

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
            layout.body.width as usize,
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
            layout.body.width as usize,
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
                layout.body.width as usize,
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
    let visible = layout.body.height as usize;
    let skip = lines.len().saturating_sub(visible);
    let visible_lines: Vec<Line> = lines.into_iter().skip(skip).collect();
    let para = Paragraph::new(visible_lines);
    f.render_widget(para, layout.body);

    // The pane's own chrome: its plan strip and queued preview in the slots
    // the budget carved, its stashed input read-only at the bottom (#2494).
    // Inactive pane: its chrome is drawn dim so the live pane is obvious (#2577).
    render_pane_strips(f, app, pane_id, layout.strip, layout.queue, false);
    render_pane_input_readonly(f, &pane.input_buffer, layout.input);
    // The pane's own footer, compact and dim: the window bar no longer spans
    // the split, so each session panel carries its identity row (#2578).
    super::input::render_pane_footer(f, app, pane_id, layout.footer, false);
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

    if msg.role == crate::services::message::PLAN_COMPLETED_ROLE {
        if let Some(card) = super::chat::completed_plan_lines(msg, width) {
            lines.extend(card);
            lines.push(Line::from(""));
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

/// How a pane divides its inner area bottom-up: transcript body on top, then
/// the plan strip and queued preview, its input box at the bottom (#2494).
/// Slots the row budget could not afford come back `None` and are not drawn.
pub(super) struct PaneLayout {
    /// Transcript rows.
    pub body: Rect,
    /// Plan strip slot, `None` when the pane has no plan or no room.
    pub strip: Option<Rect>,
    /// Queued-preview slot, `None` when nothing is queued or no room.
    pub queue: Option<Rect>,
    /// Input rows, above the footer.
    pub input: Rect,
    /// Compact footer row at the bottom edge: model, policy, pane index.
    pub footer: Rect,
}

/// Slice a pane's inner area into body / strip / queue / input using the
/// shared budget [`PaneRows`] (#2494). Placement is bottom-up: input at the
/// bottom edge, then queue, then strip, with the body taking everything
/// above, so the input box always sits where the window input used to.
pub(super) fn pane_layout(inner: Rect, rows: PaneRows) -> PaneLayout {
    let mut bottom = inner.bottom();

    // The footer owns the pane's last row; everything else stacks above it.
    let footer_h = rows.footer.min(bottom.saturating_sub(inner.y));
    bottom = bottom.saturating_sub(footer_h);
    let footer = Rect {
        x: inner.x,
        y: bottom,
        width: inner.width,
        height: footer_h,
    };

    let input_h = rows.input.min(bottom.saturating_sub(inner.y));
    bottom = bottom.saturating_sub(input_h);
    let input = Rect {
        x: inner.x,
        y: bottom,
        width: inner.width,
        height: input_h,
    };

    let queue = if rows.queue > 0 {
        let h = rows.queue.min(bottom.saturating_sub(inner.y));
        bottom = bottom.saturating_sub(h);
        Some(Rect {
            x: inner.x,
            y: bottom,
            width: inner.width,
            height: h,
        })
    } else {
        None
    };

    let strip = if rows.strip > 0 {
        let h = rows.strip.min(bottom.saturating_sub(inner.y));
        bottom = bottom.saturating_sub(h);
        Some(Rect {
            x: inner.x,
            y: bottom,
            width: inner.width,
            height: h,
        })
    } else {
        None
    };

    let body = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: bottom.saturating_sub(inner.y),
    };
    PaneLayout {
        body,
        strip,
        queue,
        input,
        footer,
    }
}

/// Style for a pane's own chrome text. The focused pane reads at full
/// strength; an unfocused pane drops to the existing inactive colour so its
/// composer and plan strip cannot be mistaken for the live one (#2577). No new
/// colour is introduced: the dim role is the one the inactive border already
/// uses.
fn pane_chrome_style(focused: bool, role: Role) -> Style {
    if focused {
        Style::default().fg(theme::role(role))
    } else {
        Style::default().fg(theme::role(Role::GrayDim))
    }
}

/// The three strip lines a pane shows for its live checklist: the title with
/// done/total, the bar with the percent, and the running task (or its
/// absence) (#2494). An Active plan whose checklist was never seeded gets a
/// single warning line instead of a zero division dressed up as progress.
fn pane_strip_lines(plan: &PlanDocument, focused: bool) -> Vec<Line<'static>> {
    let done = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
        .count();
    let total = plan.tasks.len();
    if total == 0 {
        return vec![Line::from(Span::styled(
            format!("📋 {} · checklist not seeded", plan.title),
            pane_chrome_style(focused, Role::Warning),
        ))];
    }

    let pct = done * 100 / total;
    let filled = done * 10 / total;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled));
    let running = match plan
        .tasks
        .iter()
        .find(|t| t.status == TaskStatus::InProgress)
    {
        Some(t) => format!("▸ {}", t.title),
        None if done == total => "✓ complete".to_string(),
        None => "▸ waiting".to_string(),
    };

    vec![
        Line::from(Span::styled(
            format!("📋 {} · {}/{}", plan.title, done, total),
            pane_chrome_style(focused, Role::AccentTeal),
        )),
        Line::from(Span::styled(
            format!("{} {}%", bar, pct),
            pane_chrome_style(focused, Role::Success),
        )),
        Line::from(Span::styled(
            running,
            pane_chrome_style(focused, Role::TextPrimary),
        )),
    ]
}

/// One-line queued-message preview for a pane's queue slot (#2494): how many
/// are waiting and the head of the oldest one.
fn pane_queue_line(msgs: &[QueuedUserMessage]) -> Line<'static> {
    let first = msgs
        .first()
        .map(|m| m.display_text.replace('\n', " "))
        .unwrap_or_default();
    Line::from(Span::styled(
        format!("⏳ {} queued: {}", msgs.len(), first),
        Style::default().fg(theme::role(Role::Warning)),
    ))
}

/// Draw a pane's own plan strip and queued preview into the slots the shared
/// budget carved out (#2494). Shared by the focused and the inactive pane so
/// both read exactly the same.
pub(super) fn render_pane_strips(
    f: &mut Frame,
    app: &App,
    pane_id: PaneId,
    strip: Option<Rect>,
    queue: Option<Rect>,
    focused: bool,
) {
    let Some(pane) = app.pane_manager.get(pane_id) else {
        return;
    };

    if let Some(area) = strip
        && let Some(plan) = pane
            .plan_document
            .clone()
            .filter(|p| p.status == PlanStatus::Active)
    {
        f.render_widget(Paragraph::new(pane_strip_lines(&plan, focused)), area);
    }

    if let Some(area) = queue
        && let Some(sid) = pane.session_id
    {
        let msgs = app
            .queued_messages
            .lock()
            .ok()
            .and_then(|q| q.get(&sid).cloned())
            .unwrap_or_default();
        if !msgs.is_empty() {
            f.render_widget(Paragraph::new(pane_queue_line(&msgs)), area);
        }
    }
}

/// A pane's stashed input, read-only: a dim box showing the tail lines that
/// fit, so tabbing to a pane shows what was about to be sent there without
/// implying the buffer is editable from a pane that is not focused (#2494).
/// Its text uses the dim role rather than the live composer's text colour, so
/// an unfocused pane's composer reads as inactive at a glance (#2577).
fn render_pane_input_readonly(f: &mut Frame, buffer: &str, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let inner_h = area.height.saturating_sub(2) as usize;
    let all: Vec<&str> = buffer.lines().collect();
    let start = all.len().saturating_sub(inner_h);
    let lines: Vec<Line> = all[start..]
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                pane_chrome_style(false, Role::TextPrimary),
            ))
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::role(Role::GrayDim)));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// The row budget for one pane: its live plan (the pane's own document, only
/// while Active) and whether that pane's session has queued messages, fed
/// through the shared [`pane_rows_with`](crate::ui_core::split::pane_rows_with)
/// budget (#2494). The snapshot happens here so the borrow of the pane ends
/// before the renderers' mutable borrows begin.
pub(super) fn pane_rows_for(app: &App, pane_id: PaneId, pane_height: u16) -> PaneRows {
    let pane = match app.pane_manager.get(pane_id) {
        Some(p) => p,
        None => return pane_rows_with(pane_height, 0, 0),
    };
    let desired_strip = if pane
        .plan_document
        .as_ref()
        .is_some_and(|p| p.status == PlanStatus::Active)
    {
        PANE_STRIP_MIN
    } else {
        0
    };
    let desired_queue = pane
        .session_id
        .map(|sid| {
            app.queued_messages
                .lock()
                .ok()
                .is_some_and(|q| q.get(&sid).is_some_and(|v| !v.is_empty()))
        })
        .unwrap_or(false);
    pane_rows_with(pane_height, desired_strip, u16::from(desired_queue))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::agent::QueuedUserMessage;
    use crate::tui::plan::{PlanStatus, PlanTask, TaskStatus, TaskType};
    use uuid::Uuid;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    fn plan_with(tasks: impl FnOnce(&mut PlanDocument)) -> PlanDocument {
        let mut plan = PlanDocument::new(Uuid::new_v4(), "Landing".to_string());
        plan.status = PlanStatus::Active;
        tasks(&mut plan);
        plan
    }

    #[test]
    fn split_pane_layout_slices_bottom_up() {
        let inner = rect(0, 0, 40, 30);
        let rows = PaneRows {
            body: 19,
            strip: 5,
            queue: 2,
            input: 3,
            footer: 1,
        };
        let l = pane_layout(inner, rows);
        assert_eq!(l.footer, rect(0, 29, 40, 1));
        assert_eq!(l.input, rect(0, 26, 40, 3));
        assert_eq!(l.queue, Some(rect(0, 24, 40, 2)));
        assert_eq!(l.strip, Some(rect(0, 19, 40, 5)));
        assert_eq!(l.body, rect(0, 0, 40, 19));
    }

    #[test]
    fn split_pane_layout_zero_strip_and_queue_fill_the_body() {
        let inner = rect(0, 0, 40, 30);
        let rows = PaneRows {
            body: 26,
            strip: 0,
            queue: 0,
            input: 3,
            footer: 1,
        };
        let l = pane_layout(inner, rows);
        assert_eq!(l.strip, None);
        assert_eq!(l.queue, None);
        assert_eq!(l.input, rect(0, 26, 40, 3));
        assert_eq!(l.footer, rect(0, 29, 40, 1));
        assert_eq!(l.body, rect(0, 0, 40, 26));
    }

    #[test]
    fn split_pane_strip_shows_three_of_eight_progress() {
        let plan = plan_with(|p| {
            for i in 0..8 {
                let mut t =
                    PlanTask::new(i + 1, format!("task {i}"), String::new(), TaskType::Edit);
                if i < 3 {
                    t.status = TaskStatus::Completed;
                }
                p.add_task(t);
            }
        });
        let lines = pane_strip_lines(&plan, true);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].spans[0].content.contains("📋 Landing · 3/8"));
        assert_eq!(lines[1].spans[0].content, "███░░░░░░░ 37%");
        assert_eq!(lines[2].spans[0].content, "▸ waiting");
    }

    #[test]
    fn split_pane_strip_seed_line_when_checklist_not_seeded() {
        let plan = plan_with(|_| {});
        let lines = pane_strip_lines(&plan, true);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].spans[0]
                .content
                .contains("📋 Landing · checklist not seeded")
        );
    }

    #[test]
    fn split_pane_strip_running_task_and_complete_lines() {
        let mut running = plan_with(|p| {
            for i in 0..8 {
                let mut t =
                    PlanTask::new(i + 1, format!("task {i}"), String::new(), TaskType::Edit);
                if i < 3 {
                    t.status = TaskStatus::Completed;
                }
                if i == 4 {
                    t.status = TaskStatus::InProgress;
                }
                p.add_task(t);
            }
        });
        let lines = pane_strip_lines(&running, true);
        assert_eq!(lines[2].spans[0].content, "▸ task 4");

        running
            .tasks
            .iter_mut()
            .for_each(|t| t.status = TaskStatus::Completed);
        let lines = pane_strip_lines(&running, true);
        assert_eq!(lines[2].spans[0].content, "✓ complete");
    }

    #[test]
    fn split_pane_queue_line_previews_the_first_message() {
        let msgs = vec![
            QueuedUserMessage::plain("first".to_string()),
            QueuedUserMessage::plain("second".to_string()),
        ];
        let line = pane_queue_line(&msgs);
        assert_eq!(line.spans[0].content, "⏳ 2 queued: first");
    }

    /// An unfocused pane's composer text drops to the dim role while the live
    /// composer keeps its text colour, and the two differ (#2577).
    #[test]
    fn pane_composer_dim_text_style_when_unfocused() {
        let live = pane_chrome_style(true, Role::TextPrimary);
        let dim = pane_chrome_style(false, Role::TextPrimary);
        assert_eq!(live.fg, Some(theme::role(Role::TextPrimary)));
        assert_eq!(dim.fg, Some(theme::role(Role::GrayDim)));
        assert_ne!(live.fg, dim.fg);
    }

    /// The read-only composer box actually renders its text in the dim role,
    /// which is what makes an unfocused pane read as inactive on screen.
    #[test]
    fn pane_composer_dim_renders_the_unfocused_box_dim() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        terminal
            .draw(|f| render_pane_input_readonly(f, "hello", rect(0, 0, 20, 4)))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let cell = &buffer[(1, 1)];
        assert_eq!(cell.symbol(), "h");
        assert_eq!(cell.fg, theme::role(Role::GrayDim));
    }

    /// The plan strip follows the same rule: full colour while focused, dim in
    /// every line once the pane loses focus, with the text itself unchanged
    /// (#2577).
    #[test]
    fn pane_composer_dim_plan_strip_when_unfocused() {
        let plan = plan_with(|p| {
            for i in 0..8 {
                let mut t =
                    PlanTask::new(i + 1, format!("task {i}"), String::new(), TaskType::Edit);
                if i < 3 {
                    t.status = TaskStatus::Completed;
                }
                p.add_task(t);
            }
        });

        let live = pane_strip_lines(&plan, true);
        assert_eq!(
            live[0].spans[0].style.fg,
            Some(theme::role(Role::AccentTeal))
        );
        assert_eq!(live[1].spans[0].style.fg, Some(theme::role(Role::Success)));

        let dim = pane_strip_lines(&plan, false);
        for line in &dim {
            assert_eq!(line.spans[0].style.fg, Some(theme::role(Role::GrayDim)));
        }
        assert_eq!(dim[0].spans[0].content, live[0].spans[0].content);
        assert_eq!(dim[1].spans[0].content, live[1].spans[0].content);
    }
}
