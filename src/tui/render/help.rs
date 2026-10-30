//! Help, plan mode, and settings rendering
//!
//! Help screen, plan mode view, plan mode help bar, and settings screen.

use super::super::app::App;
use super::super::app::dialog_keys::{DialogScope, dialog_keys};
use super::super::app::help_catalog;
use super::super::render::hints;
use super::theme::{self, Role};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

/// Render the help screen
pub(super) fn render_help(f: &mut Frame, app: &mut App, area: Rect) {
    // Helper to build a "key → description" line
    fn kv<'a>(key: &'a str, desc: &'a str, key_color: Color) -> Line<'a> {
        Line::from(vec![
            Span::styled(
                format!(" {:<14}", key),
                Style::default().fg(key_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default().fg(theme::role(Role::GrayDim))),
            Span::styled(desc, Style::default().fg(theme::role(Role::TextPrimary))),
        ])
    }

    fn section_header(title: &str) -> Line<'_> {
        Line::from(Span::styled(
            format!(" {} ", title),
            Style::default()
                .fg(theme::role(Role::Accent))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ))
    }

    // ── LEFT COLUMN ──
    let cyan = theme::role(Role::AccentTeal);

    let version_line = format!("v{}", env!("CARGO_PKG_VERSION"));
    // Session-aware: the global name lies when the loaded session runs a
    // per-session provider swap or a sticky fallback (#1464).
    let provider_name = app.provider_name_for_current_session();
    let model_name = app.provider_model_for_current_session();

    let mut left = vec![
        section_header("ABOUT"),
        kv("Version", &version_line, cyan),
        kv("Provider", &provider_name, cyan),
        kv("Model", &model_name, cyan),
        Line::from(""),
        section_header("GLOBAL"),
        kv("Ctrl+C", "Snap bottom / clear, quit (2x)", cyan),
        kv("Ctrl+N", "New session", cyan),
        kv("Ctrl+L", "List sessions", cyan),
        kv("Ctrl+K", "Clear session", cyan),
        kv("Mouse click", "Open URL/path under click", cyan),
        Line::from(""),
        section_header("CHAT"),
        kv("Enter", "Send message", cyan),
        kv("Shift+Enter", "New line", cyan),
        kv("Ctrl+J", "New line (alt)", cyan),
        kv("Escape (x2)", "Cancel / abort immediately", cyan),
        kv("Page Up/Down", "Scroll history", cyan),
        kv("@", "File picker", cyan),
        Line::from(""),
        section_header("INPUT EDITING"),
        kv("↑ / ↓", "Line nav / start-end / history", cyan),
        kv("← / →", "Move cursor", cyan),
        kv("Ctrl/Alt+←→", "Jump word", cyan),
        kv("Home / End", "Start / end of line", cyan),
        kv("Alt+Backspace", "Delete word", cyan),
        kv("Ctrl+Backspace", "Delete word (alias)", cyan),
        kv("Ctrl+W", "Delete word (vim)", cyan),
        kv("Ctrl+U", "Delete to line start (vim)", cyan),
        Line::from(""),
        section_header("SLASH COMMANDS"),
    ];

    // Commands come from the catalogue collected on entry (#1530) rather than
    // from a directory walk and a TOML parse per frame. The SLASH COMMANDS
    // header is already on `left` above; every other section prints its own,
    // and only when the filter left something under it — a lone header over
    // empty space reads as a rendering bug.
    let needle = app.help_search.clone();
    // Hoisted: `kv` borrows its arguments into the returned `Line`, so a
    // summary built inside the loop would be dropped before the frame draws.
    let skills_summary = format!(
        "Browse {} skills",
        help_catalog::section_matches(&app.help_catalog, help_catalog::HelpSection::Skill, "")
            .len()
    );
    for section in help_catalog::HelpSection::all() {
        let rows = help_catalog::section_matches(&app.help_catalog, section, &needle);
        if rows.is_empty() {
            continue;
        }
        if section != help_catalog::HelpSection::BuiltIn {
            left.push(Line::from(""));
            left.push(section_header(section.title()));
        }
        if section == help_catalog::HelpSection::Skill && needle.is_empty() {
            left.push(kv("/skills", &skills_summary, cyan));
        }
        for row in rows {
            left.push(kv(&row.name, &row.description, cyan));
        }
    }

    // A filter that matches nothing says so. Falling through would leave the
    // headers gone and the pane blank, which looks like a crash.
    let match_count = help_catalog::match_count(&app.help_catalog, &needle);
    if match_count == 0 {
        left.push(Line::from(""));
        left.push(Line::from(Span::styled(
            format!("  No command matches \"{needle}\""),
            Style::default().fg(theme::role(Role::Gray)),
        )));
    }

    left.extend([
        Line::from(""),
        Line::from(vec![
            Span::styled(" 📖 ", Style::default().fg(theme::role(Role::Accent))),
            Span::styled(
                "docs.opencrabs.com",
                Style::default()
                    .fg(theme::role(Role::AccentTeal))
                    .add_modifier(Modifier::UNDERLINED),
            ),
            Span::styled(
                "  Official documentation",
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
        ]),
    ]);

    // ── RIGHT COLUMN ──
    let right = vec![
        section_header("SESSIONS"),
        kv("↑ / ↓", "Navigate", cyan),
        kv("Enter", "Load session", cyan),
        kv("N", "New session", cyan),
        kv("R", "Rename", cyan),
        kv("D", "Delete", cyan),
        kv("/", "Search sessions", cyan),
        kv("F", "Session files", cyan),
        kv("P", "Projects", cyan),
        kv("Esc", "Back to chat", cyan),
        Line::from(""),
        section_header("FILE ARTIFACTS"),
        kv("↑ / ↓", "Navigate files", cyan),
        kv("Enter", "Open file", cyan),
        kv("D", "Remove tracking", cyan),
        kv("O", "Open containing folder", cyan),
        kv("Esc", "Back to sessions", cyan),
        Line::from(""),
        section_header("PROJECTS"),
        kv("↑ / ↓", "Navigate projects", cyan),
        kv("Enter", "View sessions", cyan),
        kv("N", "New project", cyan),
        kv("A", "Assign current session", cyan),
        kv("D", "Delete project", cyan),
        kv("U", "Unassign session (detail)", cyan),
        kv("Esc", "Back / exit", cyan),
        Line::from(""),
        section_header("TOOL APPROVAL"),
        kv("↑ / ↓", "Navigate options", cyan),
        kv("a", "Approve once", cyan),
        kv("Enter", "Confirm selection", cyan),
        kv("D / Esc", "Deny", cyan),
        kv("V", "Toggle details", cyan),
        Line::from(""),
        section_header("SPLIT PANES (from Sessions)"),
        kv("| (in sessions)", "Split horizontal (L|R)", cyan),
        kv("_ (in sessions)", "Split vertical (T/B)", cyan),
        kv("Tab", "Cycle pane focus", cyan),
        kv("Ctrl+X", "Close pane", cyan),
        Line::from(""),
        section_header("MISSION CONTROL"),
        kv("h/l or Tab", "Cycle panel focus", cyan),
        kv("L", "Open the log viewer", cyan),
        kv("e/w/i/d (in logs)", "Filter by minimum level", cyan),
        kv("/ (in logs)", "Filter by text", cyan),
        kv("[ ] (in logs)", "Previous / next day's file", cyan),
        Line::from(""),
        section_header("FEATURES"),
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(theme::role(Role::AccentTeal))),
            Span::styled(
                "Markdown & Syntax Highlighting",
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(theme::role(Role::AccentTeal))),
            Span::styled(
                "Multi-line Input & Streaming",
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(theme::role(Role::AccentTeal))),
            Span::styled(
                "Session Management & History",
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(theme::role(Role::AccentTeal))),
            Span::styled(
                "Token & Cost Tracking",
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(theme::role(Role::AccentTeal))),
            Span::styled(
                "Inline Tool Approval (3 policies)",
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(theme::role(Role::AccentTeal))),
            Span::styled(
                "Session File Tracking",
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(theme::role(Role::AccentTeal))),
            Span::styled(
                "Project Organization",
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
        ]),
        Line::from(""),
    ];

    // Full-bleed (#2162): content fills everything above the last two rows.
    // The second to last is a dim rule and the last is the shared command
    // footer, so the commands read as a footer instead of as two more
    // entries in the catalog. No border, no title row.
    let [content_area, separator_area, footer_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // The search row exists only while a filter is being typed or applied.
    // Idle Help has no header at all, which is the row that was eating space.
    let show_search = app.help_search_active || !needle.is_empty();
    let mut body = content_area;
    if show_search {
        let [search_area, rest] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(content_area);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  🔍 ", Style::default().fg(theme::role(Role::AccentTeal))),
                Span::styled(
                    format!("{needle}▏"),
                    Style::default()
                        .fg(theme::role(Role::Accent))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("   {match_count} matches"),
                    Style::default().fg(theme::role(Role::GrayDim)),
                ),
            ])),
            search_area,
        );
        body = rest;
    }

    // Responsive: two columns with a one-column rule when there is room, a
    // single full-width column under 110 cols. The border used to do the
    // separating; now the rule does, and it costs exactly one column. The
    // threshold sits above 100 so a 100-col terminal keeps a full-width
    // column instead of squeezing both halves into 49 cells.
    let wide = body.width >= 110;
    let [left_area, rule_area, right_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(body);

    // Record what was rendered so the key handler can clamp scroll to it.
    // `render_chat` does the same with `chat_area_height`; without it the
    // offset has no ceiling and winds off the end of the content (#1527).
    // (Plain field writes, not `&mut app`: `left` may still borrow
    // `app.help_catalog` through the section rows.)
    let viewport = body.height as usize;
    let content_rows = if wide {
        left.len()
    } else {
        left.len() + right.len()
    };
    app.help_content_rows = content_rows;
    // No border any more, so the viewport is the whole content height.
    // `render_settings` keeps its `- 2` because Settings keeps its border.
    app.help_viewport_rows = viewport;

    // One offset, but each column is clamped to its OWN length: the shorter
    // column stops at its end instead of scrolling past it into blank space.
    let left_scroll = app
        .help_scroll_offset
        .min(help_catalog::max_scroll(content_rows, viewport)) as u16;
    let right_scroll = app
        .help_scroll_offset
        .min(help_catalog::max_scroll(right.len(), viewport)) as u16;

    if wide {
        f.render_widget(Paragraph::new(left).scroll((left_scroll, 0)), left_area);
        f.render_widget(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(theme::role(Role::GrayDim))),
            rule_area,
        );
        f.render_widget(Paragraph::new(right).scroll((right_scroll, 0)), right_area);
    } else {
        let mut combined = left;
        combined.extend(right);
        f.render_widget(Paragraph::new(combined).scroll((left_scroll, 0)), body);
    }

    hints::render_separator(f, separator_area);
    hints::render_footer_with_context(
        f,
        footer_area,
        dialog_keys(DialogScope::Help),
        &format!("{match_count} commands"),
    );
}

/// Render the settings screen
pub(super) fn render_settings(f: &mut Frame, app: &mut App, area: Rect) {
    fn section(title: &str) -> Line<'_> {
        Line::from(Span::styled(
            format!("  {} ", title),
            Style::default()
                .fg(theme::role(Role::BlueSlate))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ))
    }

    fn kv<'a>(key: &'a str, val: &'a str) -> Line<'a> {
        Line::from(vec![
            Span::styled(
                format!("   {:<20}", key),
                Style::default().fg(theme::role(Role::Accent)),
            ),
            Span::styled(val, Style::default().fg(theme::role(Role::TextPrimary))),
        ])
    }

    fn status_dot<'a>(label: &'a str, enabled: bool) -> Line<'a> {
        let (dot, color) = if enabled {
            ("●", theme::role(Role::AccentTeal))
        } else {
            ("○", theme::role(Role::GrayDim))
        };
        Line::from(vec![
            Span::styled(
                format!("   {:<20}", label),
                Style::default().fg(theme::role(Role::Accent)),
            ),
            Span::styled(dot, Style::default().fg(color)),
            Span::styled(
                if enabled { " enabled" } else { " disabled" },
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
        ])
    }

    // Approval policy display
    let approval = if app.approval_auto_always {
        "auto-always"
    } else if app.approval_auto_session {
        "auto-session"
    } else {
        "ask"
    };

    // Memory search is always available (built-in FTS5)
    let memory_available = true;

    // User commands count
    let cmd_count = app.user_commands.len();
    let cmd_summary = if cmd_count == 0 {
        "none".to_string()
    } else {
        let names: Vec<&str> = app.user_commands.iter().map(|c| c.name.as_str()).collect();
        format!("{} ({})", cmd_count, names.join(", "))
    };

    // Config file path
    let config_path = crate::config::Config::system_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.opencrabs/config.toml".into());

    let home_dir = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    let collapse_home = |path: &str| -> String {
        if !home_dir.is_empty() && path.starts_with(&home_dir) {
            format!("~{}", &path[home_dir.len()..])
        } else {
            path.to_string()
        }
    };
    let brain_display = collapse_home(&app.brain_path.display().to_string());
    let wd_display = collapse_home(&app.working_directory.display().to_string());

    // Session-aware: /debug must show what actually serves this session,
    // not the global default (#1464).
    let provider_name = app.provider_name_for_current_session();
    let model_name = app.provider_model_for_current_session();
    let mut lines = vec![
        Line::from(""),
        section("PROVIDER"),
        kv("Provider", &provider_name),
        kv("Model", &model_name),
        Line::from(""),
        section("APPROVAL"),
        kv("Policy", approval),
        Line::from(""),
        section("COMMANDS"),
        kv("User commands", &cmd_summary),
        Line::from(""),
        section("MEMORY"),
        status_dot("Memory search", memory_available),
        Line::from(""),
        section("PATHS"),
        kv("Config", &config_path),
        kv("Brain", &brain_display),
        kv("Working dir", &wd_display),
        Line::from(""),
        Line::from(""),
    ];

    // Footer row at the bottom (#1775): shared footer replaces the
    // in-content bracket strip.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    // Pad to fill the area
    let min_height = rows[0].height as usize;
    while lines.len() < min_height {
        lines.push(Line::from(""));
    }

    // Settings shares `help_scroll_offset` and the key-handler arm that clamps
    // it, so it has to record its own height too. Clamping against the help
    // screen's numbers would be wrong when help was last open, and fatal when
    // it was never opened: max_scroll(0, 0) is 0, and Settings would refuse to
    // scroll at all.
    app.help_content_rows = lines.len();
    app.help_viewport_rows = rows[0].height.saturating_sub(2) as usize; // minus borders
    let scroll = app.help_scroll_offset.min(help_catalog::max_scroll(
        app.help_content_rows,
        app.help_viewport_rows,
    )) as u16;

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    " Settings ",
                    Style::default()
                        .fg(theme::role(Role::BlueSlate))
                        .add_modifier(Modifier::BOLD),
                ))
                .border_style(Style::default().fg(theme::role(Role::Gray))),
        )
        .scroll((scroll, 0));

    f.render_widget(para, rows[0]);
    hints::render_footer(f, rows[1], dialog_keys(DialogScope::Settings));
}
