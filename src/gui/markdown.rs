//! Markdown to gpui: the same job the TUI's `tui/markdown.rs` does for
//! ratatui lines, done here for gpui styled text. `pulldown-cmark` (already
//! a dependency of the TUI) parses, this module turns the events into lines
//! with `HighlightStyle` ranges, and the transcript renders each line as a
//! `StyledText` so a stored answer shows its headings, code, emphasis,
//! lists and links instead of raw `##` and backticks.
//!
//! Phase-1 scope: inline styling (strong, emphasis, inline code, links,
//! strikethrough), headings, lists (bullets and ordered), blockquote
//! indentation, rules, and code fences as mono blocks. Tables render as
//! plain rows with the header bold; syntax highlighting inside fences is
//! not attempted (the TUI's syntect pass stays terminal-side for now).

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
    out: Vec<MdLine>,
    buf: String,
    hl: Vec<(Range<usize>, HighlightStyle)>,
    mono: bool,
    indent: u8,
    inline: Inline,
    /// `Some(n)` while inside an ordered list item: the marker to print.
    ordered_marker: Option<u64>,
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
        if !text.is_empty() {
            self.out.push(MdLine {
                text,
                highlights: hl,
                mono,
                indent,
                heading,
            });
        }
    }

    fn rule(&mut self) {
        self.flush();
        self.out.push(MdLine {
            text: "────────────────────────".into(),
            highlights: Vec::new(),
            mono: false,
            indent: self.indent,
            heading: false,
        });
    }
}

/// Parse markdown source into renderable lines.
pub fn lines(src: &str) -> Vec<MdLine> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut b = Builder {
        out: Vec::new(),
        buf: String::new(),
        hl: Vec::new(),
        mono: false,
        indent: 0,
        inline: Inline::default(),
        ordered_marker: None,
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
        Tag::TableHead => {
            b.flush();
            b.inline.strong += 1;
        }
        Tag::TableCell => {
            b.push_inline("   ", false);
        }
        Tag::Table(_) | Tag::TableRow => {}
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
        }
        TagEnd::TableCell | TagEnd::TableRow | TagEnd::Table => {
            b.flush();
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
