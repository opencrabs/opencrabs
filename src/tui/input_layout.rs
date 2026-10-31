//! Shared geometry for the TUI input box.
//!
//! Three callers need the same answer to "which byte sits under this screen
//! cell": the renderer that paints the drag highlight, the drag to copy
//! handler, and click to place the caret. Before this module that math was
//! copied three times and the copies could disagree (the drag copy and the
//! highlight differed on the first character of every soft wrapped row). One
//! copy, one answer.
//!
//! Coordinates are terminal screen cells. The input box is drawn with a top
//! and a bottom border, every rendered row carries a two column prefix
//! (`❯ `, `$ ` or two spaces), and the box scrolls once the buffer is taller
//! than the visible rows. All three facts are handled here so a caller only
//! has to say where the box is and where the mouse is.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Columns of text one input row can hold.
///
/// The box has no side borders, but every row spends two columns on its
/// prefix (`❯ `, `$ `, `⏳` or two spaces), so the text room is the area
/// width minus two.
pub fn content_width(area_width: u16) -> usize {
    area_width.saturating_sub(2) as usize
}

/// Byte range `[start, end)` of every visual (soft wrapped) row in `buf`.
///
/// Mirrors the wrapping in `render::input::render_input`: a row holds at most
/// `content_width` display columns, an empty logical line is still one row,
/// and a trailing `\n` produces a final empty row (the caret line).
pub fn visual_rows(buf: &str, content_width: usize) -> Vec<(usize, usize)> {
    let mut rows: Vec<(usize, usize)> = Vec::new();
    if content_width == 0 {
        return rows;
    }
    let buf_len = buf.len();
    let mut line_start = 0usize;
    while line_start <= buf_len {
        let line_end = buf
            .get(line_start..)
            .unwrap_or_default()
            .find('\n')
            .map(|i| line_start + i)
            .unwrap_or(buf_len);
        let line = buf.get(line_start..line_end).unwrap_or_default();
        let line_width = UnicodeWidthStr::width(line);
        let row_count = if line_width == 0 {
            1
        } else {
            line_width.div_ceil(content_width)
        };

        for vr in 0..row_count {
            let chunk_start = vr * content_width;
            let chunk_end = chunk_start + content_width;
            let mut disp = 0usize;
            let mut char_start = 0usize;
            let mut char_end = line.len();
            let mut found_start = false;
            for (idx, ch) in line.char_indices() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if vr > 0 && !found_start && disp >= chunk_start {
                    char_start = idx;
                    found_start = true;
                }
                if disp + cw > chunk_end {
                    char_end = idx;
                    break;
                }
                disp += cw;
            }
            rows.push((line_start + char_start, line_start + char_end));
        }

        if line_end == buf_len {
            break;
        }
        line_start = line_end + 1;
    }
    rows
}

/// Byte offset of `disp_col` inside visual row `visual_row`.
///
/// A column past the end of the row lands on the row end. A row index past the
/// last row lands on the end of the buffer, so a click below the text puts the
/// caret at the end of the message instead of nowhere. The result is always a
/// char boundary: a column inside a double width character snaps forward.
pub fn byte_at_visual_pos(
    rows: &[(usize, usize)],
    buf: &str,
    visual_row: usize,
    disp_col: usize,
) -> usize {
    let Some(&(row_start, row_end)) = rows.get(visual_row) else {
        return buf.len();
    };
    let chunk = buf.get(row_start..row_end).unwrap_or_default();
    let mut width = 0usize;
    for (idx, ch) in chunk.char_indices() {
        if width >= disp_col {
            return row_start + idx;
        }
        width += UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    row_end
}

/// Map a screen cell inside the input box to `(visual_row, disp_col)`.
///
/// Returns `None` for the top border row and for a column inside the row
/// prefix, so a click there is ignored instead of yanking the caret to
/// column zero. The bottom border is the caller's job because only the
/// caller knows the box height.
pub fn screen_to_visual(
    row: u16,
    col: u16,
    input_x: u16,
    input_y: u16,
    scroll_y: u16,
) -> Option<(usize, usize)> {
    if row <= input_y {
        return None;
    }
    let content_left = input_x.saturating_add(2);
    if col < content_left {
        return None;
    }
    Some((
        (row - input_y - 1) as usize + scroll_y as usize,
        (col - content_left) as usize,
    ))
}

/// Visual row for a drag point, clamped instead of rejected.
///
/// A drag that leaves the box upward keeps selecting from the start of the
/// buffer, which is what the drag to copy path has always done.
fn clamped_visual_row(row: u16, input_y: u16, scroll_y: u16) -> usize {
    if row <= input_y {
        0
    } else {
        (row - input_y - 1) as usize + scroll_y as usize
    }
}

fn clamped_disp_col(col: u16, input_x: u16) -> usize {
    col.saturating_sub(input_x.saturating_add(2)) as usize
}

/// Byte range selected between two screen cells, in reading order.
///
/// `None` when there is nothing to select. `a` and `b` are `(col, row)`.
#[allow(clippy::too_many_arguments)]
pub fn selection_range(
    buf: &str,
    content_width: usize,
    input_x: u16,
    input_y: u16,
    scroll_y: u16,
    a: (u16, u16),
    b: (u16, u16),
) -> Option<(usize, usize)> {
    if content_width == 0 || buf.is_empty() {
        return None;
    }
    let (start, end) = if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    };
    let rows = visual_rows(buf, content_width);
    let byte_of = |point: (u16, u16)| -> usize {
        byte_at_visual_pos(
            &rows,
            buf,
            clamped_visual_row(point.1, input_y, scroll_y),
            clamped_disp_col(point.0, input_x),
        )
    };
    let from = byte_of(start);
    let to = byte_of(end);
    Some((from.min(to), from.max(to)))
}

/// Text between two screen cells, in reading order.
#[allow(clippy::too_many_arguments)]
pub fn selection_text(
    buf: &str,
    content_width: usize,
    input_x: u16,
    input_y: u16,
    scroll_y: u16,
    a: (u16, u16),
    b: (u16, u16),
) -> String {
    match selection_range(buf, content_width, input_x, input_y, scroll_y, a, b) {
        Some((from, to)) => buf.get(from..to).unwrap_or_default().to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 10;

    #[test]
    fn content_width_reserves_the_row_prefix() {
        assert_eq!(content_width(80), 78);
        assert_eq!(content_width(1), 0);
    }

    #[test]
    fn plain_text_is_one_row() {
        assert_eq!(visual_rows("hello", W), vec![(0, 5)]);
    }

    #[test]
    fn empty_buffer_is_one_empty_row() {
        assert_eq!(visual_rows("", W), vec![(0, 0)]);
    }

    #[test]
    fn a_long_line_wraps_at_the_content_width() {
        // Row one holds ten cells, row two holds the remaining two. The
        // second row starts at byte 10, not byte 9: the boundary character
        // belongs to exactly one row.
        assert_eq!(visual_rows("abcdefghijkl", W), vec![(0, 10), (10, 12)]);
    }

    #[test]
    fn newlines_split_logical_lines() {
        assert_eq!(visual_rows("ab\ncd", W), vec![(0, 2), (3, 5)]);
    }

    #[test]
    fn trailing_newline_yields_a_final_empty_row() {
        assert_eq!(visual_rows("ab\n", W), vec![(0, 2), (3, 3)]);
    }

    #[test]
    fn wide_characters_never_split() {
        // Three double width characters, six cells, wrapped at four: the
        // break lands between characters, never inside one.
        assert_eq!(visual_rows("日本語", 4), vec![(0, 6), (6, 9)]);
    }

    #[test]
    fn byte_at_visual_pos_hits_row_start_and_end() {
        let rows = visual_rows("abcdefghijkl", W);
        assert_eq!(byte_at_visual_pos(&rows, "abcdefghijkl", 0, 0), 0);
        assert_eq!(byte_at_visual_pos(&rows, "abcdefghijkl", 0, 10), 10);
        assert_eq!(byte_at_visual_pos(&rows, "abcdefghijkl", 1, 0), 10);
        assert_eq!(byte_at_visual_pos(&rows, "abcdefghijkl", 1, 2), 12);
    }

    #[test]
    fn byte_at_visual_pos_clamps_past_the_last_row() {
        let rows = visual_rows("abc", W);
        assert_eq!(byte_at_visual_pos(&rows, "abc", 9, 0), 3);
    }

    #[test]
    fn byte_at_visual_pos_always_lands_on_a_char_boundary() {
        let buf = "日本語";
        let rows = visual_rows(buf, 4);
        for row in 0..rows.len() {
            for col in 0..8 {
                let byte = byte_at_visual_pos(&rows, buf, row, col);
                assert!(buf.is_char_boundary(byte), "row {row} col {col} -> {byte}");
            }
        }
    }

    #[test]
    fn screen_to_visual_rejects_borders_and_the_prefix_column() {
        assert_eq!(screen_to_visual(10, 5, 0, 10, 0), None, "top border");
        assert_eq!(screen_to_visual(11, 1, 0, 10, 0), None, "left of text");
        assert_eq!(screen_to_visual(11, 2, 0, 10, 0), Some((0, 0)));
    }

    #[test]
    fn screen_to_visual_adds_the_scroll_offset() {
        // Row 11 is the first content row; with two rows scrolled away it is
        // visual row 2 of the buffer.
        assert_eq!(screen_to_visual(11, 2, 0, 10, 2), Some((2, 0)));
        assert_eq!(screen_to_visual(13, 7, 0, 10, 2), Some((4, 5)));
    }

    #[test]
    fn selection_text_copies_a_multiline_range() {
        let buf = "hello\nworld";
        let text = selection_text(buf, 20, 0, 10, 0, (2, 11), (5, 12));
        assert_eq!(text, "hello\nwor");
    }

    #[test]
    fn selection_text_follows_the_scroll_offset() {
        let buf = "l0\nl1\nl2\nl3";
        // Same screen row, two different buffer rows depending on the scroll.
        assert_eq!(selection_text(buf, 20, 0, 10, 0, (2, 11), (4, 11)), "l0");
        assert_eq!(selection_text(buf, 20, 0, 10, 2, (2, 11), (4, 11)), "l2");
    }

    #[test]
    fn selection_text_on_a_wrapped_row_does_not_duplicate_the_boundary_char() {
        let buf = "abcdefghijkl";
        // Row two starts at "k", so selecting its first two cells is "kl".
        assert_eq!(selection_text(buf, W, 0, 10, 0, (2, 12), (4, 12)), "kl");
        // And the whole buffer comes back once, in order.
        assert_eq!(
            selection_text(buf, W, 0, 10, 0, (2, 11), (4, 12)),
            "abcdefghijkl"
        );
    }

    #[test]
    fn selection_of_an_empty_buffer_is_empty() {
        assert_eq!(selection_text("", 20, 0, 10, 0, (2, 11), (4, 12)), "");
    }
}
