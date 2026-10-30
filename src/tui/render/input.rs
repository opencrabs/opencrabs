//! Input box rendering
//!
//! Text input area with cursor and slash command autocomplete dropdown.

use super::super::app::App;
use super::theme::{self, Role};
use super::utils::format_token_count_raw;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

struct SelConfig<'a> {
    sel_from: usize,
    sel_to: usize,
    selection_style: &'a Style,
    cursor_style: &'a Style,
    cursor_local_byte: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
/// Apply selection highlighting to a visual row's text at the character level.
fn spans_with_selection(
    text: &str,
    row_byte_start: usize,
    row_byte_end: usize,
    sel: &SelConfig<'_>,
    prefix: Span<'static>,
) -> Line<'static> {
    let has_sel = row_byte_end > sel.sel_from && row_byte_start < sel.sel_to;
    let has_cursor = sel.cursor_local_byte.is_some();

    if !has_sel && !has_cursor {
        return Line::from(vec![prefix, Span::raw(text.to_string())]);
    }

    let mut spans: Vec<Span<'static>> = vec![prefix];
    for (idx, ch) in text.char_indices() {
        let global_start = row_byte_start + idx;
        let ch_len = ch.len_utf8();
        let global_end = global_start + ch_len;

        let in_sel = global_start >= sel.sel_from && global_end <= sel.sel_to;
        let is_cursor = sel.cursor_local_byte == Some(idx);

        let span_text = if is_cursor && !in_sel {
            Span::styled(ch.to_string(), *sel.cursor_style)
        } else if in_sel {
            Span::styled(ch.to_string(), *sel.selection_style)
        } else if is_cursor && in_sel {
            Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(theme::role(Role::AccentSoft))
                    .bg(theme::role(Role::SelectionBg)),
            )
        } else {
            Span::raw(ch.to_string())
        };
        spans.push(span_text);
    }

    if sel.cursor_local_byte == Some(text.len()) {
        let at_end_sel =
            row_byte_start + text.len() >= sel.sel_from && row_byte_start + text.len() < sel.sel_to;
        if at_end_sel {
            spans.push(Span::styled(" ", *sel.selection_style));
        } else {
            spans.push(Span::styled(" ", *sel.cursor_style));
        }
    }

    Line::from(spans)
}

/// Pieces of the queue preview row, split out so the truncation maths can be
/// unit-tested without a frame.
pub(crate) struct QueueLineParts {
    /// `⏳ queued (2): ` when a preview fits, otherwise just the count.
    pub label: String,
    /// The last queued message, flattened and cut to the room that is left.
    pub preview: String,
    /// What the queue does and which key edits it, chosen by available width.
    pub hint: &'static str,
}

/// Build the queue preview row (#2237).
///
/// The row has to carry three facts on one line: how many messages are
/// waiting, what is waiting, and that Enter did not send them. The hint
/// degrades with the terminal width and the preview is what gets cut, so the
/// explanation is never dropped before the key that acts on it.
pub(crate) fn queue_line_parts(
    msgs_count: usize,
    last_msg: &str,
    content_width: usize,
) -> QueueLineParts {
    let count = if msgs_count > 1 {
        format!("⏳ queued ({msgs_count})")
    } else {
        "⏳ queued".to_string()
    };

    // The hint degrades by what is LEFT after the count and a usable preview,
    // not by absolute width: the full sentence plus the count already spends
    // most of an 80 column row.
    const MIN_PREVIEW: usize = 24;
    let count_width = unicode_width::UnicodeWidthStr::width(count.as_str());
    let room_for_hint = content_width.saturating_sub(count_width + MIN_PREVIEW + 3);
    let hint: &'static str = if room_for_hint >= 58 {
        " · sends at the next tool call, or back to this box · ↑ edit"
    } else if room_for_hint >= 36 {
        " · sends at the next tool call · ↑ edit"
    } else if room_for_hint >= 26 {
        " · next tool call · ↑ edit"
    } else {
        " · ↑ edit"
    };

    // ": " after the count, plus a column of slack before the hint.
    let reserved = count_width + unicode_width::UnicodeWidthStr::width(hint) + 3;
    let budget = content_width.saturating_sub(reserved);

    if budget < 8 {
        return QueueLineParts {
            label: count,
            preview: String::new(),
            hint,
        };
    }

    let flat = last_msg.replace('\n', " ");
    let preview = if unicode_width::UnicodeWidthStr::width(flat.as_str()) > budget {
        let mut truncated: String = flat.chars().take(budget.saturating_sub(3)).collect();
        truncated.push_str("...");
        truncated
    } else {
        flat
    };

    QueueLineParts {
        label: format!("{count}: "),
        preview,
        hint,
    }
}

/// Render queued messages above the input area.
/// Returns the height consumed (0 if no queue).
pub(super) fn render_queue(f: &mut Frame, app: &App, area: Rect) -> u16 {
    let Some(session) = &app.current_session else {
        return 0;
    };
    // Snapshot the relevant entry under the lock, then drop the guard
    // before doing any rendering — keeps the critical section short and
    // avoids holding the mutex across render work.
    let (msgs_count, last_msg) = {
        let Ok(map) = app.queued_messages.lock() else {
            return 0;
        };
        let Some(msgs) = map.get(&session.id) else {
            return 0;
        };
        if msgs.is_empty() {
            return 0;
        }
        // Preview the compact form: a queued system entry must not spill its
        // context into the input chrome either.
        (
            msgs.len(),
            msgs.last()
                .map(|m| m.display_text.clone())
                .unwrap_or_default(),
        )
    };

    let content_width = area.width.saturating_sub(2) as usize; // borders
    let parts = queue_line_parts(msgs_count, &last_msg, content_width);

    // Quiet by design (#2244). The row has to read as its own row and no
    // more: it sits directly above the input box, so anything loud pulls the
    // eye away from the answer being written. #2237 raised every part of the
    // row above the chrome around it to separate it from the status line, and
    // the side effect was that the label went amber and bold and the preview
    // went to the body colour, so a queued message read like a message instead
    // of like chrome.
    //
    // Nothing here is amber, because a queued message is a state and not a
    // warning, and the preview is deliberately not the body colour. On the
    // default palette: label GraySoft 170, preview TextSecondary 141, hint
    // GrayMid 100, border GrayDim 80. Presets remap these roles, so those
    // numbers describe the default theme rather than a fixed contract. The
    // border and the step between the parts are what separate the row now.
    //
    // Honest note for the next reader: the thinking line this row sits near is
    // GrayDim (80), so the row is still brighter than that line. The amber and
    // the body-coloured preview were what made it shout, not the step itself.
    let line = Line::from(vec![
        Span::styled(
            parts.label,
            Style::default().fg(theme::role(Role::GraySoft)),
        ),
        Span::styled(
            parts.preview,
            Style::default()
                .fg(theme::role(Role::TextSecondary))
                .add_modifier(Modifier::ITALIC),
        ),
        Span::styled(parts.hint, Style::default().fg(theme::role(Role::GrayMid))),
    ]);

    let queue_height = 2u16;
    let queue_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: queue_height,
    };

    let para = Paragraph::new(vec![line]).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(theme::role(Role::GrayDim))),
    );
    f.render_widget(para, queue_area);
    queue_height
}

/// Render the input box
pub(super) fn render_input(f: &mut Frame, app: &App, area: Rect) {
    let input_content_width = area.width.saturating_sub(2) as usize; // borders
    let mut input_lines: Vec<Line> = Vec::new();
    let mut cursor_row: usize = 0;

    // Check if current session has a queued message (for prefix styling)
    let has_queue = app
        .current_session
        .as_ref()
        .and_then(|s| {
            app.queued_messages
                .lock()
                .ok()
                .map(|q| q.contains_key(&s.id))
        })
        .unwrap_or(false);

    let cursor_style = Style::default()
        .fg(theme::role(Role::Ink))
        .bg(theme::role(Role::Gray));
    let selection_style = Style::default()
        .bg(theme::role(Role::SelectionBg))
        .fg(theme::role(Role::AccentSoft));

    // Compute selection byte range from drag coordinates
    let sel_from_to: Option<(usize, usize)> = if app.input_drag_selecting {
        let a = app.input_drag_anchor.unwrap_or((0, 0));
        let b = app.input_drag_current.unwrap_or(a);
        let (start, end) = if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        };

        let input_top = app.input_area_y;
        let input_content_width = area.width.saturating_sub(2) as usize;
        let content_left = app.input_area_x + 2;

        if input_content_width == 0 || app.input_buffer.is_empty() {
            None
        } else {
            let start_visual = start.1.saturating_sub(input_top + 1) as usize;
            let end_visual = end.1.saturating_sub(input_top + 1) as usize;

            // Build a list of visual rows: each entry is (global_byte_start, global_byte_end)
            let mut visual_rows: Vec<(usize, usize)> = Vec::new();
            let buf = &app.input_buffer;
            let blen = buf.len();
            let mut ls = 0usize;
            while ls <= blen {
                let le = buf[ls..].find('\n').map(|i| ls + i).unwrap_or(blen);
                let line = &buf[ls..le];
                let lw = unicode_width::UnicodeWidthStr::width(line);
                let rows = if lw == 0 {
                    1
                } else {
                    lw.div_ceil(input_content_width)
                };
                for r in 0..rows {
                    let chunk_start = r * input_content_width;
                    let mut display_w = 0;
                    let mut char_start = 0;
                    let mut char_end = line.len();
                    let mut found_start = false;
                    for (idx, ch) in line.char_indices() {
                        let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                        if r > 0 && !found_start && display_w >= chunk_start {
                            char_start = idx;
                            found_start = true;
                        }
                        if display_w + ch_w > chunk_start + input_content_width {
                            char_end = idx;
                            break;
                        }
                        display_w += ch_w;
                    }
                    visual_rows.push((ls + char_start, ls + char_end));
                }
                if le == blen {
                    break;
                }
                ls = le + 1;
            }

            let map_vr_to_byte = |vr: usize, col: u16| -> usize {
                if vr >= visual_rows.len() {
                    return blen;
                }
                let (rs, re) = visual_rows[vr];
                let line_text = &buf[rs..re];
                let target_disp = col.saturating_sub(content_left) as usize;
                let mut w = 0;
                for (idx, ch) in line_text.char_indices() {
                    if w >= target_disp {
                        return rs + idx;
                    }
                    w += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                }
                re
            };

            let from = map_vr_to_byte(start_visual, start.0);
            let to = map_vr_to_byte(end_visual, end.0);
            Some((from.min(to), from.max(to)))
        }
    } else {
        None
    };

    if app.input_buffer.is_empty() {
        let mut spans = vec![
            Span::styled("\u{276F} ", Style::default().fg(theme::role(Role::GrayMid))),
            Span::styled(" ", cursor_style),
        ];
        // Single follow-up suggestion → gray ghost text after the cursor; Tab
        // fills the input with it (editable, never submits). Several
        // suggestions render as a pick-list instead (see render_followup), so
        // ghost text is exactly the len == 1 case.
        if app.followup_suggestions.len() == 1 {
            spans.push(Span::styled(
                app.followup_suggestions[0].clone(),
                Style::default().fg(theme::role(Role::GrayMid)),
            ));
            spans.push(Span::styled(
                "  \u{21e5} Tab",
                Style::default().fg(theme::role(Role::GrayDark)),
            ));
        }
        input_lines.push(Line::from(spans));
    } else {
        let buf = &app.input_buffer;
        let cursor_pos = app.cursor_position;
        let is_queued = has_queue;

        // Build visual rows from logical lines with wrapping
        let mut line_start = 0usize;
        let mut line_idx = 0usize;
        let buf_len = buf.len();
        while line_start <= buf_len {
            let line_end = buf[line_start..]
                .find('\n')
                .map(|i| line_start + i)
                .unwrap_or(buf_len);
            let line = &buf[line_start..line_end];
            let next_start = line_end + 1;
            let is_last_line = line_end == buf_len;

            let cursor_in_line = cursor_pos >= line_start && cursor_pos < line_end;
            let cursor_at_end_of_last_line = cursor_pos >= buf_len && is_last_line;

            let prefix = if line_idx == 0 {
                if is_queued {
                    Span::styled("⏳", Style::default().fg(theme::role(Role::Accent)))
                } else if buf.starts_with('!') {
                    // Shell-mode prefix: brighter + orange so the prompt
                    // caret itself screams "this is shell, not LLM".
                    Span::styled(
                        "$ ",
                        Style::default()
                            .fg(theme::role(Role::Accent))
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled("\u{276F} ", Style::default().fg(theme::role(Role::GrayMid)))
                }
            } else {
                Span::raw("  ")
            };

            // Build wrapped visual rows for this logical line
            let lw = unicode_width::UnicodeWidthStr::width(line);
            let num_visual_rows = if lw == 0 {
                1
            } else {
                lw.div_ceil(input_content_width)
            };

            let (sel_from, sel_to) = sel_from_to.unwrap_or((usize::MAX, usize::MAX));

            for vr in 0..num_visual_rows {
                let chunk_start_display = vr * input_content_width;
                let chunk_end_display = chunk_start_display + input_content_width;

                // Find the byte range within `line` that fits this visual row
                let mut disp_w = 0;
                let mut char_start = 0;
                let mut char_end = line.len();
                for (idx, ch) in line.char_indices() {
                    let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if vr == 0 && disp_w + cw > chunk_end_display {
                        char_end = idx;
                        break;
                    }
                    if vr > 0 {
                        if disp_w < chunk_start_display {
                            char_start = idx;
                        }
                        if disp_w + cw > chunk_end_display {
                            char_end = idx;
                            break;
                        }
                    }
                    disp_w += cw;
                }

                let chunk_text = &line[char_start..char_end];
                let global_chunk_start = line_start + char_start;
                let global_chunk_end = line_start + char_end;

                // Cursor position within this visual row (local byte index)
                let local_cursor = if cursor_in_line {
                    let c = cursor_pos.saturating_sub(line_start);
                    if c >= char_start && c <= char_end {
                        Some(c - char_start)
                    } else if c == line.len() && vr == num_visual_rows - 1 {
                        // Cursor at end of line, show on last visual row
                        Some(chunk_text.len())
                    } else {
                        None
                    }
                } else if cursor_at_end_of_last_line && is_last_line && vr == num_visual_rows - 1 {
                    Some(chunk_text.len())
                } else {
                    None
                };

                let line_sel = SelConfig {
                    sel_from,
                    sel_to,
                    selection_style: &selection_style,
                    cursor_style: &cursor_style,
                    cursor_local_byte: local_cursor,
                };
                let padded_line = spans_with_selection(
                    chunk_text,
                    global_chunk_start,
                    global_chunk_end,
                    &line_sel,
                    if vr == 0 {
                        prefix.clone()
                    } else {
                        Span::raw("  ")
                    },
                );

                input_lines.push(padded_line);

                if local_cursor.is_some() {
                    cursor_row = input_lines.len() - 1;
                }
            }

            if is_last_line {
                break;
            }
            line_start = next_start;
            line_idx += 1;
        }

        // If buffer ends with newline, add a cursor line
        if cursor_pos >= buf_len && buf.ends_with('\n') {
            input_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(" ", cursor_style),
            ]));
            cursor_row = input_lines.len() - 1;
        }
    }

    // Shell-mode visual cue: tint the border orange when the buffer starts
    // with `!` so the user sees at-a-glance that Enter will run the rest
    // as a shell command (via `sh -c`) instead of sending it to the LLM.
    // This cue existed pre-drag-copy refactor and was lost in the rewrite.
    let border_style = if app.input_buffer.starts_with('!') {
        Style::default().fg(theme::role(Role::Accent)) // ACCENT_GOLD / shell mode
    } else {
        Style::default().fg(theme::role(Role::Gray))
    };

    // Context usage indicator (right-side bottom title)
    let context_title = if let Some(input_tok) = app.last_input_tokens {
        let pct = app.context_usage_percent();
        let context_color = if pct > 80.0 {
            theme::role(Role::Error)
        } else if pct > 60.0 {
            theme::role(Role::Accent)
        } else {
            theme::role(Role::AccentTeal)
        };
        let ctx_label = format_token_count_raw(input_tok as i64);
        let max_label = format_token_count_raw(app.context_max_tokens as i64);
        let context_label = format!(" ctx: {}/{} ({:.0}%) ", ctx_label, max_label, pct);
        Line::from(Span::styled(
            context_label,
            Style::default()
                .fg(context_color)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Right)
    } else {
        Line::from(Span::styled(
            " Context: – ",
            Style::default()
                .fg(theme::role(Role::GrayDim))
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Right)
    };

    // Build attachment indicator for the top-right title area
    let attach_title = if !app.attachments.is_empty() {
        let spans: Vec<Span> = app
            .attachments
            .iter()
            .enumerate()
            .flat_map(|(i, att)| {
                let focused = app.focused_attachment == Some(i);
                let kind = if att.is_video { "Video" } else { "Image" };
                let label = format!("{} #{}", kind, i + 1);
                let style = if focused {
                    // Highlight focused attachment — inverted colors
                    Style::default()
                        .fg(theme::role(Role::Ink))
                        .bg(theme::role(Role::TealVivid))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme::role(Role::TealVivid))
                        .add_modifier(Modifier::BOLD)
                };
                let mut result = vec![Span::styled(label, style)];
                if i + 1 < app.attachments.len() {
                    result.push(Span::styled(
                        " | ",
                        Style::default().fg(theme::role(Role::TealVivid)),
                    ));
                }
                result
            })
            .collect();
        let mut all_spans = vec![Span::styled(
            " [",
            Style::default()
                .fg(theme::role(Role::TealVivid))
                .add_modifier(Modifier::BOLD),
        )];
        all_spans.extend(spans);
        all_spans.push(Span::styled(
            "] ",
            Style::default()
                .fg(theme::role(Role::TealVivid))
                .add_modifier(Modifier::BOLD),
        ));
        Line::from(all_spans).alignment(Alignment::Right)
    } else {
        Line::from("")
    };

    // Detached background commands (#762), on the input's TOP border, left.
    //
    // These end the turn the moment they spawn, so nothing else on screen says
    // work is still happening. It used to sit in the status bar, which is where
    // the eye goes least; here it shares a border with the ctx budget below and
    // lands in the box the user is already looking at while they wait.
    let bg_task_title = app
        .current_session
        .as_ref()
        .zip(app.agent_service.background_manager())
        .and_then(|(session, mgr)| {
            let tasks: Vec<(String, u64)> = mgr
                .running_tasks(session.id)
                .into_iter()
                .map(|t| (t.label, t.started.elapsed().as_secs()))
                .collect();
            super::background::format_background_tasks(&tasks, super::background::LABEL_CHARS)
        })
        .map(|text| {
            Line::from(Span::styled(
                format!(" ⏳ {text} "),
                Style::default().fg(theme::role(Role::Warning)),
            ))
            .alignment(Alignment::Right)
        });

    // Transient notice ("Copied to clipboard" after a drag-select, "Press
    // Esc again…", an error toast) lives INSIDE the input box, as trailing
    // ghost text on the cursor's row (#1369), the same slot the single
    // follow-up suggestion uses. It used to be pushed into the chat buffer,
    // which grew the scroll content by three rows on show and shrank it on
    // expiry, so the history jumped on every drag-select. Here it changes
    // no layout: the input height is derived from the buffer alone and the
    // Paragraph below does not wrap, so a long notice is clipped, not
    // wrapped onto a row the layout never allotted.
    if let Some((kind, text)) = super::notice::pick_notice(
        app.error_message.as_deref(),
        app.notification.as_deref(),
        app.clipboard_hint.as_deref(),
    ) && let Some(row) = input_lines.get_mut(cursor_row)
    {
        let room = input_content_width.saturating_sub(row.width());
        if let Some(label) = super::notice::notice_label(text, room) {
            let color = match kind {
                super::notice::NoticeKind::Error => theme::role(Role::Error),
                super::notice::NoticeKind::Info => theme::role(Role::AccentTeal),
            };
            row.spans.push(Span::styled(
                label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
    }

    let mut block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title_bottom(context_title)
        .border_style(border_style);

    // Attachments rightmost, background task to their left (#836).
    if !app.attachments.is_empty() {
        block = block.title(attach_title);
    }

    if let Some(title) = bg_task_title {
        block = block.title(title);
    }

    // Compute a vertical scroll so the cursor row is always inside the
    // visible viewport (area.height - 2 for top/bottom borders).
    let inner_rows = area.height.saturating_sub(2) as usize;
    let total_rows = input_lines.len();
    let scroll_y: u16 = if inner_rows == 0 || total_rows <= inner_rows {
        0
    } else if cursor_row >= inner_rows {
        // Keep cursor on the last visible row.
        (cursor_row + 1 - inner_rows) as u16
    } else {
        0
    };

    let input = Paragraph::new(input_lines)
        .style(Style::default().fg(theme::role(Role::TextPrimary)))
        .scroll((scroll_y, 0))
        .block(block);

    f.render_widget(input, area);
}

/// Sizing math for the slash-autocomplete dropdown.
///
/// Pure function pulled out of `render_slash_autocomplete` so the
/// regression (long skill descriptions overshooting the frame's right
/// edge and panicking ratatui's buffer write) is unit-testable without
/// a live `App`.
///
/// Behaviour: **responsive**. The dropdown grows to fit the longest row
/// (name column + description), capped at `input_area_width - 1` so
/// the right edge stays inside the frame. There is no fixed soft cap —
/// content shorter than the terminal renders without truncation;
/// content wider than the terminal is truncated to fit.
///
/// `name_col_chars` is the visual width reserved for the name column
/// across all rows so they line up — caller passes
/// `max(10, longest_name_chars)`.
///
/// Returns `(width, inner_width, desc_budget)`:
///   - `width`       — final dropdown rect width (incl. 2 borders + 2 pads)
///   - `inner_width` — usable width inside the borders + pads
///   - `desc_budget` — char budget for the truncated description portion
pub(crate) fn dropdown_dimensions(
    name_col_chars: u16,
    description_char_counts: &[usize],
    input_area_width: u16,
    pad_x: u16,
) -> (u16, u16, usize) {
    // Dropdown is anchored at `input_area.x + 1`, so its absolute right
    // edge must stay within the frame's right edge — the maximum width
    // is therefore `input_area_width - 1`. Without this clamp, a long
    // description inflates `width` to `input_area_width` and ratatui
    // rejects the write at column `input_area_width` with an
    // "index outside of buffer" panic.
    let max_dropdown_width = input_area_width.saturating_sub(1).max(1);
    let chrome = 2 + 2 * pad_x; // 2 borders + 2 pads
    // Each rendered row is: "  " + name (padded to name_col_chars) +
    // " " + desc + " ". The leading "  " is part of the name span; the
    // trailing " " bookends the desc span. Total = 2 + name_col + 1 + desc + 1.
    let row_overhead: u16 = 2 + name_col_chars + 1 + 1;
    let max_content_width = description_char_counts
        .iter()
        .map(|&desc_len| row_overhead.saturating_add(desc_len as u16))
        .max()
        .unwrap_or(40);
    // Final width = content + chrome, with a 40-col floor for usability,
    // capped at the terminal-safe maximum.
    let width = (max_content_width.saturating_add(chrome))
        .max(40)
        .min(max_dropdown_width);
    let inner_width = width.saturating_sub(chrome);
    let desc_budget = inner_width.saturating_sub(row_overhead) as usize;
    (width, inner_width, desc_budget)
}

/// Truncate `s` to at most `max_chars` Unicode scalar values, appending an
/// ellipsis (`…`) when truncation occurred. Returns a `Cow` so the
/// allocation is skipped on the common no-truncate path. Char count is
/// not the same as terminal display width for emoji/CJK, but it's a
/// close-enough proxy for the dropdown row, and importantly it stops
/// 200-char skill descriptions from overflowing the soft width cap.
pub(crate) fn truncate_to_chars(s: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    if max_chars == 0 {
        return std::borrow::Cow::Borrowed("");
    }
    if s.chars().count() <= max_chars {
        return std::borrow::Cow::Borrowed(s);
    }
    // Reserve one char slot for the ellipsis.
    let keep = max_chars.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    std::borrow::Cow::Owned(out)
}

/// Result of fitting the slash-autocomplete dropdown into the rows
/// available above the input area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DropdownFit {
    /// Final dropdown rect height (incl. borders + padding).
    pub height: u16,
    /// Number of items rendered this frame (after scroll window).
    pub visible_items: u16,
    /// Index into the full filtered list at which the visible window starts.
    pub scroll_offset: u16,
}

/// Fit a popup of `total_items` rows into `rows_above_input` rows. If the
/// popup would overflow (default macOS Terminal / Ghostty / old Windows
/// Terminal sit at 80×24, while the full SLASH_COMMANDS + skills list
/// regularly hits 30+ entries), clamp height and scroll the visible
/// window so `selected` stays in view.
///
/// `chrome` is rows consumed by borders + padding (top + bottom).
///
/// Returns `None` when there's literally no room (need at least
/// `chrome + 1` rows for one item) — caller should skip rendering rather
/// than draw an oversized Rect that panics ratatui's buffer write. That
/// panic is the root cause of the "complete empty window, only Ctrl+C
/// escapes" lock-up seen on macOS Terminal / Ghostty.
pub(crate) fn fit_dropdown(
    total_items: u16,
    selected: u16,
    rows_above_input: u16,
    chrome: u16,
) -> Option<DropdownFit> {
    if total_items == 0 || rows_above_input < chrome + 1 {
        return None;
    }
    let desired = total_items.saturating_add(chrome);
    let height = desired.min(rows_above_input);
    let visible_items = height.saturating_sub(chrome);
    // Scroll so the selected row stays inside the visible window.
    let scroll_offset = if selected < visible_items {
        0
    } else {
        selected.saturating_sub(visible_items - 1)
    };
    // Clamp so scroll_offset + visible_items never overshoots total_items.
    let scroll_offset = scroll_offset.min(total_items.saturating_sub(visible_items));
    Some(DropdownFit {
        height,
        visible_items,
        scroll_offset,
    })
}

/// Render slash command autocomplete dropdown above the input area
pub(super) fn render_slash_autocomplete(f: &mut Frame, app: &App, input_area: Rect) {
    let count = app.slash_filtered.len() as u16;
    if count == 0 {
        return;
    }

    // Padding: 1 char each side (left/right inside border), 1 empty line top/bottom.
    let pad_x: u16 = 1;
    let pad_y: u16 = 1;
    let chrome = 2 + pad_y * 2;

    // Fit into the rows ABOVE the input area. Without this clamp,
    // a long built-in + skills list produces a dropdown taller than
    // the terminal (default Terminal.app / Ghostty / Windows Terminal
    // sit at 24 rows), ratatui panics writing out of buffer bounds,
    // catch_unwind clears the screen, the next frame panics again,
    // and the user sees a totally blank TUI that only Ctrl+C escapes.
    let selected = app.slash_selected_index as u16;
    let Some(fit) = fit_dropdown(count, selected, input_area.y, chrome) else {
        return;
    };
    let visible_items_count = fit.visible_items as usize;
    let scroll_offset = fit.scroll_offset as usize;

    // Sizing math is in `dropdown_dimensions` so the regression (long
    // skill descriptions overflowing the buffer) is unit-testable without
    // a live App. The dropdown is responsive — width grows to fit the
    // longest row, capped at `input_area.width - 1`.
    let visible_end = (scroll_offset + visible_items_count).min(app.slash_filtered.len());
    let visible_slice = &app.slash_filtered[scroll_offset..visible_end];
    let mut name_lengths: Vec<usize> = Vec::with_capacity(visible_slice.len());
    let mut desc_lengths: Vec<usize> = Vec::with_capacity(visible_slice.len());
    for &idx in visible_slice {
        name_lengths.push(app.slash_command_name(idx).unwrap_or("").chars().count());
        desc_lengths.push(
            app.slash_command_description(idx)
                .unwrap_or("")
                .chars()
                .count(),
        );
    }
    // Name column is the longest name across the visible set, with a
    // 10-char floor so very short names don't make the dropdown look
    // cramped. The display format `{:<10}` pads short names to 10
    // automatically; longer names render at their natural width.
    let name_col_chars = name_lengths.iter().copied().max().unwrap_or(10).max(10) as u16;
    let (width, _inner_width, desc_budget) =
        dropdown_dimensions(name_col_chars, &desc_lengths, input_area.width, pad_x);
    let dropdown_area = Rect {
        x: input_area.x + 1,
        y: input_area.y.saturating_sub(fit.height),
        width,
        height: fit.height,
    };

    // Build dropdown lines (supports built-in commands, user-defined commands, and skills)
    let lines: Vec<Line> = visible_slice
        .iter()
        .enumerate()
        .map(|(i, &cmd_idx)| {
            let name = app.slash_command_name(cmd_idx).unwrap_or("???");
            let raw_desc = app.slash_command_description(cmd_idx).unwrap_or("");
            let desc = truncate_to_chars(raw_desc, desc_budget);
            let is_selected = (scroll_offset + i) == app.slash_selected_index;

            let style = if is_selected {
                Style::default()
                    .fg(theme::role(Role::Ink))
                    .bg(theme::role(Role::Gray))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::role(Role::TextPrimary))
            };

            let desc_style = if is_selected {
                Style::default()
                    .fg(theme::role(Role::Ink))
                    .bg(theme::role(Role::Gray))
            } else {
                Style::default().fg(theme::role(Role::GrayDim))
            };

            // Pad the name column to `name_col_chars` so descriptions
            // line up vertically. `format!` width arg has to be `usize`.
            let name_col = name_col_chars as usize;
            Line::from(vec![
                Span::styled(format!("  {:<width$}", name, width = name_col), style),
                Span::styled(format!(" {} ", desc.as_ref()), desc_style),
            ])
        })
        .collect();

    // Wrap with empty lines for top/bottom padding
    let mut padded_lines = Vec::with_capacity(lines.len() + 2);
    padded_lines.push(Line::from(""));
    padded_lines.extend(lines);
    padded_lines.push(Line::from(""));

    // Clear the area and render the dropdown
    f.render_widget(Clear, dropdown_area);
    let dropdown = Paragraph::new(padded_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::role(Role::Gray))),
    );
    f.render_widget(dropdown, dropdown_area);
}

/// Render the follow-up suggestion pick-list above the input box (2+ options).
/// A single option renders inline as ghost text instead (see `render_input`).
/// Selecting an option FILLS the input as editable text; it never submits.
pub(super) fn render_followup(f: &mut Frame, app: &App, input_area: Rect) {
    let count = app.followup_suggestions.len() as u16;
    if count < 2 {
        return;
    }

    let pad_x: u16 = 1;
    let pad_y: u16 = 1;
    // +1 extra row for the key hint line inside the box.
    let chrome = 2 + pad_y * 2 + 1;

    let selected = app.followup_selected_index as u16;
    let Some(fit) = fit_dropdown(count, selected, input_area.y, chrome) else {
        return;
    };
    let visible_items_count = fit.visible_items as usize;
    let scroll_offset = fit.scroll_offset as usize;
    let visible_end = (scroll_offset + visible_items_count).min(app.followup_suggestions.len());
    let visible_slice = &app.followup_suggestions[scroll_offset..visible_end];

    // Width grows to the longest visible option, capped inside the frame.
    let longest = visible_slice
        .iter()
        .map(|o| o.chars().count())
        .max()
        .unwrap_or(0);
    let hint = "\u{2191}\u{2193} pick  \u{21e5}/\u{21b5} fill  Esc dismiss";
    let inner = longest.max(hint.chars().count()) + 2; // marker + trailing pad
    let width = ((inner as u16) + 2 + pad_x * 2).min(input_area.width.saturating_sub(1));
    let text_budget = width.saturating_sub(2 + pad_x * 2) as usize;

    let dropdown_area = Rect {
        x: input_area.x + 1,
        y: input_area.y.saturating_sub(fit.height),
        width,
        height: fit.height,
    };

    let mut lines: Vec<Line> = visible_slice
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let is_selected = (scroll_offset + i) == app.followup_selected_index;
            let marker = if is_selected { "\u{25b6} " } else { "  " };
            let text = truncate_to_chars(opt, text_budget.saturating_sub(2));
            let style = if is_selected {
                Style::default()
                    .fg(theme::role(Role::Ink))
                    .bg(theme::role(Role::Gray))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::role(Role::TextPrimary))
            };
            Line::from(vec![Span::styled(
                format!("{}{}", marker, text.as_ref()),
                style,
            )])
        })
        .collect();
    lines.push(Line::from(Span::styled(
        format!("  {hint}"),
        Style::default().fg(theme::role(Role::GrayDim)),
    )));

    let mut padded_lines = Vec::with_capacity(lines.len() + 2);
    padded_lines.push(Line::from(""));
    padded_lines.extend(lines);
    padded_lines.push(Line::from(""));

    f.render_widget(Clear, dropdown_area);
    let dropdown = Paragraph::new(padded_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::role(Role::Gray))),
    );
    f.render_widget(dropdown, dropdown_area);
}

/// Render the emoji picker popup above the input box.
pub(super) fn render_emoji_picker(f: &mut Frame, app: &App, input_area: Rect) {
    let count = app.emoji_filtered.len() as u16;
    if count == 0 {
        return;
    }

    // Same fit-or-skip rule as the slash dropdown — an oversized popup
    // (e.g. unfiltered emoji list = 100+ entries) on a 24-row terminal
    // would write past the buffer and crash the render loop.
    let chrome: u16 = 4; // borders + padding
    let Some(fit) = fit_dropdown(count, app.emoji_selected_index as u16, input_area.y, chrome)
    else {
        return;
    };
    let visible_items_count = fit.visible_items as usize;
    let scroll_offset = fit.scroll_offset as usize;
    let visible_end = (scroll_offset + visible_items_count).min(app.emoji_filtered.len());
    let visible_slice = &app.emoji_filtered[scroll_offset..visible_end];

    let width = 36u16.min(input_area.width);
    let dropdown_area = Rect {
        x: input_area.x + 1,
        y: input_area.y.saturating_sub(fit.height),
        width,
        height: fit.height,
    };

    let lines: Vec<Line> = visible_slice
        .iter()
        .enumerate()
        .map(|(i, &(emoji, shortcode))| {
            let is_selected = (scroll_offset + i) == app.emoji_selected_index;
            let style = if is_selected {
                Style::default()
                    .fg(theme::role(Role::Ink))
                    .bg(theme::role(Role::Gray))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::role(Role::TextPrimary))
            };
            let sc_style = if is_selected {
                Style::default()
                    .fg(theme::role(Role::Ink))
                    .bg(theme::role(Role::Gray))
            } else {
                Style::default().fg(theme::role(Role::GrayDim))
            };
            Line::from(vec![
                Span::styled(format!("  {} ", emoji), style),
                Span::styled(format!(":{}: ", shortcode), sc_style),
            ])
        })
        .collect();

    let mut padded = Vec::with_capacity(lines.len() + 2);
    padded.push(Line::from(""));
    padded.extend(lines);
    padded.push(Line::from(""));

    f.render_widget(Clear, dropdown_area);
    let dropdown = Paragraph::new(padded).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::role(Role::Gray))),
    );
    f.render_widget(dropdown, dropdown_area);
}

/// Render the single-line status bar below the input box.
///
/// Layout:  provider / model  ·  [policy]          ⠙ OpenCrabs is thinking... (3s)
pub(super) fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let orange = theme::role(Role::Accent);

    // --- Session name (left) ---
    let session_name = app
        .current_session
        .as_ref()
        .and_then(|s| s.title.as_deref())
        .unwrap_or("Chat")
        .to_string();

    // --- Provider / model ---
    // One coherent pair (#2264): name and model must come from the same
    // source. Reading them from the two separate getters mixes the chain's
    // active fallback with the primary's stale pick after a sticky fallback.
    let (provider_str, model_str) = if let Some(ref session) = app.current_session {
        let (prov, model) = app.agent_service.provider_pair_for_session(session.id);
        let prefix = format!("{}/", prov);
        let stripped = model.strip_prefix(&prefix).unwrap_or(&model);
        (
            prov,
            crate::tui::provider_selector::model_display_label(stripped).to_string(),
        )
    } else {
        (
            app.agent_service.provider_name(),
            crate::tui::provider_selector::model_display_label(
                app.agent_service.provider_model().as_str(),
            )
            .to_string(),
        )
    };

    // Working directory — collapse $HOME to ~, then truncate if still long
    let raw_dir = app.working_directory.to_string_lossy();
    let home_dir = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    let short_dir = if !home_dir.is_empty() && raw_dir.starts_with(&home_dir) {
        format!("~{}", &raw_dir[home_dir.len()..])
    } else {
        raw_dir.to_string()
    };
    let short_dir = if short_dir.len() > 40 {
        let mut start = short_dir.len().saturating_sub(37);
        while start > 0 && !short_dir.is_char_boundary(start) {
            start -= 1;
        }
        format!("...{}", &short_dir[start..])
    } else {
        short_dir
    };
    // Git branch suffix: when the working directory sits inside a git
    // repo, render `(branch)` as a separate cyan span after the dir.
    // Read per render via `utils::git_branch::current_branch` so a
    // `git checkout` in another terminal shows up on the very next
    // frame — no cache, no manual refresh. The HEAD file is ~30 bytes
    // and stays in the OS page cache, so the read cost is sub-millisecond.
    let git_branch = crate::utils::git_branch::current_branch(&app.working_directory);

    // Active profile chip. Shown only when the user explicitly picked a
    // profile (via `-p <name>` or the `OPENCRABS_PROFILE` env var). When
    // `active_profile()` returns `None` the agent is using the base
    // `~/.opencrabs/` directory — there is no real profile by that name
    // on disk, so we OMIT the chip rather than invent a "default" label
    // that doesn't exist anywhere. Closes issue #167.
    let profile_chip = crate::config::profile::active_profile()
        .map(|name| format!("  ·  profile: {name}"))
        .unwrap_or_default();

    let session_text = format!(" {}", session_name);
    let provider_model_text = format!("  ·  {} / {}{}  ·  ", provider_str, model_str, profile_chip);
    let sep_text = "  ·  ";

    // --- Approval policy (centre-left) ---
    let (policy_text, policy_color) = if app.approval_auto_always {
        ("⚡ yolo", theme::role(Role::Error))
    } else if app.approval_auto_session {
        ("⚡ auto (session)", orange)
    } else {
        ("🔒 approve", theme::role(Role::GrayDim))
    };

    let mut spans = vec![
        Span::styled(
            session_text,
            Style::default().fg(orange).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            provider_model_text,
            Style::default().fg(theme::role(Role::BlueSlate)),
        ),
        Span::styled(short_dir, Style::default().fg(theme::role(Role::BlueSlate))),
    ];
    if let Some(ref branch) = git_branch {
        spans.push(Span::styled(
            format!(" ({branch})"),
            Style::default().fg(theme::role(Role::AccentTeal)),
        ));
    }

    // Tokens/sec footer field.
    //   - Live: output_tokens / active_streaming_window_secs (the
    //     accumulator excludes tool execution waits, between-stream
    //     idle, network round-trips — only counts time the model is
    //     actually emitting tokens). 2026-05-28 user report: pre-fix
    //     this divided by total wall-clock so a 30s tool exec made
    //     "200 tok/s" decay to "8 tok/s" mid-turn.
    //   - Persisted: when no live turn is active, keep showing the
    //     last finalized rate (from `finalize_tps` on response complete)
    //     until the next turn's first token replaces it.
    let live_tps = if app.is_processing && app.streaming_output_tokens > 0 {
        let active = app.current_streaming_active_secs();
        if active > 0.0 {
            Some(app.streaming_output_tokens as f64 / active)
        } else {
            None
        }
    } else {
        None
    };
    let display_tps = live_tps.or(app.last_tps());
    if let Some(tps) = display_tps {
        spans.push(Span::styled(
            "  ·  ",
            Style::default().fg(theme::role(Role::GrayDim)),
        ));
        spans.push(Span::styled(
            format!("{:.0} tok/s", tps),
            Style::default().fg(theme::role(Role::Success)),
        ));
    }

    spans.push(Span::styled(
        sep_text,
        Style::default().fg(theme::role(Role::GrayDim)),
    ));
    spans.push(Span::styled(policy_text, Style::default().fg(policy_color)));

    // Split pane indicator
    if app.pane_manager.is_split() {
        let pane_count = app.pane_manager.pane_count();
        let focused_idx = app
            .pane_manager
            .pane_ids_in_order()
            .iter()
            .position(|id| *id == app.pane_manager.focused)
            .map(|i| i + 1)
            .unwrap_or(1);
        spans.push(Span::styled(
            "  ·  ",
            Style::default().fg(theme::role(Role::GrayDim)),
        ));
        spans.push(Span::styled(
            format!("[{}/{}]", focused_idx, pane_count),
            Style::default().fg(theme::role(Role::Success)),
        ));
    }

    let line = Line::from(spans);
    let para = Paragraph::new(line).alignment(Alignment::Left);
    f.render_widget(para, area);
}
