//! Plan overlay: the TUI surface for the design track. While a plan is
//! Editing it shows the scrollable session `.md` body with an
//! Approve/Discard footer (deliberately NOT the tool-policy approve
//! flow); while Active it shows the checklist with progress. The badge
//! line distinguishes pre-init (no approvable document yet, no Approve
//! hint) from post-init Editing and Active.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::app::App;
use crate::tui::render::theme::{self, Role};
use crate::utils::plan_files::{self, PlanModeState};

pub(super) fn render_plan_overlay(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(Clear, area);

    if app.current_session.is_none() {
        let p = Paragraph::new("No active session.")
            .block(Block::default().borders(Borders::ALL).title(" Plan "));
        f.render_widget(p, area);
        return;
    }

    // Render from state the async layer already resolved: the loaded plan
    // (reload_plan) and the resolved plan file path. Deriving the mode-state
    // and body here keeps the render loop sync, since the location resolver
    // is a DB lookup that must not run per frame.
    let md_path = app.plan_file_path().map(|p| p.with_extension("md"));
    let md_exists = md_path.as_ref().is_some_and(|p| p.exists());
    let state = plan_files::plan_mode_state_of(app.plan_document(), md_exists);
    // Pre-init is now a durable marker file, not a loaded PlanDocument (#569),
    // so a pre-init session has no `plan_document` and resolves to NoPlan above.
    // Detect it here the same way `md_exists` is derived — a sibling `.preinit`
    // of the resolved plan path — so the render loop stays sync (no DB lookup).
    let state = if state == PlanModeState::NoPlan
        && app
            .plan_file_path()
            .map(|p| p.with_extension("preinit"))
            .is_some_and(|p| p.exists())
    {
        PlanModeState::PreInitEditing
    } else {
        state
    };
    let (badge, badge_style) = match state {
        PlanModeState::NoPlan => ("no plan", Style::default().fg(theme::role(Role::GrayDim))),
        PlanModeState::PreInitEditing => (
            "Editing · pre-init",
            Style::default()
                .fg(theme::role(Role::Warning))
                .add_modifier(Modifier::BOLD),
        ),
        PlanModeState::PostInitEditing => (
            "Editing",
            Style::default()
                .fg(theme::role(Role::Warning))
                .add_modifier(Modifier::BOLD),
        ),
        PlanModeState::Active => (
            "Active",
            Style::default()
                .fg(theme::role(Role::Success))
                .add_modifier(Modifier::BOLD),
        ),
    };

    let body: String = match state {
        PlanModeState::NoPlan => "No live plan for this session.\n\nStart one with /plan \
                                  (design track) or ask for a checklist."
            .to_string(),
        PlanModeState::PreInitEditing => "Plan mode is on, but `plan init` has not created \
                                          the design document yet.\n\nDescribe what you want \
                                          planned; the agent explores, then drafts the \
                                          SESSION PLAN for your approval. Leave Plan mode \
                                          with /discard."
            .to_string(),
        PlanModeState::PostInitEditing => md_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_else(|| "(the session plan .md is unreadable)".to_string()),
        PlanModeState::Active => app
            .plan_document()
            .map(crate::utils::plan_mode::format_active_checklist)
            .unwrap_or_else(|| "Plan JSON is unreadable.".to_string()),
    };

    let title = Line::from(vec![
        Span::raw(" 📋 Plan · "),
        Span::styled(badge, badge_style),
        Span::raw(" "),
    ]);

    // Canonical footer from the scoped keymap, sliced by state suffix
    // (#1775): every state shows a true suffix of the PlanOverlay table,
    // so no state can advertise a key it does not handle.
    let keys = crate::tui::app::dialog_keys::dialog_keys(
        crate::tui::app::dialog_keys::DialogScope::PlanOverlay,
    );
    let start = match state {
        PlanModeState::PostInitEditing => 0,
        PlanModeState::Active | PlanModeState::PreInitEditing => {
            keys.iter().position(|k| k.label == "d").unwrap_or(0)
        }
        PlanModeState::NoPlan => keys.iter().position(|k| k.label == "Esc").unwrap_or(0),
    };
    let mut footer = crate::tui::render::hints::footer_line(" ", &keys[start..]);
    if matches!(state, PlanModeState::PostInitEditing) {
        footer.spans.push(Span::styled(
            " (validator runs on approve) ",
            Style::default().fg(theme::role(Role::GrayDim)),
        ));
    } else {
        footer.spans.push(Span::raw(" "));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(footer);

    let paragraph = Paragraph::new(body)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.plan_overlay_scroll as u16, 0));
    f.render_widget(paragraph, area);
}
