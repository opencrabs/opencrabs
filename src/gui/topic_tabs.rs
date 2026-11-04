//! Where the topic tab row sits in the chat (#2402). The choice is saved as
//! `[gui] topic_tabs`; the leading button of the row cycles it, as Telegram's
//! does.

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

/// The visual contract for one orientation. Filled selected cards are not
/// part of Telegram's topic language: colour and the edge rule do that work.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub icon_above: bool,
    pub square: bool,
    pub cell_rems: f32,
    pub selected_fill: Option<u32>,
    pub accent_edge: AccentEdge,
    pub control_rems: f32,
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
        control_rems: CELL_REMS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn topic_style_horizontal_keeps_icon_leading_and_bottom_accent() {
        let bar = style(false);
        assert!(!bar.square && !bar.icon_above);
        assert_eq!(bar.selected_fill, None);
        assert_eq!(bar.accent_edge, AccentEdge::Bottom);
        assert!(bar.control_rems >= 4.0);
    }

    #[test]
    fn topic_style_renderer_gives_all_an_icon_and_never_fills_a_selected_cell() {
        let src = include_str!("main.rs");
        let all = src
            .split("topic_bar::BarEntry::All =>")
            .nth(1)
            .expect("All entry")
            .split("topic_bar::BarEntry::Topic(id) =>")
            .next()
            .expect("All body");
        assert!(all.contains("store_name(\"general\")"));
        let tab = src
            .split("fn topic_tab(")
            .nth(1)
            .expect("topic renderer")
            .split("fn update_prompt_overlay(")
            .next()
            .expect("topic renderer body");
        assert!(!tab.contains(".when(current, |d| d.bg("));
        assert!(!tab.contains(".hover(|d| d.bg("));
        assert!(tab.contains(".children(icon)\n        .child("));
        assert!(tab.contains(".size(rems(style.cell_rems))"));
        assert!(tab.contains(".border_l_2()"));
        assert!(tab.contains(".border_b_2()"));
    }
}
