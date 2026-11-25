//! The context-menu primitives every list in the gui draws with: a card, a
//! row item, and the one popover that places a card at the pointer which
//! asked for it.
//!
//! They used to live in `sessions.rs` for historical reasons, which made the
//! transcript depend on the session rail module to draw its own message menu
//! (#2613). The move is mechanical: no behaviour, no ids, no styles changed.

use crate::palette;
use opencrabs::backend::cap::{self, Need};
use opencrabs::backend::hello::HelloReply;
use opencrabs::backend::{Op, session_ops, sessions};

use gpui::{
    AnyElement, App, Div, Edges, InteractiveElement, MouseButton, MouseDownEvent, Pixels, Point,
    SharedString, Stateful, Styled, Window, anchored, div, point, prelude::*, px, rems, rgb,
};

/// Which row action a context-menu item performs.
pub enum MenuKind {
    Pin,
    /// Take a new name, next to Pin: a rail row is where a conversation is
    /// managed, and the prompt used to be reachable only from the body's own
    /// menu (#2668).
    Rename,
    /// Choose the project for a top-level session and every topic inside it.
    AssignProject,
    Delete,
    EnableTopics,
    NewTopic,
    /// Fold every topic back into this session and switch topics off (#2863).
    DisableTopics,
    /// Attach existing sessions as topics of this row, transcripts untouched.
    GroupTopics,
    SplitSide,
    SplitStack,
}

const NO_NEEDS: &[Need] = &[];
const PIN: &[Need] = &[Need::Op(<sessions::SetFavorite as Op>::NAME)];
const RENAME: &[Need] = &[Need::Op(<sessions::RenameSession as Op>::NAME)];
const DELETE: &[Need] = &[Need::Op(<session_ops::DeleteSession as Op>::NAME)];
const ENABLE_TOPICS: &[Need] = &[Need::Op(<sessions::SetTopics as Op>::NAME)];
const NEW_TOPIC: &[Need] = &[Need::Op(<sessions::CreateTopic as Op>::NAME)];
const DISABLE_TOPICS: &[Need] = &[Need::Op(<sessions::MergeTopics as Op>::NAME)];
const GROUP_TOPICS: &[Need] = &[Need::Op(<sessions::AttachTopic as Op>::NAME)];

impl MenuKind {
    /// The operations the row action runs on the crab. The splits are the
    /// window's own layout and ask the crab for nothing.
    pub fn needs(&self) -> &'static [Need] {
        match self {
            MenuKind::Pin => PIN,
            MenuKind::Rename => RENAME,
            MenuKind::AssignProject => NO_NEEDS,
            MenuKind::Delete => DELETE,
            MenuKind::EnableTopics => ENABLE_TOPICS,
            MenuKind::NewTopic => NEW_TOPIC,
            MenuKind::DisableTopics => DISABLE_TOPICS,
            MenuKind::GroupTopics => GROUP_TOPICS,
            MenuKind::SplitSide | MenuKind::SplitStack => NO_NEEDS,
        }
    }

    /// Why the action is greyed out on the crab that answered `hello`, or
    /// `None` when it can run. No handshake yet greys nothing out.
    pub fn unavailable(&self, hello: Option<&HelloReply>) -> Option<String> {
        cap::check(hello?, self.needs())
            .err()
            .map(|why| why.reason())
    }
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
        .id(SharedString::from(format!("menu-item-{label}")))
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .text_sm()
        .text_color(rgb(palette::current().text))
        .hover(|d| d.bg(rgb(palette::current().selection)))
        .child(SharedString::from(label))
}

/// A row that is on the menu but cannot be picked yet: same geometry, faint
/// text, no hover and no pointer. Cut and Copy are drawn this way while the
/// composer holds no selection (#2749), instead of being dropped from the
/// menu and making its shape change under the pointer.
pub fn menu_item_disabled(label: &'static str) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("menu-item-{label}")))
        .px_2()
        .py_1()
        .rounded_sm()
        .text_sm()
        .text_color(rgb(palette::current().text_faint))
        .child(SharedString::from(label))
}

/// A row the crab will not run, with the reason under its label: a missing
/// scope, an older crab, or an action that only works on this machine. Drawn
/// like [`menu_item_disabled`], so the menu keeps its shape, and the reason
/// says why instead of leaving the user to guess.
pub fn menu_item_unavailable(label: &'static str, reason: String) -> Stateful<Div> {
    menu_item_disabled(label).flex().flex_col().child(
        div()
            .text_xs()
            .text_color(rgb(palette::current().text_faint))
            .child(SharedString::from(reason)),
    )
}

/// The row for `label`: pickable, or greyed out with its reason.
pub fn menu_item_gated(label: &'static str, unavailable: Option<String>) -> Stateful<Div> {
    match unavailable {
        Some(reason) => menu_item_unavailable(label, reason),
        None => menu_item(label),
    }
}

/// A menu's card: the bordered, shadowed column every context menu is drawn
/// on (#2384, #2480).
///
/// The caller owns the id and the placement. Every menu now goes through
/// [`menu_popover`], which puts the card at the pointer that opened it
/// (#2690); the id is what keeps two cards apart while one is closing.
pub fn menu_card(id: &'static str, items: Vec<Stateful<Div>>) -> Stateful<Div> {
    // A solid card over the rows: it occludes so nothing beneath it paints
    // through or takes a click, with a border and shadow so it reads as a
    // layer above the list rather than another row in it.
    let mut menu = div()
        .id(id)
        .occlude()
        // Its own presses stop here. The card is drawn inside the surface that
        // opened it, so without this a press on an item reaches that surface's
        // own handler, which closes the card on mouse-down before the item's
        // click can fire on mouse-up (#2613, #2749). A right press stops here
        // for the same reason: the surface under the card would otherwise open
        // a second menu at the point the card already covers.
        .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_ev, _window, cx| cx.stop_propagation())
        .min_w(rems(9.0))
        .flex()
        .flex_col()
        .gap_0p5()
        .p_1()
        .rounded_sm()
        .border_1()
        // The ring is the only thing that separates the card from what it
        // covers, so it has to read against the card's own fill and against
        // every surface a menu opens over. `panel_hi` did neither: four RGB
        // steps off `panel` is 1.04:1 on both themes, and two of the four
        // surfaces paint `panel` themselves (the rail row, and the composer
        // whose field even wears `panel_hi` as its own border), so the card
        // had no visible edge anywhere. `shadow_md` does not cover for it at
        // alpha 0.10 over a near-black fill. `text_faint` is the token the
        // split rules moved to for exactly this reason (#2473) and it lands
        // at 2.9:1 dark and 3.8:1 light.
        .border_color(rgb(palette::current().text_faint))
        .bg(rgb(palette::current().panel))
        .shadow_md();
    for item in items {
        menu = menu.child(item);
    }
    menu
}

/// The one context menu every right press draws: [`menu_card`] anchored at
/// the pointer that opened it (#2690, #2749).
///
/// It used to be placed from the row it belonged to: absolute inside the row
/// at `right_1` / `top_1` (#2742), then beside the rail at the press height.
/// Both answered "where is the row", and the card opened over the split or
/// away from the click. The press point is window coordinates, which is what
/// [`anchored`] takes, and the card is snapped to the window margin whenever
/// that would leave it off screen.
pub fn menu_popover(id: &'static str, items: Vec<Stateful<Div>>, at: Point<Pixels>) -> AnyElement {
    anchored()
        .position(at)
        .offset(point(px(2.0), px(2.0)))
        .snap_to_window_with_margin(Edges::all(px(8.0)))
        .child(menu_card(id, items))
        .into_any_element()
}

/// A popover whose card closes when any mouse button goes down outside it.
///
/// This runs in the capture phase, before transcript text can stop the press
/// in the bubble phase. The press still reaches the surface underneath; only
/// the menu's own state is dismissed (#2862).
pub fn dismissible_menu_popover(
    id: &'static str,
    items: Vec<Stateful<Div>>,
    at: Point<Pixels>,
    dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    anchored()
        .position(at)
        .offset(point(px(2.0), px(2.0)))
        .snap_to_window_with_margin(Edges::all(px(8.0)))
        .child(menu_card(id, items).on_mouse_down_out(dismiss))
        .into_any_element()
}
