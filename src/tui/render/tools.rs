//! Tool call rendering
//!
//! Tool group display, inline approval dialogs, and approval policy menu.

use super::super::app::dialog_keys::{DialogScope, dialog_keys};
use super::super::render::hints;
use super::theme::{self, Role};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

/// The state suffix a detached-task row carries (#2146).
///
/// The row updates in place, so its state has to live on the row itself:
/// without it a running task and a settled one read identically, and the user
/// cannot tell whether the thing they started is still going.
pub(crate) fn detached_state_suffix(
    call: &super::super::app::ToolCallEntry,
) -> Option<&'static str> {
    call.task_id.as_ref().map(|_| {
        if !call.completed {
            " (running)"
        } else if call.success {
            " (done)"
        } else {
            " (failed)"
        }
    })
}

/// Render a grouped tool call display (● bullet with tree lines)
pub(super) fn render_tool_group<'a>(
    lines: &mut Vec<Line<'a>>,
    group: &super::super::app::ToolCallGroup,
    is_active: bool,
    animation_frame: usize,
    content_width: usize,
) {
    // Header line: ● Processing: <tool> or ● N tool calls
    let header = if is_active {
        if let Some(last) = group.calls.last() {
            format!("Processing: {}", last.description)
        } else {
            "Processing".to_string()
        }
    } else {
        let count = group.calls.len();
        format!("{} tool call{}", count, if count == 1 { "" } else { "s" })
    };

    // Flash the dot while active (slow pulse: ~8 ticks on, ~8 ticks off = ~1.6s cycle)
    let dot = if is_active && (animation_frame / 8).is_multiple_of(2) {
        "○"
    } else {
        "●"
    };

    let mut header_spans = vec![Span::styled(
        format!("  {} {}", dot, header),
        Style::default()
            .fg(theme::role(Role::AccentTeal))
            .add_modifier(Modifier::BOLD),
    )];
    header_spans.push(Span::styled(
        if group.expanded {
            " (ctrl+o to collapse)"
        } else {
            " (ctrl+o to expand)"
        },
        Style::default().fg(theme::role(Role::GrayMid)),
    ));
    lines.push(Line::from(header_spans));

    if group.expanded {
        // Show all calls with tree lines + full input + details
        let is_last_call = |i: usize| i == group.calls.len() - 1;
        for (i, call) in group.calls.iter().enumerate() {
            let connector = if is_last_call(i) { "└─" } else { "├─" };
            let continuation = if is_last_call(i) { "   " } else { "│  " };
            let in_flight = !call.completed;

            let header_style = if call.success || in_flight {
                Style::default()
                    .fg(theme::role(Role::GrayDim))
                    .add_modifier(Modifier::ITALIC)
            } else {
                Style::default()
                    .fg(theme::role(Role::Error))
                    .add_modifier(Modifier::ITALIC)
            };
            {
                let mut desc_spans = vec![
                    Span::styled(
                        format!("    {} ", connector),
                        Style::default().fg(theme::role(Role::GrayDim)),
                    ),
                    Span::styled(call.description.clone(), header_style),
                ];
                if let Some(suffix) = detached_state_suffix(call) {
                    desc_spans.push(Span::styled(
                        suffix,
                        Style::default().fg(theme::role(Role::GrayMid)),
                    ));
                }
                let desc_line = Line::from(desc_spans);
                for wrapped in
                    super::utils::wrap_line_with_padding(desc_line, content_width, "       ")
                {
                    lines.push(wrapped);
                }
            }

            // Show full tool input parameters (untruncated) below the header
            let safe_call_input = crate::utils::redact_tool_input(&call.tool_input);
            if !safe_call_input.is_null()
                && let Some(obj) = safe_call_input.as_object()
            {
                for (key, value) in obj.iter() {
                    // Key label
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("    {}  ", continuation),
                            Style::default().fg(theme::role(Role::GrayDim)),
                        ),
                        Span::styled(
                            format!("{}:", key),
                            Style::default()
                                .fg(theme::role(Role::GrayMid))
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Value — expand strings line by line, cap at 200 lines.
                    //
                    // Models periodically emit tool args where multi-line
                    // payloads use the literal escape sequence `\n`
                    // (backslash + n) instead of actual newline bytes —
                    // either because the args body got JSON-stringified
                    // once too many times, or because the model copied
                    // a Rust string literal verbatim into the arg slot.
                    // When that happens, `s.lines()` doesn't split (it
                    // only sees actual `\n`/`\r\n` chars), so the entire
                    // multi-line payload becomes one logical line that
                    // ratatui then wraps at the content width — visible
                    // as `chars. Cap is 40. Shorten it.",\n  opt,\n  ...`
                    // all run together with literal `\n` separators.
                    // Reported 2026-06-01 on a qwen Edit tool call where
                    // a 7-line `old_string` rendered as one wrapped
                    // soup. Unescape the common JSON-shape sequences
                    // before splitting.
                    let value_lines: Vec<String> = match value {
                        serde_json::Value::String(s) => unescape_display_string(s)
                            .lines()
                            .map(|l| l.to_string())
                            .collect(),
                        _ => vec![value.to_string()],
                    };
                    let total = value_lines.len();
                    for vline in value_lines.iter().take(200) {
                        let full_line = Line::from(vec![
                            Span::styled(
                                format!("    {}    ", continuation),
                                Style::default().fg(theme::role(Role::GrayDim)),
                            ),
                            Span::styled(
                                vline.clone(),
                                Style::default().fg(theme::role(Role::GraySoft)),
                            ),
                        ]);
                        for wrapped in
                            super::utils::wrap_line_with_padding(full_line, content_width, "  ")
                        {
                            lines.push(wrapped);
                        }
                    }
                    if total > 200 {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("    {}    ", continuation),
                                Style::default().fg(theme::role(Role::GrayDim)),
                            ),
                            Span::styled(
                                format!("... ({} more lines)", total - 200),
                                Style::default()
                                    .fg(theme::role(Role::Gray))
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                    }
                }
            }

            // If the call is still in-flight, show a running indicator
            if in_flight {
                let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let frame = spinner_frames[animation_frame % spinner_frames.len()];
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("    {}  {} ", continuation, frame),
                        Style::default().fg(theme::role(Role::Gray)),
                    ),
                    Span::styled("running...", Style::default().fg(theme::role(Role::Accent))),
                ]));
            } else {
                // Show tool output details (with secrets redacted)
                if let Some(ref details) = call.details {
                    let detail_lines =
                        collapse_build_output(&crate::utils::redact_secrets(details));
                    let default_detail_style = Style::default().fg(theme::role(Role::GrayDetail));
                    for detail_line in detail_lines.iter().take(200) {
                        let line_style = if detail_line.starts_with("+ ") {
                            Style::default().fg(theme::role(Role::TealVivid))
                        } else if detail_line.starts_with("- ") {
                            Style::default().fg(theme::role(Role::Error))
                        } else if detail_line.starts_with("@@ ") {
                            Style::default().fg(theme::role(Role::AccentTeal))
                        } else {
                            default_detail_style
                        };
                        let full_line = Line::from(vec![
                            Span::styled(
                                format!("    {}  ", continuation),
                                Style::default().fg(theme::role(Role::GrayDim)),
                            ),
                            Span::styled(detail_line.clone(), line_style),
                        ]);
                        for wrapped in
                            super::utils::wrap_line_with_padding(full_line, content_width, "  ")
                        {
                            lines.push(wrapped);
                        }
                    }
                    if detail_lines.len() > 200 {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("    {}  ", continuation),
                                Style::default().fg(theme::role(Role::GrayDim)),
                            ),
                            Span::styled(
                                format!("... ({} more lines)", detail_lines.len() - 200),
                                Style::default()
                                    .fg(theme::role(Role::Gray))
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                    }
                }
            }
        }
    } else {
        // Collapsed: show only the last call (rolling wheel effect)
        if let Some(last) = group.calls.last() {
            let style = if last.success {
                Style::default()
                    .fg(theme::role(Role::GrayDim))
                    .add_modifier(Modifier::ITALIC)
            } else {
                Style::default()
                    .fg(theme::role(Role::Error))
                    .add_modifier(Modifier::ITALIC)
            };
            {
                let desc_line = Line::from(vec![
                    Span::styled(
                        "    └─ ".to_string(),
                        Style::default().fg(theme::role(Role::GrayDim)),
                    ),
                    Span::styled(last.description.clone(), style),
                ]);
                for wrapped in
                    super::utils::wrap_line_with_padding(desc_line, content_width, "       ")
                {
                    lines.push(wrapped);
                }
            }
        }
    }
}

/// Render an inline approval request or resolved approval
pub(super) fn render_inline_approval<'a>(
    lines: &mut Vec<Line<'a>>,
    approval: &super::super::app::ApprovalData,
    content_width: usize,
) {
    use super::super::app::ApprovalState;

    match &approval.state {
        ApprovalState::Pending => {
            // Header: brief description of what's being requested
            let desc = super::super::app::App::format_tool_description(
                &approval.tool_name,
                &approval.tool_input,
            );
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    desc,
                    Style::default()
                        .fg(theme::role(Role::TextPrimary))
                        .add_modifier(Modifier::BOLD),
                ),
            ]));

            // Always show hint so users know V expands full details.
            // Canonical footer from the scoped keymap (#1775): every
            // advertised key is one the approval handler consumes.
            lines.push(hints::footer_line(
                "  ",
                dialog_keys(DialogScope::ToolApproval),
            ));

            // Expanded details: show all params fully, no truncation
            let safe_approval_input = crate::utils::redact_tool_input(&approval.tool_input);
            if approval.show_details {
                if let Some(obj) = safe_approval_input.as_object() {
                    for (key, value) in obj.iter() {
                        lines.push(Line::from(vec![Span::styled(
                            format!("    {}:", key),
                            Style::default()
                                .fg(theme::role(Role::GrayDim))
                                .add_modifier(Modifier::BOLD),
                        )]));
                        // Build owned lines so there are no borrow conflicts
                        let value_lines: Vec<String> = match value {
                            serde_json::Value::String(s) => {
                                s.lines().map(|l| l.to_string()).collect()
                            }
                            _ => vec![value.to_string()],
                        };
                        let total = value_lines.len();
                        for vline in value_lines.iter().take(60) {
                            let full_line = Line::from(vec![
                                Span::styled("      ", Style::default()),
                                Span::styled(
                                    vline.clone(),
                                    Style::default().fg(theme::role(Role::GrayLight)),
                                ),
                            ]);
                            for wrapped in
                                super::utils::wrap_line_with_padding(full_line, content_width, "  ")
                            {
                                lines.push(wrapped);
                            }
                        }
                        if total > 60 {
                            lines.push(Line::from(vec![
                                Span::styled("      ", Style::default()),
                                Span::styled(
                                    format!("... ({} more lines)", total - 60),
                                    Style::default()
                                        .fg(theme::role(Role::Gray))
                                        .add_modifier(Modifier::ITALIC),
                                ),
                            ]));
                        }
                    }
                }
                // Show capabilities if the tool declares any
                if !approval.capabilities.is_empty() {
                    lines.push(Line::from(vec![Span::styled(
                        "    capabilities:",
                        Style::default()
                            .fg(theme::role(Role::GrayDim))
                            .add_modifier(Modifier::BOLD),
                    )]));
                    lines.push(Line::from(vec![
                        Span::styled("      ", Style::default()),
                        Span::styled(
                            approval.capabilities.join(", "),
                            Style::default().fg(theme::role(Role::Accent)),
                        ),
                    ]));
                }
                lines.push(Line::from(""));
            }

            // "Do you approve?" + vertical option list with ❯ selector
            // Order: Yes(0), Always(1), No(2)
            lines.push(Line::from(vec![Span::styled(
                "  Do you approve?",
                Style::default().fg(theme::role(Role::GrayDim)),
            )]));
            let options = [
                ("Yes", theme::role(Role::AccentTeal)),
                ("Always", theme::role(Role::Accent)),
                ("No", theme::role(Role::Error)),
            ];
            for (i, (label, color)) in options.iter().enumerate() {
                if i == approval.selected_option {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {} ", "\u{276F}"),
                            Style::default().fg(*color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            label.to_string(),
                            Style::default().fg(*color).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(
                            label.to_string(),
                            Style::default().fg(theme::role(Role::GrayDim)),
                        ),
                    ]));
                }
            }
        }
        ApprovalState::Approved(_option) => {
            // Silently skip — tool execution is already shown in the tool group
        }
        ApprovalState::Denied(reason) => {
            let desc = super::super::app::App::format_tool_description(
                &approval.tool_name,
                &approval.tool_input,
            );
            let suffix = if reason.is_empty() {
                String::new()
            } else {
                format!(": {}", reason)
            };
            lines.push(Line::from(vec![Span::styled(
                format!("  {} -- denied{}", desc, suffix),
                Style::default()
                    .fg(theme::role(Role::Error))
                    .add_modifier(Modifier::ITALIC),
            )]));
        }
    }
}

/// Collapse consecutive cargo build progress lines into a summary.
///
/// Lines like "Compiling foo v1.0", "Downloading crates...", "Checking bar v2.0"
/// are collapsed into a single summary line like "Compiled 47 crates".
/// Non-build lines (errors, warnings, test output) pass through unchanged.
pub(crate) fn collapse_build_output(details: &str) -> Vec<String> {
    let build_prefixes = [
        "   Compiling ",
        "   Checking ",
        "  Downloading ",
        "    Updating ",
        "   Documenting ",
        "     Running ",
        "       Fresh ",
        "    Fetching ",
        "  Downloaded ",
        "     Locking ",
        "   Packaging ",
    ];

    let mut result: Vec<String> = Vec::new();
    let mut build_count: usize = 0;
    let mut last_build_verb = "";

    let flush_build = |result: &mut Vec<String>, count: usize, verb: &str| {
        if count > 0 {
            let label = match verb {
                "Compiling" | "Checking" => "Compiled",
                "Downloading" | "Downloaded" | "Fetching" => "Downloaded",
                _ => "Processed",
            };
            result.push(format!("   {} {} crates", label, count));
        }
    };

    for line in details.lines() {
        let trimmed = line.trim_start();
        let is_build = build_prefixes.iter().any(|prefix| line.starts_with(prefix));
        // Also match "Finished" lines (e.g. "Finished `dev` profile")
        let is_finished = trimmed.starts_with("Finished ");

        if is_build {
            // Extract verb for grouping
            let verb = trimmed.split_whitespace().next().unwrap_or("");
            if build_count > 0 && verb != last_build_verb {
                flush_build(&mut result, build_count, last_build_verb);
                build_count = 0;
            }
            last_build_verb = verb;
            build_count += 1;
        } else {
            flush_build(&mut result, build_count, last_build_verb);
            build_count = 0;
            if !is_finished || result.is_empty() {
                result.push(line.to_string());
            } else {
                // Replace "Finished" with a cleaner version
                result.push(line.to_string());
            }
        }
    }
    flush_build(&mut result, build_count, last_build_verb);

    result
}

/// Render the /approve policy selector menu
pub(super) fn render_approve_menu<'a>(
    lines: &mut Vec<Line<'a>>,
    menu: &super::super::app::ApproveMenu,
    _content_width: usize,
) {
    use super::super::app::ApproveMenuState;

    match &menu.state {
        ApproveMenuState::Pending => {
            let gold = theme::role(Role::Accent);

            lines.push(Line::from(vec![Span::styled(
                "  TOOL APPROVAL POLICY",
                Style::default().fg(gold).add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(""));

            let options = [
                ("Approve-only", "Always ask before executing tools"),
                (
                    "Allow all (session)",
                    "Auto-approve all tools for this session",
                ),
                (
                    "Yolo mode",
                    "Execute everything without approval until reset",
                ),
            ];

            lines.push(Line::from(Span::styled(
                "  Select a policy:",
                Style::default().fg(theme::role(Role::Gray)),
            )));
            lines.push(Line::from(""));

            for (i, (label, desc)) in options.iter().enumerate() {
                let is_selected = i == menu.selected_option;
                let prefix = if is_selected { "\u{25b6} " } else { "  " };

                let style = if is_selected {
                    Style::default()
                        .fg(theme::role(Role::AccentTeal))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::role(Role::TextPrimary))
                };

                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{}{}", prefix, label), style),
                ]));

                if is_selected {
                    lines.push(Line::from(vec![
                        Span::raw("      "),
                        Span::styled(
                            *desc,
                            Style::default()
                                .fg(theme::role(Role::GrayDim))
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                }
            }

            lines.push(Line::from(""));
            lines.push(hints::footer_line(
                "  ",
                dialog_keys(DialogScope::ApprovePolicyMenu),
            ));
        }
        ApproveMenuState::Selected(choice) => {
            let (label, color) = match choice {
                0 => ("Approve-only", theme::role(Role::AccentTeal)),
                1 => ("Allow all (session)", theme::role(Role::Accent)),
                2 => ("Yolo mode", theme::role(Role::Error)),
                _ => ("Cancelled", theme::role(Role::GrayDim)),
            };
            lines.push(Line::from(vec![Span::styled(
                format!("  Policy set: {}", label),
                Style::default().fg(color).add_modifier(Modifier::ITALIC),
            )]));
        }
    }
}

/// Unescape common JSON-shape escape sequences in a tool-arg string
/// so the renderer can split on real newlines. Used when the model
/// emitted `"old_string": "fn foo()\\n{\\n}"` (literal backslash-n)
/// instead of `"old_string": "fn foo()\n{\n}"` (actual newline bytes).
///
/// Conservative replacement: only the sequences we KNOW would break
/// the line-by-line display when present as literal escapes. Doesn't
/// touch JSON-only escapes (`\u00XX`, `\/`) or anything that could
/// silently distort the meaning of code being edited. If the source
/// genuinely contains the two-char sequence `\n` (e.g. a Rust string
/// literal showing escape syntax in documentation), the resulting
/// false-positive is an actual newline in the preview — acceptable
/// trade-off since the preview is for human display, not editing.
pub(crate) fn unescape_display_string(s: &str) -> String {
    // Skip the work for the common case where no escapes are present.
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('n') => {
                    chars.next();
                    out.push('\n');
                }
                Some('t') => {
                    chars.next();
                    out.push_str("    ");
                }
                Some('r') => {
                    chars.next();
                    // Drop literal \r — multiline renderer treats \n as
                    // the only line separator, and a trailing \r would
                    // show as a control character on Linux/macOS.
                }
                Some('"') => {
                    chars.next();
                    out.push('"');
                }
                Some('\\') => {
                    chars.next();
                    out.push('\\');
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}
