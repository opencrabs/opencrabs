//! The topic row: the leading orientation control and the topic tabs.
//!
//! The strip is drawn twice, once for the window and once per pane, and both
//! read the same helpers: a tab's label is shortened in one place so the
//! window and a split never show a different slice of the same name (#2477).
//!
//! Moved out of the crate root (#2817 follow-up) as pure code motion: the
//! functions and their comments are unchanged, only the module boundary is
//! new. The doc line that opened the block named a name prompt that now has
//! its own overlay doc (#2398, #2414), so it did not travel.

use gpui::{
    AnyElement, App, ClickEvent, Div, IntoElement, MouseButton, MouseDownEvent, SharedString,
    Stateful, Window, div, prelude::*, px, rems, rgb,
};
use opencrabs::ui_core::topic_bar;
use uuid::Uuid;

use crate::palette;
use crate::topic_icons;
use crate::topic_tabs;

/// The topic row (#2400, #2402): the leading button that moves it and the
/// tabs. Across the top or bottom it is one row; down a side it is one column.
///
/// The `...` and its items used to close this row (#2303). They are gone from
/// here (#2480): the menu belongs to the conversation, so it lives at the
/// chat's top right and floats over the transcript instead of growing this
/// strip by five rows when it opens.
pub(crate) fn topic_strip(
    key: &str,
    edge: topic_tabs::Edge,
    toggle_edge: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    tabs: Vec<AnyElement>,
) -> Div {
    // A split draws one strip per pane (#2687), so the control cannot carry a
    // fixed id: two strips sharing one would share their hover state, and a
    // hover in one pane would light the button in another.
    let id = SharedString::from(format!("topic-edge-button-{key}"));
    // The window's strip has always sat flush with the conversation's measure
    // (#2590). A pane is a fraction of that width, so its strip keeps a small
    // inset instead of the window's 24px.
    let in_pane = key != "window";
    // The orientation control shares the topic cell's generous hit target.
    let control = topic_tabs::style(edge.is_vertical());
    let separator = topic_tabs::separator_color();
    let leading = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(rems(control.control_rems))
        .flex_none()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .on_click(toggle_edge)
        .child(topic_icons::orientation_icon(
            control.control_icon_rems,
            palette::current().text_muted,
        ));
    if edge.is_vertical() {
        // A compact fixed rail gives every cell the same square geometry.
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(rems(topic_tabs::CELL_REMS))
            .h_full()
            .items_center()
            .gap_2()
            .py_2()
            .when(edge == topic_tabs::Edge::Left, |d| d.border_r_1())
            .when(edge == topic_tabs::Edge::Right, |d| d.border_l_1())
            .border_color(separator)
            .child(leading)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .children(tabs),
            )
    } else {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .min_w_0()
            // Match the title hairline end to end without moving the tabs.
            .when(!in_pane, |d| d.mx(rems(-1.5)).px_6())
            .when(in_pane, |d| d.px_2())
            .border_b_1()
            .border_color(separator)
            .child(leading)
            // The tabs wrap onto a second line rather than being cut off, so a
            // narrow window never hides a topic (#2414).
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .children(tabs),
            )
    }
}

/// The most words a topic tab prints (#2477). A tab is a label, not the name:
/// Telegram's topic bar shows a short title and keeps the rest on the topic
/// itself, and a tab that grows with a long name pushes its neighbours off the
/// strip.
const TOPIC_TAB_WORDS: usize = 2;

/// The label a topic tab prints: at most [`TOPIC_TAB_WORDS`] words, with an
/// ellipsis when the name runs longer (#2477).
///
/// Only the tab is shortened. The stored name is never touched, so the
/// sidebar, the session list and the rename prompt still read the name that
/// was given, and shortening the tab cannot lose it.
pub(crate) fn topic_label(name: &str) -> String {
    let mut words = name.split_whitespace();
    let mut label = String::new();
    for word in words.by_ref().take(TOPIC_TAB_WORDS) {
        if !label.is_empty() {
            label.push(' ');
        }
        label.push_str(word);
    }
    if words.next().is_some() {
        label.push('\u{2026}');
    }
    label
}

/// One tab of the topic strip (#2400, #2480). The open session is marked the
/// way Telegram's topic bar marks it: the name takes the accent colour, with
/// an opaque rule on the cell's leading edge down a rail or bottom edge on a
/// horizontal bar.
///
/// Both marks reserve their space while inactive, so switching topics never
/// changes the strip's geometry. The cells themselves stay transparent.
#[allow(clippy::too_many_arguments)]
pub(crate) fn topic_tab(
    id: Uuid,
    label: String,
    icon: Option<AnyElement>,
    shape: topic_bar::ChipShape,
    current: bool,
    activity: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_right: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let colour = if current {
        topic_tabs::TELEGRAM_ACCENT
    } else {
        palette::current().text_muted
    };
    let rule = topic_tabs::focus_color(topic_tabs::TELEGRAM_ACCENT, current);
    let vertical = shape == topic_bar::ChipShape::IconAbove;
    let style = topic_tabs::style(vertical);
    let content = div()
        .when(vertical, |d| {
            d.w_full().text_center().text_xs().line_clamp(2)
        })
        .child(SharedString::from(label));
    div()
        .id(SharedString::from(format!("topic-tab-{id}")))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(rgb(colour))
        .cursor_pointer()
        .when(vertical, |d| {
            d.size(rems(style.cell_rems))
                .flex_col()
                .gap_0()
                .p_1()
                .border_l(px(style.focus_bar_px))
                .border_color(rule)
        })
        .when(!vertical, |d| {
            d.flex_row()
                .gap_1()
                .px_2()
                .py_0p5()
                .border_b(px(style.focus_bar_px))
                .border_color(rule)
        })
        .hover(|d| d.text_color(rgb(topic_tabs::TELEGRAM_ACCENT)))
        .on_click(on_click)
        .on_mouse_down(MouseButton::Right, on_right)
        .children(icon)
        // Work in flight in this topic: a dot after the name, in the accent
        // the open chip uses, so the strip answers "is anything happening in
        // there" without opening it (#2762).
        .when(activity, |d| {
            d.child(
                div()
                    .size(px(6.0))
                    .flex_none()
                    .rounded_full()
                    .bg(rgb(topic_tabs::TELEGRAM_ACCENT)),
            )
        })
        .child(content)
}

/// The pill standing for the topics a narrow bar folded away (#2762). A label,
/// not a control: it says how much of the family is off the edge, and how much
/// of that is busy. Same cell geometry as a chip, so the row keeps one height.
pub(crate) fn overflow_chip(label: String, shape: topic_bar::ChipShape) -> AnyElement {
    let vertical = shape == topic_bar::ChipShape::IconAbove;
    let style = topic_tabs::style(vertical);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(rgb(palette::current().text_faint))
        .when(vertical, |d| d.size(rems(style.cell_rems)).p_1())
        .when(!vertical, |d| d.flex_row().gap_1().px_2().py_0p5())
        .child(SharedString::from(label))
        .into_any_element()
}
