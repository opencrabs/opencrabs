//! The Help Center (#2407, #2893): four section cards, not one long column.
//!
//! The rows come from `status::help_rows()`, which reads the shared table in
//! `ui_core::help_keys`. This module only groups them by `HelpSection` and
//! paints the groups, so the window and the terminal never spell a chord
//! twice. Nothing here invents a shortcut, a count or a section order.
//!
//! The card chrome follows the Theme screen: the same panel fill, the same
//! hairline border and the same concentric radius, so the reference reads as
//! part of the window instead of a config dump.

use gpui::{Div, IntoElement, ScrollHandle, SharedString, div, prelude::*, px, rems, rgb};
use opencrabs::config::gui_theme::Palette;
use opencrabs::ui_core::card::concentric_radius;
use opencrabs::ui_core::help_keys::{HelpRow, HelpSection};

use crate::palette;
use crate::status::help_rows;
use crate::theme::{Size, text};

/// What the Help Center shows: the running version, a newer release if the
/// check found one, the `/` filter, the scroll position the arrows move, and
/// whether the window is narrow enough that two columns would clip.
pub(crate) struct HelpView {
    pub(crate) version: &'static str,
    pub(crate) update: Option<String>,
    pub(crate) filter: Option<String>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) narrow: bool,
}

/// One card's worth of rows: the section it prints and what survived the
/// filter. A group with no rows paints nothing.
pub(crate) struct HelpGroup {
    pub(crate) section: HelpSection,
    pub(crate) rows: Vec<&'static HelpRow>,
}

impl HelpGroup {
    fn count(&self) -> usize {
        self.rows.len()
    }
}

/// Every section in `HelpSection::ALL` order, with its rows, filtered.
///
/// The filter matches the description or the chord the window prints, which
/// is what the old single-column screen matched. Sections keep their declared
/// order even when a filter empties one out, so a narrowed screen never
/// reshuffles the cards.
pub(crate) fn groups(filter: Option<&str>) -> Vec<HelpGroup> {
    let query = filter.unwrap_or("").to_lowercase();
    let mut out: Vec<HelpGroup> = HelpSection::ALL
        .into_iter()
        .map(|section| HelpGroup {
            section,
            rows: Vec::new(),
        })
        .collect();
    let rows: Vec<_> = help_rows()
        .into_iter()
        .filter(|(_, row)| {
            query.is_empty()
                || row.description.to_lowercase().contains(&query)
                || row.gui.unwrap_or_default().to_lowercase().contains(&query)
        })
        .collect();
    for (section, row) in rows {
        if let Some(group) = out.iter_mut().find(|group| group.section == section) {
            group.rows.push(row);
        }
    }
    out
}

/// How many rows the cards will actually paint.
pub(crate) fn total(groups: &[HelpGroup]) -> usize {
    groups.iter().map(HelpGroup::count).sum()
}

/// A chord as a keycap: a compact bordered box, right-aligned in a fixed
/// column so the labels line up card to card.
fn keycap(chord: &str, shown: Palette) -> Div {
    div()
        .w(rems(8.5))
        .flex_shrink_0()
        .flex()
        .flex_row()
        .justify_end()
        .child(
            div()
                .px_1()
                .rounded(px(4.0))
                .border_1()
                .border_color(rgb(shown.track))
                .bg(rgb(shown.panel_hi))
                .text_xs()
                .text_color(rgb(shown.text_normal))
                .child(SharedString::from(chord.to_string())),
        )
}

/// One section card: a small accent spine, the section name, a live count,
/// then the keycap rows.
fn section_card(group: &HelpGroup, shown: Palette) -> Div {
    let mut rows = div().flex().flex_col().gap_2();
    for row in &group.rows {
        let chord = row.gui.unwrap_or_default();
        rows = rows.child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_3()
                .child(keycap(chord, shown))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .child(SharedString::from(row.description)),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .w_full()
        .rounded(px(concentric_radius(14.0)))
        .border_1()
        .border_color(rgb(shown.panel_hi))
        .bg(rgb(shown.panel))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .h(px(12.0))
                        .w(px(2.0))
                        .rounded(px(1.0))
                        .bg(rgb(shown.accent)),
                )
                .child(text(
                    SharedString::from(group.section.title()),
                    Size::Xs,
                    shown.text_strong,
                ))
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .rounded(px(6.0))
                        .bg(rgb(shown.panel_hi))
                        .text_xs()
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from(group.count().to_string())),
                ),
        )
        .child(rows)
}

/// The frame: a header, the card grid, and one footer that names the keys
/// that drive the screen. Esc is printed here once and nowhere else.
pub(crate) fn help_screen(view: &HelpView) -> impl IntoElement {
    let shown = palette::current();
    let groups = groups(view.filter.as_deref());
    let visible: Vec<&HelpGroup> = groups
        .iter()
        .filter(|group| !group.rows.is_empty())
        .collect();
    let total = total(&groups);

    let mut title = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(rgb(shown.text_faint))
                .child(SharedString::from("REFERENCE")),
        )
        .child(text(
            SharedString::from("Help Center"),
            Size::Xl,
            shown.text,
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .child(SharedString::from(format!("v{}", view.version))),
                )
                .child(div().text_xs().text_color(rgb(shown.text_faint)).child(
                    SharedString::from(if total == 1 {
                        "1 shortcut".to_string()
                    } else {
                        format!("{total} shortcuts")
                    }),
                )),
        );
    if let Some(latest) = &view.update {
        title = title.child(
            div()
                .text_xs()
                .text_color(rgb(shown.accent))
                .child(SharedString::from(format!("v{latest} available"))),
        );
    }

    let header = div()
        .flex()
        .flex_row()
        .items_start()
        .justify_between()
        .px_6()
        .pt_4()
        .pb_3()
        .child(title)
        .child(
            div()
                .text_lg()
                .text_color(rgb(shown.text_muted))
                .child(SharedString::from("🦀")),
        );

    // Two columns, filled row-major: Global and Chat share the top row,
    // Input editing and Sessions the bottom one. A narrow window collapses to
    // a single column instead of clipping a label.
    let mut body = div().flex().flex_col().gap_4().px_6().pb_4();
    if view.narrow {
        for group in &visible {
            body = body.child(section_card(group, shown));
        }
    } else {
        let mut left = div().flex().flex_col().gap_4().flex_1().min_w_0();
        let mut right = div().flex().flex_col().gap_4().flex_1().min_w_0();
        for (index, group) in visible.iter().enumerate() {
            let card = section_card(group, shown);
            if index % 2 == 0 {
                left = left.child(card);
            } else {
                right = right.child(card);
            }
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_4()
                .child(left)
                .child(right),
        );
    }

    let hint = |chord: &'static str, label: &'static str| {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                div()
                    .px_1()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(shown.track))
                    .bg(rgb(shown.panel_hi))
                    .text_xs()
                    .text_color(rgb(shown.text_normal))
                    .child(SharedString::from(chord)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(shown.text_faint))
                    .child(SharedString::from(label)),
            )
    };

    let mut footer = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_6()
        .py_3()
        .border_t_1()
        .border_color(rgb(shown.panel_hi))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_4()
                .child(hint("↑↓", "scroll"))
                .child(hint("pgup/pgdn", "page"))
                .child(hint("/", "filter"))
                .child(hint("esc", "close")),
        );
    if let Some(filter) = &view.filter {
        footer = footer.child(
            div()
                .text_xs()
                .text_color(rgb(shown.accent))
                .child(SharedString::from(format!("/ {filter}"))),
        );
    }

    div()
        .id("help-screen")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(shown.bg))
        .text_color(rgb(shown.text))
        .child(header)
        .child(
            div()
                .id("help-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&view.scroll)
                .child(body),
        )
        .child(footer)
}
