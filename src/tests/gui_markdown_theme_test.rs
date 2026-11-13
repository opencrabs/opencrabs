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
