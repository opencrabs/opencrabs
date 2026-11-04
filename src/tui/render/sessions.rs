//! Session list rendering
//!
//! Session manager view with navigation, renaming, and status indicators.
//!
//! The list is full-bleed (#2162): no border, no app title row, one row of
//! footer at the bottom carrying the keys from the scoped keymap. Every
//! session is exactly one terminal row, whatever the width, so the scroll
//! math and the viewport can never disagree about where a session is.
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

    // Compute filtered session indices based on search
    let visible_indices: Vec<usize> = if app.session_search.is_empty() {
        (0..app.sessions.len()).collect()
    } else {
        let search = app.session_search.to_lowercase();
        app.sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let name = s.title.as_deref().unwrap_or("New Chat").to_lowercase();
                name.contains(&search)
            })
            .map(|(i, _)| i)
            .collect()
    };

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

    // Track header line count for viewport scroll calculation
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

    for (display_idx, &session_idx) in visible_indices.iter().enumerate() {
        let session = &app.sessions[session_idx];
        let is_selected = display_idx == app.selected_session_index;
        let is_current = app
            .current_session
            .as_ref()
            .map(|s| s.id == session.id)
            .unwrap_or(false);

        if is_selected && app.session_renaming {
            lines.push(rename_row(app, &plan));
            continue;
        }

        lines.push(session_row(app, session, is_selected, is_current, &plan));
    }

    // Viewport scroll: keep the selected session visible. One row per
    // session, so the row index is the line index.
    let visible_height = (content_area.height as usize).max(1);
    let selected_line = header_line_count + app.selected_session_index;
    let scroll = if selected_line >= visible_height {
        (selected_line.saturating_sub(visible_height) + 1) as u16
    } else {
        0u16
    };

    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), content_area);

    hints::render_separator(f, rule_area);
    hints::render_footer_with_context(
        f,
        footer_area,
        dialog_keys(DialogScope::Sessions),
        &format!("{} sessions", visible_indices.len()),
    );
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
    /// `{.name}` wrapper, and status is a composite of up to three indicators
    /// that can all be on at once.
    fn cap(self, tier: SessionRowTier) -> usize {
        match self {
            Column::Date if tier == SessionRowTier::Narrow => 6,
            Column::Date => 17,
            Column::Status => 14,
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
                let text = if tier == SessionRowTier::Narrow {
                    format!(" {}", session.updated_at.format("%H:%M"))
                } else {
                    format!(" {}", session.updated_at.format("%Y-%m-%d %H:%M"))
                };
                vec![Span::styled(text, dim)]
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

    // Natural width per column, capped so one runaway value cannot claim the
    // frame. Zero means no visible row has a value for that column.
    let mut natural = [0usize; Column::PRIORITY.len()];
    for session in sessions {
        let is_current = app
            .current_session
            .as_ref()
            .is_some_and(|s| s.id == session.id);
        for (idx, column) in Column::PRIORITY.iter().enumerate() {
            if column.first_tier() > rank {
                continue;
            }
            let cell = column.cell(app, session, is_current, tier);
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
    } else if is_current {
        Style::default().fg(theme::role(Role::Gray))
    } else {
        Style::default().fg(theme::role(Role::TextPrimary))
    };

    // Left gutter, then the name padded to its column, then each column
    // right-aligned inside its fixed width, then the right gutter.
    let name = truncate_to_width(
        session.title.as_deref().unwrap_or("New Chat"),
        plan.name_width,
    );
    let mut spans = vec![
        Span::raw(" ".repeat(GUTTER)),
        Span::styled(name.clone(), name_style),
    ];
    let pad = plan.name_width.saturating_sub(name.width());
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    for (column, column_width) in &plan.columns {
        let cell = fit_cell(
            column.cell(app, session, is_current, plan.tier),
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
