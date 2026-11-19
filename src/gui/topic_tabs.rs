//! Where the topic tab row sits in the chat (#2402). The choice is saved as
//! `[gui] topic_tabs`; the leading button of the row cycles it, as Telegram's
//! does.

use gpui::{Hsla, rgb, rgba};

/// One side of the chat the topic row can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Left,
    Bottom,
    Right,
}

impl Edge {
    /// The saved word for this side, the value written to `[gui] topic_tabs`.
    pub fn as_str(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Left => "left",
            Edge::Bottom => "bottom",
            Edge::Right => "right",
        }
    }

    /// The saved word read back. Unset or unknown is the top row.
    pub fn from_config(word: Option<&str>) -> Edge {
        match word {
            Some("left") => Edge::Left,
            Some("bottom") => Edge::Bottom,
            Some("right") => Edge::Right,
            _ => Edge::Top,
        }
    }

    /// The next side when the leading button is pressed: top, left, bottom,
    /// right, then top again.
    pub fn next(self) -> Edge {
        match self {
            Edge::Top => Edge::Left,
            Edge::Left => Edge::Bottom,
            Edge::Bottom => Edge::Right,
            Edge::Right => Edge::Top,
        }
    }

    /// Whether the row runs down the side of the chat rather than across it.
    pub fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// The square geometry shared by every vertical topic cell, including `All`.
/// The rotate control uses the same size so it is readable and easy to hit.
pub const CELL_REMS: f32 = 4.5;

/// Telegram's selection blue from the owner's reference. Topic selection is
/// the one piece of this chrome that deliberately does not follow the theme
/// accent: the comparison target is Telegram's blue text and focus rule.
pub const TELEGRAM_ACCENT: u32 = 0x40a7e3;

/// The visual contract for one orientation. Filled selected cards are not
/// part of Telegram's topic language: colour and the edge rule do that work.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub icon_above: bool,
    pub square: bool,
    pub cell_rems: f32,
    pub selected_fill: Option<u32>,
    pub accent_edge: AccentEdge,
    pub focus_bar_px: f32,
    pub topic_icon_rems: f32,
    pub control_rems: f32,
    pub control_icon_rems: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccentEdge {
    Leading,
    Bottom,
}

pub fn style(vertical: bool) -> Style {
    Style {
        icon_above: vertical,
        square: vertical,
        cell_rems: CELL_REMS,
        selected_fill: None,
        accent_edge: if vertical {
            AccentEdge::Leading
        } else {
            AccentEdge::Bottom
        },
        focus_bar_px: 3.0,
        topic_icon_rems: if vertical { 2.5 } else { 1.5 },
        control_rems: CELL_REMS,
        control_icon_rems: 1.125,
    }
}

/// The selection rule is fully opaque when active and absent otherwise.
/// Palette colors are 0xRRGGBB, so passing one to `rgba` directly would move
/// the blue channel into alpha and make the rule almost invisible.
pub fn focus_color(accent: u32, selected: bool) -> Hsla {
    if selected {
        rgb(accent).into()
    } else {
        rgba(0x00000000).into()
    }
}

/// The hairline under the chat title and around the topic strip. Reading the
/// live palette on every paint keeps it in step with theme and appearance
/// changes instead of freezing a light or dark literal into the chrome.
pub fn separator_color() -> Hsla {
    rgb(crate::palette::current().track).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The topic row is drawn across three files: the `All` cell and its glyph
    /// stayed with the dashboard's own render, the cell itself moved to
    /// `topic_bar_view`, and the overlay the old single file cut it off at is
    /// `overlays`. The concatenation keeps the order the one file had, so a
    /// split between two of them still means what it meant (#2817, #2822).
    const TOPIC_ROW: &str = concat!(
        include_str!("dashboard_topics.rs"),
        include_str!("topic_bar_view.rs"),
        include_str!("overlays.rs"),
    );

    /// The separator and its one negative gutter, plus the window's own copy of
    /// the accent: the strip moved to `topic_bar_view`, the window kept the
    /// other call (#2817), which draws with the pane body.
    const TOPIC_SEPARATORS: &str = concat!(
        include_str!("topic_bar_view.rs"),
        include_str!("dashboard_panes.rs"),
    );

    #[test]
    fn topic_orientation_navigation_runs_counterclockwise() {
        assert_eq!(Edge::Top.next(), Edge::Left);
        assert_eq!(Edge::Left.next(), Edge::Bottom);
        assert_eq!(Edge::Bottom.next(), Edge::Right);
        assert_eq!(Edge::Right.next(), Edge::Top);
    }

    #[test]
    fn topic_style_vertical_all_and_topics_share_square_transparent_cells() {
        let all = style(true);
        let topic = style(true);
        assert_eq!(all.cell_rems, topic.cell_rems);
        assert!(all.square && topic.square);
        let rail = all;
        assert!(rail.square && rail.icon_above);
        assert_eq!(rail.selected_fill, None);
        assert_eq!(rail.accent_edge, AccentEdge::Leading);
        assert_eq!(rail.focus_bar_px, 3.0);
        assert_eq!(rail.topic_icon_rems, 2.5);
        assert_eq!(rail.control_icon_rems, 1.125);
    }

    #[test]
    fn topic_style_horizontal_keeps_icon_leading_and_bottom_accent() {
        let bar = style(false);
        assert!(!bar.square && !bar.icon_above);
        assert_eq!(bar.selected_fill, None);
        assert_eq!(bar.accent_edge, AccentEdge::Bottom);
        assert_eq!(bar.focus_bar_px, 3.0);
        assert_eq!(bar.topic_icon_rems, 1.5);
        assert!(bar.control_rems >= 4.0);
        assert_eq!(bar.control_icon_rems, 1.125);
    }

    #[test]
    fn topic_style_focus_color_is_opaque_only_when_selected() {
        let selected = focus_color(TELEGRAM_ACCENT, true);
        let expected: Hsla = rgb(0x40a7e3).into();
        assert_eq!(selected, expected);
        assert_eq!(selected.a, 1.0);
        assert_eq!(focus_color(TELEGRAM_ACCENT, false).a, 0.0);
    }

    #[test]
    fn topic_style_separators_read_the_live_theme() {
        let expected: Hsla = rgb(crate::palette::current().track).into();
        assert_eq!(separator_color(), expected);
        let src = TOPIC_SEPARATORS;
        assert!(src.matches("topic_tabs::separator_color()").count() >= 2);
        // One negative gutter is left: the topic strip. The chat title row
        // carried the other until it was removed (#2596).
        assert_eq!(src.matches(".mx(rems(-1.5))").count(), 1);
        assert!(
            !src.contains("let mut header = div()"),
            "the chat screen draws no title row (#2596)"
        );
    }

    #[test]
    fn topic_style_renderer_gives_all_an_icon_and_never_fills_a_selected_cell() {
        let src = TOPIC_ROW;
        let all = src
            .split("topic_bar::BarEntry::All =>")
            .nth(1)
            .expect("All entry")
            .split("topic_bar::BarEntry::Topic(id) =>")
            .next()
            .expect("All body");
        assert!(all.contains("topic_icons::all_icon(icon_rems, dark)"));
        assert!(all.contains("relative_luminance(bg) < 0.18"));
        assert!(
            !all.contains("store_name(\"general\")"),
            "All is not a crab-themed topic"
        );
        assert!(src.contains("topic_icons::tab_glyph(icon, icon_rems)"));
        assert!(!src.contains("format!(\"{icon} {label}\")"));
        assert!(src.contains("topic_tabs::TELEGRAM_ACCENT"));
        let tab = src
            .split("fn topic_tab(")
            .nth(1)
            .expect("topic renderer")
            .split("fn update_prompt_overlay(")
            .next()
            .expect("topic renderer body");
        assert!(!tab.contains(".when(current, |d| d.bg("));
        assert!(!tab.contains(".hover(|d| d.bg("));
        assert!(tab.contains(".children(icon)"));
        assert!(tab.contains(".child(content)"));
        assert!(tab.contains(".size(rems(style.cell_rems))"));
        assert!(tab.contains(".border_l(px(style.focus_bar_px))"));
        assert!(tab.contains(".border_b(px(style.focus_bar_px))"));
        // The one circle in the cell is the activity dot (#2762). The cell
        // itself is never a filled pill, which the two bg bans above hold.
        let dot = tab
            .split(".when(activity, |d| {")
            .nth(1)
            .expect("the activity dot")
            .split(".child(content)")
            .next()
            .expect("the activity dot body");
        assert!(dot.contains(".rounded_full()"), "the dot is a circle");
        assert_eq!(
            tab.matches(".rounded_full()").count(),
            1,
            "and the cell is never a filled pill"
        );
        assert!(tab.contains("focus_color"));

        let strip = src
            .split("fn topic_strip(")
            .nth(1)
            .expect("strip renderer")
            .split("/// The most words")
            .next()
            .expect("strip renderer body");
        assert!(strip.contains(".border_b_1()"));
        assert!(!strip.contains(".w_full()"));
        assert!(strip.contains(".border_r_1()"));
        assert!(strip.contains(".border_l_1()"));
        assert!(strip.contains(".w(rems(topic_tabs::CELL_REMS))"));
        assert!(strip.contains(".py_2()"));
        assert!(!strip.contains(".p_2()"));
        assert!(strip.contains("control.control_icon_rems,"));
        assert!(!strip.contains(".rounded(px(12.0))"));
        assert_eq!(strip.matches(".border_color(separator)").count(), 2);
        assert!(!strip.contains("\\u{25EB}"));
    }

    /// The chat row's pill is the expand control (#2949). It used to be a
    /// label: the count printed and the topics it counted could only be opened
    /// from the rail, which is where the reader was not.
    #[test]
    fn the_chat_pill_expands_the_row_and_the_expanded_row_scrolls() {
        let bar = include_str!("dashboard_topics.rs");
        let limit = bar
            .split("let limit = topic_bar::bar_limit(")
            .nth(1)
            .expect("the row reads the expand state")
            .split(';')
            .next()
            .expect("the limit call");
        assert!(limit.contains("self.chat_topics_expanded"));
        assert!(limit.contains("topics.len()"));
        let pill = bar
            .split("if let Some(overflow) = plan.overflow {")
            .nth(1)
            .expect("the folded pill")
            .split("} else if")
            .next()
            .expect("the pill body");
        assert!(
            pill.contains("this.chat_topics_expanded = true;"),
            "the pill expands the row instead of printing a count"
        );
        assert!(
            pill.contains("overflow_chip("),
            "the pill keeps the chip cell"
        );
        let fold = bar
            .split("} else if self.chat_topics_expanded")
            .nth(1)
            .expect("the fold-back pill")
            .split("(cells, pill)")
            .next()
            .expect("the fold body");
        assert!(
            fold.contains("this.chat_topics_expanded = false;"),
            "the open row folds back from the same pill"
        );
        // The pill never joins the chips (#2949). It used to be pushed into the
        // cell list, which put the fold control at the end of a scrolled row.
        assert!(
            !bar.contains("cells.push(overflow_chip("),
            "the pill travels beside the chips, not among them"
        );
        assert_eq!(
            bar.matches("pill = Some(overflow_chip(").count(),
            2,
            "both the expand and the fold pill come back separately"
        );
        let strip = include_str!("topic_bar_view.rs");
        let row = strip
            .split("pub(crate) fn topic_strip(")
            .nth(1)
            .expect("the strip renderer")
            .split("/// The most words")
            .next()
            .expect("the strip body");
        assert_eq!(
            row.matches(".when(scroll,").count(),
            1,
            "only the side column changes when the strip expands"
        );
        // Across the top or bottom the row is ALWAYS one line that scrolls
        // sideways, collapsed or expanded (#3002). Gating this on `scroll`
        // left the collapsed row wrapping onto extra lines.
        assert!(!row.contains(".flex_wrap()"));
        // Compared without whitespace: rustfmt breaks a long chain across
        // lines, and pinning the joined spelling made the check depend on
        // how the formatter happened to wrap it.
        let dense: String = row.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            dense.contains(".flex_nowrap().overflow_x_scroll()"),
            "the one-line row never wraps and scrolls sideways"
        );
        assert_eq!(
            row.matches("overflow_x_scroll()").count(),
            1,
            "the horizontal row is the one that scrolls sideways"
        );
        assert_eq!(
            row.matches("overflow_y_scroll()").count(),
            1,
            "only the side column scrolls down"
        );
        assert!(row.contains("flex_1().min_h_0().overflow_y_scroll()"));
        assert!(
            !row.contains("max_h(rems("),
            "a fixed height was the wrong bound: it wrapped, then clipped"
        );
        // The pill is a sibling of the scroller (#2949). Inside it, a long
        // family carried the fold control to the far end of the row.
        assert!(row.contains("pill: Option<AnyElement>,"));
        assert_eq!(
            row.matches(".children(pill)").count(),
            2,
            "both edges draw the pill outside their scroller"
        );
        assert_eq!(
            row.matches(".children(tabs)").count(),
            2,
            "both edges scroll the chips alone"
        );
        // A scroll container is state, so GPUI only offers `overflow_y_scroll`
        // once the element has an id. The first pass asked for the scroll and
        // skipped the id, and the GUI did not build. Both edges carry one.
        assert_eq!(
            row.matches("topic-strip-tabs-{key}").count(),
            2,
            "both edges key their scroll container"
        );
        let pill = strip
            .split("pub(crate) fn overflow_chip(")
            .nth(1)
            .expect("the pill renderer");
        assert!(pill.contains("on_click: impl Fn(&ClickEvent"));
        assert!(pill.contains(".on_click(on_click)"));
        assert!(pill.contains(".cursor_pointer()"));
        // Same trap for the click target: `on_click` is stateful too, so the
        // id has to come before it in the builder chain or the pill does not
        // compile and, once it does, does not respond.
        let id_at = pill.find(".id(").expect("the click target carries an id");
        let click_at = pill
            .find(".on_click(on_click)")
            .expect("the pill is pressed");
        assert!(id_at < click_at, "the id comes before the click handler");
    }

    /// The rail's chip strip is not bounded (#2949). An earlier pass capped it
    /// and hid topics the rail already showed, so the owner caught a regression
    /// nobody asked for. The rail keeps the behaviour it has always had: three
    /// chips, the pill, and a card that grows with the family.
    #[test]
    fn the_rail_chip_strip_stays_unbounded() {
        let rail = include_str!("rail_view.rs");
        let strip = rail
            .split("fn chip_strip(")
            .nth(1)
            .expect("the rail chip strip")
            .split("/// The channel chip")
            .next()
            .expect("the strip body");
        assert!(!strip.contains("max_h("));
        assert!(!strip.contains("overflow_y_scroll"));
        assert!(strip.contains("sessions::topic_chips(id, rows)"));
        assert!(strip.contains(r#"format!("+{hidden}")"#));
    }
}
