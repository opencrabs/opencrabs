//! Shared rendering utilities
//!
//! Text wrapping, character boundary helpers, and token formatting used across render modules.

use ratatui::{
    style::Style,
    text::{Line, Span},
};
use unicode_bidi::{BidiClass, BidiInfo};
use unicode_width::UnicodeWidthStr;

/// Pre-wrap a Line's text content to fit within max_width, preserving the style
/// of the first span and prepending `padding` to each continuation line.
pub(crate) fn wrap_line_with_padding<'a>(
    line: Line<'a>,
    max_width: usize,
    padding: &'a str,
) -> Vec<Line<'a>> {
    if max_width == 0 {
        return vec![line];
    }
    // Use display width (not byte length) for wrapping decisions
    let total_width: usize = line.spans.iter().map(|s| s.content.width()).sum();
    if total_width <= max_width {
        return vec![line];
    }

    let padding_width = padding.width();

    // Collect all text and track style boundaries
    let mut segments: Vec<(String, Style)> = Vec::new();
    for span in &line.spans {
        segments.push((span.content.to_string(), span.style));
    }

    // Build wrapped lines
    let mut result: Vec<Line<'a>> = Vec::new();
    let mut current_spans: Vec<Span<'a>> = Vec::new();
    let mut current_width: usize = 0;

    for (text, style) in segments {
        let mut remaining = text.as_str();
        while !remaining.is_empty() {
            let available = max_width.saturating_sub(current_width);
            if available == 0 {
                result.push(Line::from(current_spans));
                current_spans = vec![Span::styled(padding.to_string(), Style::default())];
                current_width = padding_width;
                continue;
            }

            let remaining_width = remaining.width();
            if remaining_width <= available {
                current_spans.push(Span::styled(remaining.to_string(), style));
                current_width += remaining_width;
                break;
            } else {
                // Find the byte index where cumulative display width reaches `available`
                let byte_limit = char_boundary_at_width(remaining, available);
                // Look for a word break (space) within that range
                let break_at = remaining[..byte_limit]
                    .rfind(' ')
                    .map(|p| p + 1)
                    .unwrap_or(byte_limit);
                let break_at = if break_at == 0 {
                    byte_limit.max(remaining.ceil_char_boundary(1))
                } else {
                    break_at
                };
                let (chunk, rest) = remaining.split_at(break_at);
                current_spans.push(Span::styled(chunk.to_string(), style));
                remaining = rest.trim_start();
                result.push(Line::from(current_spans));
                current_spans = vec![Span::styled(padding.to_string(), Style::default())];
                current_width = padding_width;
            }
        }
    }
    if !current_spans.is_empty() {
        result.push(Line::from(current_spans));
    }
    if result.is_empty() {
        result.push(line);
    }
    result
}

/// Find the byte index in `s` where the cumulative display width first reaches or exceeds `target_width`.
/// Always returns a valid char boundary.
pub(crate) fn char_boundary_at_width(s: &str, target_width: usize) -> usize {
    let mut width = 0;
    for (idx, ch) in s.char_indices() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > target_width {
            return idx;
        }
        width += ch_width;
    }
    s.len()
}

/// Shorten `text` to at most `width` display cells, ending in an ellipsis
/// when anything was dropped.
///
/// Width is measured in terminal cells, not bytes or chars, so a wide CJK
/// glyph or an emoji never gets split down the middle. Text that already fits
/// is returned unchanged (no ellipsis, no reallocation of its content).
pub(crate) fn truncate_to_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.width() <= width {
        return text.to_string();
    }
    // One cell belongs to the ellipsis, the rest to the visible prefix.
    let budget = width - 1;
    let cut = char_boundary_at_width(text, budget);
    let mut out = text[..cut].trim_end().to_string();
    out.push('…');
    out
}

/// Format token count with a custom label (e.g. "1.2M total", "150K total")
pub(super) fn format_token_count_with_label(tokens: i64, label: &str) -> String {
    let tokens = tokens.max(0) as f64;
    if tokens >= 1_000_000.0 {
        format!("{:.1}M {}", tokens / 1_000_000.0, label)
    } else if tokens >= 1_000.0 {
        format!("{:.1}K {}", tokens / 1_000.0, label)
    } else if tokens > 0.0 {
        format!("{} {}", tokens as i64, label)
    } else {
        "new".to_string()
    }
}

/// Format token count as raw number without label (e.g. "150K", "1.2M")
pub fn format_token_count_raw(tokens: i64) -> String {
    let tokens = tokens.max(0) as f64;
    if tokens >= 1_000_000.0 {
        format!("{:.1}M", tokens / 1_000_000.0)
    } else if tokens >= 1_000.0 {
        format!("{:.0}K", tokens / 1_000.0)
    } else if tokens > 0.0 {
        format!("{}", tokens as i64)
    } else {
        "0".to_string()
    }
}

// ── RTL rendering (#1897) ────────────────────────────────────────────────────

/// True when `ch` is a strong right-to-left character: Arabic letter (`AL`)
/// or any other RTL letter (`R`). This is the fast-path trigger for
/// [`apply_rtl`]: text without these characters renders unchanged.
pub(crate) fn is_rtl_char(ch: char) -> bool {
    matches!(unicode_bidi::bidi_class(ch), BidiClass::AL | BidiClass::R)
}

/// True when `ch` belongs to an Arabic script block (main, supplement,
/// extended A/B and the presentation forms the reshaper emits). Used to skip
/// the joining stage for Hebrew, which needs none.
pub(crate) fn is_arabic_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{0600}'..='\u{06FF}'
            | '\u{0750}'..='\u{077F}'
            | '\u{08A0}'..='\u{08FF}'
            | '\u{FB50}'..='\u{FDFF}'
            | '\u{FE70}'..='\u{FEFF}'
            | '\u{10EC0}'..='\u{10EFA}'
    )
}

/// Shape and reorder a run of text into terminal visual order (#1897).
///
/// Terminals lay out cells left-to-right, so logical-order Arabic/Hebrew
/// reaches the screen mirrored and disconnected. Returns `None` when the text
/// carries no strong RTL characters (Latin fast path, zero cost, zero change);
/// otherwise returns the reordered string.
///
/// Ordering matters: joining runs first, on logical order, because the
/// reshaper's context windows are defined by adjacency; the Unicode
/// Bidirectional Algorithm runs last, on the shaped text (presentation forms
/// keep their `AL` bidi class, so paragraph detection is unaffected).
pub(crate) fn apply_rtl(text: &str) -> Option<String> {
    if !text.chars().any(is_rtl_char) {
        return None;
    }
    let shaped = if text.chars().any(is_arabic_char) {
        arabic_reshaper::arabic_reshape(text)
    } else {
        text.to_string()
    };
    let bidi = BidiInfo::new(&shaped, None);
    if !bidi.has_rtl() {
        return Some(shaped);
    }
    let mut visual = String::with_capacity(shaped.len());
    for paragraph in &bidi.paragraphs {
        visual.push_str(&bidi.reorder_line(paragraph, paragraph.range.clone()));
    }
    Some(visual)
}

/// Apply [`apply_rtl`] to every span of every line, in place, preserving
/// each span's style. Untouched lines keep their borrowed allocations.
pub(crate) fn apply_rtl_lines(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    for line in &mut lines {
        for span in &mut line.spans {
            if let Some(visual) = apply_rtl(span.content.as_ref()) {
                *span = Span::styled(visual, span.style);
            }
        }
    }
    lines
}
