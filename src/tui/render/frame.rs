//! The per-frame entry point: lays out chat / plan / queue / input / status
//! for the current [`AppMode`] and hands each region to its renderer.

use super::super::app::App;
use super::super::events::AppMode;
use super::super::onboarding_render;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    widgets::{Block, Clear},
};
use unicode_width::UnicodeWidthStr;

use super::chat::render_chat;
use super::dialogs::{
    render_directory_picker, render_file_picker, render_restart_dialog, render_update_dialog,
};
use super::help::{render_help, render_settings};
use super::input::{
    render_emoji_picker, render_followup, render_input, render_slash_autocomplete,
    render_status_bar,
};
use super::plan_widget::render_plan_checklist;
use super::projects::render_projects;
use super::session_files::render_session_files;
use super::sessions::render_sessions;
use super::theme;
use super::title::{render_app_title, split_title_area};
use super::{mission_control, panes, plan_overlay, profiles_dialog, skills_dialog};

/// Paint the canvas the rest of the frame draws on, when the active theme
/// declares one (#1634).
///
/// Themes that declare no background (`crab-dark`, and every user file
/// written before the canvas existed) are a no-op here, so the terminal's
/// own background keeps showing and transparency or blur still works. A
/// full-area `Block` rather than `Clear`: `Clear` resets cells to the
/// terminal default, which is the behaviour this exists to override.
fn paint_canvas(f: &mut Frame) {
    let Some(bg) = theme::background() else {
        return;
    };
    f.render_widget(Block::default().style(Style::default().bg(bg)), f.area());
}

/// Rows the chat transcript is guaranteed before the plan card takes any.
///
/// One number, one owner: the layout below asks for exactly this with
/// `Constraint::Min`, so the card's budget and the layout's demand can never
/// disagree. They used to (card reserved 6, layout demanded 10), and on a short
/// terminal the solver satisfied the fixed card height by starving the
/// transcript, which slid the live "thinking" row and the turn's answer under
/// the checklist widget (#2165).
pub(crate) const CHAT_FLOOR: u16 = 10;

/// Budget the plan card against the real terminal rows (#1750).
///
/// The card used to take `min(tasks + 2, 12)` unconditionally, so on a
/// short pane the fixed-height demands (chat min, card, queue preview,
/// input, status bar) exceeded the screen and the solver starved the
/// only flexible chunk: chat. The live tool rows then got a 2-row
/// sliver against the card's edge and every tool event repainted it,
/// which read as content popping out from behind the widget. The card
/// yields first instead: it shrinks to the rows left after reserving
/// the status bar, the input box, the queue preview, and a chat floor,
/// and its own windowing indicator ("N before/after") makes the
/// truncation honest. Only called with `desired >= 3` (seed strip or
/// task card), so the floor never inflates the result.
pub(crate) fn plan_card_height(
    desired: u16,
    area_height: u16,
    input_height: u16,
    queue_height: u16,
) -> u16 {
    const CARD_MIN: u16 = 3; // header + one windowed task row + footer
    const STATUS_BAR: u16 = 1;
    let reserved = STATUS_BAR
        .saturating_add(input_height)
        .saturating_add(queue_height)
        .saturating_add(CHAT_FLOOR);
    // Not even the minimum card fits beside the chat floor: give the transcript
    // the rows and draw no card. A three-row card on a screen this short buries
    // the live turn, which is the thing being read.
    if area_height < reserved.saturating_add(CARD_MIN) {
        return 0;
    }
    let budget = area_height.saturating_sub(reserved);
    desired.min(budget).max(CARD_MIN)
}

/// Render the entire UI
pub fn render(f: &mut Frame, app: &mut App) {
    paint_canvas(f);

    if app.mode == AppMode::Onboarding {
        if let Some(ref wizard) = app.onboarding {
            onboarding_render::render_onboarding(f, wizard);
        }
        return;
    }

    use super::input::render_queue;

    // Compute queue height (2 rows if current session has queued message)
    let queue_height: u16 = {
        let has_queue = app
            .current_session
            .as_ref()
            .and_then(|s| {
                app.queued_messages
                    .lock()
                    .ok()
                    .map(|q| q.get(&s.id).is_some_and(|v| !v.is_empty()))
            })
            .unwrap_or(false);
        if has_queue { 2 } else { 0 }
    };

    // Dynamic input height: grows with content, capped at 10.
    // Early-exit once we hit the cap so huge pastes don't walk every line.
    let input_height = if app.input_buffer.is_empty() {
        3 // 1 content row + 2 borders
    } else {
        let terminal_width = (f.area().width.saturating_sub(4) as usize).max(1);
        const MAX_CONTENT_ROWS: usize = 8; // 10 cap - 2 borders
        let mut rows = 0usize;
        // split('\n') preserves empty trailing lines (Ctrl+J / Shift+Enter
        // newlines), unlike lines() which silently strips them.
        for line in app.input_buffer.split('\n') {
            rows += if line.is_empty() {
                1
            } else {
                (UnicodeWidthStr::width(line) + 2).div_ceil(terminal_width)
            };
            if rows >= MAX_CONTENT_ROWS {
                rows = MAX_CONTENT_ROWS;
                break;
            }
        }
        (rows.max(1) as u16 + 2).min(10)
    };

    // Show the plan checklist only while the checklist is live (Active
    // with tasks). Editing plans are design prose with no checklist to
    // render; a seed-failed Active plan has no tasks yet, so no panel.
    //
    // Sizing:
    //   - chrome (header + border) takes 2 lines.
    //   - For ≤10 tasks: grow the panel so all tasks fit (no truncation
    //     for the common case — most plans are 3-10 steps).
    //   - For >10 tasks: cap at 12 lines (10 task rows + chrome) and
    //     let `plan_widget` scroll a window centered on the current
    //     task. The "… (N before, M after)" indicator inside the widget
    //     tells the user there's more above/below.
    //   - On short panes the card yields first (`plan_card_height`):
    //     it shrinks to the rows left after reserving the status bar,
    //     input, queue preview, and a chat floor, so chat never gets
    //     crushed into a sliver against the card's edge (#1750).
    let plan_height = app
        .plan_document
        .as_ref()
        .filter(|p| p.status == crate::tui::plan::PlanStatus::Active)
        .map(|p| {
            let desired: u16 = if p.tasks.is_empty() {
                // Seed window (approved design, checklist not built yet):
                // a 5-line strip carries Building checklist… / seed-error
                // with a blank row above and below the text; the 3-line
                // budget left the text squeezed between the two borders
                // and it read as broken (#1945).
                5
            } else {
                (p.tasks.len() + 2).min(12) as u16
            };
            plan_card_height(desired, f.area().height, input_height, queue_height)
        })
        .unwrap_or(0);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(CHAT_FLOOR),      // [0] Chat messages
            Constraint::Length(plan_height),  // [1] Plan checklist (0 when no plan)
            Constraint::Length(queue_height), // [2] Queue preview (0 when no queue)
            Constraint::Length(input_height), // [3] Input (dynamic)
            Constraint::Length(1),            // [4] Status bar
        ])
        .split(f.area());

    // Full area for modes that replace the chat+input (Sessions, Help, etc.)
    // These modes do not show the plan checklist.
    let full_content_area = Rect {
        x: chunks[0].x,
        y: chunks[0].y,
        width: chunks[0].width,
        height: chunks[0].height + chunks[1].height + chunks[2].height + chunks[3].height,
    };

    match app.mode {
        AppMode::Onboarding => {
            // Handled by early return above
        }
        AppMode::Chat => {
            if app.pane_manager.is_split() {
                render_split_panes(f, app, chunks[0]);
            } else {
                render_chat(f, app, chunks[0]);
            }
            if plan_height > 0 {
                render_plan_checklist(f, app, chunks[1]);
            }
            // Render queue preview above input
            if queue_height > 0 {
                render_queue(f, app, chunks[2]);
            }
            // Store input area coordinates for mouse event mapping
            // chunks[3] is always the input slot (indices are fixed)
            app.input_area_x = chunks[3].x;
            app.input_area_y = chunks[3].y;
            app.input_area_width = chunks[3].width;
            app.input_area_height = chunks[3].height;
            // The paragraph scrolls once the buffer outgrows the box; store
            // it so click and drag map the row the user sees, not the row the
            // buffer starts at.
            let input_scroll_y = render_input(f, app, chunks[3]);
            app.input_scroll_y = input_scroll_y;
            render_status_bar(f, app, chunks[4]);
            if app.slash_suggestions_active {
                render_slash_autocomplete(f, app, chunks[3]);
            } else if app.emoji_picker_active {
                render_emoji_picker(f, app, chunks[3]);
            } else if app.followup_suggestions.len() >= 2 && app.input_buffer.is_empty() {
                render_followup(f, app, chunks[3]);
            }
        }
        AppMode::Sessions => {
            // Full-bleed (#2162): no app title row, no border, the whole
            // terminal is the list. Clearing `f.area()` rather than
            // `full_content_area` also repaints the bottom row that the
            // latter excludes and this arm never drew, which used to leave
            // stale chat pixels under the last session.
            let area = f.area();
            f.render_widget(Clear, area);
            render_sessions(f, app, area);
        }
        AppMode::Help => {
            // Full-bleed (#2162), same as Sessions: the border and the
            // title row were spending three of the screen's rows.
            let area = f.area();
            f.render_widget(Clear, area);
            render_help(f, app, area);
        }
        AppMode::PlanOverlay => {
            f.render_widget(Clear, full_content_area);
            let (title_area, content_area) = split_title_area(full_content_area);
            render_app_title(f, title_area);
            plan_overlay::render_plan_overlay(f, app, content_area);
        }
        AppMode::MissionControl => {
            // Full-screen overlay (like /sessions and /help). Clears the
            // chat area first so split panes don't bleed through.
            f.render_widget(Clear, full_content_area);
            let (title_area, content_area) = split_title_area(full_content_area);
            render_app_title(f, title_area);
            mission_control::draw(f, app, content_area);
        }
        AppMode::SkillsList => {
            f.render_widget(Clear, full_content_area);
            let (title_area, content_area) = split_title_area(full_content_area);
            render_app_title(f, title_area);
            skills_dialog::draw(f, app, content_area);
        }
        AppMode::Profiles => {
            f.render_widget(Clear, full_content_area);
            let (title_area, content_area) = split_title_area(full_content_area);
            render_app_title(f, title_area);
            profiles_dialog::draw(f, app, content_area);
        }
        AppMode::SessionFiles => {
            f.render_widget(Clear, full_content_area);
            let (title_area, content_area) = split_title_area(full_content_area);
            render_app_title(f, title_area);
            render_session_files(f, app, content_area);
        }
        AppMode::Projects => {
            f.render_widget(Clear, full_content_area);
            let (title_area, content_area) = split_title_area(full_content_area);
            render_app_title(f, title_area);
            render_projects(f, app, content_area);
        }
        AppMode::Settings => {
            let (title_area, content_area) = split_title_area(full_content_area);
            render_app_title(f, title_area);
            render_settings(f, app, content_area);
        }
        AppMode::FilePicker => {
            render_file_picker(f, app, full_content_area);
        }
        AppMode::DirectoryPicker => {
            render_directory_picker(f, app, full_content_area);
        }
        AppMode::UsageDashboard => {
            // Full-screen surface, not an overlay on the chat (#2073). The
            // chat, plan, queue and input underneath are gone, so nothing
            // stale shows through the gaps between cards.
            f.render_widget(Clear, full_content_area);
            if let Some(ref ds) = app.dashboard_state {
                crate::usage::dashboard::render(f, ds, full_content_area);
            }
        }
        AppMode::RestartPending => {
            render_chat(f, app, chunks[0]);
            if plan_height > 0 {
                render_plan_checklist(f, app, chunks[1]);
            }
            if queue_height > 0 {
                render_queue(f, app, chunks[2]);
            }
            app.input_area_x = chunks[3].x;
            app.input_area_y = chunks[3].y;
            app.input_area_width = chunks[3].width;
            app.input_area_height = chunks[3].height;
            // The paragraph scrolls once the buffer outgrows the box; store
            // it so click and drag map the row the user sees, not the row the
            // buffer starts at.
            let input_scroll_y = render_input(f, app, chunks[3]);
            app.input_scroll_y = input_scroll_y;
            render_status_bar(f, app, chunks[4]);
            render_restart_dialog(f, f.area());
        }
        AppMode::UpdatePrompt => {
            // Overlay the update dialog on top of the normal chat UI.
            if app.pane_manager.is_split() {
                render_split_panes(f, app, chunks[0]);
            } else {
                render_chat(f, app, chunks[0]);
            }
            if plan_height > 0 {
                render_plan_checklist(f, app, chunks[1]);
            }
            if queue_height > 0 {
                render_queue(f, app, chunks[2]);
            }
            app.input_area_x = chunks[3].x;
            app.input_area_y = chunks[3].y;
            app.input_area_width = chunks[3].width;
            app.input_area_height = chunks[3].height;
            // The paragraph scrolls once the buffer outgrows the box; store
            // it so click and drag map the row the user sees, not the row the
            // buffer starts at.
            let input_scroll_y = render_input(f, app, chunks[3]);
            app.input_scroll_y = input_scroll_y;
            render_status_bar(f, app, chunks[4]);
            render_update_dialog(f, app, f.area());
        }
    }

    // Theme picker overlay (#1371): drawn above every base surface so the
    // live preview recolors the real UI underneath the dialog.
    if let Some(picker) = &app.theme_picker {
        let area = f.area();
        crate::tui::render::theme_picker::draw(f, area, picker);
    }

    // Ctrl+C expanded command panel (#1775), topmost. Self-heals if the
    // dialog it documents closed on its own (approval resolved, popup
    // dismissed) while the panel was open.
    if app.dialog_help_open && app.active_dialog_scope() != Some(app.dialog_help_scope) {
        app.dialog_help_open = false;
    }
    if app.dialog_help_open {
        let area = f.area();
        crate::tui::render::hints::render_command_panel(f, area, app.dialog_help_scope);
    }
}

/// Render the chat area as split panes.
fn render_split_panes(f: &mut Frame, app: &mut App, area: Rect) {
    let tree = match &app.pane_manager.root {
        Some(t) => t.clone(),
        None => return render_chat(f, app, area),
    };

    let focused_id = app.pane_manager.focused;
    let pane_rects = tree.layout(area);

    for (pane_id, rect) in pane_rects {
        if rect.width < 3 || rect.height < 3 {
            continue; // too small to render
        }
        if pane_id == focused_id {
            let inner = panes::focused_pane_border(f, app, rect);
            render_chat(f, app, inner);
        } else {
            panes::render_inactive_pane(f, app, pane_id, rect);
        }
    }
}
