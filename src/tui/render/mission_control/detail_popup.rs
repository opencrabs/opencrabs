//! Detail popup overlay — opens on Enter, dismissed on Esc.
//!
//! Renders the full record for the currently-selected item in the
//! focused panel. Inbox proposals show their rationale + tool/command
//! definition; activity entries show the parsed metadata; schedule
//! rows show the cron expression + paused/next-run state.
//!
//! Pure read of `app.mc` snapshots — no service calls during render.

use super::theme;
use crate::brain::mission_control::{
    McActivity, McInboxItem, McInboxKind, McScheduleItem, McScheduleKind, inbox_service,
};
use crate::tui::app::App;
use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};
use crate::tui::app::mission_control::McPanel;
use crate::tui::render::{chrome, hints};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

/// Render a centred popup that fits its content (max ~60% × 70% of `area`).
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let pw = (area.width * 60 / 100).max(40);
    let max_ph = (area.height * 70 / 100).max(12);

    // Analytics renders its own rich 2x2 + full-width Top tools view (the same
    // renderer as the panel, just larger) rather than a flat text body.
    if app.mc.focused_panel == McPanel::Analytics {
        let ph = max_ph;
        let px = area.x + area.width.saturating_sub(pw) / 2;
        let py = area.y + area.height.saturating_sub(ph) / 2;
        let popup = Rect::new(px, py, pw, ph);
        frame.render_widget(Clear, popup);
        super::analytics_panel::render(
            frame,
            &app.mc.analytics,
            app.mc.analytics_window,
            app.mc.scroll_offset,
            popup,
            true,
        );
        return;
    }

    let (title, accent, lines) = match app.mc.focused_panel {
        McPanel::Inbox => inbox_detail(app),
        McPanel::Activity => activity_detail(app),
        McPanel::Schedule => schedule_detail(app),
        McPanel::Analytics => unreachable!("analytics handled above"),
    };

    // Estimate content height: count visual lines accounting for soft-wrapping.
    let content_width = pw.saturating_sub(4) as usize; // borders + inner padding
    let mut visual_lines: u16 = 0;
    for line in &lines {
        let line_width: usize = line.spans.iter().map(|s| s.width()).sum();
        let wrapped = if content_width > 0 {
            line_width.div_ceil(content_width).max(1)
        } else {
            1
        };
        visual_lines += wrapped as u16;
    }
    // +2 for top/bottom border, +1 breathing room, +1 for the shared
    // footer row (#1775)
    let content_height = visual_lines.saturating_add(4);
    let ph = content_height.min(max_ph).max(12);

    let px = area.x + area.width.saturating_sub(pw) / 2;
    let py = area.y + area.height.saturating_sub(ph) / 2;
    let popup = Rect::new(px, py, pw, ph);

    frame.render_widget(Clear, popup);

    let block = chrome::modal_block(&title, accent);

    let body = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block);
    frame.render_widget(body, popup);

    // The shared command footer, bottom inside the border (#1775). Inbox
    // items carry verbs (apply/reject — real since the popup handler
    // consumes a/r); the read-only panels get the navigate/close set.
    let scope = if app.mc.focused_panel == McPanel::Inbox {
        DialogScope::McDetailPopup
    } else {
        DialogScope::McDetailRead
    };
    hints::render_footer(frame, chrome::footer_row(popup), dialog_keys(scope));
}

// ── Inbox detail ────────────────────────────────────────────────────────────

fn inbox_detail(app: &App) -> (String, ratatui::style::Color, Vec<Line<'static>>) {
    let items = inbox_service::list();
    let item = items.get(app.mc.selected_index);
    let lines = match item {
        Some(item) => render_inbox_item(item),
        None => empty_lines("No proposal selected"),
    };
    (" Inbox detail ".to_string(), theme::teal(), lines)
}

fn render_inbox_item(item: &McInboxItem) -> Vec<Line<'static>> {
    let kind_label = match item.kind {
        McInboxKind::ProposedTool => "tool",
        McInboxKind::ProposedCommand => "command",
        McInboxKind::ProposedSkill => "skill",
    };
    let filed = item.created_at.format("%Y-%m-%d %H:%M UTC").to_string();
    let mut lines: Vec<Line<'static>> = vec![
        blank(),
        kv("Kind", kind_label),
        kv("Label", &item.label),
        kv("Source", &item.source),
        kv("Filed", &filed),
        kv("ID", &item.id),
        blank(),
        section_heading("Summary"),
    ];
    for s in wrap_paragraph(&item.summary) {
        lines.push(body_line(s));
    }

    // Render type-specific detail sections when available.
    if let Some(_detail) = &item.detail {
        // No variant-specific details currently active.
    }

    // No trailing "Apply / reject" section: the shared footer at the
    // bottom of the popup carries the real a/r bindings (#1775). The
    // old body line pointed at a proposal-apply CLI that does not
    // exist on the PATH.
    lines
}

// ── Activity detail ─────────────────────────────────────────────────────────

fn activity_detail(app: &App) -> (String, ratatui::style::Color, Vec<Line<'static>>) {
    let entry = app.mc.activity.get(app.mc.selected_index);
    let lines = match entry {
        Some(e) => render_activity(e),
        None => empty_lines("No activity selected"),
    };
    (" Activity detail ".to_string(), theme::orange(), lines)
}

fn render_activity(entry: &McActivity) -> Vec<Line<'static>> {
    let when = entry.timestamp.map_or_else(
        || "undated".to_string(),
        |ts| ts.format("%Y-%m-%d %H:%M UTC").to_string(),
    );
    let mut lines: Vec<Line<'static>> = vec![
        blank(),
        kv("When", &when),
        kv("Source", &entry.source),
        kv("Level", level_label(entry.level)),
        blank(),
        section_heading("Detail"),
    ];
    for s in wrap_paragraph(&entry.detail) {
        lines.push(body_line(s));
    }
    lines
}

fn level_label(level: crate::brain::mission_control::McActivityLevel) -> &'static str {
    use crate::brain::mission_control::McActivityLevel as L;
    match level {
        L::Info => "info",
        L::Success => "success",
        L::Warn => "warn",
        L::Error => "error",
    }
}

// ── Schedule detail ─────────────────────────────────────────────────────────

fn schedule_detail(app: &App) -> (String, ratatui::style::Color, Vec<Line<'static>>) {
    let item = app.mc.schedule.get(app.mc.selected_index);
    let lines = match item {
        Some(i) => render_schedule(i),
        None => empty_lines("No schedule item selected"),
    };
    (" Schedule detail ".to_string(), theme::white(), lines)
}

fn render_schedule(item: &McScheduleItem) -> Vec<Line<'static>> {
    let kind = match item.kind {
        McScheduleKind::Cron => "cron job",
        McScheduleKind::PendingApproval => "pending approval",
    };
    let state = if item.awaiting_user {
        "awaiting user"
    } else if !item.enabled {
        "paused"
    } else {
        "active"
    };
    let mut lines: Vec<Line<'static>> = vec![
        blank(),
        kv("Kind", kind),
        kv("Label", &item.label),
        kv("State", state),
    ];

    // Schedule + next run
    lines.push(blank());
    lines.push(section_heading("Schedule"));
    lines.push(kv("Cron", &item.schedule));
    if let Some(next) = &item.next_run_at {
        lines.push(kv("Next", &next.format("%Y-%m-%d %H:%M UTC").to_string()));
    } else {
        lines.push(kv("Next", "not scheduled"));
    }

    // Last run
    lines.push(blank());
    lines.push(section_heading("Last run"));
    if let Some(last) = &item.last_run_at {
        lines.push(kv("At", &last.format("%Y-%m-%d %H:%M UTC").to_string()));
    } else {
        lines.push(kv("At", "never"));
    }
    if let Some(status) = &item.last_run_status {
        let status_display = match status.as_str() {
            "success" => "success",
            "error" => "error",
            "running" => "running...",
            _ => status.as_str(),
        };
        lines.push(kv("Status", status_display));
    }
    if let Some(cost) = &item.last_run_cost {
        lines.push(kv("Cost", &format!("${:.4}", cost)));
    }
    if let Some(secs) = &item.last_run_duration_secs {
        let dur = if *secs < 60 {
            format!("{}s", secs)
        } else {
            format!("{}m {}s", secs / 60, secs % 60)
        };
        lines.push(kv("Duration", &dur));
    }

    // Delivery target
    if let Some(target) = &item.deliver_to {
        lines.push(blank());
        lines.push(section_heading("Delivery"));
        lines.push(kv("Target", target));
    }

    // Prompt / description
    if !item.prompt.is_empty() {
        lines.push(blank());
        lines.push(section_heading("Prompt"));
        let preview = if item.prompt.len() > 300 {
            format!(
                "{}...",
                crate::utils::string::truncate_str(&item.prompt, 300)
            )
        } else {
            item.prompt.clone()
        };
        for s in wrap_paragraph(&preview) {
            lines.push(body_line(s));
        }
    }

    // Meta
    lines.push(blank());
    lines.push(section_heading("Meta"));
    lines.push(kv(
        "Created",
        &item.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
    ));
    if let Some(profile) = &item.profile_name {
        lines.push(kv("Profile", profile));
    }
    lines.push(kv("ID", &item.id));

    lines
}

// ── Layout helpers ──────────────────────────────────────────────────────────

fn empty_lines(message: &str) -> Vec<Line<'static>> {
    vec![
        blank(),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(message.to_string(), theme::muted()),
        ]),
    ]
}

fn blank() -> Line<'static> {
    Line::raw("")
}

fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{key:<8} "),
            Style::default()
                .fg(theme::text_dim())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            value.to_string(),
            Style::default().fg(theme::text_primary()),
        ),
    ])
}

fn section_heading(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            text.to_string(),
            Style::default()
                .fg(theme::text_dim())
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn body_line(text: String) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(text, Style::default().fg(theme::text_primary())),
    ])
}

/// Greedy soft-wrap on whitespace so each line in the detail popup
/// fits comfortably within typical popup widths. We let the
/// `Paragraph` widget handle hard wrapping for over-long single
/// tokens via `Wrap { trim: false }`.
fn wrap_paragraph(text: &str) -> Vec<String> {
    text.split('\n').map(|s| s.to_string()).collect()
}
