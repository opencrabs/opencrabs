//! Markdown to gpui: the same job the TUI's `tui/markdown.rs` does for
//! ratatui lines, done here for gpui styled text. `pulldown-cmark` (already
//! a dependency of the TUI) parses, this module turns the events into lines
//! with `HighlightStyle` ranges, and the transcript renders each line as a
//! `StyledText` so a stored answer shows its headings, code, emphasis,
//! lists and links instead of raw `##` and backticks.
//!
//! Phase-1 scope: inline styling (strong, emphasis, inline code, links,
//! strikethrough), headings, lists (bullets and ordered), blockquote
//! indentation, rules, code fences as mono blocks, and tables as real
//! tables. Tables come out of the parser as [`MdBlock::Table`]: the old
//! shape flattened every cell into its own line, which is exactly the
//! "table that renders as a list" defect the window showed. Syntax
//! highlighting inside fences is not attempted (the TUI's syntect pass
//! stays terminal-side for now).

use std::ops::Range;

use gpui::{FontStyle, FontWeight, HighlightStyle, StrikethroughStyle, UnderlineStyle, rgb};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::theme::{BLUE, PANEL_HI, TEXT_FAINT, TEXT_MUTED, TEXT_STRONG};

/// One renderable line: the text plus byte-range highlight runs into it,
/// whether it belongs to a code fence (mono, grouped by the renderer), the
/// list/quote nesting depth, and whether it is a heading line.
pub struct MdLine {
    pub text: String,
    pub highlights: Vec<(Range<usize>, HighlightStyle)>,
    pub mono: bool,
    pub indent: u8,
    pub heading: bool,
}

/// One table cell: its text plus the inline highlight runs into it, byte
/// ranges relative to `text` so the renderer can style the cell on its own.
pub struct MdCell {
    pub text: String,
    pub highlights: Vec<(Range<usize>, HighlightStyle)>,
}

/// A whole table, header row first. Column count is `header.len()` (or the
/// widest row, if a malformed table disagrees).
#[derive(Default)]
pub struct MdTable {
    pub header: Vec<MdCell>,
    pub rows: Vec<Vec<MdCell>>,
}

impl MdTable {
    /// The widest row's cell count: what the renderer must lay out.
    pub fn columns(&self) -> usize {
        self.rows
            .iter()
            .map(|r| r.len())
            .chain(std::iter::once(self.header.len()))
            .max()
            .unwrap_or(0)
    }
}

/// A renderable block: either a run of lines (paragraphs, headings, code
/// fences) or a table, which must not be flattened into lines.
pub enum MdBlock {
    Line(MdLine),
    Table(MdTable),
}

/// Inline context while walking events: which emphases are open.
#[derive(Default)]
struct Inline {
    strong: usize,
    emphasis: usize,
    strike: bool,
    link: bool,
    heading: bool,
}

impl Inline {
    fn style(&self, code_bg: bool) -> HighlightStyle {
        let mut s = HighlightStyle::default();
        if self.strong > 0 {
            s.font_weight = Some(FontWeight::BOLD);
        }
        if self.emphasis > 0 {
            s.font_style = Some(FontStyle::Italic);
        }
        if self.heading {
            s.font_weight = Some(FontWeight::BOLD);
            s.color = Some(rgb(TEXT_STRONG).into());
        }
        if self.link {
            s.color = Some(rgb(BLUE).into());
            s.underline = Some(UnderlineStyle::default());
        }
        if self.strike {
            s.strikethrough = Some(StrikethroughStyle::default());
        }
        if code_bg {
            s.color = Some(rgb(BLUE).into());
            s.background_color = Some(rgb(PANEL_HI).into());
        }
        s
    }
}

struct Builder {
    out: Vec<MdBlock>,
    buf: String,
    hl: Vec<(Range<usize>, HighlightStyle)>,
    mono: bool,
    indent: u8,
    inline: Inline,
    /// `Some(n)` while inside an ordered list item: the marker to print.
    ordered_marker: Option<u64>,
    /// `Some` while inside a table: text collects into cells instead of
    /// lines, because a cell is not a line and flattening it hides the grid.
    table: Option<TableBuild>,
}

struct TableBuild {
    header: Vec<MdCell>,
    rows: Vec<Vec<MdCell>>,
    row: Vec<MdCell>,
    in_head: bool,
}

impl TableBuild {
    /// The widest row's cell count, the same measure [`MdTable::columns`]
    /// uses, so an empty table is dropped instead of drawn as a bare frame.
    fn columns(&self) -> usize {
        self.rows
            .iter()
            .map(|r| r.len())
            .chain(std::iter::once(self.header.len()))
            .max()
            .unwrap_or(0)
    }

    fn finish(self) -> MdTable {
        MdTable {
            header: self.header,
            rows: self.rows,
        }
    }
}

impl Builder {
    fn push_inline(&mut self, t: &str, code_bg: bool) {
        if t.is_empty() {
            return;
        }
        let start = self.buf.len();
        self.buf.push_str(t);
        let style = self.inline.style(code_bg);
        // Only record a run when it would style something; unstyled text
        // falls through the default style in `compute_runs`.
        let styled = code_bg
            || self.inline.strong > 0
            || self.inline.emphasis > 0
            || self.inline.strike
            || self.inline.link
            || self.inline.heading;
        if styled {
            self.hl.push((start..self.buf.len(), style));
        }
    }

    fn flush(&mut self) {
        let text = std::mem::take(&mut self.buf);
        let hl = std::mem::take(&mut self.hl);
        let mono = self.mono;
        let indent = self.indent;
        let heading = self.inline.heading;
        if text.trim().is_empty() {
            return;
        }
        if let Some(table) = &mut self.table {
            let cell = cell_from(text, hl);
            if table.in_head {
                table.header.push(cell);
            } else {
                table.row.push(cell);
            }
            return;
        }
        self.out.push(MdBlock::Line(MdLine {
            text,
            highlights: hl,
            mono,
            indent,
            heading,
        }));
    }

    fn rule(&mut self) {
        self.flush();
        self.out.push(MdBlock::Line(MdLine {
            text: "────────────────────────".into(),
            highlights: Vec::new(),
            mono: false,
            indent: self.indent,
            heading: false,
        }));
    }
}

/// Turn a finished buffer into a cell, moving highlight offsets with the
/// leading whitespace the trim removes.
fn cell_from(text: String, hl: Vec<(Range<usize>, HighlightStyle)>) -> MdCell {
    let trimmed = text.trim();
    let lead = text.len() - text.trim_start().len();
    let lo = lead;
    let hi = lead + trimmed.len();
    let mut moved = Vec::with_capacity(hl.len());
    for (range, style) in hl {
        let start = range.start.max(lo);
        let end = range.end.min(hi);
        if start < end {
            moved.push((start - lo..end - lo, style));
        }
    }
    MdCell {
        text: trimmed.to_string(),
        highlights: moved,
    }
}

/// Parse markdown source into renderable blocks. Tables stay tables; every
/// other construct is a run of [`MdLine`]s.
pub fn blocks(src: &str) -> Vec<MdBlock> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut b = Builder {
        out: Vec::new(),
        buf: String::new(),
        hl: Vec::new(),
        mono: false,
        indent: 0,
        inline: Inline::default(),
        ordered_marker: None,
        table: None,
    };
    for event in Parser::new_ext(src, options) {
        match event {
            Event::Start(tag) => start_tag(&mut b, tag),
            Event::End(tag) => end_tag(&mut b, tag),
            Event::Text(t) => {
                if b.mono {
                    // Fence content: one MdLine per source line, none styled.
                    for (i, line) in t.split('\n').enumerate() {
                        if i > 0 {
                            b.flush();
                        }
                        b.push_inline(line, false);
                    }
                } else {
                    b.push_inline(&t, false);
                }
            }
            Event::Code(t) => b.push_inline(t.trim(), true),
            Event::SoftBreak => b.push_inline(" ", false),
            Event::HardBreak => b.flush(),
            Event::Rule => b.rule(),
            Event::TaskListMarker(done) => {
                b.push_inline(if done { "[x] " } else { "[ ] " }, false);
            }
            Event::FootnoteReference(n) => {
                b.push_inline(&format!("[{n}]"), false);
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                // Stored answers rarely carry HTML; when they do, show the
                // raw text dim rather than silently dropping content.
                let dim = HighlightStyle {
                    color: Some(rgb(TEXT_FAINT).into()),
                    ..Default::default()
                };
                let start = b.buf.len();
                b.buf.push_str(t.as_ref());
                b.hl.push((start..b.buf.len(), dim));
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => b.push_inline(&t, true),
        }
    }
    b.flush();
    b.out
}

fn start_tag(b: &mut Builder, tag: Tag) {
    match tag {
        Tag::Paragraph => {}
        Tag::Heading { .. } => {
            b.flush();
            b.inline.heading = true;
        }
        Tag::BlockQuote(_) => {
            b.flush();
            b.indent = b.indent.saturating_add(1);
        }
        Tag::CodeBlock(kind) => {
            b.flush();
            b.mono = matches!(kind, CodeBlockKind::Fenced(_) | CodeBlockKind::Indented);
        }
        Tag::List(start) => {
            b.flush();
            b.ordered_marker = start;
        }
        Tag::Item => {
            b.flush();
            match b.ordered_marker.take() {
                Some(n) => {
                    let start = b.buf.len();
                    b.buf.push_str(&format!("{n}. "));
                    b.hl.push((
                        start..b.buf.len(),
                        HighlightStyle {
                            color: Some(rgb(TEXT_MUTED).into()),
                            ..Default::default()
                        },
                    ));
                }
                None => {
                    let start = b.buf.len();
                    b.buf.push_str("\u{2022}  ");
                    b.hl.push((
                        start..b.buf.len(),
                        HighlightStyle {
                            color: Some(rgb(TEXT_MUTED).into()),
                            ..Default::default()
                        },
                    ));
                }
            }
        }
        Tag::Strong => b.inline.strong += 1,
        Tag::Emphasis => b.inline.emphasis += 1,
        Tag::Strikethrough => b.inline.strike = true,
        Tag::Link { .. } => b.inline.link = true,
        // A table is collected as a table, never as lines: the window draws
        // the grid, and flattening here is what made cells look like a list.
        Tag::Table(_) => {
            b.flush();
            b.table = Some(TableBuild {
                header: Vec::new(),
                rows: Vec::new(),
                row: Vec::new(),
                in_head: false,
            });
        }
        Tag::TableHead => {
            b.flush();
            b.inline.strong += 1;
            if let Some(table) = &mut b.table {
                table.in_head = true;
            }
        }
        Tag::TableCell => b.flush(),
        Tag::TableRow => b.flush(),
        Tag::MetadataBlock(_) => {
            // Frontmatter: context, not answer text, but rendered honestly
            // as mono lines rather than dropped.
            b.flush();
            b.mono = true;
        }
        Tag::FootnoteDefinition(_) => {
            b.flush();
        }
        Tag::DefinitionList { .. }
        | Tag::DefinitionListTitle
        | Tag::DefinitionListDefinition
        | Tag::Superscript
        | Tag::Subscript
        | Tag::HtmlBlock
        | Tag::Image { .. } => {
            b.flush();
        }
    }
}

fn end_tag(b: &mut Builder, tag: TagEnd) {
    match tag {
        TagEnd::Paragraph => b.flush(),
        TagEnd::Heading(_) => {
            b.flush();
            b.inline.heading = false;
        }
        TagEnd::BlockQuote(_) => {
            b.flush();
            b.indent = b.indent.saturating_sub(1);
        }
        TagEnd::CodeBlock => {
            b.flush();
            b.mono = false;
        }
        TagEnd::List(_) => {
            b.flush();
            b.ordered_marker = None;
        }
        TagEnd::Item => b.flush(),
        TagEnd::Strong => b.inline.strong = b.inline.strong.saturating_sub(1),
        TagEnd::Emphasis => b.inline.emphasis = b.inline.emphasis.saturating_sub(1),
        TagEnd::Strikethrough => b.inline.strike = false,
        TagEnd::Link => b.inline.link = false,
        TagEnd::TableHead => {
            b.flush();
            b.inline.strong = b.inline.strong.saturating_sub(1);
            if let Some(table) = &mut b.table {
                if !table.row.is_empty() {
                    table.header = std::mem::take(&mut table.row);
                }
                table.in_head = false;
            }
        }
        TagEnd::TableCell => b.flush(),
        TagEnd::TableRow => {
            b.flush();
            if let Some(table) = &mut b.table
                && !table.row.is_empty()
            {
                let row = std::mem::take(&mut table.row);
                table.rows.push(row);
            }
        }
        TagEnd::Table => {
            b.flush();
            if let Some(table) = b.table.take()
                && (table.columns() > 0)
            {
                b.out.push(MdBlock::Table(table.finish()));
            }
        }
        TagEnd::MetadataBlock(_) => {
            b.flush();
            b.mono = false;
        }
        TagEnd::FootnoteDefinition
        | TagEnd::DefinitionList
        | TagEnd::DefinitionListTitle
        | TagEnd::DefinitionListDefinition
        | TagEnd::Superscript
        | TagEnd::Subscript
        | TagEnd::HtmlBlock
        | TagEnd::Image => b.flush(),
    }
}
