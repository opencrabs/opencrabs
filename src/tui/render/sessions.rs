//! Session list rendering
//!
//! Session manager view with navigation, renaming, and status indicators.
//!
//! The list is full-bleed (#2162): no border, no app title row, one row of
//! footer at the bottom carrying the keys from the scoped keymap. A session
//! card is one to three rows (#2701): the name line, the plan bar when the
//! session has a live plan, and the newest message preview. The scroll math
//! walks the same heights the drawing does, so the selected card is always
//! fully in view.
//!
//! Rows are laid out from one [`RowPlan`] computed per frame (#2175). Before
//! that, each row solved its own gap and pushed a badge only when it had a
//! value, so the right half was a right-aligned blob whose width changed with
//! the row. Two adjacent rows could differ by up to 101 columns, which is why
//! the dates never lined up.

use super::super::app::App;
use super::hints;
use super::palette;
use super::responsive::{self, SessionRowTier};
use super::theme::{self, Role};
use super::utils::{format_token_count_raw, format_token_count_with_label, truncate_to_width};
use crate::db::models::Session;
use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

/// On-brand palette for project badges — variations of orange, cyan, white,
/// and blue. Each project is mapped to one entry deterministically so the same
/// project always shows the same colour and different projects stay
/// distinguishable in the session list.
const PROJECT_BADGE_COLORS: &[Color] = palette::PROJECT_BADGE_COLORS;

/// Pick a stable badge colour for `project_id` from [`PROJECT_BADGE_COLORS`].
fn project_badge_color(project_id: uuid::Uuid) -> Color {
    let idx = (project_id.as_u128() % PROJECT_BADGE_COLORS.len() as u128) as usize;
    PROJECT_BADGE_COLORS[idx]
}

/// Blank columns between a row and the pane edge, on both sides (#2175).
///
/// The #2162 border removal took away the right padding the border used to
/// supply for free, and only the left gutter was put back, so every row ended
/// on the last column while the left had two blank columns. Both sides inset
/// by the same amount now.
///
/// Derived from [`responsive::EDGE_INSET`] rather than written out again, so
/// the rows and the footer below them cannot drift apart: they read one
/// number.
const GUTTER: usize = responsive::EDGE_INSET as usize;

/// A session name never gets narrower than this, so the list stays readable
/// even when every badge has a value.
const NAME_MIN: usize = 24;

/// Render the sessions list.
///
/// Content fills everything above the last two rows; the second to last is a
/// dim rule and the last is the shared command footer. Without the rule the
/// footer sat flush under the final session and read as one more row, which
/// is what made the screen hard to parse. Nothing else is drawn, which is
/// where the three rows the bordered version spent on chrome now go.
pub(super) fn render_sessions(f: &mut Frame, app: &App, area: Rect) {
    let [content_area, rule_area, footer_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // The group-topics picker takes the whole screen while it is open: the
    // list behind it cannot be navigated and a half-covered picker reads as a
    // broken list (#2701).
    if let Some(pick) = &app.group_topics {
        render_group_topics(f, pick, area);
        return;
    }

    // One source of truth for the visible rows: the same search + plan chip +
    // automation rule the input handler navigates and selects through (#2723).
    let visible_indices = app.visible_session_indices();

    let mut lines: Vec<Line<'static>> = Vec::new();

    // A pane that was just split is waiting for a session: state, one row.
    let has_unassigned = app
        .pane_manager
        .focused_pane()
        .is_some_and(|p| p.session_id.is_none());
    if has_unassigned {
        lines.push(Line::from(Span::styled(
            "  Select a session for the new pane (or n for new)",
            Style::default().fg(theme::role(Role::Success)),
        )));
    }

    // Assign mode banner, one row.
    if let Some(project_id) = app.assigning_to_project {
        let project_name = app
            .project_name_cache
            .get(&project_id)
            .map(|s| s.as_str())
            .unwrap_or("project");
        lines.push(Line::from(vec![
            Span::styled(
                format!("  Assigning to {project_name}"),
                Style::default()
                    .fg(theme::role(Role::Warning))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  Enter: assign  Esc: done",
                Style::default().fg(theme::role(Role::TextDim)),
            ),
        ]));
    }

    // Delete confirmation (#2701): the row action asks, and this is the ask.
    if let Some(confirm) = &app.delete_confirm {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  Delete \"{}\"?", confirm.title),
                Style::default()
                    .fg(theme::role(Role::Error))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  y: delete  n: cancel",
                Style::default().fg(theme::role(Role::TextDim)),
            ),
        ]));
    }

    // Search editor, only while the user is typing a filter.
    if app.session_search_active {
        lines.push(Line::from(vec![
            Span::styled("  🔍 ", Style::default().fg(theme::role(Role::AccentTeal))),
            Span::styled(
                format!("{}▏", app.session_search),
                Style::default()
                    .fg(theme::role(Role::TextPrimary))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {} matches", visible_indices.len()),
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
        ]));
    }

    // Filter chip header, only while the browsing filter narrows the list and
    // no search is overriding it (#2723). An explicit search spans every
    // session (A1), so the chip would mislead there; the default list keeps
    // its clean look because the chip stays hidden until a key moves it.
    if app.session_filter.is_active() && app.session_search.is_empty() {
        let automation = if app.session_filter.show_automation {
            "automation shown"
        } else {
            "automation hidden"
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  ▾ {} ", app.session_filter.plan.label()),
                Style::default()
                    .fg(theme::role(Role::AccentTeal))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {automation}  ·  s: plan  a: automation"),
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
        ]));
    }

    // Banners above the list: the scroll math below is measured from the top
    // of the paragraph, so the selected card's offset includes these.
    let header_line_count = lines.len();

    if visible_indices.is_empty() && !app.session_search.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching sessions",
            Style::default().fg(theme::role(Role::GrayDim)),
        )));
    }

    // One plan for the whole frame, so a column's width can no longer depend
    // on which row is being drawn.
    let visible: Vec<&Session> = visible_indices
        .iter()
        .map(|&idx| &app.sessions[idx])
        .collect();
    let plan = row_plan(app, &visible, content_area.width);

    // One entry per visible session, each holding the 1-3 lines its card
    // draws. The scroll math below walks these heights, so it can never
    // disagree with the drawing about where a card starts.
    let mut rows: Vec<Vec<Line<'static>>> = Vec::with_capacity(visible_indices.len());
    for (display_idx, &session_idx) in visible_indices.iter().enumerate() {
        let session = &app.sessions[session_idx];
        let is_selected = display_idx == app.selected_session_index;
        let is_current = app
            .current_session
            .as_ref()
            .map(|s| s.id == session.id)
            .unwrap_or(false);

        if is_selected && app.session_renaming {
            rows.push(vec![rename_row(app, &plan)]);
            continue;
        }
        if is_selected && app.topic_naming {
            rows.push(vec![topic_name_row(app, &plan)]);
            continue;
        }

        rows.push(session_card_rows(
            app,
            session,
            is_selected,
            is_current,
            &plan,
        ));
    }

    // Viewport scroll: keep the WHOLE selected card visible, so a selected
    // card's preview and plan bar are never the part that falls off the top.
    let visible_height = (content_area.height as usize).max(1);
    let selected_row = app.selected_session_index.min(rows.len().saturating_sub(1));
    let selected_start =
        header_line_count + rows.iter().take(selected_row).map(Vec::len).sum::<usize>();
    let selected_end = selected_start + rows.get(selected_row).map_or(1, Vec::len);
    let scroll = selected_end.saturating_sub(visible_height) as u16;

    lines.extend(rows.into_iter().flatten());

    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), content_area);

    hints::render_separator(f, rule_area);
    hints::render_footer_with_context(
        f,
        footer_area,
        dialog_keys(DialogScope::Sessions),
        &format!("{} sessions", visible_indices.len()),
    );
}

/// The group-topics picker, full screen (#2701).
///
/// Same shape as the window's multi-select: the target named at the top, one
/// row per candidate with a mark, Space toggles, Enter attaches. A guard
/// refusal prints on the header instead of closing the picker, so the user
/// keeps their marks and can drop the offending row.
fn render_group_topics(
    f: &mut Frame,
    pick: &crate::tui::app::session_cards::GroupTopicsPick,
    area: Rect,
) {
    let [content_area, rule_area, footer_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(vec![
            Span::styled(
                "  Group topics under ",
                Style::default().fg(theme::role(Role::TextDim)),
            ),
            Span::styled(
                pick.parent_title.clone(),
                Style::default()
                    .fg(theme::role(Role::Accent))
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            "  Space marks, Enter attaches, Esc cancels",
            Style::default().fg(theme::role(Role::GrayDim)),
        )),
    ];
    if let Some(error) = &pick.error {
        lines.push(Line::from(Span::styled(
            format!("  {error}"),
            Style::default().fg(theme::role(Role::Error)),
        )));
    }

    // Keep the cursor on screen on a long list, and never scroll past the
    // end: the window starts at the cursor once it cannot all fit.
    let room = (content_area.height as usize)
        .saturating_sub(lines.len())
        .max(1);
    let first = pick.index.saturating_add(1).saturating_sub(room);

    for (offset, candidate) in pick.candidates.iter().enumerate().skip(first).take(room) {
        let is_cursor = offset == pick.index;
        let mark = if candidate.selected { "[x]" } else { "[ ]" };
        let style = if is_cursor {
            Style::default()
                .fg(theme::role(Role::Accent))
                .add_modifier(Modifier::BOLD)
        } else if candidate.selected {
            Style::default().fg(theme::role(Role::Success))
        } else {
            Style::default().fg(theme::role(Role::TextPrimary))
        };
        let mut spans = vec![
            Span::raw(" ".repeat(GUTTER)),
            Span::styled(format!("{mark} "), style),
            Span::styled(
                truncate_to_width(&candidate.title, pick_title_room(content_area.width)),
                style,
            ),
        ];
        if is_cursor {
            let bg = theme::role(Role::SelectionBg);
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            if used < content_area.width as usize {
                spans.push(Span::styled(
                    " ".repeat(content_area.width as usize - used),
                    Style::default().bg(bg),
                ));
            }
            spans = spans
                .into_iter()
                .map(|s| Span::styled(s.content, s.style.bg(bg)))
                .collect();
        }
        lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(lines), content_area);
    hints::render_separator(f, rule_area);
    hints::render_footer_with_context(
        f,
        footer_area,
        &[
            crate::tui::app::dialog_keys::DialogKey {
                events: &[],
                label: "Space",
                verb: "mark",
            },
            crate::tui::app::dialog_keys::DialogKey {
                events: &[],
                label: "Enter",
                verb: "attach",
            },
            crate::tui::app::dialog_keys::DialogKey {
                events: &[],
                label: "Esc",
                verb: "cancel",
            },
        ],
        &format!("{} marked", pick.selected_ids().len()),
    );
}

/// Room for a candidate's title on the picker row, after both gutters and the
/// four-cell mark.
fn pick_title_room(width: u16) -> usize {
    (width as usize).saturating_sub(GUTTER * 2 + 5).max(1)
}

/// A right-hand column of the session list.
///
/// The right half used to be built row by row and drawn as one right-aligned
/// blob. Columns make it a table: every row draws the same columns in the
/// same order, and a missing value is blank space rather than a shift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Date,
    Status,
    Model,
    Branch,
    Size,
    Project,
    Ctx,
    Wd,
}

impl Column {
    /// Allocation order. A column is kept only when every column before it
    /// already fits, so the visible set is always a prefix of this list and
    /// never a ragged subset that differs row to row.
    const PRIORITY: [Column; 8] = [
        Column::Date,
        Column::Status,
        Column::Model,
        Column::Branch,
        Column::Size,
        Column::Project,
        Column::Ctx,
        Column::Wd,
    ];

    /// Drawing order, left to right. Deliberately different from
    /// [`Self::PRIORITY`]: priority says what survives a narrow terminal,
    /// this says where it sits. It keeps the order the badges had before
    /// columns existed, with status last because it is a state marker.
    const DRAW: [Column; 8] = [
        Column::Date,
        Column::Size,
        Column::Ctx,
        Column::Model,
        Column::Project,
        Column::Wd,
        Column::Branch,
        Column::Status,
    ];

    /// Lowest tier at which this column is a candidate, as a rank.
    ///
    /// The tier still decides which data the screen shows, exactly as it did
    /// before columns existed; the width budget only trims that set further.
    fn first_tier(self) -> u8 {
        match self {
            Column::Date | Column::Status => 0,
            Column::Size => 1,
            Column::Model | Column::Ctx => 2,
            Column::Project | Column::Wd | Column::Branch => 3,
        }
    }

    /// Widest this column is ever drawn.
    ///
    /// A value longer than the cap is ellipsised, so a deep working directory
    /// or a long branch name cannot shove its neighbours off the row. Two are
    /// wider than the common value needs: the project badge carries a
    /// `{.name}` wrapper, and status is a composite of up to four indicators
    /// that can all be on at once (activity, detached commands, the current
    /// mark, and the pane index).
    fn cap(self, tier: SessionRowTier) -> usize {
        match self {
            Column::Date if tier == SessionRowTier::Narrow => 6,
            // Timestamp plus ` $12.34`: a card that has run up a bill shows it
            // without ellipsising the date it belongs to.
            Column::Date => 26,
            Column::Status => 18,
            Column::Model => 34,
            Column::Branch => 10,
            Column::Size => 13,
            Column::Project => 20,
            Column::Ctx => 18,
            Column::Wd => 40,
        }
    }

    /// The cells this column draws for one session, empty when the row has no
    /// value for it.
    fn cell(
        self,
        app: &App,
        session: &Session,
        is_current: bool,
        tier: SessionRowTier,
        background: Option<&str>,
    ) -> Vec<Span<'static>> {
        let dim = Style::default().fg(theme::role(Role::GrayDim));
        let mid = Style::default().fg(theme::role(Role::GrayMid));
        let gray = Style::default().fg(theme::role(Role::Gray));

        match self {
            // Show LAST ACTIVITY, not creation. The list is ordered by
            // `updated_at DESC` (`list_sessions`), so printing `created_at`
            // made the rows look mis-sorted: a session last used minutes ago
            // but opened in March rendered "2026-03-26" above one created
            // yesterday.
            Column::Date => {
                if tier == SessionRowTier::Narrow {
                    return vec![Span::styled(
                        format!(" {}", session.updated_at.format("%H:%M")),
                        dim,
                    )];
                }
                // The card's cost rides on this column (#2701): the row already
                // answers "when", so "what it cost" belongs beside it rather
                // than in a column of its own that a narrow terminal drops.
                let mut spans = vec![Span::styled(
                    format!(" {}", session.updated_at.format("%Y-%m-%d %H:%M")),
                    dim,
                )];
                if session.total_cost > 0.0 {
                    spans.push(Span::styled(
                        format!(" ${:.2}", session.total_cost),
                        Style::default().fg(theme::role(Role::Warning)),
                    ));
                }
                spans
            }
            Column::Size => {
                if session.token_count <= 0 {
                    return Vec::new();
                }
                vec![Span::styled(
                    format!(
                        "  {}",
                        format_token_count_with_label(session.token_count, "total")
                    ),
                    mid,
                )]
            }
            Column::Ctx => {
                let Some(input_tok) = app.last_input_tokens.filter(|_| is_current) else {
                    return Vec::new();
                };
                let pct = app.context_usage_percent();
                let color = if pct > 80.0 {
                    theme::role(Role::Error)
                } else if pct > 50.0 {
                    theme::role(Role::Accent)
                } else {
                    theme::role(Role::AccentTeal)
                };
                vec![Span::styled(
                    format!(
                        "  ctx {}/{} {:.0}%",
                        format_token_count_raw(input_tok as i64),
                        format_token_count_raw(app.context_max_tokens as i64),
                        pct
                    ),
                    Style::default().fg(color),
                )]
            }
            Column::Model => match session.provider_name.as_ref() {
                Some(prov) => {
                    let model = session.model.as_deref().unwrap_or("default");
                    vec![Span::styled(format!("  [{prov}/{model}]"), gray)]
                }
                None => Vec::new(),
            },
            Column::Project => match session.project_id {
                Some(pid) => {
                    let project_name = app
                        .project_name_cache
                        .get(&pid)
                        .map(|s| s.as_str())
                        .unwrap_or("?");
                    vec![Span::styled(
                        format!("  {{.{project_name}}}"),
                        Style::default().fg(project_badge_color(pid)),
                    )]
                }
                None => Vec::new(),
            },
            Column::Wd => match session.working_directory.as_ref() {
                Some(wd) => {
                    let home_dir = dirs::home_dir()
                        .map(|h| h.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let short = if !home_dir.is_empty() && wd.starts_with(&home_dir) {
                        format!("~{}", &wd[home_dir.len()..])
                    } else {
                        wd.clone()
                    };
                    vec![Span::styled(
                        format!("  {short}"),
                        Style::default().fg(theme::role(Role::BlueSteel)),
                    )]
                }
                None => Vec::new(),
            },
            Column::Branch => match session.working_directory.as_ref() {
                Some(wd) => {
                    match crate::utils::git_branch::current_branch(std::path::Path::new(wd)) {
                        Some(branch) => vec![Span::styled(
                            format!(" ({branch})"),
                            Style::default().fg(theme::role(Role::AccentTeal)),
                        )],
                        None => Vec::new(),
                    }
                }
                None => Vec::new(),
            },
            // Status is state, not decoration, so it is kept at every width
            // and carries every indicator that applies.
            Column::Status => {
                let mut spans: Vec<Span<'static>> = Vec::new();
                if app.processing_sessions.contains(&session.id) {
                    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                    let frame = app.animation_frame % SPINNER.len();
                    spans.push(Span::styled(
                        format!(" {}", SPINNER[frame]),
                        Style::default().fg(theme::role(Role::Accent)),
                    ));
                } else if app.sessions_with_pending_approval.contains(&session.id) {
                    spans.push(Span::styled(
                        " !",
                        Style::default()
                            .fg(theme::role(Role::Accent))
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if app.sessions_with_unread.contains(&session.id) {
                    spans.push(Span::styled(
                        " ●",
                        Style::default().fg(theme::role(Role::AccentTeal)),
                    ));
                }
                // Detached commands outlive the turn that spawned them (#762),
                // so the row keeps saying "work is still going here" when no
                // spinner can: the elapsed time is the part that answers "is
                // it stuck", and the count says how many are behind it. The
                // badge arrives ready-made from the frame's RowPlan, so this
                // cell never touches the task manager.
                if let Some(badge) = background {
                    spans.push(Span::styled(
                        format!(" {badge}"),
                        Style::default().fg(theme::role(Role::Warning)),
                    ));
                }
                // Live in-flight activity (#2722): the tool running now, else
                // thinking/responding, worded like the GUI card's live lead.
                // It rides inside this column and `fit_cell` truncates it, so
                // a session stays exactly one row (A3). The spinner above is
                // still the state marker; this is what the turn is doing.
                if let Some(activity) = session_activity(app, session.id) {
                    spans.push(Span::styled(
                        format!(" {activity}"),
                        Style::default().fg(theme::role(Role::GrayMid)),
                    ));
                }
                // A plan waiting for a hand (#2701): the rail rings for this,
                // the row shows the same mark so a waiting plan is visible
                // from the list itself.
                if app.session_plan_ready.contains(&session.id) {
                    spans.push(Span::styled(
                        " \u{2691}",
                        Style::default()
                            .fg(theme::role(Role::Warning))
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                if is_current {
                    spans.push(Span::styled(
                        " *",
                        Style::default()
                            .fg(theme::role(Role::Gray))
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                // Pane indicator: which pane this session already occupies.
                if app.pane_manager.is_split() {
                    let pane_ids = app.pane_manager.pane_ids_in_order();
                    if let Some(pos) = pane_ids.iter().position(|pid| {
                        app.pane_manager
                            .get(*pid)
                            .is_some_and(|p| p.session_id == Some(session.id))
                    }) {
                        spans.push(Span::styled(
                            format!(" [pane {}]", pos + 1),
                            Style::default().fg(theme::role(Role::Success)),
                        ));
                    }
                }
                spans
            }
        }
    }
}

/// The in-flight activity phrase for a session row (#2722), worded like the
/// GUI card's live lead: the tool description when one is running, else
/// `thinking…` while reasoning streams, else `responding…` while text streams.
/// `None` when there is nothing to say. Pure so the precedence is testable
/// without an `App`; the Status column truncates whatever this returns.
fn activity_text(
    tool_description: Option<&str>,
    thinking: bool,
    responding: bool,
) -> Option<String> {
    if let Some(desc) = tool_description.map(str::trim).filter(|d| !d.is_empty()) {
        return Some(desc.to_string());
    }
    if thinking {
        return Some("thinking…".to_string());
    }
    if responding {
        return Some("responding…".to_string());
    }
    None
}

/// Gather one session's live activity from the foreground fields (when it is
/// the focused session) or its background sidecar, then word it. Only a
/// processing session has anything to show; a turn that has started but not
/// yet streamed reads as `thinking…`, the same feedback the GUI card gives
/// during the first tool calls. One row per session is preserved (A3): the
/// caller paints this inside the Status column and truncates it.
fn session_activity(app: &App, session_id: uuid::Uuid) -> Option<String> {
    if !app.is_session_processing(session_id) {
        return None;
    }
    let (tool_group, reasoning, response) = if app.is_current_session(session_id) {
        (
            app.active_tool_group.as_ref(),
            app.streaming_reasoning.as_deref(),
            app.streaming_response.as_deref(),
        )
    } else if let Some(bg) = app.background_sessions.get(&session_id) {
        (
            bg.active_tool_group.as_ref(),
            bg.streaming_reasoning.as_deref(),
            bg.streaming_response.as_deref(),
        )
    } else {
        return Some("thinking…".to_string());
    };
    // The call running now, else the last one that ran, mirroring the GUI's
    // `live_activity`. A completed-only group still names what just happened.
    let tool_description = tool_group.and_then(|g| {
        g.calls
            .iter()
            .rev()
            .find(|c| !c.completed)
            .or_else(|| g.calls.last())
            .map(|c| c.description.as_str())
    });
    let thinking = reasoning.is_some_and(|r| !r.trim().is_empty());
    let responding = response.is_some_and(|r| !r.trim().is_empty());
    Some(
        activity_text(tool_description, thinking, responding).unwrap_or_else(|| "thinking…".into()),
    )
}

/// One frame's column layout, computed once and shared by every row.
#[derive(Debug, Clone)]
struct RowPlan {
    /// Full width the rows are drawn in.
    width: usize,
    /// Room for the name, after both gutters and the chosen columns.
    name_width: usize,
    /// Chosen columns and their fixed widths, in drawing order.
    columns: Vec<(Column, usize)>,
    /// The badge tier this frame resolved to.
    tier: SessionRowTier,
    /// Detached commands still running per session, resolved ONCE for the
    /// frame: the status column reads this instead of taking the manager's
    /// lock again for every row, twice (natural width, then paint).
    background: std::collections::HashMap<uuid::Uuid, String>,
}

/// Rank a tier so a column can ask "is this tier high enough?".
fn tier_rank(tier: SessionRowTier) -> u8 {
    match tier {
        SessionRowTier::Narrow => 0,
        SessionRowTier::Medium => 1,
        SessionRowTier::Wide => 2,
        SessionRowTier::ExtraWide => 3,
    }
}

/// Display width of a cell built from styled spans.
fn cell_width(cell: &[Span<'static>]) -> usize {
    cell.iter().map(|s| s.content.width()).sum()
}

/// Work out the frame's columns from the widest value each one has to hold.
///
/// Called once per frame with every visible session, so a column's width is
/// the same for all of them. That is the whole fix: before this, the width of
/// the right half depended on which row was being drawn, and the date slid up
/// to 101 columns between neighbours.
fn row_plan(app: &App, sessions: &[&Session], width: u16) -> RowPlan {
    let width = width as usize;
    let tier = responsive::session_row_tier(width as u16);
    let rank = tier_rank(tier);

    // Detached commands, resolved once for the whole visible list: one lock
    // for the frame instead of two per row inside the status cell.
    let background: std::collections::HashMap<uuid::Uuid, String> = app
        .agent_service
        .background_manager()
        .map(|mgr| {
            sessions
                .iter()
                .filter_map(|s| {
                    let tasks: Vec<(String, u64)> = mgr
                        .running_tasks(s.id)
                        .into_iter()
                        .map(|t| (t.label, t.started.elapsed().as_secs()))
                        .collect();
                    super::background::row_background_badge(&tasks).map(|badge| (s.id, badge))
                })
                .collect()
        })
        .unwrap_or_default();

    // Natural width per column, capped so one runaway value cannot claim the
    // frame. Zero means no visible row has a value for that column.
    let mut natural = [0usize; Column::PRIORITY.len()];
    for session in sessions {
        let is_current = app
            .current_session
            .as_ref()
            .is_some_and(|s| s.id == session.id);
        let badge = background.get(&session.id).map(String::as_str);
        for (idx, column) in Column::PRIORITY.iter().enumerate() {
            if column.first_tier() > rank {
                continue;
            }
            let cell = column.cell(app, session, is_current, tier, badge);
            natural[idx] = natural[idx].max(cell_width(&cell).min(column.cap(tier)));
        }
    }

    // Allocate in priority order and stop at the first column that does not
    // fit, so the kept set is a prefix: a wide terminal shows everything a
    // narrow one shows plus more, never a different mix. A column with no
    // value anywhere is skipped rather than ending the run.
    let mut columns: Vec<(Column, usize)> = Vec::new();
    let mut used = GUTTER * 2 + NAME_MIN;
    for (idx, column) in Column::PRIORITY.iter().enumerate() {
        let column_width = natural[idx];
        if column_width == 0 {
            continue;
        }
        if used + column_width > width {
            break;
        }
        used += column_width;
        columns.push((*column, column_width));
    }
    columns.sort_by_key(|(column, _)| {
        Column::DRAW
            .iter()
            .position(|c| c == column)
            .unwrap_or(usize::MAX)
    });

    let right_total: usize = columns.iter().map(|(_, w)| w).sum();
    let name_width = width.saturating_sub(GUTTER * 2 + right_total).max(1);

    RowPlan {
        width,
        name_width,
        columns,
        tier,
        background,
    }
}

/// Trim a cell to its column width, ellipsising rather than cutting a glyph.
///
/// Only a value that exceeds its column's cap reaches here, so the ellipsis is
/// the signal that a working directory or a branch name was too long for the
/// frame. Nothing is ever cut mid-token without it.
fn fit_cell(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let span_width = span.content.width();
        if used + span_width <= width {
            used += span_width;
            out.push(span);
            continue;
        }
        let room = width.saturating_sub(used);
        if room > 0 {
            let text = truncate_to_width(&span.content, room);
            out.push(Span::styled(text, span.style));
        }
        break;
    }
    out
}

/// One session's card: the name line, then its plan bar and preview when it
/// has them (#2701).
///
/// The rail card in the window carries the same three facts. The row keeps the
/// single-column list the owner asked for: the extras hang under the name
/// instead of widening the row, and a session with neither costs no extra
/// line at all.
fn session_card_rows(
    app: &App,
    session: &Session,
    is_selected: bool,
    is_current: bool,
    plan: &RowPlan,
) -> Vec<Line<'static>> {
    let mut rows = vec![session_row(app, session, is_selected, is_current, plan)];

    // Only a live plan with tasks draws a bar, so a session whose plan is
    // finished or empty stays a one-line row.
    if let Some(&(done, total)) = app.session_plan_progress.get(&session.id) {
        rows.push(plan_row(app, session, done, total, plan, is_selected));
    }

    if let Some(preview) = app.session_previews.get(&session.id) {
        rows.push(preview_row(preview, plan, is_selected));
    }

    rows
}

/// The plan bar under a session's name: `██████░░░░ 6/10 60%`.
///
/// The bar itself comes from `ui_core`, the same function the window's rail
/// strip draws, so both surfaces show identical progress for the same plan.
fn plan_row(
    app: &App,
    session: &Session,
    done: usize,
    total: usize,
    plan: &RowPlan,
    is_selected: bool,
) -> Line<'static> {
    let (percent, bar) = crate::ui_core::plan_index::progress_bar(done, total);
    let mut spans = vec![
        Span::raw(" ".repeat(GUTTER + 2)),
        Span::styled(bar, Style::default().fg(theme::role(Role::Accent))),
        Span::styled(
            format!(" {done}/{total} {percent}%"),
            Style::default().fg(theme::role(Role::GrayMid)),
        ),
    ];
    if app.session_plan_ready.contains(&session.id) {
        spans.push(Span::styled(
            "  plan ready",
            Style::default()
                .fg(theme::role(Role::Warning))
                .add_modifier(Modifier::BOLD),
        ));
    }
    finish_row(spans, plan.width, is_selected)
}

/// The newest message under a session's name, worded by `ui_core` so the row
/// and the rail card say the same thing about the same session.
fn preview_row(
    preview: &crate::tui::app::session_cards::SessionPreview,
    plan: &RowPlan,
    is_selected: bool,
) -> Line<'static> {
    let room = plan.width.saturating_sub(GUTTER * 2 + 2).max(1);
    let spans = vec![
        Span::raw(" ".repeat(GUTTER + 2)),
        Span::styled(
            truncate_to_width(&preview.line(), room),
            Style::default().fg(theme::role(Role::GrayDim)),
        ),
    ];
    finish_row(spans, plan.width, is_selected)
}

/// One session, exactly one terminal row.
///
/// Drawn from the frame's [`RowPlan`]: the name is padded to the plan's name
/// width and each chosen column is right-aligned inside its fixed width, so
/// every row's date and badges land on the same columns. A row with no value
/// for a column draws blanks there instead of sliding everything to its left.
fn session_row(
    app: &App,
    session: &Session,
    is_selected: bool,
    is_current: bool,
    plan: &RowPlan,
) -> Line<'static> {
    // Green marks a session already assigned to the project being assigned.
    let is_assigned_to_target = app
        .assigning_to_project
        .is_some_and(|pid| session.project_id == Some(pid));
    let name_style = if is_selected {
        Style::default()
            .fg(theme::role(Role::Accent))
            .add_modifier(Modifier::BOLD)
    } else if is_assigned_to_target {
        Style::default()
            .fg(theme::role(Role::Success))
            .add_modifier(Modifier::BOLD)
    } else if app.closed_sessions.contains(&session.id) {
        // A closed topic reads dimmed: still there, no longer taking work (#2305).
        Style::default().fg(theme::role(Role::TextDim))
    } else if is_current {
        Style::default().fg(theme::role(Role::Gray))
    } else {
        Style::default().fg(theme::role(Role::TextPrimary))
    };

    // Left gutter, then the name padded to its column, then each column
    // right-aligned inside its fixed width, then the right gutter.
    // A pinned session carries a star inside the name cell, so the columns
    // to its right stay put: the row plan is identical for every row and a
    // star only costs the name two cells of its own width.
    // A topic shown under its parent carries a tree mark, so the grouping reads
    // in the dialog (#2306).
    let is_topic = app
        .topic_parents
        .get(&session.id)
        .is_some_and(|p| app.sessions.iter().any(|s| s.id == *p));
    // The topic store outranks the stored session name (#2490): a Telegram
    // rename reaches this row on the next list load, no restart.
    let raw_title = crate::ui_core::rail::rail_title(
        session.title.as_deref(),
        app.topic_titles.get(&session.id).map(String::as_str),
    );
    let raw_title = raw_title.as_str();
    // A session with topics carries a chevron that says whether its tree is
    // folded: ▸ folded, ▾ open (#2427).
    let has_topics = app.topic_parents.values().any(|p| *p == session.id);
    let fold_mark = match (has_topics, app.collapsed_sessions.contains(&session.id)) {
        (true, true) => "▸ ",
        (true, false) => "▾ ",
        _ => "",
    };
    let title_owned = if is_topic {
        format!("  └ {raw_title}")
    } else {
        format!("{fold_mark}{raw_title}")
    };
    let title = title_owned.as_str();
    // A deep search hit shows why it matched: the snippet rides after the
    // title in the same name cell, so a hit never costs an extra row (#2724).
    // Mirrors the GUI rail, where a content match's snippet stands in for the
    // preview (#2720).
    let search_title = if !app.session_search.trim().is_empty() {
        app.session_content_matches
            .iter()
            .find(|r| r.session_id == session.id)
            .map(|r| format!("{title} · {}", r.snippet))
    } else {
        None
    };
    let title = search_title.as_deref().unwrap_or(title);
    let name = if app.session_favorites.contains(&session.id) {
        truncate_to_width(&format!("★ {title}"), plan.name_width)
    } else {
        truncate_to_width(title, plan.name_width)
    };
    let mut spans = vec![
        Span::raw(" ".repeat(GUTTER)),
        Span::styled(name.clone(), name_style),
    ];
    let pad = plan.name_width.saturating_sub(name.width());
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    for (column, column_width) in &plan.columns {
        let badge = plan.background.get(&session.id).map(String::as_str);
        let cell = fit_cell(
            column.cell(app, session, is_current, plan.tier, badge),
            *column_width,
        );
        let pad = column_width.saturating_sub(cell_width(&cell));
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        spans.extend(cell);
    }
    spans.push(Span::raw(" ".repeat(GUTTER)));

    finish_row(spans, plan.width, is_selected)
}

/// The rename editor, on the selected row, one row high.
///
/// Padded to the plan's name width so the editor sits exactly where the names
/// it replaces sit.
/// The prompt row for a new topic under the selected session (#2306).
fn topic_name_row(app: &App, plan: &RowPlan) -> Line<'static> {
    let text = truncate_to_width(
        &format!("  └ new topic: {}▏", app.topic_name_buffer),
        plan.name_width,
    );
    let spans = vec![
        Span::raw(" ".repeat(GUTTER)),
        Span::styled(
            text,
            Style::default()
                .fg(theme::role(Role::TextPrimary))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    finish_row(spans, plan.width, true)
}

fn rename_row(app: &App, plan: &RowPlan) -> Line<'static> {
    let text = truncate_to_width(&format!("{}▏", app.session_rename_buffer), plan.name_width);
    let spans = vec![
        Span::raw(" ".repeat(GUTTER)),
        Span::styled(
            text,
            Style::default()
                .fg(theme::role(Role::TextPrimary))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    finish_row(spans, plan.width, true)
}

/// Give the selected row a full-width selection bar.
///
/// The background has to reach the last column, so the row is padded to
/// `width` first: a bar that stops at the end of the text reads as a
/// highlight of the text rather than of the row.
fn finish_row(mut spans: Vec<Span<'static>>, width: usize, is_selected: bool) -> Line<'static> {
    if !is_selected {
        return Line::from(spans);
    }
    let bg = theme::role(Role::SelectionBg);
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(bg),
        ));
    }
    Line::from(
        spans
            .into_iter()
            .map(|s| Span::styled(s.content, s.style.bg(bg)))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::agent::service::AgentService;
    use crate::db::Database;
    use crate::services::ServiceContext;
    use crate::tests::agent_service_mocks::MockProvider;
    use crate::tui::app::state::{ToolCallEntry, ToolCallGroup};
    use crate::tui::pane::PaneManager;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use std::sync::Arc;

    // ── Pure precedence ──────────────────────────────────────────────────

    #[test]
    fn activity_running_tool_wins_over_thinking_and_responding() {
        assert_eq!(
            activity_text(Some("Reading src/main.rs"), true, true).as_deref(),
            Some("Reading src/main.rs")
        );
    }

    #[test]
    fn activity_thinking_before_responding() {
        assert_eq!(
            activity_text(None, true, true).as_deref(),
            Some("thinking…")
        );
        assert_eq!(
            activity_text(None, true, false).as_deref(),
            Some("thinking…")
        );
    }

    #[test]
    fn activity_responding_when_only_text_streams() {
        assert_eq!(
            activity_text(None, false, true).as_deref(),
            Some("responding…")
        );
    }

    #[test]
    fn activity_blank_description_falls_through() {
        assert_eq!(
            activity_text(Some("   "), false, true).as_deref(),
            Some("responding…")
        );
        assert_eq!(activity_text(Some(""), false, false), None);
    }

    #[test]
    fn activity_idle_is_none() {
        assert_eq!(activity_text(None, false, false), None);
    }

    // ── Rendered Status column (TestBackend) ─────────────────────────────

    async fn app() -> App {
        let db = Database::connect_in_memory().await.unwrap();
        db.run_migrations().await.unwrap();
        let context = ServiceContext::new(db.pool().clone());
        let provider: Arc<dyn crate::brain::provider::Provider> = Arc::new(MockProvider);
        let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
        #[cfg(feature = "whatsapp")]
        let mut app = App::new(
            service,
            context,
            Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
        );
        #[cfg(not(feature = "whatsapp"))]
        let mut app = App::new(service, context);
        app.pane_manager = PaneManager::new();
        app
    }

    fn session(title: &str) -> Session {
        let now = chrono::Utc::now();
        Session {
            id: uuid::Uuid::new_v4(),
            title: Some(title.to_string()),
            model: None,
            provider_name: None,
            created_at: now,
            updated_at: now,
            archived_at: None,
            token_count: 0,
            total_cost: 0.0,
            working_directory: None,
            auto_title_attempted: false,
            project_id: None,
            channel_chat_key: None,
        }
    }

    fn tool_group(description: &str, completed: bool) -> ToolCallGroup {
        ToolCallGroup {
            calls: vec![ToolCallEntry {
                description: description.to_string(),
                success: false,
                details: None,
                completed,
                tool_input: serde_json::Value::Null,
                task_id: None,
            }],
            expanded: false,
        }
    }

    fn draw(app: &App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| render_sessions(f, app, Rect::new(0, 0, width, height)))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    fn screen_text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| row_text(buffer, y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Count the rows a needle appears on, to prove one row per session (A3).
    fn rows_containing(buffer: &Buffer, needle: &str) -> usize {
        (0..buffer.area.height)
            .filter(|&y| row_text(buffer, y).contains(needle))
            .count()
    }

    #[tokio::test]
    async fn status_column_shows_the_running_tool_description() {
        let mut app = app().await;
        let s = session("Alpha session");
        app.current_session = Some(s.clone());
        app.processing_sessions.insert(s.id);
        app.active_tool_group = Some(tool_group("Reading src/main.rs", false));
        app.sessions.push(s);

        let buf = draw(&app, 120, 10);
        let text = screen_text(&buf);
        assert!(text.contains("Reading"), "activity text missing: {text}");
        assert!(text.contains("Alpha session"), "title missing: {text}");
    }

    #[tokio::test]
    async fn processing_session_with_no_activity_reads_thinking() {
        let mut app = app().await;
        let s = session("Beta session");
        app.current_session = Some(s.clone());
        app.processing_sessions.insert(s.id);
        app.sessions.push(s);

        let text = screen_text(&draw(&app, 120, 10));
        assert!(
            text.contains("thinking"),
            "expected thinking fallback: {text}"
        );
    }

    #[tokio::test]
    async fn idle_session_has_no_activity_text() {
        let mut app = app().await;
        let s = session("Gamma session");
        app.current_session = Some(s.clone());
        app.sessions.push(s);

        let text = screen_text(&draw(&app, 120, 10));
        assert!(text.contains("Gamma session"), "title missing: {text}");
        assert!(
            !text.contains("thinking"),
            "idle row leaked thinking: {text}"
        );
        assert!(
            !text.contains("responding"),
            "idle row leaked responding: {text}"
        );
    }

    #[tokio::test]
    async fn background_session_activity_is_rendered() {
        let mut app = app().await;
        let s = session("Delta session");
        // Not the focused session: its live state lives in the sidecar.
        app.current_session = None;
        app.processing_sessions.insert(s.id);
        let bg = crate::tui::app::background_session::BackgroundSessionState {
            is_processing: true,
            active_tool_group: Some(tool_group("Writing tests", false)),
            ..Default::default()
        };
        app.background_sessions.insert(s.id, bg);
        app.sessions.push(s);

        let text = screen_text(&draw(&app, 120, 10));
        assert!(
            text.contains("Writing"),
            "background activity missing: {text}"
        );
    }

    #[tokio::test]
    async fn one_row_per_session_is_preserved_with_activity() {
        let mut app = app().await;
        let busy = session("Busy session");
        let idle = session("Idle session");
        app.current_session = Some(busy.clone());
        app.processing_sessions.insert(busy.id);
        app.active_tool_group = Some(tool_group(
            "A very long tool description that must truncate",
            false,
        ));
        app.sessions.push(busy);
        app.sessions.push(idle);

        let buf = draw(&app, 120, 12);
        assert_eq!(
            rows_containing(&buf, "Busy session"),
            1,
            "busy session wrapped"
        );
        assert_eq!(
            rows_containing(&buf, "Idle session"),
            1,
            "idle session wrapped"
        );
    }

    // ── Session-list filter (#2723) ──────────────────────────────────────

    use crate::tui::session_filter::{PlanChip, SessionFilter, session_filter_allows};
    use crate::utils::plan_files::PlanFilterState;

    #[test]
    fn plan_chip_cycles_through_every_state() {
        assert_eq!(PlanChip::All.next(), PlanChip::Wip);
        assert_eq!(PlanChip::Wip.next(), PlanChip::Finished);
        assert_eq!(PlanChip::Finished.next(), PlanChip::NoPlan);
        assert_eq!(PlanChip::NoPlan.next(), PlanChip::All);
    }

    #[test]
    fn filter_hides_cron_and_rsi_but_keeps_subagents_and_humans() {
        let f = SessionFilter::default();
        assert!(!f.show_automation, "automation must default to hidden (A1)");
        assert!(!session_filter_allows(
            &f,
            "Recap [cron-job:x]",
            PlanFilterState::None
        ));
        assert!(!session_filter_allows(
            &f,
            "RSI autonomous cycle",
            PlanFilterState::None
        ));
        assert!(session_filter_allows(
            &f,
            "subagent: worker",
            PlanFilterState::None
        ));
        assert!(session_filter_allows(
            &f,
            "Human chat",
            PlanFilterState::None
        ));
    }

    #[tokio::test]
    async fn visible_indices_hide_automation_by_default() {
        let mut app = app().await;
        let human = session("Planning the roadmap");
        let cron = session("Nightly recap [cron-job:x]");
        let rsi = session("RSI autonomous cycle");
        app.session_plan_states
            .insert(human.id, PlanFilterState::Wip);
        app.sessions = vec![human, cron, rsi];

        // Default: automation hidden, chip All -> only the human row.
        assert_eq!(app.visible_session_indices(), vec![0]);

        // Toggle automation on -> all three.
        app.session_filter.show_automation = true;
        assert_eq!(app.visible_session_indices(), vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn visible_indices_plan_chip_narrows_by_state() {
        let mut app = app().await;
        let wip = session("Wip chat");
        let done = session("Done chat");
        let none = session("No plan chat");
        app.session_plan_states.insert(wip.id, PlanFilterState::Wip);
        app.session_plan_states
            .insert(done.id, PlanFilterState::Finished);
        app.session_plan_states
            .insert(none.id, PlanFilterState::None);
        app.sessions = vec![wip, done, none];

        app.session_filter.plan = PlanChip::Wip;
        assert_eq!(app.visible_session_indices(), vec![0]);
        app.session_filter.plan = PlanChip::Finished;
        assert_eq!(app.visible_session_indices(), vec![1]);
        app.session_filter.plan = PlanChip::NoPlan;
        assert_eq!(app.visible_session_indices(), vec![2]);
        app.session_filter.plan = PlanChip::All;
        assert_eq!(app.visible_session_indices(), vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn visible_indices_search_spans_automation() {
        let mut app = app().await;
        let human = session("Roadmap");
        let cron = session("Roadmap recap [cron-job:x]");
        app.sessions = vec![human, cron];

        // Browsing hides the cron row.
        assert_eq!(app.visible_session_indices(), vec![0]);
        // An explicit search spans every session, automation included (A1).
        app.session_search = "roadmap".to_string();
        assert_eq!(app.visible_session_indices(), vec![0, 1]);
    }

    #[tokio::test]
    async fn selected_index_resolves_through_the_filtered_view() {
        let mut app = app().await;
        let cron = session("Recap [cron-job:x]");
        let human = session("Real chat");
        app.sessions = vec![cron, human];

        // Cron hidden by default: the only visible row is the human, which
        // sits at full-list index 1. The cursor (display position 0) must
        // resolve to 1, never open the hidden cron at index 0.
        app.selected_session_index = 0;
        assert_eq!(app.selected_session_actual_index(), Some(1));
    }

    #[tokio::test]
    async fn filter_chip_header_shows_only_when_active() {
        let mut app = app().await;
        app.sessions = vec![session("Human chat")];

        // Default filter: no chip header, the list keeps its clean look.
        let buf = draw(&app, 100, 10);
        assert_eq!(rows_containing(&buf, "automation hidden"), 0);

        // Move the chip: the header names the chip and the automation state.
        app.session_filter.plan = PlanChip::Wip;
        let buf = draw(&app, 100, 10);
        assert_eq!(rows_containing(&buf, "plan wip"), 1);
        assert_eq!(rows_containing(&buf, "automation hidden"), 1);
    }

    // ── Session-list keyword search over message content (#2724) ─────────

    use crate::db::repository::ContentSearchRow;
    use crate::tui::events::TuiEvent;
    use std::sync::atomic::Ordering::SeqCst;

    /// A deep-hit row for `sid`, as the shared content search would return it.
    fn content_row(sid: uuid::Uuid, snippet: &str) -> ContentSearchRow {
        ContentSearchRow {
            session_id: sid,
            title: "Untitled".to_string(),
            snippet: snippet.to_string(),
            match_count: 1,
            last_hit_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn session_search_content_only_hit_surfaces_and_renders_snippet() {
        let mut app = app().await;
        // The title never mentions the query; only a message body does.
        let s = session("Roadmap");
        app.sessions = vec![s.clone()];
        app.session_search = "rust".to_string();

        // No deep hit yet: the title misses, so nothing is visible.
        assert!(app.visible_session_indices().is_empty());

        // The content search lands a hit: the row surfaces and keeps one row
        // (A3), with the snippet riding after the title.
        app.session_content_matches = vec![content_row(s.id, "we use rust here")];
        assert_eq!(app.visible_session_indices(), vec![0]);
        let buf = draw(&app, 120, 10);
        let text = screen_text(&buf);
        assert!(text.contains("Roadmap"), "title missing: {text}");
        assert!(text.contains("rust"), "snippet missing: {text}");
        assert_eq!(rows_containing(&buf, "Roadmap"), 1, "hit wrapped (A3)");
    }

    #[tokio::test]
    async fn session_search_title_matches_lead_content_only_hits() {
        let mut app = app().await;
        let a = session("Rust notes"); // title match, ledger index 0
        let b = session("Beta"); // content-only, ledger index 1
        let c = session("Rust roadmap"); // title match, ledger index 2
        app.sessions = vec![a.clone(), b.clone(), c.clone()];
        app.session_search = "rust".to_string();
        app.session_content_matches = vec![content_row(b.id, "rust in beta")];

        // Title matches keep ledger order and lead; the content-only session
        // follows, mirroring the GUI rail's merge (#2720).
        assert_eq!(app.visible_session_indices(), vec![0, 2, 1]);
    }

    #[tokio::test]
    async fn session_search_stale_generation_result_is_ignored() {
        let mut app = app().await;
        let s = session("Roadmap");
        app.sessions = vec![s.clone()];
        app.session_search = "rust".to_string();
        // The current query is generation 5.
        app.session_search_gen.store(5, SeqCst);

        // An older read (generation 3) finishes last: it must not land.
        app.handle_event(TuiEvent::SessionContentSearchResult {
            generation: 3,
            rows: vec![content_row(s.id, "stale rust hit")],
        })
        .await
        .unwrap();
        assert!(
            app.session_content_matches.is_empty(),
            "stale generation overwrote the hits"
        );

        // The current read (generation 5) lands.
        app.handle_event(TuiEvent::SessionContentSearchResult {
            generation: 5,
            rows: vec![content_row(s.id, "fresh rust hit")],
        })
        .await
        .unwrap();
        assert_eq!(app.session_content_matches.len(), 1);
        assert_eq!(app.session_content_matches[0].snippet, "fresh rust hit");
    }

    #[tokio::test]
    async fn session_search_end_to_end_reads_message_content() {
        use crate::db::models::Message;
        use crate::db::repository::{MessageRepository, SessionRepository};

        let mut app = app().await;
        // Persist a session whose title misses the query but whose message
        // body carries it, then let the real debounced read surface it.
        let sessions = SessionRepository::new(app.session_service.pool());
        let messages = MessageRepository::new(app.session_service.pool());
        let s = session("Roadmap");
        sessions.create(&s).await.unwrap();
        let m = Message::new(s.id, "user".to_string(), "we use rust here".to_string(), 1);
        messages.create(&m).await.unwrap();
        app.sessions = vec![s.clone()];
        app.session_search = "rust".to_string();

        app.schedule_session_content_search();
        // The read debounces 250ms then lands on the event rail; wait for it.
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.event_handler_mut().next(),
        )
        .await
        .expect("content search timed out")
        .expect("event channel closed");
        app.handle_event(event).await.unwrap();

        assert_eq!(app.visible_session_indices(), vec![0]);
        assert_eq!(app.session_content_matches.len(), 1);
        assert!(app.session_content_matches[0].snippet.contains("rust"));
    }
}
