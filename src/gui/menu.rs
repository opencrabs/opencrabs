//! The context-menu primitives every list in the gui draws with: a card, a
//! row item, and the popover that pins a card inside a row.
//!
//! They used to live in `sessions.rs` for historical reasons, which made the
//! transcript depend on the session rail module to draw its own message menu
//! (#2613). The move is mechanical: no behaviour, no ids, no styles changed.

use crate::palette;

use gpui::{Div, InteractiveElement, SharedString, Stateful, Styled, div, prelude::*, rems, rgb};

/// Which row action a context-menu item performs.
pub enum MenuKind {
    Pin,
    Delete,
    EnableTopics,
    NewTopic,
    SplitSide,
    SplitStack,
}

/// One item of a context menu. The labels are the interface: Pin flips to
/// Unpin on a pinned row, Delete never changes.
pub struct MenuEntry {
    pub kind: MenuKind,
    pub label: &'static str,
}

/// One row of the context menu: a label and a full-height hit target.
pub fn menu_item(label: &'static str) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("rail-menu-{label}")))
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .text_sm()
        .text_color(rgb(palette::current().text))
        .hover(|d| d.bg(rgb(palette::current().selection)))
        .child(SharedString::from(label))
}

/// A menu's card: the bordered, shadowed column every context menu is drawn
/// on (#2384, #2480).
///
/// The caller owns the id and the placement, because two menus live on
/// different anchors: the rail's card pins itself to the row that opened it,
/// and the chat's pins itself under the conversation's `...`.
pub fn menu_card(id: &'static str, items: Vec<Stateful<Div>>) -> Stateful<Div> {
    // A solid card over the rows: it occludes so nothing beneath it paints
    // through or takes a click, with a border and shadow so it reads as a
    // layer above the list rather than another row in it.
    let mut menu = div()
        .id(id)
        .occlude()
        .min_w(rems(9.0))
        .flex()
        .flex_col()
        .gap_0p5()
        .p_1()
        .rounded_sm()
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(rgb(palette::current().panel))
        .shadow_md();
    for item in items {
        menu = menu.child(item);
    }
    menu
}

/// A row's context menu: [`menu_card`] absolute inside the row, which is
/// relative, so it tracks the row through rail scrolls without
/// window-coordinate math.
pub fn menu_popover(items: Vec<Stateful<Div>>) -> Stateful<Div> {
    menu_card("rail-context-menu", items)
        .absolute()
        .right_1()
        .top_1()
}
