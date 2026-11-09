//! The context-menu primitives every list in the gui draws with: a card, a
//! row item, and the popover that anchors a card beside the row that asked
//! for it.
//!
//! They used to live in `sessions.rs` for historical reasons, which made the
//! transcript depend on the session rail module to draw its own message menu
//! (#2613). The move is mechanical: no behaviour, no ids, no styles changed.

use crate::palette;

use gpui::{
    AnyElement, Div, Edges, InteractiveElement, Pixels, Point, SharedString, Stateful, Styled,
    anchored, div, point, prelude::*, px, rems, rgb,
};

/// Which row action a context-menu item performs.
pub enum MenuKind {
    Pin,
    /// Take a new name, next to Pin: a rail row is where a conversation is
    /// managed, and the prompt used to be reachable only from the body's own
    /// menu (#2668).
    Rename,
    Delete,
    EnableTopics,
    NewTopic,
    /// Attach existing sessions as topics of this row, transcripts untouched.
    GroupTopics,
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

/// A row's context menu: [`menu_card`] placed beside the row it was asked
/// from, not on top of it (#2742).
///
/// The card used to be absolute inside the row at `right_1` / `top_1`, so a
/// nine-rem card covered most of the row it belonged to, and on the last rows
/// half of it hung below the window. Its anchor is the point the caller
/// computed from the press, already clear of the rail, so the only offset here
/// lifts the card level with the pointer rather than below it, and the card is
/// snapped to the window margin whenever that would leave it off screen.
pub fn menu_popover(items: Vec<Stateful<Div>>, at: Point<Pixels>) -> AnyElement {
    anchored()
        .position(at)
        .offset(point(px(0.0), px(-10.0)))
        .snap_to_window_with_margin(Edges::all(px(8.0)))
        .child(menu_card("rail-context-menu", items))
        .into_any_element()
}
