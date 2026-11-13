//! Cached markdown must resolve its semantic colours against the palette in
//! force at render time, in both theme-switch directions.

use crate::config::gui_theme::{LIGHT, ORGN, Palette};
use gpui::rgb;
use markdown::{MdBlock, MdLine, MdPalette, MdTint};

#[path = "../gui/markdown.rs"]
pub mod markdown;

impl MdPalette for Palette {
    fn tint(&self, tint: MdTint) -> u32 {
        match tint {
            MdTint::Strong => self.text_strong,
            MdTint::Blue => self.blue,
            MdTint::PanelHi => self.panel_hi,
            MdTint::Faint => self.text_faint,
            MdTint::Muted => self.text_muted,
        }
    }
}

fn one_line(src: &str) -> MdLine {
    let mut parsed = markdown::blocks(src);
    assert_eq!(parsed.len(), 1, "{src:?} must produce one line");
    match parsed.pop().expect("length checked") {
        MdBlock::Line(line) => line,
        MdBlock::Table(_) => panic!("{src:?} unexpectedly produced a table"),
    }
}

struct Parsed {
    heading: MdLine,
    inline: MdLine,
    list: MdLine,
    html: MdLine,
}

impl Parsed {
    fn new() -> Self {
        Self {
            heading: one_line("# Heading"),
            inline: one_line("`code` [link](https://example.com)"),
            list: one_line("- item"),
            html: one_line("<span>raw</span>"),
        }
    }

    fn assert_palette(&self, palette: Palette) {
        let heading = markdown::resolve_highlights(&self.heading.highlights, &palette);
        assert_eq!(heading.len(), 1);
        assert_eq!(heading[0].1.color, Some(rgb(palette.text_strong).into()));

        let inline = markdown::resolve_highlights(&self.inline.highlights, &palette);
        assert_eq!(inline.len(), 2);
        assert_eq!(inline[0].1.color, Some(rgb(palette.blue).into()));
        assert_eq!(
            inline[0].1.background_color,
            Some(rgb(palette.panel_hi).into())
        );
        assert_eq!(inline[1].1.color, Some(rgb(palette.blue).into()));
        assert!(inline[1].1.underline.is_some());

        let list = markdown::resolve_highlights(&self.list.highlights, &palette);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].1.color, Some(rgb(palette.text_muted).into()));

        let html = markdown::resolve_highlights(&self.html.highlights, &palette);
        assert!(!html.is_empty());
        for (_, style) in html {
            assert_eq!(style.color, Some(rgb(palette.text_faint).into()));
        }
    }
}

#[test]
fn cached_markdown_follows_light_to_dark() {
    let parsed = Parsed::new();
    parsed.assert_palette(LIGHT);
    parsed.assert_palette(ORGN);
}

#[test]
fn cached_markdown_follows_dark_to_light() {
    let parsed = Parsed::new();
    parsed.assert_palette(ORGN);
    parsed.assert_palette(LIGHT);
}

/// The transcript reads every field on these types (`src/gui/transcript.rs`),
/// but this module is included by path, so the bin that consumes them is not
/// part of this compilation unit. Pinning the structure here keeps that
/// contract honest, and is what stops the fields reading as dead code.
#[test]
fn parsed_markdown_carries_the_structure_the_transcript_paints() {
    let heading = one_line("## Ship it");
    assert!(heading.heading);
    assert_eq!(heading.heading_level, 2);
    assert_eq!(heading.text, "Ship it");
    assert_eq!(heading.indent, 0);
    assert!(!heading.mono);
    assert!(heading.links.is_empty());

    let quoted = one_line("> quoted");
    assert_eq!(quoted.indent, 1);
    assert!(!quoted.heading);
    assert_eq!(quoted.heading_level, 0);

    let link = one_line("[docs](https://example.com)");
    assert_eq!(link.text, "docs");
    assert_eq!(link.links.len(), 1);
    assert_eq!(link.links[0].1, "https://example.com");
    assert_eq!(
        link.text.get(link.links[0].0.clone()),
        Some("docs"),
        "the link range must point at the label, which is what a click lands on"
    );

    let fenced = one_line("```\nfn main() {}\n```");
    assert!(fenced.mono);
    assert_eq!(fenced.text, "fn main() {}");

    let blocks = markdown::blocks("| [a](https://example.com) | b |\n| --- | --- |\n| 1 | 2 |");
    assert_eq!(blocks.len(), 1, "a table is one block, never flattened");
    let table = match blocks.into_iter().next().expect("one block") {
        MdBlock::Table(table) => table,
        MdBlock::Line(line) => panic!("a table must stay a table, got {:?}", line.text),
    };
    assert_eq!(table.columns(), 2);
    assert_eq!(table.header.len(), 2);
    assert_eq!(table.header[0].text, "a");
    assert_eq!(table.header[0].links[0].1, "https://example.com");
    assert!(
        !table.header[0].highlights.is_empty(),
        "a link in a cell must carry its highlight run"
    );
    assert_eq!(table.rows.len(), 1);
    assert_eq!(table.rows[0][1].text, "2");
}
