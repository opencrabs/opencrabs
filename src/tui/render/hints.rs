//! The one canonical in-dialog command footer and the Ctrl+C command
//! panel (#1775).
//!
//! Before this module there were seven hint shapes in the tree (colon+bold,
//! colon+all-dim, no-colon+accent, bracket spans, bare keys, middle-dot
//! strings, kv rows), each hand-rolled where it rendered. This module is
//! the single renderer: `key: verb` pairs from the scoped keymap in
//! [`crate::tui::app::dialog_keys`], keys in the accent-teal footer color,
//! verbs dim, two-space separators — always at the bottom, inside the
//! border, left-aligned.
//!
//! The same table also drives the expanded command panel toggled by
//! Ctrl+C inside any open dialog, so the compact footer and the expanded
//! panel can never disagree.

use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};

use super::chrome;
use super::responsive;
use super::theme::{self, Role};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

/// Footer key style: accent teal + bold, the /help key coloring the
/// ticket locked as the reference.
fn key_style() -> Style {
    Style::default()
        .fg(theme::role(Role::AccentTeal))
        .add_modifier(Modifier::BOLD)
}

/// Build the canonical footer line for `keys`.
///
/// `indent` prefixes the line (dialogs that sit inside content indented
/// by two spaces pass `"  "`); it is raw, not styled, so the first key
/// still starts its own colored span.
pub fn footer_line(
    indent: &str,
    keys: &[crate::tui::app::dialog_keys::DialogKey],
) -> Line<'static> {
    let mut spans = Vec::new();
    if !indent.is_empty() {
        spans.push(Span::raw(indent.to_string()));
    }
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                "  ",
                Style::default().fg(theme::role(Role::GrayDim)),
            ));
        }
        spans.push(Span::styled(key.label, key_style()));
        spans.push(Span::styled(
            format!(": {}", key.verb),
            Style::default().fg(theme::role(Role::TextDim)),
        ));
    }
    Line::from(spans)
}

/// Render the canonical footer into `area` (one row, left-aligned).
pub fn render_footer(
    frame: &mut Frame,
    area: Rect,
    keys: &[crate::tui::app::dialog_keys::DialogKey],
) {
    if area.width < 3 {
        return;
    }
    frame.render_widget(Paragraph::new(footer_line("", keys)), area);
}

/// A dim horizontal rule across `area`, drawn on the row above a footer.
///
/// The full-bleed dialogs have no border left (#2162), so without this the
/// footer reads as one more row of the list: same background, same left
/// edge, nothing marking where the content stops. One row of `─` in
/// `GrayDim` is the whole cost, and it is the only chrome those screens
/// keep. It is deliberately not a border: no corners, no sides, no title.
pub fn render_separator(frame: &mut Frame, area: Rect) {
    if area.width < 3 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(theme::role(Role::GrayDim)),
        ))),
        area,
    );
}

/// [`footer_line`] fitted to `width`.
///
/// Bindings that do not fit are dropped **whole**, in table order, so the
/// row never ends on half a verb ("Searc"): a footer either says
/// `/: search` or it says nothing about search. The first binding always
/// renders, however narrow the terminal, because a footer with no keys at
/// all is worse than a clipped one.
pub fn footer_line_fitting(
    indent: &str,
    keys: &[crate::tui::app::dialog_keys::DialogKey],
    width: u16,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = indent.width();
    if !indent.is_empty() {
        spans.push(Span::raw(indent.to_string()));
    }
    for (i, key) in keys.iter().enumerate() {
        let binding = key.label.width() + 2 + key.verb.width();
        if i > 0 && used + 2 + binding > width as usize {
            break;
        }
        if i > 0 {
            spans.push(Span::styled(
                "  ",
                Style::default().fg(theme::role(Role::GrayDim)),
            ));
            used += 2;
        }
        spans.push(Span::styled(key.label, key_style()));
        spans.push(Span::styled(
            format!(": {}", key.verb),
            Style::default().fg(theme::role(Role::TextDim)),
        ));
        used += binding;
    }
    Line::from(spans)
}

/// Footer with a dim right-aligned context label ("38 sessions",
/// "104 commands") on the same row.
///
/// The label is drawn only when it fits beside the fitted bindings with at
/// least one blank cell between them, so the two can never collide or
/// overwrite each other; when it does not fit the footer renders alone. Both
/// ends inset by [`responsive::EDGE_INSET`] so the footer shares the margin of
/// the rows above it (#2175).
pub fn render_footer_with_context(
    frame: &mut Frame,
    area: Rect,
    keys: &[crate::tui::app::dialog_keys::DialogKey],
    right_label: &str,
) {
    let inset = responsive::EDGE_INSET;
    if area.width < inset * 2 + 1 {
        return;
    }
    // Everything the footer draws lives inside this rect, so the bindings and
    // the label share one margin instead of each doing its own arithmetic.
    let inner = Rect {
        x: area.x + inset,
        width: area.width - inset * 2,
        ..area
    };
    let line = footer_line_fitting("", keys, inner.width);
    let footer_width: usize = line.spans.iter().map(|s| s.content.width()).sum();
    let label_width = right_label.width();
    // One blank cell between the keys and the label, inside the insets.
    // Without room for all of it the label is dropped, never squeezed against
    // the keys.
    let needed = footer_width + label_width + 1;
    if right_label.is_empty() || needed > inner.width as usize {
        frame.render_widget(Paragraph::new(line), inner);
        return;
    }

    let label_width = label_width as u16;
    frame.render_widget(
        Paragraph::new(line),
        Rect {
            width: inner.width - label_width - 1,
            ..inner
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            right_label.to_string(),
            Style::default().fg(theme::role(Role::GrayDim)),
        )))
        .alignment(Alignment::Right),
        Rect {
            x: area.x + area.width - label_width - inset,
            width: label_width,
            ..area
        },
    );
}

/// Render the expanded command panel for `scope` (#1775 Ctrl+C).
///
/// Drawn over whatever dialog is open, centered on `area`, using the
/// shared modal chrome. Content is the scope's keymap, one `key: verb`
/// row per binding, plus the dismiss hint.
pub fn render_command_panel(frame: &mut Frame, area: Rect, scope: DialogScope) {
    let keys = dialog_keys(scope);
    let longest = keys
        .iter()
        .map(|k| k.label.chars().count() + k.verb.len() + 2)
        .max()
        .unwrap_or(10) as u16;
    let width = (longest + 10).clamp(28, (area.width.saturating_sub(4)).max(28));
    let height =
        (keys.len() as u16 + 4).min(area.height.saturating_sub(2).max(keys.len() as u16 + 4));

    let popup = chrome::centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = chrome::modal_block(
        &format!(" {} commands ", scope.title()),
        theme::role(Role::Accent),
    );

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(keys.len() + 2);
    for key in keys {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(format!("{:<10}", key.label), key_style()),
            Span::styled(key.verb, Style::default().fg(theme::role(Role::TextDim))),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled("Esc/Ctrl+C", key_style()),
        Span::styled(": close", Style::default().fg(theme::role(Role::TextDim))),
    ]));

    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The footer separates keys from verbs: key spans carry bold and a
    /// distinct fg from the dim verb spans. Asserted positionally
    /// (leading key chars vs the chars after `": "`) so it holds under
    /// any active theme — this is the "footer keys are visually
    /// distinct from their labels" acceptance criterion, checked
    /// against the rendered buffer.
    #[test]
    fn footer_keys_and_verbs_are_styled_differently() {
        let keys = dialog_keys(DialogScope::RestartPending);
        let terminal = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(terminal).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 1, 80, 1);
                render_footer(f, area, keys);
            })
            .unwrap();

        // Row 1 renders "Enter: confirm  Esc: cancel": chars 0..5 are
        // the key, char 7 starts the verb, char 16 starts the second
        // key (after the two-space separator at 14..15).
        let buffer = terminal.backend().buffer();
        let key_cell = &buffer[(0, 1)];
        let verb_cell = &buffer[(7, 1)];
        let second_key_cell = &buffer[(16, 1)];
        assert_eq!(key_cell.symbol(), "E");
        assert_eq!(verb_cell.symbol(), "c");
        assert_eq!(second_key_cell.symbol(), "E");
        assert!(
            key_cell.modifier.contains(Modifier::BOLD),
            "footer keys must be bold"
        );
        assert!(
            !verb_cell.modifier.contains(Modifier::BOLD),
            "footer verbs must not be bold"
        );
        assert_ne!(key_cell.fg, verb_cell.fg, "key and verb colors must differ");
        assert!(
            second_key_cell.modifier.contains(Modifier::BOLD),
            "every binding's key is bold, not just the first"
        );
    }

    /// The command panel renders one row per binding plus the dismiss
    /// hint, and clears behind itself.
    #[test]
    fn command_panel_lists_every_binding() {
        let scope = DialogScope::ThemePicker;
        let keys = dialog_keys(scope);
        let terminal = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(terminal).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_command_panel(f, area, scope);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let content: String = buffer
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(content.contains("Theme picker commands"), "title missing");
        for key in keys {
            assert!(
                content.contains(key.verb),
                "verb {} missing from panel",
                key.verb
            );
        }
    }

    /// The footer line format is exactly `key: verb` pairs joined by two
    /// spaces — one canonical shape, no brackets, no middle dots.
    #[test]
    fn footer_line_is_key_colon_verb() {
        let line = footer_line("", dialog_keys(DialogScope::RestartPending));
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(text, "Enter: confirm  Esc: cancel");
    }
}
