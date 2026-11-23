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

/// A palette role, stored in parsed markdown instead of a resolved colour.
/// The transcript cache can therefore survive any number of theme switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdTint {
    Strong,
    Blue,
    PanelHi,
    Faint,
    Muted,
}

/// The small slice of the window palette markdown needs. Keeping this as a
/// trait leaves the parser independent of the config crate and window state.
pub trait MdPalette {
    fn tint(&self, tint: MdTint) -> u32;
}

/// Theme-independent inline styling. Font attributes are ready to paint;
/// colours stay semantic until the renderer supplies the live palette.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MdStyle {
    pub font_weight: Option<FontWeight>,
    pub font_style: Option<FontStyle>,
    pub underline: Option<UnderlineStyle>,
    pub strikethrough: Option<StrikethroughStyle>,
    pub color: Option<MdTint>,
    pub background: Option<MdTint>,
}

impl MdStyle {
    pub fn resolve<P: MdPalette>(self, palette: &P) -> HighlightStyle {
        HighlightStyle {
            color: self.color.map(|tint| rgb(palette.tint(tint)).into()),
            background_color: self.background.map(|tint| rgb(palette.tint(tint)).into()),
            font_weight: self.font_weight,
            font_style: self.font_style,
            underline: self.underline,
            strikethrough: self.strikethrough,
            ..Default::default()
        }
    }
}

/// Resolve cached markdown runs for this frame's palette.
pub fn resolve_highlights<P: MdPalette>(
    highlights: &[(Range<usize>, MdStyle)],
    palette: &P,
) -> Vec<(Range<usize>, HighlightStyle)> {
    highlights
        .iter()
        .map(|(range, style)| (range.clone(), style.resolve(palette)))
        .collect()
}

/// How a single newline in the source is read.
///
/// CommonMark folds a soft break into a space, which is right for an answer:
/// the model wraps its own prose and a paragraph should reflow with the pane.
/// It is wrong for anything a person typed. Someone who presses Enter twice
/// expects two lines, and a reply that silently joins them reads as if the
/// window ate the break.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MdFlow {
    /// A soft break is a space (CommonMark).
    #[default]
    Prose,
    /// A soft break is a line break.
    Verbatim,
}

/// One renderable line: the text plus byte-range highlight runs into it,
/// whether it belongs to a code fence (mono, grouped by the renderer), the
/// list/quote nesting depth, and whether it is a heading line.
#[derive(Clone)]
pub struct MdLine {
    pub text: String,
    pub highlights: Vec<(Range<usize>, MdStyle)>,
    /// Every link label on this line, as a byte range into `text` and the
    /// destination it pointed at (#2317). The label alone carries no url, so
    /// without this a click on `[docs](https://x)` has nothing to open.
    pub links: Vec<(Range<usize>, String)>,
    pub mono: bool,
    pub indent: u8,
    pub heading: bool,
    pub heading_level: u8,
    /// A thematic break. It carries no text: the renderer draws a full-width
    /// hairline for it, so the rule follows the pane instead of being a fixed
    /// run of box characters that stopped a third of the way across.
    pub rule: bool,
}

/// A column's alignment, as the table header declared it.
///
/// Own enum rather than `pulldown_cmark::Alignment` so the renderer does not
/// have to import the parser to read a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MdAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// One table cell: its text plus the inline highlight runs into it, byte
/// ranges relative to `text` so the renderer can style the cell on its own.
#[derive(Clone)]
pub struct MdCell {
    pub text: String,
    pub highlights: Vec<(Range<usize>, MdStyle)>,
    /// Link labels inside the cell, ranges relative to `text` like the
    /// highlights above (#2317).
    pub links: Vec<(Range<usize>, String)>,
    /// How the column this cell belongs to asked to be aligned. It used to be
    /// thrown away, so `|---:|` painted left like every other column.
    pub align: MdAlign,
}

/// A whole table, header row first. Column count is `header.len()` (or the
/// widest row, if a malformed table disagrees).
#[derive(Clone, Default)]
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
#[derive(Clone)]
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
    /// The destination of the link currently open, kept so the renderer can
    /// open it when the label is clicked (#2317).
    link_dest: Option<String>,
    heading: bool,
    heading_level: u8,
}

impl Inline {
    fn style(&self, code_bg: bool) -> MdStyle {
        let mut s = MdStyle::default();
        if self.strong > 0 {
            s.font_weight = Some(FontWeight::BOLD);
        }
        if self.emphasis > 0 {
            s.font_style = Some(FontStyle::Italic);
        }
        if self.heading {
            s.font_weight = Some(FontWeight::BOLD);
            s.color = Some(MdTint::Strong);
        }
        if self.link {
            s.color = Some(MdTint::Blue);
            s.underline = Some(UnderlineStyle::default());
        }
        if self.strike {
            s.strikethrough = Some(StrikethroughStyle::default());
        }
        if code_bg {
            s.color = Some(MdTint::Blue);
            s.background = Some(MdTint::PanelHi);
        }
        s
    }
}

/// One open list: whether it numbers its items, and the number the next item
/// takes.
///
/// A stack, not a single slot. The old builder kept one `Option<u64>` that
/// `Tag::Item` took, so the first item printed its number and every item after
/// it fell back to a bullet, and a nested list's end wiped the outer list's
/// counter on the way out.
struct ListState {
    ordered: bool,
    next: u64,
}

struct Builder {
    out: Vec<MdBlock>,
    buf: String,
    hl: Vec<(Range<usize>, MdStyle)>,
    links: Vec<(Range<usize>, String)>,
    mono: bool,
    indent: u8,
    inline: Inline,
    /// Every list currently open, outermost first.
    list_stack: Vec<ListState>,
    /// `Some` while inside a table: text collects into cells instead of
    /// lines, because a cell is not a line and flattening it hides the grid.
    table: Option<TableBuild>,
}

struct TableBuild {
    header: Vec<MdCell>,
    rows: Vec<Vec<MdCell>>,
    row: Vec<MdCell>,
    in_head: bool,
    /// One entry per column, from the table's delimiter row.
    aligns: Vec<MdAlign>,
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
        // A link label's own bytes are what a click lands on, so the range is
        // recorded where the text is written, not derived later.
        if let Some(dest) = &self.inline.link_dest {
            self.links.push((start..self.buf.len(), dest.clone()));
        }
    }

    /// Depth steps a list adds to a line's indent: one per nesting level past
    /// the first, so a top-level list stays flush with the body and a bullet
    /// under a bullet steps in instead of painting flush left.
    fn list_indent(&self) -> u8 {
        (self.list_stack.len().saturating_sub(1)).min(6) as u8
    }

    fn flush(&mut self) {
        let text = std::mem::take(&mut self.buf);
        let hl = std::mem::take(&mut self.hl);
        let links = std::mem::take(&mut self.links);
        let mono = self.mono;
        let indent = self.indent.saturating_add(self.list_indent());
        let heading = self.inline.heading;
        let heading_level = self.inline.heading_level;
        if text.trim().is_empty() {
            return;
        }
        if let Some(table) = &mut self.table {
            // The column is the cell's position in the row being built, which
            // is also how the delimiter row's alignments are indexed.
            let column = if table.in_head {
                table.header.len()
            } else {
                table.row.len()
            };
            let align = table.aligns.get(column).copied().unwrap_or_default();
            let cell = cell_from(text, hl, links, align);
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
            links,
            mono,
            indent,
            heading,
            heading_level,
            rule: false,
        }));
    }

    fn rule(&mut self) {
        self.flush();
        self.out.push(MdBlock::Line(MdLine {
            text: String::new(),
            highlights: Vec::new(),
            links: Vec::new(),
            mono: false,
            indent: self.indent.saturating_add(self.list_indent()),
            heading: false,
            heading_level: 0,
            rule: true,
        }));
    }
}

/// Turn a finished buffer into a cell, moving highlight offsets with the
/// leading whitespace the trim removes.
fn cell_from(
    text: String,
    hl: Vec<(Range<usize>, MdStyle)>,
    links: Vec<(Range<usize>, String)>,
    align: MdAlign,
) -> MdCell {
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
    let mut moved_links = Vec::with_capacity(links.len());
    for (range, dest) in links {
        let start = range.start.max(lo);
        let end = range.end.min(hi);
        if start < end {
            moved_links.push((start - lo..end - lo, dest));
        }
    }
    MdCell {
        text: trimmed.to_string(),
        highlights: moved,
        links: moved_links,
        align,
    }
}

/// pulldown_cmark's HeadingLevel as a plain 1-6 number (0 = not a heading),
/// so the renderer can run a size ladder without importing its types (#2521).
fn heading_level(level: pulldown_cmark::HeadingLevel) -> u8 {
    match level {
        pulldown_cmark::HeadingLevel::H1 => 1,
        pulldown_cmark::HeadingLevel::H2 => 2,
        pulldown_cmark::HeadingLevel::H3 => 3,
        pulldown_cmark::HeadingLevel::H4 => 4,
        pulldown_cmark::HeadingLevel::H5 => 5,
        pulldown_cmark::HeadingLevel::H6 => 6,
    }
}

/// Parse markdown source into renderable blocks. Tables stay tables; every
/// other construct is a run of [`MdLine`]s.
///
/// `flow` decides what a single newline means, so a caller states which
/// surface it is parsing for instead of getting one rule that is wrong for
/// half of them.
pub fn blocks_flow(src: &str, flow: MdFlow) -> Vec<MdBlock> {
    // Every option here is one an event handler below already serves. The
    // task-list marker, footnote reference and math arms used to be dead code
    // because the parser never emitted their events: `TABLES` and
    // `STRIKETHROUGH` were the only options enabled, so `[x]`, `[^1]` and `$x$`
    // all arrived as plain text.
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH;
    let mut b = Builder {
        out: Vec::new(),
        buf: String::new(),
        hl: Vec::new(),
        links: Vec::new(),
        mono: false,
        indent: 0,
        inline: Inline::default(),
        list_stack: Vec::new(),
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
            Event::SoftBreak => match flow {
                MdFlow::Prose => b.push_inline(" ", false),
                MdFlow::Verbatim => b.flush(),
            },
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
                let dim = MdStyle {
                    color: Some(MdTint::Faint),
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
        Tag::Heading { level, .. } => {
            b.flush();
            b.inline.heading = true;
            b.inline.heading_level = heading_level(level);
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
            b.list_stack.push(ListState {
                ordered: start.is_some(),
                next: start.unwrap_or(1),
            });
        }
        Tag::Item => {
            b.flush();
            // The marker is read from the innermost open list and the counter
            // advances, so item 2 prints 2. and not a bullet. A list that
            // started at 3 keeps counting from 3.
            let marker = match b.list_stack.last_mut() {
                Some(list) if list.ordered => {
                    let n = list.next;
                    list.next = list.next.saturating_add(1);
                    format!("{n}. ")
                }
                Some(_) => "\u{2022}  ".to_string(),
                None => "\u{2022}  ".to_string(),
            };
            let start = b.buf.len();
            b.buf.push_str(&marker);
            b.hl.push((
                start..b.buf.len(),
                MdStyle {
                    color: Some(MdTint::Muted),
                    ..Default::default()
                },
            ));
        }
        Tag::Strong => b.inline.strong += 1,
        Tag::Emphasis => b.inline.emphasis += 1,
        Tag::Strikethrough => b.inline.strike = true,
        Tag::Link { dest_url, .. } => {
            b.inline.link = true;
            b.inline.link_dest = Some(dest_url.to_string());
        }
        // A table is collected as a table, never as lines: the window draws
        // the grid, and flattening here is what made cells look like a list.
        Tag::Table(aligns) => {
            b.flush();
            b.table = Some(TableBuild {
                header: Vec::new(),
                rows: Vec::new(),
                row: Vec::new(),
                in_head: false,
                aligns: aligns
                    .iter()
                    .map(|a| match a {
                        pulldown_cmark::Alignment::Center => MdAlign::Center,
                        pulldown_cmark::Alignment::Right => MdAlign::Right,
                        _ => MdAlign::Left,
                    })
                    .collect(),
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
            b.inline.heading_level = 0;
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
            // Only the list that just closed leaves the stack: a nested list's
            // end must not reset the outer counter.
            b.list_stack.pop();
        }
        TagEnd::Item => b.flush(),
        TagEnd::Strong => b.inline.strong = b.inline.strong.saturating_sub(1),
        TagEnd::Emphasis => b.inline.emphasis = b.inline.emphasis.saturating_sub(1),
        TagEnd::Strikethrough => b.inline.strike = false,
        TagEnd::Link => {
            b.inline.link = false;
            b.inline.link_dest = None;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The line texts of a parse, tables dropped: every assertion below is
    /// about what a row of prose says and how far it is indented.
    fn lines(src: &str) -> Vec<MdLine> {
        blocks_flow(src, MdFlow::Prose)
            .into_iter()
            .filter_map(|b| match b {
                MdBlock::Line(l) => Some(l),
                MdBlock::Table(_) => None,
            })
            .collect()
    }

    /// The defect this stack exists for: item one printed `1.` and every item
    /// after it printed a bullet, because `Tag::Item` took the only stored
    /// number and nothing put it back.
    #[test]
    fn ordered_list_numbers_every_item() {
        let parsed = lines("1. one\n2. two\n3. three\n");
        let texts: Vec<&str> = parsed.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["1. one", "2. two", "3. three"]);
    }

    /// A list that starts at a number other than one keeps counting from it.
    #[test]
    fn ordered_list_honours_its_start_number() {
        let parsed = lines("3. three\n4. four\n");
        let texts: Vec<&str> = parsed.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["3. three", "4. four"]);
    }

    /// A bullet list stays bullets: the counter only advances for an ordered
    /// list.
    #[test]
    fn bullet_list_never_prints_numbers() {
        let parsed = lines("- one\n- two\n");
        let texts: Vec<&str> = parsed.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["\u{2022}  one", "\u{2022}  two"]);
    }

    /// A nested list steps in one level and leaves the outer counter alone:
    /// the end of the inner list must not reset what the outer one was up to.
    #[test]
    fn nested_list_indents_and_keeps_the_outer_counter() {
        let parsed = lines("1. one\n   - inner\n2. two\n");
        let texts: Vec<&str> = parsed.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["1. one", "\u{2022}  inner", "2. two"],
            "the outer list keeps counting past the nested one"
        );
        assert_eq!(parsed[0].indent, 0, "a top-level item stays flush");
        assert_eq!(parsed[1].indent, 1, "the nested item steps in");
        assert_eq!(parsed[2].indent, 0, "the outer item steps back out");
    }

    /// CommonMark folds a soft break into a space. Prose wants that; a
    /// hand-typed row wants its Enter key to survive.
    #[test]
    fn verbatim_flow_keeps_single_newlines() {
        let prose = blocks_flow("one\ntwo\n", MdFlow::Prose);
        let prose_texts: Vec<&str> = prose
            .iter()
            .filter_map(|b| match b {
                MdBlock::Line(l) => Some(l.text.as_str()),
                MdBlock::Table(_) => None,
            })
            .collect();
        assert_eq!(prose_texts, vec!["one two"], "prose folds the break");

        let verbatim = blocks_flow("one\ntwo\n", MdFlow::Verbatim);
        let verbatim_texts: Vec<&str> = verbatim
            .iter()
            .filter_map(|b| match b {
                MdBlock::Line(l) => Some(l.text.as_str()),
                MdBlock::Table(_) => None,
            })
            .collect();
        assert_eq!(verbatim_texts, vec!["one", "two"], "verbatim keeps it");
    }

    /// The delimiter row's alignment used to be dropped, so a numeric column
    /// painted left like prose. Each cell now carries its column's alignment.
    #[test]
    fn table_cells_carry_their_column_alignment() {
        let parsed = blocks_flow("| name | qty |\n|:--|--:|\n| a | 1 |\n", MdFlow::Prose);
        let table = parsed
            .into_iter()
            .find_map(|b| match b {
                MdBlock::Table(t) => Some(t),
                MdBlock::Line(_) => None,
            })
            .expect("a table");
        assert_eq!(table.header[0].align, MdAlign::Left);
        assert_eq!(table.header[1].align, MdAlign::Right);
        assert_eq!(table.rows[0][0].align, MdAlign::Left, "body cells too");
        assert_eq!(table.rows[0][1].align, MdAlign::Right, "body cells too");
    }

    /// A thematic break is a flag, not a string: the renderer draws a
    /// hairline that follows the pane width.
    #[test]
    fn rule_is_a_flag_with_no_text() {
        let parsed = lines("above\n\n---\n\nbelow\n");
        let rule = parsed
            .iter()
            .find(|l| l.rule)
            .expect("a thematic break must parse as a rule line");
        assert!(
            rule.text.is_empty(),
            "a rule carries no text: {:?}",
            rule.text
        );
        assert!(
            parsed.iter().any(|l| !l.rule && l.text.contains("above")),
            "the prose around the rule survives"
        );
    }

    /// `TaskListMarker` could never fire while task lists were disabled, so
    /// `- [x] done` reached the window as literal brackets.
    #[test]
    fn task_list_marker_is_rendered() {
        let parsed = lines("- [x] done\n- [ ] todo\n");
        let texts: Vec<&str> = parsed.iter().map(|l| l.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.contains("[x] done")),
            "a checked task keeps its mark: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("[ ] todo")),
            "an open task keeps its empty box: {texts:?}"
        );
    }

    /// Same dead-arm problem: `FootnoteReference` needs `ENABLE_FOOTNOTES`.
    #[test]
    fn footnote_reference_is_rendered() {
        let parsed = lines("body[^1]\n\n[^1]: the note\n");
        assert!(
            parsed.iter().any(|l| l.text.contains("body[1]")),
            "a footnote reference becomes a bracketed number: {:?}",
            parsed.iter().map(|l| l.text.as_str()).collect::<Vec<_>>()
        );
    }

    /// `InlineMath` needs `ENABLE_MATH` to be emitted at all. Math is drawn
    /// with the code treatment (the parser cannot give one inline run its own
    /// font family), which is what keeps `$x^2$` from reading as prose.
    #[test]
    fn inline_math_keeps_its_code_treatment() {
        let parsed = lines("cost is $x^2$ per unit");
        let line = parsed.first().expect("one line of prose");
        assert!(
            line.text.contains("x^2"),
            "the expression survives: {:?}",
            line.text
        );
        assert!(
            line.highlights
                .iter()
                .any(|(range, style)| style.background.is_some()
                    && line
                        .text
                        .get(range.clone())
                        .is_some_and(|t| t.contains("x^2"))),
            "the math run carries the code background: {:?}",
            line.highlights
        );
    }
}
