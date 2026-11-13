//! Onboarding Wizard Rendering
//!
//! Render functions for each step of the onboarding wizard.

use super::onboarding::{
    AuthField, BrainField, CHANNEL_NAMES, ChannelTestStatus, DiscordField, HealthStatus,
    ImageField, OnboardingStep, OnboardingWizard, PROVIDERS, SlackField, TelegramField,
    TrelloField, WizardMode,
};
use super::onboarding_layout as layout;
use super::provider_selector::CUSTOM_PROVIDER_IDX;
use crate::tui::render::theme::{self, Role};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
};

/// Main color palette (matches existing OpenCrabs theme)
fn brand_blue() -> Color {
    theme::role(Role::Gray)
}
fn brand_gold() -> Color {
    theme::role(Role::Accent)
}
fn accent_gold() -> Color {
    theme::role(Role::Accent)
}

/// Render the entire onboarding wizard
/// The `[start, end)` slice of a list to draw so that `selected` stays visible
/// and at most `max` rows are used.
///
/// Shared by the provider list and the model list on the same step. They each
/// had their own copy of this arithmetic, which is how the provider list ended
/// up without any — it filled an 80x24 terminal and pushed the fields below it
/// off screen (#948). One implementation means a list cannot be added without
/// the window that keeps the rest of the form reachable.
///
/// A list shorter than `max` returns the whole range, so short lists render
/// exactly as they did before any windowing existed.
pub fn visible_window(total: usize, selected: usize, max: usize) -> (usize, usize) {
    if max == 0 || total <= max {
        return (0, total);
    }
    // Centre the selection, then clamp so the window never runs past the end —
    // without the clamp, selecting the last item leaves a window of blanks.
    let start = selected.saturating_sub(max / 2).min(total - max);
    (start, start + max)
}

pub fn render_onboarding(f: &mut Frame, wizard: &OnboardingWizard) {
    let area = f.area();
    // The header centers its text over the same width the content column
    // would get on a timeline-less screen; the subtitle wraps inside it.
    let head_w = layout::content_width(area.width);
    let head_wrap = (head_w.saturating_sub(8) as usize).max(20);

    // The footer band insets its text one column on each side.
    let footer = build_footer(wizard, area.width.saturating_sub(2) as usize);
    // Header and footer are tinted bands with breathing room, so they read
    // as bars instead of text glued to the screen edge (#1975).
    let pad = layout::band_padding(area.height);
    let band = Style::default().bg(theme::role(Role::SurfacePanel));
    let footer_h = band_height(footer.len(), pad);
    // Tall terminals get a blank row between header lines (#1980).
    let spaced = layout::header_spacing(area.height);

    // The left-side timeline replaces the header dots when it fits (#1979).
    // Its fit depends on the body height, which depends on the header, so
    // measure with the dot-less header and keep the dots if it does not fit.
    let steps = OnboardingStep::flow_steps(wizard.mode);
    let timeline = if !wizard.quick_jump && layout::shows_timeline(area.width) {
        let header = build_header(wizard, head_w as usize, head_wrap, false, spaced);
        let body_h = area
            .height
            .saturating_sub(band_height(header.len(), pad) + footer_h);
        match layout::timeline_fit(steps.len(), body_h.saturating_sub(1)) {
            layout::TimelineFit::Hidden => None,
            fit => Some(fit),
        }
    } else {
        None
    };

    let header = build_header(
        wizard,
        head_w as usize,
        head_wrap,
        timeline.is_none(),
        spaced,
    );
    let header_h = band_height(header.len(), pad);
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Min(1),
        Constraint::Length(footer_h),
    ])
    .areas(area);

    let content_area = match timeline {
        Some(fit) => {
            let [timeline_area, rest] = Layout::horizontal([
                Constraint::Length(layout::TIMELINE_WIDTH),
                Constraint::Fill(1),
            ])
            .areas(body_area);
            render_timeline(f, timeline_area, wizard, steps, fit);
            rest
        }
        None => body_area,
    };
    let col_w = layout::content_width(content_area.width);
    // Text areas (brain setup) wrap inside the column, leaving room for the
    // field indent.
    let wrap_width = (col_w.saturating_sub(8) as usize).max(20);
    let [_, col_area, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(col_w),
        Constraint::Fill(1),
    ])
    .areas(content_area);

    let visible = col_area.height as usize;
    let (rows, focus_row) = build_body(wizard, area, col_w as usize, wrap_width, visible);
    let scroll = layout::scroll_offset(
        focus_row,
        rows.len(),
        visible,
        wizard.user_scroll_offset as usize,
    );
    let hidden_below = rows.len().saturating_sub(scroll + visible);

    f.render_widget(
        Paragraph::new(header)
            .alignment(Alignment::Center)
            .style(band)
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(brand_blue()))
                    .padding(Padding {
                        left: 1,
                        right: 1,
                        top: pad.outer,
                        bottom: pad.inner,
                    }),
            ),
        header_area,
    );
    f.render_widget(Paragraph::new(rows).scroll((scroll as u16, 0)), col_area);
    if hidden_below > 0 && col_area.height > 0 {
        render_more_below(f, col_area, hidden_below);
    }
    if footer_h > 0 {
        f.render_widget(
            Paragraph::new(footer)
                .alignment(Alignment::Center)
                .style(band)
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(brand_blue()))
                        .padding(Padding {
                            left: 1,
                            right: 1,
                            top: pad.inner,
                            bottom: pad.outer,
                        }),
                ),
            footer_area,
        );
    }

    // WhatsApp QR popup: rendered as a centered overlay with white bg so the
    // QR modules have the contrast needed to be scannable by a phone camera.
    if wizard.step == OnboardingStep::WhatsAppSetup
        && let Some(ref qr_text) = wizard.whatsapp_qr_text
    {
        render_whatsapp_qr_popup(f, qr_text, area);
    }
}

/// Rows a header/footer band takes: its text, the rule, and the padding.
fn band_height(text_rows: usize, pad: layout::BandPadding) -> u16 {
    if text_rows == 0 {
        0
    } else {
        text_rows as u16 + 1 + pad.outer + pad.inner
    }
}

/// The left-side step timeline (#1979): a heading, then one node per step of
/// the active flow, done / current / upcoming, joined by connector rows when
/// there is room for them.
fn render_timeline(
    f: &mut Frame,
    area: Rect,
    wizard: &OnboardingWizard,
    steps: &[OnboardingStep],
    fit: layout::TimelineFit,
) {
    let complete = wizard.step == OnboardingStep::Complete;
    let current = if complete {
        steps.len() + 1
    } else {
        wizard.step.flow_number(wizard.mode)
    };
    let heading = if complete {
        "All steps done".to_string()
    } else {
        format!("Step {current} of {}", steps.len())
    };
    let label_room = (area.width as usize).saturating_sub(6);
    let done = Style::default().fg(theme::role(Role::Success));
    let dim = Style::default().fg(theme::role(Role::GrayDim));

    // The brand line lives here when the timeline is shown, so the header
    // can be just the step (#1980).
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "  OpenCrabs Setup",
            Style::default()
                .fg(brand_gold())
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("  {heading}"),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (i, s) in steps.iter().enumerate() {
        let (node, node_style, label_style) = match layout::node_state(i, current) {
            layout::NodeState::Done => ("●", done, Style::default().fg(theme::role(Role::Gray))),
            layout::NodeState::Current => {
                let hot = Style::default()
                    .fg(brand_gold())
                    .add_modifier(Modifier::BOLD);
                ("◉", hot, hot)
            }
            layout::NodeState::Upcoming => ("○", dim, dim),
        };
        let label: String = s.title().chars().take(label_room).collect();
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(node, node_style),
            Span::raw("  "),
            Span::styled(label, label_style),
        ]));
        if fit == layout::TimelineFit::Spacious && i + 1 < steps.len() {
            // A segment is travelled once the step below it is reached.
            let link = if i + 2 <= current { done } else { dim };
            lines.push(Line::from(vec![Span::raw("  "), Span::styled("│", link)]));
        }
    }
    let inner = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// Pinned top of the screen: what this is and how far along the user is.
/// Rows are pre-wrapped to `width` so the header height is exact.
/// `show_dots` keeps the progress in the header; off when the left timeline
/// already shows it and carries the brand line (#1979, #1980). `spaced` puts a
/// blank row between header lines on tall terminals.
fn build_header(
    wizard: &OnboardingWizard,
    width: usize,
    wrap_width: usize,
    show_dots: bool,
    spaced: bool,
) -> Vec<Line<'static>> {
    let step = wizard.step;
    let bar_style = Style::default()
        .fg(brand_gold())
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = Vec::new();

    if step == OnboardingStep::Complete {
        lines.push(Line::from(Span::styled(
            "OpenCrabs Setup Complete",
            bar_style,
        )));
    } else if wizard.quick_jump {
        // Deep links (/models, /doctor, /voice and the other setup commands) show one step: name it.
        lines.push(Line::from(Span::styled(
            format!("OpenCrabs · {}", step.title()),
            bar_style,
        )));
    } else {
        let gap = |lines: &mut Vec<Line<'static>>| {
            if spaced {
                lines.push(Line::from(""));
            }
        };
        if show_dots {
            // No timeline: the brand and the step share one line, and the
            // progress gets its own line under it.
            lines.push(Line::from(Span::styled(
                format!("OpenCrabs Setup: {}", step.title()),
                bar_style,
            )));
            gap(&mut lines);
            // Progress counter is flow-aware: QuickStart shows its own
            // sequence, Advanced the full one.
            let current = step.flow_number(wizard.mode);
            let total = OnboardingStep::flow_steps(wizard.mode).len();
            lines.push(Line::from(vec![
                Span::styled(
                    render_progress_dots(current, total),
                    Style::default().fg(brand_blue()),
                ),
                Span::styled(
                    format!("   {current}/{total}"),
                    Style::default().fg(theme::role(Role::Gray)),
                ),
            ]));
        } else {
            // The timeline carries the brand; the header is the step itself.
            lines.push(Line::from(Span::styled(
                step.title().to_string(),
                bar_style,
            )));
        }
        gap(&mut lines);
        let subtitle_style = Style::default().fg(theme::role(Role::Gray));
        for chunk in wrap_text(step.subtitle(), wrap_width) {
            lines.push(Line::from(Span::styled(chunk, subtitle_style)));
        }
    }
    layout::wrap_lines(lines, width).0
}

/// Pinned bottom of the screen: the current error (always visible, never
/// scrolled away under the form) and the keys this step takes.
fn build_footer(wizard: &OnboardingWizard, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if wizard.step == OnboardingStep::ProviderAuth {
        let matched = wizard.ps.model_count();
        let total = wizard.ps.all_model_names().len();
        let counted = if matched == total {
            format!("{total} model{}", if total == 1 { "" } else { "s" })
        } else {
            format!(
                "{matched} of {total} models · filter: {}",
                wizard.ps.model_filter
            )
        };
        lines.push(Line::from(Span::styled(
            counted,
            Style::default().fg(theme::role(Role::Gray)),
        )));
        if wizard.ps.models_fetching {
            lines.push(Line::from(Span::styled(
                "fetching live models...",
                Style::default().fg(theme::role(Role::AccentSoft)),
            )));
        } else if let Some(ref err) = wizard.model_fetch_lifecycle.error {
            lines.push(Line::from(Span::styled(
                format!("! model fetch failed: {err}"),
                Style::default().fg(theme::role(Role::Error)),
            )));
        }
    }
    if let Some(ref err) = wizard.error_message {
        lines.push(Line::from(Span::styled(
            format!("! {err}"),
            Style::default().fg(theme::role(Role::Error)),
        )));
    }
    if wizard.step != OnboardingStep::Complete {
        lines.push(Line::from(footer_key_spans(wizard)));
    }
    layout::wrap_lines(lines, width).0
}

fn footer_key_spans(wizard: &OnboardingWizard) -> Vec<Span<'static>> {
    let step = wizard.step;
    let is_channels = step == OnboardingStep::Channels;
    let is_channel_sub = matches!(
        step,
        OnboardingStep::TelegramSetup
            | OnboardingStep::DiscordSetup
            | OnboardingStep::WhatsAppSetup
            | OnboardingStep::SlackSetup
            | OnboardingStep::TrelloSetup
    );
    // Step 1 has nothing to go back to: a first run quits the app, a re-run
    // exits to chat (#1976). A deep link exits to chat from any step, every
    // other step goes back one.
    let esc_label = if wizard.quick_jump {
        "Exit"
    } else if step == OnboardingStep::ModeSelect {
        if wizard.is_first_time { "Quit" } else { "Exit" }
    } else {
        "Back"
    };

    let mut footer: Vec<Span<'static>> = vec![
        Span::styled(
            " [Esc] ",
            Style::default()
                .fg(theme::role(Role::Error))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{}  ", esc_label),
            Style::default().fg(theme::role(Role::AccentSoft)),
        ),
    ];

    if is_channels {
        // Channels list: Space toggles, Enter opens setup, arrow keys navigate
        footer.push(Span::styled(
            "[Space] ",
            Style::default()
                .fg(brand_blue())
                .add_modifier(Modifier::BOLD),
        ));
        footer.push(Span::styled(
            "Toggle  ",
            Style::default().fg(theme::role(Role::AccentSoft)),
        ));
        footer.push(Span::styled(
            "[Enter] ",
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD),
        ));
        footer.push(Span::styled(
            "Setup",
            Style::default().fg(theme::role(Role::AccentSoft)),
        ));
    } else if is_channel_sub {
        // Channel setup screens: tab nav + editing hints
        footer.push(Span::styled(
            "[Tab] ",
            Style::default()
                .fg(brand_blue())
                .add_modifier(Modifier::BOLD),
        ));
        footer.push(Span::styled(
            "Next  ",
            Style::default().fg(theme::role(Role::AccentSoft)),
        ));
        footer.push(Span::styled(
            "[←→] ",
            Style::default()
                .fg(brand_blue())
                .add_modifier(Modifier::BOLD),
        ));
        footer.push(Span::styled(
            "Cursor  ",
            Style::default().fg(theme::role(Role::AccentSoft)),
        ));
        footer.push(Span::styled(
            "[Enter] ",
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD),
        ));
        footer.push(Span::styled(
            "Confirm",
            Style::default().fg(theme::role(Role::AccentSoft)),
        ));
    } else if step == OnboardingStep::HealthCheck {
        footer.push(Span::styled(
            "[Enter] ",
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD),
        ));
        if wizard.health_complete {
            footer.push(Span::styled(
                "Re-check",
                Style::default().fg(theme::role(Role::AccentSoft)),
            ));
        } else {
            footer.push(Span::styled(
                "Check",
                Style::default().fg(theme::role(Role::AccentSoft)),
            ));
        }
    } else {
        // All other steps: Tab/Shift+Tab field nav + Enter confirm
        if step != OnboardingStep::ModeSelect {
            footer.push(Span::styled(
                "[Tab] ",
                Style::default()
                    .fg(brand_blue())
                    .add_modifier(Modifier::BOLD),
            ));
            footer.push(Span::styled(
                "Next Field  ",
                Style::default().fg(theme::role(Role::AccentSoft)),
            ));
            // Shift+Tab goes back a field; it was reachable but never shown (#2043).
            footer.push(Span::styled(
                "[Shift+Tab] ",
                Style::default()
                    .fg(brand_blue())
                    .add_modifier(Modifier::BOLD),
            ));
            footer.push(Span::styled(
                "Back  ",
                Style::default().fg(theme::role(Role::AccentSoft)),
            ));
        }
        footer.push(Span::styled(
            "[Enter] ",
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD),
        ));
        footer.push(Span::styled(
            "Confirm",
            Style::default().fg(theme::role(Role::AccentSoft)),
        ));
    }

    footer
}

/// The scrolling middle of the screen, pre-wrapped to the column width, and
/// the row the focused field starts on. The provider step's lists shrink
/// until the whole form fits, so on a small terminal the key and model
/// fields show without scrolling at all (#1973).
fn build_body(
    wizard: &OnboardingWizard,
    area: Rect,
    col_w: usize,
    wrap_width: usize,
    visible: usize,
) -> (Vec<Line<'static>>, usize) {
    let mut list_rows = layout::MAX_LIST_ROWS;
    loop {
        let (lines, focused_line) = build_body_lines(wizard, area, wrap_width, list_rows);
        let (rows, starts) = layout::wrap_lines(lines, col_w);
        if rows.len() <= visible || list_rows <= layout::MIN_LIST_ROWS {
            let focus_row = starts.get(focused_line).copied().unwrap_or(0);
            return (rows, focus_row);
        }
        list_rows -= 1;
    }
}

fn build_body_lines(
    wizard: &OnboardingWizard,
    area: Rect,
    wrap_width: usize,
    list_rows: usize,
) -> (Vec<Line<'static>>, usize) {
    let step = wizard.step;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Logo + tagline on the first step only, and only when the terminal has
    // rows to spare: on a small window the form wins.
    if step == OnboardingStep::ModeSelect
        && !wizard.quick_jump
        && layout::shows_logo(area.width, area.height)
    {
        let logo_style = Style::default()
            .fg(brand_gold())
            .add_modifier(Modifier::BOLD);
        lines.push(Line::from(""));
        for logo_line in [
            "   ___                    ___           _",
            "  / _ \\ _ __  ___ _ _    / __|_ _ __ _| |__  ___",
            " | (_) | '_ \\/ -_) ' \\  | (__| '_/ _` | '_ \\(_-<",
            r"  \___/| .__/\___|_||_|  \___|_| \__,_|_.__//__/",
            "       |_|",
        ] {
            lines.push(Line::from(Span::styled(logo_line.to_string(), logo_style)));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  \u{1f980} The autonomous AI agent. Self-improving. Every channel.",
            Style::default()
                .fg(brand_gold())
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Resume notice: what is still outstanding after picking up a run that
    // was left halfway (#919).
    if let Some(ref notice) = wizard.resume_notice {
        lines.push(Line::from(""));
        for chunk in wrap_text(notice, wrap_width) {
            lines.push(Line::from(Span::styled(
                format!("  {chunk}"),
                Style::default().fg(brand_blue()),
            )));
        }
    }
    lines.push(Line::from(""));

    // Step-specific content; returns a focused-line hint for scrolling
    let focused_line: usize = match step {
        OnboardingStep::ProviderAuth => render_provider_auth(&mut lines, wizard, list_rows),
        OnboardingStep::Channels => render_channels(&mut lines, wizard),
        OnboardingStep::TelegramSetup => render_telegram_setup(&mut lines, wizard),
        OnboardingStep::DiscordSetup => render_discord_setup(&mut lines, wizard),
        OnboardingStep::WhatsAppSetup => render_whatsapp_setup(&mut lines, wizard),
        OnboardingStep::SlackSetup => render_slack_setup(&mut lines, wizard),
        OnboardingStep::TrelloSetup => render_trello_setup(&mut lines, wizard),
        other => {
            match other {
                OnboardingStep::ModeSelect => render_mode_select(&mut lines, wizard),
                OnboardingStep::Workspace => render_workspace(&mut lines, wizard),
                OnboardingStep::VoiceSetup => render_voice_setup(&mut lines, wizard),
                OnboardingStep::ImageSetup => render_image_setup(&mut lines, wizard),
                OnboardingStep::Daemon => render_daemon(&mut lines, wizard),
                OnboardingStep::HealthCheck => render_health_check(&mut lines, wizard),
                OnboardingStep::BrainSetup => render_brain_setup(&mut lines, wizard, wrap_width),
                OnboardingStep::Complete => render_complete(&mut lines, wizard),
                _ => unreachable!(),
            }
            0
        }
    };
    (lines, focused_line)
}

/// A dim cue on the last visible row when the form continues below, so a
/// field under the fold is never invisible without a hint it exists.
fn render_more_below(f: &mut Frame, col_area: Rect, hidden: usize) {
    let row = Rect {
        y: col_area.y + col_area.height - 1,
        height: 1,
        ..col_area
    };
    f.render_widget(ratatui::widgets::Clear, row);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("↓ {hidden} more below · PgDn"),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )))
        .alignment(Alignment::Right),
        row,
    );
}

/// Least terminal size that shows the whole pairing popup: the widest content
/// line (instruction, QR row, or footer) inside two border columns, and the QR
/// rows plus instruction, spacing and footer inside two border rows.
pub(crate) fn whatsapp_qr_popup_size(qr_w: u16, qr_h: u16) -> (u16, u16) {
    use unicode_width::UnicodeWidthStr;
    const HINT: &str = " Open WhatsApp › Linked Devices › Link a Device ";
    const FOOTER: &str = " Waiting for scan... ";
    let content_w = qr_w
        .saturating_add(4)
        .max(HINT.width() as u16)
        .max(FOOTER.width() as u16);
    // instruction + blank + qr rows + blank + footer
    let content_h = qr_h.saturating_add(4);
    (content_w.saturating_add(2), content_h.saturating_add(2))
}

/// Render the WhatsApp QR code as a centered popup. The modules are drawn in
/// the accent colour on the theme's `SurfaceQr`, which inverts a dark theme
/// (light modules on dark) and stays standard on a light one.
fn render_whatsapp_qr_popup(f: &mut Frame, qr_text: &str, area: Rect) {
    use unicode_width::UnicodeWidthStr;
    let qr_lines: Vec<&str> = qr_text.lines().collect();
    let qr_w = qr_lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
    let qr_h = qr_lines.len() as u16;

    // The popup needs its border plus every content line. Below this, ratatui
    // clips silently and the code becomes unscannable with no hint why, so the
    // small terminal gets an honest message instead (#2246).
    let (need_w, need_h) = whatsapp_qr_popup_size(qr_w, qr_h);
    if area.width < need_w || area.height < need_h {
        f.render_widget(ratatui::widgets::Clear, area);
        let message = vec![
            Line::from(Span::styled(
                "Terminal too small for the WhatsApp QR code",
                Style::default()
                    .fg(theme::role(Role::AccentSoft))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!("Needs at least {need_w} columns and {need_h} rows."),
                Style::default().fg(theme::role(Role::Gray)),
            )),
        ];
        f.render_widget(Paragraph::new(message).alignment(Alignment::Center), area);
        return;
    }

    let popup_w = need_w;
    let popup_h = need_h;

    let x = area.x + area.width.saturating_sub(popup_w) / 2;
    let y = area.y + area.height.saturating_sub(popup_h) / 2;
    let popup_area = Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    };

    // Content lines. Modules are accent-on-SurfaceQr, so a dark theme renders
    // them light-on-dark (inverted) and a light theme dark-on-light (standard).
    let qr_style = Style::default()
        .fg(theme::role(Role::AccentSoft))
        .bg(theme::role(Role::SurfaceQr));
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(qr_h as usize + 3);
    lines.push(Line::from(Span::styled(
        " Open WhatsApp › Linked Devices › Link a Device ",
        Style::default().fg(theme::role(Role::Gray)),
    )));
    lines.push(Line::from(""));
    for qr_line in qr_lines {
        lines.push(Line::from(Span::styled(
            format!("  {}  ", qr_line),
            qr_style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Waiting for scan... ",
        Style::default().fg(brand_gold()),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::role(Role::Gray)))
        .style(Style::default().bg(theme::role(Role::SurfaceQr)));

    let para = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Center);

    f.render_widget(ratatui::widgets::Clear, popup_area);
    f.render_widget(para, popup_area);
}

/// Render progress dots (filled for completed, hollow for remaining)
fn render_progress_dots(current: usize, total: usize) -> String {
    (1..=total)
        .map(|i| if i <= current { "●" } else { "○" })
        .collect::<Vec<_>>()
        .join(" ")
}

// --- Individual step renderers ---
// All functions produce Vec<Line<'static>> by using owned strings throughout.

fn render_mode_select(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) {
    let qs_selected = wizard.mode == WizardMode::QuickStart;

    lines.push(Line::from(vec![
        Span::styled(
            if qs_selected { " > " } else { "   " },
            Style::default().fg(accent_gold()),
        ),
        Span::styled(
            if qs_selected { "[*]" } else { "[ ]" },
            Style::default().fg(if qs_selected {
                brand_gold()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            " QuickStart",
            Style::default()
                .fg(if qs_selected {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                })
                .add_modifier(if qs_selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ]));
    // Counts come from the same totals as the progress counter, so the
    // promise on this screen matches the "n/N" in the header (#1977).
    lines.push(Line::from(Span::styled(
        format!(
            "       Sensible defaults, {} steps",
            OnboardingStep::quick_total()
        ),
        Style::default().fg(theme::role(Role::Gray)),
    )));
    lines.push(Line::from(""));

    let adv_selected = !qs_selected;
    lines.push(Line::from(vec![
        Span::styled(
            if adv_selected { " > " } else { "   " },
            Style::default().fg(accent_gold()),
        ),
        Span::styled(
            if adv_selected { "[*]" } else { "[ ]" },
            Style::default().fg(if adv_selected {
                brand_gold()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            " Advanced",
            Style::default()
                .fg(if adv_selected {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                })
                .add_modifier(if adv_selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!("       Full control, all {} steps", OnboardingStep::total()),
        Style::default().fg(theme::role(Role::Gray)),
    )));
}

/// Returns the line index (in `lines`) of the currently focused element —
/// used by `render_onboarding` to scroll the Paragraph and keep it visible.
/// `list_rows` caps the provider and model lists; the caller shrinks it until
/// the form fits the screen.
fn render_provider_auth(
    lines: &mut Vec<Line<'static>>,
    wizard: &OnboardingWizard,
    list_rows: usize,
) -> usize {
    let is_custom = wizard.ps.is_custom();
    let mut focused_line: usize = 0;

    // Provider list — 8 static providers (0-7), then existing custom names (9+), then "+ New Custom" (8) last.
    // Visual order: 0-7, then 9+, then 8 (existing customs before add button).
    let display_order = wizard.ps.provider_display_order();

    // Window the list the same way the model list below is windowed (#916).
    // Rendering every entry filled an 80x24 terminal — the default size, and
    // what a zoomed-in display gives you — with providers alone, pushing the
    // model and key fields off screen where they could not be seen or reached
    // (#948). The count only grows: every custom provider the user adds is
    // another row.
    let total_providers = display_order.len();
    let sel_pos = display_order
        .iter()
        .position(|&i| i == wizard.ps.selected_provider)
        .unwrap_or(0);
    let (p_start, p_end) = visible_window(total_providers, sel_pos, list_rows);

    if p_start > 0 {
        lines.push(Line::from(Span::styled(
            format!("   ↑ {} more", p_start),
            Style::default().fg(theme::role(Role::Gray)),
        )));
    }
    for &idx in &display_order[p_start..p_end] {
        let selected = idx == wizard.ps.selected_provider;
        let focused = wizard.auth_field == AuthField::Provider;
        let configured = wizard.ps.provider_has_credentials(idx);

        let prefix = if selected && focused { " > " } else { "   " };
        let marker = if selected { "[*]" } else { "[ ]" };

        let label = if idx == PROVIDERS.len() - 1 {
            "+ New Custom Provider".to_string()
        } else if idx < PROVIDERS.len() {
            PROVIDERS[idx].name.to_string()
        } else {
            let custom_idx = idx - crate::tui::provider_selector::CUSTOM_INSTANCES_START;
            wizard
                .ps
                .custom_names
                .get(custom_idx)
                .cloned()
                .unwrap_or_else(|| "custom".to_string())
        };

        // Green for configured providers, white/bold for selected, light gray
        // for the rest. The unselected rows MUST stay readable: ANSI bright
        // black collapses into the background on dark terminal themes, which
        // made every unselected provider invisible (only the white selected row
        // and green configured row showed, so the list looked like it was
        // missing most providers).
        let label_color = if selected {
            theme::role(Role::AccentSoft)
        } else if configured {
            theme::role(Role::Success)
        } else {
            theme::role(Role::Gray)
        };

        let mut spans = vec![
            Span::styled(prefix, Style::default().fg(accent_gold())),
            Span::styled(
                marker,
                Style::default().fg(if selected {
                    brand_gold()
                } else if configured {
                    theme::role(Role::Success)
                } else {
                    theme::role(Role::Gray)
                }),
            ),
            Span::styled(
                format!(" {}", label),
                Style::default().fg(label_color).add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
        ];
        if configured && !selected {
            spans.push(Span::styled(
                " ✓",
                Style::default().fg(theme::role(Role::Success)),
            ));
        }
        lines.push(Line::from(spans));
    }
    if p_end < total_providers {
        lines.push(Line::from(Span::styled(
            format!("   ↓ {} more", total_providers - p_end),
            Style::default().fg(theme::role(Role::Gray)),
        )));
    }

    lines.push(Line::from(""));

    if is_custom {
        let name_focused = wizard.auth_field == AuthField::CustomName;
        let base_focused = wizard.auth_field == AuthField::CustomBaseUrl;
        let api_key_focused = wizard.auth_field == AuthField::CustomApiKey;
        let model_focused = wizard.auth_field == AuthField::CustomModel;

        // Provider Name field
        let name_display = if wizard.ps.custom_name.is_empty() {
            "enter a name (e.g. nvidia, ollama)".to_string()
        } else {
            wizard.ps.custom_name.clone()
        };
        let cursor = if name_focused { "█" } else { "" };
        if name_focused {
            focused_line = lines.len();
        }
        lines.push(Line::from(vec![
            Span::styled(
                "  Name:     ",
                Style::default().fg(if name_focused {
                    brand_blue()
                } else {
                    theme::role(Role::Gray)
                }),
            ),
            Span::styled(
                format!("{}{}", name_display, cursor),
                Style::default().fg(if name_focused {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                }),
            ),
        ]));

        let base_display = if wizard.ps.base_url.is_empty() {
            "http://localhost:8000/v1".to_string()
        } else {
            wizard.ps.base_url.clone()
        };
        let cursor = if base_focused { "█" } else { "" };
        if base_focused {
            focused_line = lines.len();
        }
        lines.push(Line::from(vec![
            Span::styled(
                "  Base URL: ",
                Style::default().fg(if base_focused {
                    brand_blue()
                } else {
                    theme::role(Role::Gray)
                }),
            ),
            Span::styled(
                format!("{}{}", base_display, cursor),
                Style::default().fg(if base_focused {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                }),
            ),
        ]));

        // API Key field (optional for custom providers)
        let has_existing = wizard.ps.has_existing_key_sentinel();
        let key_display = if wizard.ps.api_key_input.is_empty() {
            "optional".to_string()
        } else if has_existing {
            "● configured".to_string()
        } else {
            "*".repeat(wizard.ps.api_key_input.len().min(30))
        };
        let cursor = if api_key_focused && !has_existing {
            "█"
        } else {
            ""
        };
        if api_key_focused {
            focused_line = lines.len();
        }
        lines.push(Line::from(vec![
            Span::styled(
                "  API Key:  ",
                Style::default().fg(if api_key_focused {
                    brand_blue()
                } else {
                    theme::role(Role::Gray)
                }),
            ),
            Span::styled(
                format!("{}{}", key_display, cursor),
                Style::default().fg(if has_existing {
                    theme::role(Role::AccentTeal)
                } else if api_key_focused {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                }),
            ),
        ]));

        // Custom provider: show model list if fetched, otherwise free-text
        if model_focused {
            focused_line = lines.len();
        }
        if wizard.ps.models.is_empty() {
            let model_display = if wizard.ps.custom_model.is_empty() {
                "model-name".to_string()
            } else {
                wizard.ps.custom_model.clone()
            };
            let cursor = if model_focused { "█" } else { "" };
            lines.push(Line::from(vec![
                Span::styled(
                    "  Model:    ",
                    Style::default().fg(if model_focused {
                        brand_blue()
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
                Span::styled(
                    format!("{}{}", model_display, cursor),
                    Style::default().fg(if model_focused {
                        theme::role(Role::AccentSoft)
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
            ]));
        } else {
            // Models fetched — show scrollable list picker.
            //
            // Typing filters (input.rs pushes to `model_filter`) and Enter
            // commits the typed text as a model ID when nothing matches, but
            // this list used to draw the UNFILTERED `ps.models` and never
            // showed the filter at all (#916). Two things went wrong: the
            // user typed and the screen did not react, so the feature looked
            // absent, and navigation indexes the FILTERED list while the draw
            // indexed the full one, so the highlight pointed at the wrong row.
            let filter = wizard.ps.model_filter.trim().to_lowercase();
            let visible: Vec<String> = wizard
                .ps
                .models
                .iter()
                .filter(|m| filter.is_empty() || m.to_lowercase().contains(&filter))
                .cloned()
                .collect();

            // Echo what was typed, so filtering is visible and a typed ID that
            // matches nothing still shows what would be committed on Enter.
            if model_focused || !wizard.ps.model_filter.is_empty() {
                let cursor = if model_focused { "█" } else { "" };
                let shown = if wizard.ps.model_filter.is_empty() {
                    "type to filter, or enter a model ID".to_string()
                } else {
                    wizard.ps.model_filter.clone()
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        "  Model:    ",
                        Style::default().fg(if model_focused {
                            brand_blue()
                        } else {
                            theme::role(Role::Gray)
                        }),
                    ),
                    Span::styled(
                        format!("{shown}{cursor}"),
                        Style::default().fg(if wizard.ps.model_filter.is_empty() {
                            theme::role(Role::Gray)
                        } else {
                            theme::role(Role::TextPrimary)
                        }),
                    ),
                ]));
            }

            // Nothing matched: the typed text becomes the model ID on Enter,
            // which is the only way to use a model the provider did not list.
            if visible.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  no match — Enter uses \"{}\" as the model ID",
                        wizard.ps.model_filter.trim()
                    ),
                    Style::default().fg(theme::role(Role::Gray)),
                )));
            }

            let total = visible.len();
            let safe_sel = wizard.ps.selected_model.min(total.saturating_sub(1));
            let (start, end) = visible_window(total, safe_sel, list_rows);

            if start > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  ↑ {} more", start),
                    Style::default().fg(theme::role(Role::Gray)),
                )));
            }
            for (off, m) in visible[start..end].iter().enumerate() {
                let idx = start + off;
                let sel = idx == safe_sel;
                let prefix = if sel && model_focused { " > " } else { "   " };
                let style = if sel && model_focused {
                    Style::default()
                        .fg(theme::role(Role::Ink))
                        .bg(brand_blue())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::role(Role::TextPrimary))
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(m.clone(), style),
                ]));
            }
            if end < total {
                lines.push(Line::from(Span::styled(
                    format!("  ↓ {} more", total - end),
                    Style::default().fg(theme::role(Role::Gray)),
                )));
            }
        }

        // Context Window field
        let cw_focused = wizard.auth_field == AuthField::CustomContextWindow;
        let cw_display = if wizard.ps.context_window.is_empty() {
            "e.g. 128000 (optional)".to_string()
        } else {
            wizard.ps.context_window.clone()
        };
        let cursor = if cw_focused { "█" } else { "" };
        if cw_focused {
            focused_line = lines.len();
        }
        lines.push(Line::from(vec![
            Span::styled(
                "  Context:  ",
                Style::default().fg(if cw_focused {
                    brand_blue()
                } else {
                    theme::role(Role::Gray)
                }),
            ),
            Span::styled(
                format!("{}{}", cw_display, cursor),
                Style::default().fg(if cw_focused {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                }),
            ),
        ]));
    } else if wizard.ps.provider_id() == "github" {
        // GitHub Copilot — OAuth device flow
        use crate::tui::onboarding::GitHubDeviceFlowStatus;

        if wizard.ps.has_existing_key_sentinel() {
            // Already authenticated
            lines.push(Line::from(Span::styled(
                "  ● Authenticated with GitHub Copilot",
                Style::default().fg(theme::role(Role::Success)),
            )));
            lines.push(Line::from(Span::styled(
                "  Press Enter to continue, or re-authenticate below",
                Style::default()
                    .fg(theme::role(Role::Gray))
                    .add_modifier(Modifier::ITALIC),
            )));
        } else {
            match &wizard.github_device_flow_status {
                GitHubDeviceFlowStatus::Idle => {
                    lines.push(Line::from(Span::styled(
                        "  Uses your GitHub Copilot subscription (no API charges)",
                        Style::default()
                            .fg(theme::role(Role::Gray))
                            .add_modifier(Modifier::ITALIC),
                    )));
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "  Press Enter to sign in with GitHub",
                        Style::default()
                            .fg(brand_blue())
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                GitHubDeviceFlowStatus::WaitingForUser => {
                    lines.push(Line::from(Span::styled(
                        "  1. Go to: github.com/login/device",
                        Style::default()
                            .fg(brand_blue())
                            .add_modifier(Modifier::BOLD),
                    )));
                    if let Some(ref code) = wizard.github_user_code {
                        lines.push(Line::from(Span::styled(
                            format!("  2. Enter code: {}", code),
                            Style::default()
                                .fg(theme::role(Role::Warning))
                                .add_modifier(Modifier::BOLD),
                        )));
                    }
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "  Waiting for authorization...",
                        Style::default()
                            .fg(theme::role(Role::Gray))
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
                GitHubDeviceFlowStatus::Complete => {
                    lines.push(Line::from(Span::styled(
                        "  ● Authenticated successfully!",
                        Style::default()
                            .fg(theme::role(Role::Success))
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                GitHubDeviceFlowStatus::Failed(err) => {
                    lines.push(Line::from(Span::styled(
                        format!("  ✗ {}", err),
                        Style::default().fg(theme::role(Role::Error)),
                    )));
                    lines.push(Line::from(Span::styled(
                        "  Press Enter to try again",
                        Style::default()
                            .fg(theme::role(Role::Gray))
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }
        }
    } else if wizard.ps.provider_id() == "codex" {
        // Codex OAuth — device-code flow
        use crate::tui::onboarding::CodexDeviceFlowStatus;

        if wizard.ps.has_existing_key_sentinel() {
            // Already authenticated
            lines.push(Line::from(Span::styled(
                "  ● Authenticated with Codex (OpenAI)",
                Style::default().fg(theme::role(Role::Success)),
            )));
            lines.push(Line::from(Span::styled(
                "  Press Enter to continue, or re-authenticate below",
                Style::default()
                    .fg(theme::role(Role::Gray))
                    .add_modifier(Modifier::ITALIC),
            )));
        } else {
            match &wizard.ps.codex_device_flow_status {
                CodexDeviceFlowStatus::Idle => {
                    lines.push(Line::from(Span::styled(
                        "  Uses your OpenAI Codex subscription (no API charges)",
                        Style::default()
                            .fg(theme::role(Role::Gray))
                            .add_modifier(Modifier::ITALIC),
                    )));
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "  Press Enter to sign in with OpenAI",
                        Style::default()
                            .fg(brand_blue())
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                CodexDeviceFlowStatus::WaitingForUser => {
                    lines.push(Line::from(Span::styled(
                        "  1. Go to: https://auth.openai.com/codex/device",
                        Style::default()
                            .fg(brand_blue())
                            .add_modifier(Modifier::BOLD),
                    )));
                    if let Some(ref code) = wizard.ps.codex_user_code {
                        lines.push(Line::from(Span::styled(
                            format!("  2. Enter code: {}", code),
                            Style::default()
                                .fg(theme::role(Role::Warning))
                                .add_modifier(Modifier::BOLD),
                        )));
                    }
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "  Waiting for authorization...",
                        Style::default()
                            .fg(theme::role(Role::Gray))
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
                CodexDeviceFlowStatus::Complete => {
                    lines.push(Line::from(Span::styled(
                        "  ● Authenticated successfully!",
                        Style::default()
                            .fg(theme::role(Role::Success))
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                CodexDeviceFlowStatus::Failed(err) => {
                    lines.push(Line::from(Span::styled(
                        format!("  ✗ {}", err),
                        Style::default().fg(theme::role(Role::Error)),
                    )));
                    lines.push(Line::from(Span::styled(
                        "  Press Enter to try again",
                        Style::default()
                            .fg(theme::role(Role::Gray))
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }
        }
    } else {
        // Show help text for selected provider
        let provider = wizard.ps.current_provider();
        for help_line in provider.help_lines {
            lines.push(Line::from(Span::styled(
                format!("  {}", help_line),
                Style::default()
                    .fg(theme::role(Role::Gray))
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        lines.push(Line::from(""));

        // z.ai GLM endpoint type toggle (api vs coding) — BEFORE API key
        if wizard.ps.provider_id() == "zai" {
            let et_focused = wizard.auth_field == AuthField::ZhipuEndpointType;
            let api_marker = if wizard.ps.zhipu_endpoint_type == 0 {
                "[*]"
            } else {
                "[ ]"
            };
            let coding_marker = if wizard.ps.zhipu_endpoint_type == 1 {
                "[*]"
            } else {
                "[ ]"
            };
            lines.push(Line::from(Span::styled(
                "  Endpoint Type:",
                Style::default().fg(if et_focused {
                    brand_blue()
                } else {
                    theme::role(Role::Gray)
                }),
            )));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {} General API  ", api_marker),
                    Style::default().fg(if et_focused && wizard.ps.zhipu_endpoint_type == 0 {
                        theme::role(Role::AccentSoft)
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
                Span::styled(
                    format!("{} Coding API", coding_marker),
                    Style::default().fg(if et_focused && wizard.ps.zhipu_endpoint_type == 1 {
                        theme::role(Role::AccentSoft)
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
            ]));
            lines.push(Line::from(""));
        }

        // Xiaomi MiMo endpoint type toggle (api vs token-plan) — BEFORE API key
        if wizard.ps.provider_id() == "xiaomi" {
            let et_focused = wizard.auth_field == AuthField::XiaomiEndpointType;
            let api_marker = if wizard.ps.xiaomi_endpoint_type == 0 {
                "[*]"
            } else {
                "[ ]"
            };
            let tp_marker = if wizard.ps.xiaomi_endpoint_type == 1 {
                "[*]"
            } else {
                "[ ]"
            };
            lines.push(Line::from(Span::styled(
                "  Endpoint Type:",
                Style::default().fg(if et_focused {
                    brand_blue()
                } else {
                    theme::role(Role::Gray)
                }),
            )));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {} API  ", api_marker),
                    Style::default().fg(if et_focused && wizard.ps.xiaomi_endpoint_type == 0 {
                        theme::role(Role::AccentSoft)
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
                Span::styled(
                    format!("{} Token Plan", tp_marker),
                    Style::default().fg(if et_focused && wizard.ps.xiaomi_endpoint_type == 1 {
                        theme::role(Role::AccentSoft)
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
            ]));
            lines.push(Line::from(""));
        }

        // OpenCode plan toggle (Go or Zen) — BEFORE API key (#2040)
        if wizard.ps.provider_id() == "opencode" {
            let et_focused = wizard.auth_field == AuthField::OpenCodeEndpointType;
            let go_marker = if wizard.ps.opencode_endpoint_type == 0 {
                "[*]"
            } else {
                "[ ]"
            };
            let zen_marker = if wizard.ps.opencode_endpoint_type == 1 {
                "[*]"
            } else {
                "[ ]"
            };
            lines.push(Line::from(Span::styled(
                "  Plan:",
                Style::default().fg(if et_focused {
                    brand_blue()
                } else {
                    theme::role(Role::Gray)
                }),
            )));
            lines.push(Line::from(Span::styled(
                format!("    {go_marker} Go    {zen_marker} Zen"),
                Style::default().fg(if et_focused {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                }),
            )));
            lines.push(Line::from(""));
        }

        // Moonshot AI plan toggle (api vs coding) — BEFORE API key
        if wizard.ps.provider_id() == "moonshot" {
            let et_focused = wizard.auth_field == AuthField::MoonshotEndpointType;
            let api_marker = if wizard.ps.moonshot_endpoint_type == 0 {
                "[*]"
            } else {
                "[ ]"
            };
            let coding_marker = if wizard.ps.moonshot_endpoint_type == 1 {
                "[*]"
            } else {
                "[ ]"
            };
            lines.push(Line::from(Span::styled(
                "  Plan:",
                Style::default().fg(if et_focused {
                    brand_blue()
                } else {
                    theme::role(Role::Gray)
                }),
            )));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {} API plan (platform.moonshot.ai)  ", api_marker),
                    Style::default().fg(if et_focused && wizard.ps.moonshot_endpoint_type == 0 {
                        theme::role(Role::AccentSoft)
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
                Span::styled(
                    format!(
                        "{} Coding plan (api.kimi.com — token subscription)",
                        coding_marker
                    ),
                    Style::default().fg(if et_focused && wizard.ps.moonshot_endpoint_type == 1 {
                        theme::role(Role::AccentSoft)
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
            ]));
            lines.push(Line::from(""));

            // Coding plan carries a subscription tier that sets the context
            // window (256K on moderato, up to 1M on allegretto and above).
            if wizard.ps.moonshot_endpoint_type == 1 {
                let tier_focused = wizard.auth_field == AuthField::MoonshotPlan;
                lines.push(Line::from(Span::styled(
                    "  Plan tier (sets context window):",
                    Style::default().fg(if tier_focused {
                        brand_blue()
                    } else {
                        theme::role(Role::Gray)
                    }),
                )));
                let mut tier_spans: Vec<Span> = Vec::new();
                for (i, tier) in crate::brain::provider::kimi_plan::PLAN_TIERS
                    .iter()
                    .enumerate()
                {
                    let marker = if wizard.ps.moonshot_plan == i {
                        "[*]"
                    } else {
                        "[ ]"
                    };
                    let window = if *tier == "moderato" { "256K" } else { "1M" };
                    tier_spans.push(Span::styled(
                        format!("    {} {} ({})  ", marker, tier, window),
                        Style::default().fg(if tier_focused && wizard.ps.moonshot_plan == i {
                            theme::role(Role::AccentSoft)
                        } else {
                            theme::role(Role::Gray)
                        }),
                    ));
                }
                lines.push(Line::from(tier_spans));
                lines.push(Line::from(""));
            }
        }

        // CLI providers have no API key — skip the field
        if !wizard.ps.is_cli() {
            let key_focused = wizard.auth_field == AuthField::ApiKey;
            let key_label = provider.key_label;
            let (masked_key, key_hint) = if wizard.ps.has_existing_key_sentinel() {
                (
                    "**************************".to_string(),
                    " (already configured, type to replace)".to_string(),
                )
            } else if wizard.ps.api_key_input.is_empty() {
                (
                    format!("enter your {}", key_label.to_lowercase()),
                    String::new(),
                )
            } else {
                (
                    "*".repeat(wizard.ps.api_key_input.len().min(30)),
                    String::new(),
                )
            };
            let cursor = if key_focused && !wizard.ps.has_existing_key_sentinel() {
                "█"
            } else {
                ""
            };
            if key_focused {
                focused_line = lines.len();
            }

            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}: ", key_label),
                    Style::default().fg(if key_focused {
                        brand_blue()
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
                Span::styled(
                    format!("{}{}", masked_key, cursor),
                    Style::default().fg(if wizard.ps.has_existing_key_sentinel() {
                        theme::role(Role::AccentTeal)
                    } else if key_focused {
                        theme::role(Role::AccentSoft)
                    } else {
                        theme::role(Role::Gray)
                    }),
                ),
            ]));

            if !key_hint.is_empty() && key_focused {
                lines.push(Line::from(Span::styled(
                    format!("  {}", key_hint.trim()),
                    Style::default()
                        .fg(theme::role(Role::Gray))
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }
    }

    // Model selection — shared across all non-custom providers (including GitHub)
    if !is_custom {
        let model_focused = wizard.auth_field == AuthField::Model;
        let model_count = wizard.ps.model_count();
        // Drawn whenever the field is focused, even with nothing listed: a
        // provider whose fetch came back empty still needs a model, and the
        // typed name is the only way to give it one.
        if model_count > 0 || wizard.ps.models_fetching || model_focused {
            lines.push(Line::from(""));
            // Record scroll anchor: 2 lines above the Model: label so the key
            // line stays visible as context when scrolling into the model section.
            if model_focused {
                focused_line = lines.len().saturating_sub(1);
            }
            let label = if wizard.ps.models_fetching {
                "  Model: (fetching...)".to_string()
            } else {
                "  Model:".to_string()
            };
            lines.push(Line::from(Span::styled(
                label,
                Style::default().fg(if model_focused {
                    brand_blue()
                } else {
                    theme::role(Role::Gray)
                }),
            )));

            // Helper: render a windowed slice of models, keeping selection visible
            let render_model_window = |lines: &mut Vec<Line<'static>>,
                                       models: &[&str],
                                       selected: usize,
                                       focused: bool| {
                let total = models.len();
                let (start, end) = visible_window(total, selected, list_rows);
                if start > 0 {
                    lines.push(Line::from(Span::styled(
                        format!("  ↑ {} more", start),
                        Style::default().fg(theme::role(Role::Gray)),
                    )));
                }
                for (offset, model) in models[start..end].iter().enumerate() {
                    let i = start + offset;
                    let is_sel = i == selected;
                    let prefix = if is_sel && focused { " > " } else { "   " };
                    let marker = if is_sel { "(*)" } else { "( )" };
                    let label =
                        crate::tui::provider_selector::model_display_label(model).to_string();
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {}{} ", prefix, marker),
                            Style::default().fg(if is_sel {
                                accent_gold()
                            } else {
                                theme::role(Role::Gray)
                            }),
                        ),
                        Span::styled(
                            label,
                            Style::default().fg(if is_sel {
                                theme::role(Role::AccentSoft)
                            } else {
                                theme::role(Role::Gray)
                            }),
                        ),
                    ]));
                }
                if end < total {
                    lines.push(Line::from(Span::styled(
                        format!("  ↓ {} more", total - end),
                        Style::default().fg(theme::role(Role::Gray)),
                    )));
                }
            };

            // Filter input stays visible during a fetch so the next search is
            // never blocked by network progress.
            if model_focused {
                let cursor = "█";
                let filter_display = if wizard.ps.model_filter.is_empty() {
                    format!("  / type to filter…{}", cursor)
                } else {
                    format!("  / {}{}", wizard.ps.model_filter, cursor)
                };
                lines.push(Line::from(Span::styled(
                    filter_display,
                    Style::default().fg(if wizard.ps.model_filter.is_empty() {
                        theme::role(Role::Gray)
                    } else {
                        theme::role(Role::AccentSoft)
                    }),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "  / type to filter…".to_string(),
                    Style::default().fg(theme::role(Role::GrayDim)),
                )));
            }

            if !wizard.ps.models_fetching {
                let filtered = wizard.ps.filtered_model_names();
                if filtered.is_empty() {
                    // No suggestion match for the typed filter. Tell the
                    // user the typed text will become the model name on
                    // Enter — the "type a custom model" escape hatch for
                    // new releases not yet in the suggestion list (e.g.
                    // `MiniMax-M3` typed on a build that ships M2.7/M2.5/
                    // M2.1 as suggestions). Without this hint the user
                    // sees "no models match" and assumes they need to
                    // wait for a binary update.
                    let typed = wizard.ps.model_filter.trim();
                    if !typed.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("  > {} (custom — press Enter to use)", typed),
                            Style::default().fg(brand_gold()),
                        )));
                    } else {
                        lines.push(Line::from(Span::styled(
                            if model_count == 0 {
                                "  no models listed, type the model name".to_string()
                            } else {
                                "  no models match".to_string()
                            },
                            Style::default()
                                .fg(theme::role(Role::Gray))
                                .add_modifier(Modifier::ITALIC),
                        )));
                    }
                } else {
                    render_model_window(lines, &filtered, wizard.ps.selected_model, model_focused);
                }
            }
        }
    }
    focused_line
}

fn render_workspace(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) {
    let path_focused = wizard.focused_field == 0;
    let seed_focused = wizard.focused_field == 1;

    let cursor = if path_focused { "█" } else { "" };
    lines.push(Line::from(vec![
        Span::styled(
            "  Path: ",
            Style::default().fg(if path_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", wizard.workspace_path, cursor),
            Style::default().fg(if path_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));
    lines.push(Line::from(""));

    lines.push(Line::from(vec![
        Span::styled(
            if seed_focused { " > " } else { "   " },
            Style::default().fg(accent_gold()),
        ),
        Span::styled(
            if wizard.seed_templates { "[x]" } else { "[ ]" },
            Style::default().fg(if wizard.seed_templates {
                brand_gold()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            " Seed template files",
            Style::default().fg(if seed_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    lines.push(Line::from(Span::styled(
        "       SOUL.md, USER.md, ...",
        Style::default().fg(theme::role(Role::Gray)),
    )));
}

fn render_channels(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) -> usize {
    lines.push(Line::from(Span::styled(
        "  Pick your channels (Space to toggle):",
        Style::default().fg(theme::role(Role::Gray)),
    )));
    lines.push(Line::from(""));

    let mut focused_line = 0;
    for (i, (name, enabled)) in wizard.channel_toggles.iter().enumerate() {
        let focused = i == wizard.focused_field;
        if focused {
            focused_line = lines.len();
        }
        let prefix = if focused { " > " } else { "   " };
        let marker = if *enabled { "[x]" } else { "[ ]" };
        // Get the description from CHANNEL_NAMES
        let desc = CHANNEL_NAMES.get(i).map(|(_, d)| *d).unwrap_or("");

        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(accent_gold())),
            Span::styled(
                marker,
                Style::default().fg(if *enabled {
                    brand_gold()
                } else {
                    theme::role(Role::Gray)
                }),
            ),
            Span::styled(
                format!(" {}", name),
                Style::default()
                    .fg(if focused {
                        theme::role(Role::AccentSoft)
                    } else {
                        theme::role(Role::Gray)
                    })
                    .add_modifier(if focused {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!("       {}", desc),
            Style::default().fg(theme::role(Role::Gray)),
        )));
    }

    // "Continue" button at the bottom
    let continue_focused = wizard.focused_field >= wizard.channel_toggles.len();
    if continue_focused {
        focused_line = lines.len() + 1; // +1 for the blank line
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            if continue_focused { " > " } else { "   " },
            Style::default().fg(accent_gold()),
        ),
        Span::styled(
            "Continue →",
            Style::default()
                .fg(if continue_focused {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                })
                .add_modifier(if continue_focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ]));
    lines.push(Line::from(""));
    focused_line
}

fn render_telegram_setup(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) -> usize {
    let base = lines.len();
    // Help text
    lines.push(Line::from(Span::styled(
        "  1. Open Telegram, search @BotFather",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  2. Send /newbot, follow the prompts",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  3. Copy the bot token and paste below",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(""));

    // Bot token input
    let token_focused = wizard.telegram_field == TelegramField::BotToken;
    let (masked_token, token_hint) = if wizard.has_existing_telegram_token() {
        (
            "**************************".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.telegram_token_input.is_empty() {
        ("paste your bot token".to_string(), String::new())
    } else {
        (
            "*".repeat(wizard.telegram_token_input.len().min(30)),
            String::new(),
        )
    };
    let cursor = if token_focused && !wizard.has_existing_telegram_token() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Bot Token: ",
            Style::default().fg(if token_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", masked_token, cursor),
            Style::default().fg(if wizard.has_existing_telegram_token() {
                theme::role(Role::AccentTeal)
            } else if token_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !token_hint.is_empty() && token_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", token_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // User ID input — NOT a secret, always show the real value.
    let uid_focused = wizard.telegram_field == TelegramField::UserID;
    let uid_display = if wizard.telegram_user_id_input.is_empty() {
        "your numeric user ID (required)".to_string()
    } else {
        wizard.telegram_user_id_input.clone()
    };
    let uid_cursor = if uid_focused { "\u{2588}" } else { "" };

    lines.push(Line::from(vec![
        Span::styled(
            "  User ID:   ",
            Style::default().fg(if uid_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", uid_display, uid_cursor),
            Style::default().fg(if uid_focused {
                theme::role(Role::AccentSoft)
            } else if wizard.telegram_user_id_input.is_empty() {
                theme::role(Role::Gray)
            } else {
                theme::role(Role::AccentTeal)
            }),
        ),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  To get your User ID: open Telegram, message @userinfobot",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  — it replies with your numeric ID. Paste it above.",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  (Leave empty to auto-detect from your messages, or allow any user.)",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));

    lines.push(Line::from(""));
    let rich_focused = wizard.telegram_field == TelegramField::RichText;
    lines.push(Line::from(vec![
        Span::styled(
            format!("  [{}] ", if wizard.telegram_rich_text { "x" } else { " " }),
            Style::default().fg(if rich_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            "Rich text experience",
            Style::default().fg(if rich_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            "  (Space toggles)",
            Style::default().fg(theme::role(Role::GrayDim)),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "      Enable only if you run the latest Telegram app version:",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "      older clients will not render rich messages correctly.",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));

    lines.push(Line::from(""));
    render_respond_to_selector(
        lines,
        wizard.telegram_respond_to,
        wizard.telegram_field == TelegramField::RespondTo,
    );

    // Test status
    render_channel_test_status(lines, wizard);

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Tab/Shift+Tab: nav fields | \u{2190}\u{2192}: cursor | Ctrl+\u{232b}: clear | Enter: confirm",
        Style::default().fg(theme::role(Role::Gray)),
    )));
    // Focused field for scrolling: token=base+4, uid=base+6, respond_to=base+10 (approx)
    let offset = match wizard.telegram_field {
        TelegramField::BotToken => 4,
        TelegramField::UserID => 6,
        TelegramField::RichText => lines.len().saturating_sub(base).saturating_sub(8),
        TelegramField::RespondTo => lines.len().saturating_sub(base).saturating_sub(4),
    };
    base + offset
}

fn render_discord_setup(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) -> usize {
    let base = lines.len();
    // Help text
    lines.push(Line::from(Span::styled(
        "  1. Go to discord.com/developers/applications",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  2. Create app > Bot > Copy token",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  3. Enable Message Content Intent",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(""));

    // Bot token input
    let token_focused = wizard.discord_field == DiscordField::BotToken;
    let (masked_token, token_hint) = if wizard.has_existing_discord_token() {
        (
            "**************************".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.discord_token_input.is_empty() {
        ("paste your bot token".to_string(), String::new())
    } else {
        (
            "*".repeat(wizard.discord_token_input.len().min(30)),
            String::new(),
        )
    };
    let cursor = if token_focused && !wizard.has_existing_discord_token() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Bot Token:   ",
            Style::default().fg(if token_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", masked_token, cursor),
            Style::default().fg(if wizard.has_existing_discord_token() {
                theme::role(Role::AccentTeal)
            } else if token_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !token_hint.is_empty() && token_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", token_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Channel ID input
    let ch_focused = wizard.discord_field == DiscordField::ChannelID;
    let (ch_display, ch_hint) = if wizard.has_existing_discord_channel_id() {
        (
            "**********".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.discord_channel_id_input.is_empty() {
        (
            "right-click channel > Copy Channel ID".to_string(),
            String::new(),
        )
    } else {
        (wizard.discord_channel_id_input.clone(), String::new())
    };
    let ch_cursor = if ch_focused && !wizard.has_existing_discord_channel_id() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Channel ID:  ",
            Style::default().fg(if ch_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", ch_display, ch_cursor),
            Style::default().fg(if wizard.has_existing_discord_channel_id() {
                theme::role(Role::AccentTeal)
            } else if ch_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !ch_hint.is_empty() && ch_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", ch_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Allowed List input (Discord user ID — who the bot replies to)
    let al_focused = wizard.discord_field == DiscordField::AllowedList;
    let (al_display, al_hint) = if wizard.has_existing_discord_allowed_list() {
        (
            "**********".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.discord_allowed_list_input.is_empty() {
        (
            "user ID (optional — empty = reply to all)".to_string(),
            String::new(),
        )
    } else {
        (wizard.discord_allowed_list_input.clone(), String::new())
    };
    let al_cursor = if al_focused && !wizard.has_existing_discord_allowed_list() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Allowed List: ",
            Style::default().fg(if al_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", al_display, al_cursor),
            Style::default().fg(if wizard.has_existing_discord_allowed_list() {
                theme::role(Role::AccentTeal)
            } else if al_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !al_hint.is_empty() && al_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", al_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    lines.push(Line::from(""));
    render_respond_to_selector(
        lines,
        wizard.discord_respond_to,
        wizard.discord_field == DiscordField::RespondTo,
    );

    // Test status
    render_channel_test_status(lines, wizard);

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Tab/Shift+Tab: nav fields | \u{2190}\u{2192}: cursor | Ctrl+\u{232b}: clear | Enter: confirm",
        Style::default().fg(theme::role(Role::Gray)),
    )));
    let offset = match wizard.discord_field {
        DiscordField::BotToken => 4,
        DiscordField::ChannelID => 6,
        DiscordField::AllowedList => 8,
        DiscordField::RespondTo => lines.len().saturating_sub(base).saturating_sub(4),
    };
    base + offset
}

fn render_whatsapp_setup(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) -> usize {
    use crate::tui::onboarding::WhatsAppField;
    let base = lines.len();

    // Connection section
    let conn_focused = wizard.whatsapp_field == WhatsAppField::Connection;
    if wizard.whatsapp_connected {
        lines.push(Line::from(Span::styled(
            "  WhatsApp connected!",
            Style::default()
                .fg(theme::role(Role::AccentTeal))
                .add_modifier(Modifier::BOLD),
        )));
    } else if wizard.whatsapp_qr_text.is_some() {
        // QR is rendered as a full-screen popup overlay — just show a placeholder here
        lines.push(Line::from(Span::styled(
            "  QR code displayed — scan with WhatsApp",
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
        lines.push(Line::from(Span::styled(
            "  Waiting for scan...",
            Style::default().fg(brand_gold()),
        )));
    } else if wizard.whatsapp_connecting {
        lines.push(Line::from(Span::styled(
            "  Starting WhatsApp connection...",
            Style::default().fg(theme::role(Role::Gray)),
        )));
    } else if let Some(ref err) = wizard.whatsapp_error {
        lines.push(Line::from(Span::styled(
            format!("  Error: {}", err),
            Style::default().fg(theme::role(Role::Error)),
        )));
        lines.push(Line::from(Span::styled(
            "  Logs: ~/.opencrabs/logs/",
            Style::default().fg(theme::role(Role::Gray)),
        )));
        lines.push(Line::from(""));
        if conn_focused {
            lines.push(Line::from(Span::styled(
                "  Press Enter to retry, 'R' to reset session, or 'S' to skip",
                Style::default().fg(theme::role(Role::Gray)),
            )));
        }
    } else if conn_focused {
        let session_db = crate::config::opencrabs_home()
            .join("whatsapp")
            .join("session.db");
        if session_db.exists() {
            lines.push(Line::from(Span::styled(
                "  Previously connected  ·  Press R to reset and re-pair",
                Style::default().fg(theme::role(Role::Gray)),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "  Press Enter to show QR code",
                Style::default().fg(theme::role(Role::Gray)),
            )));
        }
    }

    lines.push(Line::from(""));

    // Phone allowlist field
    let phone_focused = wizard.whatsapp_field == WhatsAppField::PhoneAllowlist;
    let phone_display = if wizard.has_existing_whatsapp_phone() {
        "**********".to_string()
    } else if wizard.whatsapp_phone_input.is_empty() {
        "+15551234567".to_string()
    } else {
        wizard.whatsapp_phone_input.clone()
    };
    let phone_cursor = if phone_focused && !wizard.has_existing_whatsapp_phone() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Allowed Phone: ",
            Style::default().fg(if phone_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", phone_display, phone_cursor),
            Style::default().fg(if wizard.has_existing_whatsapp_phone() {
                theme::role(Role::AccentTeal)
            } else if phone_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if wizard.has_existing_whatsapp_phone() && phone_focused {
        lines.push(Line::from(Span::styled(
            "  Type a new number to replace, or press Enter to keep existing",
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    } else if wizard.whatsapp_phone_input.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Optional — leave empty to allow all numbers",
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Test status (shown after phone is confirmed and test fires)
    render_channel_test_status(lines, wizard);

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Tab/Shift+Tab: nav fields | \u{2190}\u{2192}: cursor | Ctrl+\u{232b}: clear | Enter: confirm | S: skip",
        Style::default().fg(theme::role(Role::Gray)),
    )));
    let offset = match wizard.whatsapp_field {
        WhatsAppField::Connection => 0,
        WhatsAppField::PhoneAllowlist => lines.len().saturating_sub(base).saturating_sub(6),
    };
    base + offset
}

fn render_slack_setup(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) -> usize {
    let base = lines.len();
    // Help text
    lines.push(Line::from(Span::styled(
        "  1. Go to api.slack.com/apps > Create App",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  2. OAuth > Bot Token Scopes: chat:write, channels:history,",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "     groups:history, im:history, mpim:history, users:read,",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "     files:read, files:write, reactions:write, app_mentions:read",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  3. Enable Socket Mode > copy App Token (xapp-...)",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  4. Install App to Workspace > copy Bot Token (xoxb-...)",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(""));

    // Bot token input
    let bot_focused = wizard.slack_field == SlackField::BotToken;
    let (masked_bot, bot_hint) = if wizard.has_existing_slack_bot_token() {
        (
            "**************************".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.slack_bot_token_input.is_empty() {
        ("xoxb-...".to_string(), String::new())
    } else {
        (
            "*".repeat(wizard.slack_bot_token_input.len().min(30)),
            String::new(),
        )
    };
    let cursor_b = if bot_focused && !wizard.has_existing_slack_bot_token() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Bot Token: ",
            Style::default().fg(if bot_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", masked_bot, cursor_b),
            Style::default().fg(if wizard.has_existing_slack_bot_token() {
                theme::role(Role::AccentTeal)
            } else if bot_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !bot_hint.is_empty() && bot_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", bot_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // App token input
    let app_focused = wizard.slack_field == SlackField::AppToken;
    let (masked_app, app_hint) = if wizard.has_existing_slack_app_token() {
        (
            "**************************".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.slack_app_token_input.is_empty() {
        ("xapp-...".to_string(), String::new())
    } else {
        (
            "*".repeat(wizard.slack_app_token_input.len().min(30)),
            String::new(),
        )
    };
    let cursor_a = if app_focused && !wizard.has_existing_slack_app_token() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  App Token: ",
            Style::default().fg(if app_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", masked_app, cursor_a),
            Style::default().fg(if wizard.has_existing_slack_app_token() {
                theme::role(Role::AccentTeal)
            } else if app_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !app_hint.is_empty() && app_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", app_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Channel ID input
    let ch_focused = wizard.slack_field == SlackField::ChannelID;
    let (ch_display, ch_hint) = if wizard.has_existing_slack_channel_id() {
        (
            "**********".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.slack_channel_id_input.is_empty() {
        ("C12345678".to_string(), String::new())
    } else {
        (wizard.slack_channel_id_input.clone(), String::new())
    };
    let ch_cursor = if ch_focused && !wizard.has_existing_slack_channel_id() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Channel ID: ",
            Style::default().fg(if ch_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", ch_display, ch_cursor),
            Style::default().fg(if wizard.has_existing_slack_channel_id() {
                theme::role(Role::AccentTeal)
            } else if ch_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !ch_hint.is_empty() && ch_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", ch_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Allowed List input (Slack user ID — who the bot replies to)
    let al_focused = wizard.slack_field == SlackField::AllowedList;
    let (al_display, al_hint) = if wizard.has_existing_slack_allowed_list() {
        (
            "**********".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.slack_allowed_list_input.is_empty() {
        (
            "U12345678 (optional — empty = reply to all)".to_string(),
            String::new(),
        )
    } else {
        (wizard.slack_allowed_list_input.clone(), String::new())
    };
    let al_cursor = if al_focused && !wizard.has_existing_slack_allowed_list() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Allowed List: ",
            Style::default().fg(if al_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", al_display, al_cursor),
            Style::default().fg(if wizard.has_existing_slack_allowed_list() {
                theme::role(Role::AccentTeal)
            } else if al_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !al_hint.is_empty() && al_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", al_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    lines.push(Line::from(""));
    render_respond_to_selector(
        lines,
        wizard.slack_respond_to,
        wizard.slack_field == SlackField::RespondTo,
    );

    // Test status
    render_channel_test_status(lines, wizard);

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Tab/Shift+Tab: nav fields | \u{2190}\u{2192}: cursor | Ctrl+\u{232b}: clear | Enter: confirm",
        Style::default().fg(theme::role(Role::Gray)),
    )));
    let offset = match wizard.slack_field {
        SlackField::BotToken => 4,
        SlackField::AppToken => 6,
        SlackField::ChannelID => 8,
        SlackField::AllowedList => 10,
        SlackField::RespondTo => lines.len().saturating_sub(base).saturating_sub(4),
    };
    base + offset
}

/// Render respond_to selector: `  Respond to: [ all ]  dm_only  mention`
/// `selected` = 0..2, `focused` = whether this field has keyboard focus.
fn render_respond_to_selector(lines: &mut Vec<Line<'static>>, selected: usize, focused: bool) {
    const OPTIONS: [&str; 3] = ["all", "dm_only", "mention"];
    let label_style = Style::default().fg(if focused {
        brand_blue()
    } else {
        theme::role(Role::Gray)
    });
    let mut spans: Vec<Span<'static>> = vec![Span::styled("  Respond to: ", label_style)];
    for (i, opt) in OPTIONS.iter().enumerate() {
        let is_sel = i == selected;
        let (prefix, suffix) = if is_sel { ("[", "]") } else { (" ", " ") };
        let style = if is_sel && focused {
            Style::default()
                .fg(brand_gold())
                .add_modifier(Modifier::BOLD)
        } else if is_sel {
            Style::default().fg(theme::role(Role::AccentTeal))
        } else {
            Style::default().fg(theme::role(Role::Gray))
        };
        spans.push(Span::styled(format!("{}{}{}", prefix, opt, suffix), style));
        if i < OPTIONS.len() - 1 {
            spans.push(Span::styled("  ", Style::default()));
        }
    }
    lines.push(Line::from(spans));
    if focused {
        lines.push(Line::from(Span::styled(
            "  ← → to change",
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }
}

/// Render the channel test connection status line (shared by Telegram/Discord/Slack)
fn render_channel_test_status(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) {
    match &wizard.channel_test_status {
        ChannelTestStatus::Idle => {}
        ChannelTestStatus::Testing => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Testing connection...",
                Style::default().fg(brand_gold()),
            )));
        }
        ChannelTestStatus::Success => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Connected! Press Enter to continue",
                Style::default()
                    .fg(theme::role(Role::AccentTeal))
                    .add_modifier(Modifier::BOLD),
            )));
        }
        ChannelTestStatus::Failed(err) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  Error: {}", err),
                Style::default().fg(theme::role(Role::Error)),
            )));
            lines.push(Line::from(Span::styled(
                "  Enter to retry | S to skip",
                Style::default().fg(theme::role(Role::Gray)),
            )));
        }
    }
}

fn render_image_setup(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) {
    // Provider row. Keep it short so it never wraps inside the box — the model
    // name is shown again in the "Generation model" row below, so it's omitted
    // here (it used to push "🍌 Nano Banana" onto a wrapped second line).
    lines.push(Line::from(vec![
        Span::styled(
            "  Provider: ".to_string(),
            Style::default().fg(theme::role(Role::Gray)),
        ),
        Span::styled(
            "[ Google ]".to_string(),
            Style::default()
                .fg(brand_gold())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  🍌 Nano Banana".to_string(),
            Style::default().fg(brand_gold()),
        ),
    ]));
    lines.push(Line::from(""));

    // Contextual key hints — kept short so they don't wrap onto the box border.
    let hint_text = match wizard.image_field {
        ImageField::VisionToggle | ImageField::GenerationToggle => {
            "  Space toggle · Tab/Enter next · Esc back"
        }
        ImageField::GenerationModel => "  Type to override · Tab/Enter next · Esc back",
        ImageField::ApiKey => "  Enter next · BackTab/Esc back",
    };
    lines.push(Line::from(Span::styled(
        hint_text,
        Style::default().fg(theme::role(Role::Gray)),
    )));
    lines.push(Line::from(""));

    // Vision toggle
    let vision_focused = wizard.image_field == ImageField::VisionToggle;
    lines.push(Line::from(vec![
        Span::styled(
            if vision_focused { " > " } else { "   " },
            Style::default().fg(accent_gold()),
        ),
        Span::styled(
            if wizard.image_vision_enabled {
                "[x]".to_string()
            } else {
                "[ ]".to_string()
            },
            Style::default().fg(if wizard.image_vision_enabled {
                brand_gold()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            " Vision Analysis",
            Style::default()
                .fg(if vision_focused {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                })
                .add_modifier(if vision_focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "        analyze images the agent receives",
        Style::default().fg(theme::role(Role::Gray)),
    )));

    // Generation toggle
    let gen_focused = wizard.image_field == ImageField::GenerationToggle;
    lines.push(Line::from(vec![
        Span::styled(
            if gen_focused { " > " } else { "   " },
            Style::default().fg(accent_gold()),
        ),
        Span::styled(
            if wizard.image_generation_enabled {
                "[x]".to_string()
            } else {
                "[ ]".to_string()
            },
            Style::default().fg(if wizard.image_generation_enabled {
                brand_gold()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            " Image Generation",
            Style::default()
                .fg(if gen_focused {
                    theme::role(Role::AccentSoft)
                } else {
                    theme::role(Role::Gray)
                })
                .add_modifier(if gen_focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "        generate images from text prompts",
        Style::default().fg(theme::role(Role::Gray)),
    )));

    // Generation model override input — only meaningful when generation
    // is enabled. Empty input keeps the seeded
    // `gemini-3.1-flash-image-preview` default; populated value writes
    // through to `image.generation.model`.
    if wizard.image_generation_enabled {
        lines.push(Line::from(""));
        let model_focused = wizard.image_field == ImageField::GenerationModel;
        let (display, dim) = if wizard.image_generation_model_input.is_empty() {
            ("gemini-3.1-flash-image-preview (default)".to_string(), true)
        } else {
            (wizard.image_generation_model_input.clone(), false)
        };
        let cursor = if model_focused { "█" } else { "" };
        // Label on its own line, value indented below it — the model name is
        // long enough that "label: value" wrapped onto the box border.
        lines.push(Line::from(Span::styled(
            "  Generation model",
            Style::default().fg(if model_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        )));
        lines.push(Line::from(Span::styled(
            format!("    {}{}", display, cursor),
            Style::default().fg(if dim {
                theme::role(Role::Gray)
            } else if model_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::AccentTeal)
            }),
        )));
    }

    // API key field (only when either is enabled)
    if wizard.image_vision_enabled || wizard.image_generation_enabled {
        lines.push(Line::from(""));

        let key_focused = wizard.image_field == ImageField::ApiKey;
        let (masked_key, key_hint) = if wizard.has_existing_image_key() {
            (
                "**************************".to_string(),
                " (key already set)".to_string(),
            )
        } else if wizard.image_api_key_input.is_empty() {
            (
                "paste key from aistudio.google.com".to_string(),
                String::new(),
            )
        } else {
            (
                "*".repeat(wizard.image_api_key_input.len().min(30)),
                String::new(),
            )
        };
        let cursor = if key_focused && !wizard.has_existing_image_key() {
            "█"
        } else {
            ""
        };

        lines.push(Line::from(Span::styled(
            "  Google API Key",
            Style::default().fg(if key_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        )));
        lines.push(Line::from(Span::styled(
            format!("    {}{}", masked_key, cursor),
            Style::default().fg(if wizard.has_existing_image_key() {
                theme::role(Role::AccentTeal)
            } else if key_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        )));

        if !key_hint.is_empty() && key_focused {
            lines.push(Line::from(Span::styled(
                format!("  {}", key_hint.trim()),
                Style::default()
                    .fg(theme::role(Role::Gray))
                    .add_modifier(Modifier::ITALIC),
            )));
        }

        // "get your key" hint — shown whenever key isn't set yet
        if !wizard.has_existing_image_key() {
            lines.push(Line::from(vec![
                Span::styled(
                    "  Get a free key at ",
                    Style::default().fg(theme::role(Role::Gray)),
                ),
                Span::styled(
                    "aistudio.google.com",
                    Style::default()
                        .fg(brand_blue())
                        .add_modifier(Modifier::UNDERLINED),
                ),
                Span::styled(
                    "  →  Google AI Studio",
                    Style::default().fg(theme::role(Role::Gray)),
                ),
            ]));
        }
    }
}

fn render_voice_setup(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) {
    super::onboarding::voice::render(lines, wizard);
}

fn render_daemon(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) {
    let platform = if cfg!(target_os = "linux") {
        "systemd user unit"
    } else if cfg!(target_os = "macos") {
        "LaunchAgent"
    } else {
        "background service"
    };

    lines.push(Line::from(Span::styled(
        format!("  Install as {} ?", platform),
        Style::default().fg(theme::role(Role::Gray)),
    )));
    lines.push(Line::from(""));

    let yes_selected = wizard.install_daemon;
    lines.push(Line::from(vec![
        Span::styled(
            if yes_selected { " > " } else { "   " },
            Style::default().fg(accent_gold()),
        ),
        Span::styled(
            if yes_selected { "(*)" } else { "( )" },
            Style::default().fg(if yes_selected {
                brand_gold()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            " Yes, install daemon",
            Style::default().fg(if yes_selected {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled(
            if !yes_selected { " > " } else { "   " },
            Style::default().fg(accent_gold()),
        ),
        Span::styled(
            if !yes_selected { "(*)" } else { "( )" },
            Style::default().fg(if !yes_selected {
                brand_gold()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            " Skip for now",
            Style::default().fg(if !yes_selected {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));
}

fn render_health_check(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) {
    for (name, status) in &wizard.health_results {
        let (icon, color) = match status {
            HealthStatus::Pending => ("...", theme::role(Role::Gray)),
            HealthStatus::Running => ("...", accent_gold()),
            HealthStatus::Pass => ("OK", theme::role(Role::AccentTeal)),
            HealthStatus::Fail(_) => ("FAIL", theme::role(Role::Error)),
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("  [{:<4}] ", icon),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                name.clone(),
                Style::default().fg(theme::role(Role::AccentSoft)),
            ),
        ]));

        if let HealthStatus::Fail(reason) = status {
            lines.push(Line::from(Span::styled(
                format!("          {}", reason),
                Style::default().fg(theme::role(Role::Error)),
            )));
        }
    }

    lines.push(Line::from(""));

    if wizard.health_complete {
        if wizard.all_health_passed() {
            lines.push(Line::from(Span::styled(
                "  All checks passed!".to_string(),
                Style::default()
                    .fg(theme::role(Role::AccentTeal))
                    .add_modifier(Modifier::BOLD),
            )));
            if !wizard.quick_jump {
                lines.push(Line::from(Span::styled(
                    "  Press Enter to finish setup".to_string(),
                    Style::default().fg(theme::role(Role::Gray)),
                )));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "  Some checks failed.".to_string(),
                Style::default().fg(theme::role(Role::Error)),
            )));
            if !wizard.quick_jump {
                lines.push(Line::from(vec![
                    Span::styled(
                        "  [R] ",
                        Style::default()
                            .fg(brand_blue())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "Re-run  ",
                        Style::default().fg(theme::role(Role::AccentSoft)),
                    ),
                    Span::styled(
                        "[Esc] ",
                        Style::default()
                            .fg(theme::role(Role::Error))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "Go back and fix",
                        Style::default().fg(theme::role(Role::AccentSoft)),
                    ),
                ]));
            }
        }
    }
}

fn render_brain_setup(
    lines: &mut Vec<Line<'static>>,
    wizard: &OnboardingWizard,
    wrap_width: usize,
) {
    // Show generating state
    if wizard.brain_generating {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Cooking up your brain files...".to_string(),
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        )));
        lines.push(Line::from(Span::styled(
            "  Your agent is getting to know you".to_string(),
            Style::default().fg(theme::role(Role::Gray)),
        )));
        return;
    }

    // Show success state
    if wizard.brain_generated {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Brain files locked in!".to_string(),
            Style::default()
                .fg(theme::role(Role::AccentTeal))
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "  Your agent knows the deal now".to_string(),
            Style::default().fg(theme::role(Role::Gray)),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Press Enter to finish setup".to_string(),
            Style::default().fg(theme::role(Role::Gray)),
        )));
        return;
    }

    // Show error state (with fallback notice)
    if let Some(ref err) = wizard.brain_error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {} — rolling with defaults", err),
            Style::default().fg(theme::role(Role::Accent)),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Press Enter to continue".to_string(),
            Style::default().fg(theme::role(Role::Gray)),
        )));
        return;
    }

    // Preview block intentionally omitted from the editing view — the
    // auto-formatted markdown is only built at the moment generation kicks
    // off (via `normalize_brain_inputs`) and is surfaced by the generating /
    // generated states above. Showing it here would conflict with the
    // live-edit text areas below and has no actionable UI left.

    // "About You" text area
    let me_focused = wizard.brain_field == BrainField::AboutMe;
    lines.push(Line::from(Span::styled(
        "  About You:".to_string(),
        Style::default()
            .fg(if me_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            })
            .add_modifier(Modifier::BOLD),
    )));

    render_brain_field(
        lines,
        &wizard.about_me,
        me_focused,
        "  name, role, links, projects, whatever you got",
        wrap_width,
    );

    lines.push(Line::from(""));

    // "Your OpenCrabs" text area
    let agent_focused = wizard.brain_field == BrainField::AboutAgent;
    lines.push(Line::from(Span::styled(
        "  Your OpenCrabs:".to_string(),
        Style::default()
            .fg(if agent_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            })
            .add_modifier(Modifier::BOLD),
    )));

    render_brain_field(
        lines,
        &wizard.about_opencrabs,
        agent_focused,
        "  personality, vibe, how I should talk to you",
        wrap_width,
    );

    lines.push(Line::from(""));
    let italic_style = Style::default()
        .fg(theme::role(Role::Gray))
        .add_modifier(Modifier::ITALIC);
    for chunk in wrap_text(
        "  The more you drop the better I cover your ass",
        wrap_width,
    ) {
        lines.push(Line::from(Span::styled(chunk, italic_style)));
    }

    // Show loaded hint if brain files exist
    if !wizard.original_about_me.is_empty() || !wizard.original_about_opencrabs.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Loaded from existing brain files".to_string(),
            Style::default().fg(accent_gold()),
        )));
    }
    let hint_style = Style::default().fg(theme::role(Role::Gray));
    for chunk in wrap_text(
        "  Esc to skip · Tab to switch · Enter to generate",
        wrap_width,
    ) {
        lines.push(Line::from(Span::styled(chunk, hint_style)));
    }
}

/// Wrap a string into chunks of max_width display columns
/// Render a brain text field with capped visible lines.
/// Large content shows the last few lines + a "N lines pasted" indicator.
fn render_brain_field(
    lines: &mut Vec<Line<'static>>,
    content: &str,
    focused: bool,
    placeholder: &str,
    wrap_width: usize,
) {
    const MAX_VISIBLE_LINES: usize = 4;

    if content.is_empty() && !focused {
        lines.push(Line::from(Span::styled(
            placeholder.to_string(),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
        return;
    }

    let style = Style::default().fg(if focused {
        theme::role(Role::AccentSoft)
    } else {
        theme::role(Role::Gray)
    });

    let cursor = if focused { "\u{2588}" } else { "" };
    let display = format!("  {}{}", content, cursor);
    let wrapped: Vec<String> = wrap_text(&display, wrap_width);
    let total = wrapped.len();

    if total <= MAX_VISIBLE_LINES {
        for chunk in &wrapped {
            lines.push(Line::from(Span::styled(chunk.clone(), style)));
        }
    } else {
        // Show line count + last few lines so user sees the tail
        let content_lines = content.matches('\n').count() + 1;
        lines.push(Line::from(Span::styled(
            format!("  ({} lines pasted)", content_lines),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
        for chunk in &wrapped[total - MAX_VISIBLE_LINES..] {
            lines.push(Line::from(Span::styled(chunk.clone(), style)));
        }
    }
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let mut result = Vec::new();
    // Split on newlines first, then wrap each line independently
    for line in text.split('\n') {
        if line.width() <= max_width {
            result.push(line.to_string());
            continue;
        }
        let mut remaining = line;
        while !remaining.is_empty() {
            if remaining.width() <= max_width {
                result.push(remaining.to_string());
                break;
            }
            // Find byte index at display width limit
            let byte_limit = super::render::char_boundary_at_width(remaining, max_width);
            // Try to break at a space
            let break_at = remaining[..byte_limit].rfind(' ').unwrap_or(byte_limit);
            let break_at = if break_at == 0 {
                byte_limit.max(remaining.ceil_char_boundary(1))
            } else {
                break_at
            };
            result.push(remaining[..break_at].to_string());
            remaining = remaining[break_at..].trim_start();
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

fn render_complete(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) {
    lines.push(Line::from(""));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Setup complete!".to_string(),
        Style::default()
            .fg(brand_gold())
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Summary
    let provider = &PROVIDERS[wizard.ps.selected_provider.min(PROVIDERS.len() - 1)];
    let provider_label = if wizard.ps.selected_provider >= CUSTOM_PROVIDER_IDX
        && !wizard.ps.custom_name.is_empty()
    {
        wizard.ps.custom_name.clone()
    } else {
        provider.name.to_string()
    };
    lines.push(Line::from(vec![
        Span::styled("  Provider: ", Style::default().fg(theme::role(Role::Gray))),
        Span::styled(
            provider_label,
            Style::default()
                .fg(theme::role(Role::AccentSoft))
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    if wizard.ps.is_custom() {
        lines.push(Line::from(vec![
            Span::styled("  Base URL: ", Style::default().fg(theme::role(Role::Gray))),
            Span::styled(
                wizard.ps.base_url.clone(),
                Style::default().fg(theme::role(Role::AccentSoft)),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  Model:    ", Style::default().fg(theme::role(Role::Gray))),
            Span::styled(
                wizard.ps.custom_model.clone(),
                Style::default().fg(theme::role(Role::AccentSoft)),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  Model:    ", Style::default().fg(theme::role(Role::Gray))),
            Span::styled(
                wizard.ps.selected_model_name().to_string(),
                Style::default()
                    .fg(theme::role(Role::AccentSoft))
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // #2265: the exclusive enable is deliberate, but a user who had more
    // than one provider on deserves to see which flags this save clears.
    let disabled = wizard.providers_to_be_disabled();
    if wizard.provider_step_touched && !disabled.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "  Disabling: ",
                Style::default().fg(theme::role(Role::Gray)),
            ),
            Span::styled(
                disabled.join(", "),
                Style::default().fg(theme::role(Role::Warning)),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            "  only the provider above stays enabled",
            Style::default().fg(theme::role(Role::GrayMid)),
        )));
    }

    lines.push(Line::from(vec![
        Span::styled("  Workspace:", Style::default().fg(theme::role(Role::Gray))),
        Span::styled(
            format!(" {}", wizard.workspace_path),
            Style::default().fg(theme::role(Role::AccentSoft)),
        ),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Entering OpenCrabs...".to_string(),
        Style::default()
            .fg(accent_gold())
            .add_modifier(Modifier::BOLD | Modifier::ITALIC),
    )));
}

fn render_trello_setup(lines: &mut Vec<Line<'static>>, wizard: &OnboardingWizard) -> usize {
    let base = lines.len();
    // Help text
    lines.push(Line::from(Span::styled(
        "  1. Go to trello.com/power-ups/admin > Create Power-Up",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  2. Click 'API Key' tab > copy your API Key",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "  3. Click 'Token' link > authorize > copy Token",
        Style::default()
            .fg(theme::role(Role::Gray))
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(""));

    // API Key input (masked)
    let ak_focused = wizard.trello_field == TrelloField::ApiKey;
    let (masked_ak, ak_hint) = if wizard.has_existing_trello_api_key() {
        (
            "**************************".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.trello_api_key_input.is_empty() {
        ("trello-api-key".to_string(), String::new())
    } else {
        (
            "*".repeat(wizard.trello_api_key_input.len().min(30)),
            String::new(),
        )
    };
    let cursor_ak = if ak_focused && !wizard.has_existing_trello_api_key() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  API Key: ",
            Style::default().fg(if ak_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", masked_ak, cursor_ak),
            Style::default().fg(if wizard.has_existing_trello_api_key() {
                theme::role(Role::AccentTeal)
            } else if ak_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !ak_hint.is_empty() && ak_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", ak_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // API Token input (masked)
    let at_focused = wizard.trello_field == TrelloField::ApiToken;
    let (masked_at, at_hint) = if wizard.has_existing_trello_api_token() {
        (
            "**************************".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.trello_api_token_input.is_empty() {
        ("trello-api-token".to_string(), String::new())
    } else {
        (
            "*".repeat(wizard.trello_api_token_input.len().min(30)),
            String::new(),
        )
    };
    let cursor_at = if at_focused && !wizard.has_existing_trello_api_token() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  API Token: ",
            Style::default().fg(if at_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", masked_at, cursor_at),
            Style::default().fg(if wizard.has_existing_trello_api_token() {
                theme::role(Role::AccentTeal)
            } else if at_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !at_hint.is_empty() && at_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", at_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Board(s) input — comma-separated IDs or names (visible)
    let bd_focused = wizard.trello_field == TrelloField::BoardId;
    let (bd_display, bd_hint) = if wizard.has_existing_trello_board_id() {
        (
            "**********".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.trello_board_id_input.is_empty() {
        (
            "board-name, id1, workspace-board (comma-separated)".to_string(),
            String::new(),
        )
    } else {
        (wizard.trello_board_id_input.clone(), String::new())
    };
    let bd_cursor = if bd_focused && !wizard.has_existing_trello_board_id() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Board(s): ",
            Style::default().fg(if bd_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", bd_display, bd_cursor),
            Style::default().fg(if wizard.has_existing_trello_board_id() {
                theme::role(Role::AccentTeal)
            } else if bd_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !bd_hint.is_empty() && bd_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", bd_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Allowed Users input (member IDs, optional)
    let au_focused = wizard.trello_field == TrelloField::AllowedUsers;
    let (au_display, au_hint) = if wizard.has_existing_trello_allowed_users() {
        (
            "**********".to_string(),
            " (already configured)".to_string(),
        )
    } else if wizard.trello_allowed_users_input.is_empty() {
        (
            "memberid1,memberid2 (optional — empty = reply to all)".to_string(),
            String::new(),
        )
    } else {
        (wizard.trello_allowed_users_input.clone(), String::new())
    };
    let au_cursor = if au_focused && !wizard.has_existing_trello_allowed_users() {
        "\u{2588}"
    } else {
        ""
    };

    lines.push(Line::from(vec![
        Span::styled(
            "  Allowed Users: ",
            Style::default().fg(if au_focused {
                brand_blue()
            } else {
                theme::role(Role::Gray)
            }),
        ),
        Span::styled(
            format!("{}{}", au_display, au_cursor),
            Style::default().fg(if wizard.has_existing_trello_allowed_users() {
                theme::role(Role::AccentTeal)
            } else if au_focused {
                theme::role(Role::AccentSoft)
            } else {
                theme::role(Role::Gray)
            }),
        ),
    ]));

    if !au_hint.is_empty() && au_focused {
        lines.push(Line::from(Span::styled(
            format!("  {}", au_hint.trim()),
            Style::default()
                .fg(theme::role(Role::Gray))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Test status
    render_channel_test_status(lines, wizard);

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Tab/Shift+Tab: nav fields | \u{2190}\u{2192}: cursor | Ctrl+\u{232b}: clear | Enter: confirm",
        Style::default().fg(theme::role(Role::Gray)),
    )));
    let offset = match wizard.trello_field {
        TrelloField::ApiKey => 4,
        TrelloField::ApiToken => 6,
        TrelloField::BoardId => 8,
        TrelloField::AllowedUsers => lines.len().saturating_sub(base).saturating_sub(4),
    };
    base + offset
}
