//! Plan Checklist Widget
//!
//! Renders a live-updating checklist of plan tasks above the input box.

use chrono::{DateTime, Utc};

use super::super::app::App;
use super::plan_window::{current_task_index, pick_visible_window};
use super::theme::{self, Role};
use crate::tui::plan::TaskStatus;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

/// Render the plan checklist panel.
pub(super) fn render_plan_checklist(f: &mut Frame, app: &App, area: Rect) {
    let plan = match app.plan_document() {
        Some(p) => p,
        None => return,
    };
    // One clock reading for the whole list: every settled row is aged against
    // the same instant, so a redraw cannot make two rows disagree by a frame.
    let now = Utc::now();

    if area.height == 0 {
        return;
    }

    // Seed window (Building checklist… machine, locked): an Active plan
    // with no tasks yet is an approved design whose checklist is still
    // being seeded. Show Building checklist… while the agent is working;
    // once idle, the seed failed and the strip carries the retry hint.
    if plan.tasks.is_empty() {
        let (text, style) = if app.is_processing {
            (
                "⏳ Building checklist…".to_string(),
                Style::default().fg(theme::role(Role::Warning)),
            )
        } else {
            (
                "⚠️ Checklist seed incomplete. Retry with /execute, or /discard.".to_string(),
                Style::default().fg(theme::role(Role::Error)),
            )
        };
        let title = format!(" 📋 {} · Active ", plan.title);
        // Blank rows above and below the text inside the 5-line seed box
        // (#1945): with only the bordered text row the strip read as text
        // jammed between its own borders. On a squeezed 3-line box (chat
        // floor wins over the card) there is one inner row for text, so
        // padding would push the message out of view: degrade to the
        // tight single line there instead.
        let styled = Span::styled(text, style);
        let lines = if area.height >= 5 {
            vec![Line::from(""), Line::from(styled), Line::from("")]
        } else {
            vec![Line::from(styled)]
        };
        let para = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(para, area);
        return;
    }

    let total = plan.tasks.len();
    let completed = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
        .count();

    let percent = (completed * 100).checked_div(total).unwrap_or(0);

    // Progress bar: 10 chars wide
    let filled = (completed * 10).checked_div(total).unwrap_or(0);
    let bar: String = "█".repeat(filled) + &"░".repeat(10 - filled);

    // Truncate title to fit header
    let max_title_len = area.width.saturating_sub(40) as usize;
    let title = if plan.title.len() > max_title_len && max_title_len > 3 {
        format!("{}…", &plan.title[..max_title_len - 1])
    } else {
        plan.title.clone()
    };

    let header = Line::from(vec![
        Span::styled(
            format!(" Plan: {}  ·  {}/{}  ", title, completed, total),
            Style::default()
                .fg(theme::role(Role::GrayMuted))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(bar, Style::default().fg(theme::role(Role::TealCalm))),
        Span::styled(
            format!("  {}%", percent),
            Style::default().fg(theme::role(Role::GrayMuted)),
        ),
    ]);

    // Dynamic visible count: area height minus header (1), top border (1).
    // For plans that fit we just show everything. For overflowing plans
    // we scroll a window centered on the current InProgress task and
    // emit a leading and/or trailing "… (N more)" indicator that
    // consumes a visible slot in each direction it applies.
    let chrome_lines: usize = 2; // header + top border
    let available = (area.height as usize).saturating_sub(chrome_lines);
    let current_idx = current_task_index(&plan.tasks);
    let window = pick_visible_window(total, available, current_idx);

    let visible: Vec<&crate::tui::plan::PlanTask> = plan
        .tasks
        .iter()
        .skip(window.start)
        .take(window.len)
        .collect();
    let hidden_before = window.start;
    let hidden_after = total.saturating_sub(window.start + window.len);

    let mut lines: Vec<Line> = vec![header];

    if hidden_before > 0 {
        lines.push(Line::from(Span::styled(
            format!("  … ({} above)", hidden_before),
            Style::default().fg(theme::role(Role::GrayDim)),
        )));
    }

    for task in &visible {
        let icon = crate::tui::plan::status_mark(&task.status).to_string();
        let color = match &task.status {
            TaskStatus::Completed | TaskStatus::Skipped => {
                theme::role(settled_task_role(task.completed_at, now))
            }
            TaskStatus::InProgress => theme::role(Role::Accent),
            TaskStatus::Failed => theme::role(Role::Error),
            TaskStatus::Blocked(_) | TaskStatus::Pending => theme::role(Role::GrayDim),
        };

        // Truncate task title to 60 chars
        let task_title = if task.title.len() > 60 {
            format!("{}…", task.title.chars().take(59).collect::<String>())
        } else {
            task.title.clone()
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("  {}  #{:<2}  ", icon, task.order),
                Style::default().fg(color),
            ),
            Span::styled(task_title, Style::default().fg(color)),
        ]));
    }

    if hidden_after > 0 {
        lines.push(Line::from(Span::styled(
            format!("  … ({} below)", hidden_after),
            Style::default().fg(theme::role(Role::GrayDim)),
        )));
    }

    let border_style = Style::default().fg(theme::role(Role::GrayBase));
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(border_style);

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

/// How brightly a settled task is drawn, by how long ago it settled (#2574).
///
/// A finished row used to snap straight to one muted colour, so a checklist
/// gave no sense of motion: the task the agent had just closed looked exactly
/// like one closed an hour earlier. The ramp starts bright, then falls away in
/// steps over the first minute, which the 100ms TUI tick renders as a slow
/// dim rather than a jump. Discrete palette roles, not blended RGB, so every
/// theme keeps its own contrast.
///
/// `None` means the age is unknown (a plan written before the field existed):
/// it lands on the settled colour those plans always had, never on "just now".
pub(crate) fn settled_task_role(completed_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Role {
    let Some(completed_at) = completed_at else {
        return Role::TealMuted;
    };
    let age = now.signed_duration_since(completed_at);
    // A stamp in the future (clock skew across a restore) is not an age we can
    // read, so it takes the legacy colour instead of pretending to be fresh.
    if age.num_seconds() < 0 {
        return Role::TealMuted;
    }
    if age.num_seconds() < 4 {
        Role::TealCalm
    } else if age.num_seconds() < 15 {
        Role::TealMuted
    } else if age.num_seconds() < 60 {
        Role::GrayMuted
    } else {
        Role::GrayDim
    }
}
