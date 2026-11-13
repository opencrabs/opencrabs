//! The pane walk: the window's tree of panes, drawn into one element (#2472).
//!
//! This module is the tree and nothing else. A leaf is one pane and a pane
//! draws itself (#2805): its transcript, its strips, its composer, its name,
//! its topics, its status row and the band a dragged pane would land on all
//! live on the pane entity. What is left here is geometry, which is the one
//! thing a child cannot answer for itself.
//!
//! A split lays its children along its own axis with a divider between each
//! pair, and every share comes from the tree rather than from a single
//! window-wide ratio, so a nested split divides only the pane it grew from. A
//! tree of one leaf draws the chat untouched, which is the unsplit window the
//! window has always had.
//!
//! Split out of the crate root (#2817 follow-up): the walk reads the state the
//! window keeps and owns none of it, so it takes a [`PanesCtx`] built in
//! `panes_body` and draws.

use gpui::{
    AnyElement, AnyView, App, Entity, IntoElement, MouseDownEvent, SharedString, Window, div,
    prelude::*,
};
use opencrabs::ui_core::pane_tree::{DropHalf, PaneTree};
use std::collections::HashMap;
use std::rc::Rc;
use uuid::Uuid;

use crate::Dashboard;
use crate::pane_chrome::split_divider;
use crate::pane_view::{LeafBox, PaneView, leaf_box, pane_root_style};

/// Everything the pane walk needs to draw one leaf or one split: the chat
/// element (drawn once, at its own leaf), the state each other pane keeps, the
/// caret, the tree's measured areas, and the window handle a leaf reports
/// through (#2472).
pub(crate) struct PanesCtx<'a> {
    pub(crate) chat: Option<AnyElement>,
    pub(crate) chat_session: Option<Uuid>,
    pub(crate) chat_title: SharedString,
    pub(crate) states: &'a HashMap<Uuid, Entity<PaneView>>,
    pub(crate) focus: Option<Uuid>,
    pub(crate) areas: &'a Rc<std::cell::RefCell<HashMap<Vec<usize>, [f32; 4]>>>,
    pub(crate) entity: gpui::WeakEntity<Dashboard>,
    /// Whether the dividers take the pointer, and whether the chat leaf's
    /// frame installs its listeners. The live screen is the only caller and
    /// passes true; the flag stays because a draw with no pointer behind it
    /// must not resize the tree.
    pub(crate) interactive: bool,
    /// The chat leaf's compact status row (#2593), precomputed before the walk
    /// so a draw pass never reads the bridge or the plan file. A split pane
    /// writes its own from the same text (#2805).
    pub(crate) footers: &'a HashMap<Uuid, SharedString>,
    /// The chat leaf's own plan strip, queue and composer (#2530). A split
    /// pane builds its own column in its own render (#2805); the chat leaf's
    /// state is the window's own, so the window builds it here. An unsplit
    /// window draws that chrome through its normal path, so nothing is
    /// duplicated.
    pub(crate) chrome: &'a mut HashMap<Uuid, AnyElement>,
    /// The chat leaf's own topic strip (#2686, #2687), drawn between its name
    /// and its conversation. Built in `panes_body` because only the window can
    /// mint a listener against the sessions it holds; a split pane builds its
    /// own.
    pub(crate) tops: &'a mut HashMap<Uuid, AnyElement>,
    /// Where a dragged pane would land (#2683): the pane under the pointer and
    /// the half of it. The chat leaf is drawn while the window's own state is
    /// borrowed, so it cannot read this back through the handle a pane uses.
    pub(crate) drop: Option<(Uuid, DropHalf)>,
}

/// Draw the pane tree (#2472). A leaf is one pane and draws itself; a split
/// lays its children along its own axis with a divider between each pair, and
/// every share comes from the tree rather than from a single window-wide
/// ratio, so a nested split divides only the pane it grew from. A tree of one
/// leaf draws the chat untouched, which is the unsplit window the window has
/// always had.
///
/// A leaf's state is its own entity (#2805), so the walk only decides where
/// each leaf sits: it takes no `App` because there is nothing left here to
/// read from one. Nothing in this module mutates the pane map.
pub(crate) fn pane_element(
    tree: &PaneTree,
    ctx: &mut PanesCtx,
    path: &mut Vec<usize>,
) -> AnyElement {
    match tree {
        PaneTree::Leaf { session, .. } => {
            let is_chat = Some(*session) == ctx.chat_session;
            // A split pane draws its own frame: its transcript, its strips, its
            // composer, its name, its topics and its status row all live on the
            // pane entity (#2805). The walk only says where it sits.
            if !is_chat {
                return match ctx.states.get(session) {
                    // A pane is inserted through gpui's cached view path
                    // (#2805). A plain entity element renders its whole subtree
                    // in `request_layout` on every frame the window draws, so
                    // one row landing in one leaf re-walked every other leaf's
                    // transcript: three panes meant three full transcripts per
                    // keystroke, per hover, per spinner frame. Cached, the pane
                    // is laid out from the box it draws with and its last
                    // prepaint is replayed, and only a pane the window notified
                    // draws again. `pane_root_style` is that same box, and it
                    // has to be: the cached path lays the view out as a ROOT
                    // (`layout_as_root(bounds.size)`), where `flex_1` sizes
                    // nothing, which is what once made a pane taller than its
                    // box and painted it over the strips and the leaf beside it.
                    // The pane's frame now fills that box with `size_full`, so
                    // the drawn leaf and the replayed one are the same size.
                    Some(pane) => AnyView::from(pane.clone())
                        .cached(pane_root_style())
                        .into_any_element(),
                    None => div().into_any_element(),
                };
            }
            // The window's own conversation is the leaf whose state the window
            // still holds, so the window hands it the same frame a pane builds
            // for itself (#2612, #2687).
            leaf_box(LeafBox {
                session: *session,
                title: ctx.chat_title.clone(),
                is_chat: true,
                focused: ctx.focus.is_none(),
                interactive: ctx.interactive,
                topics: ctx.tops.remove(session),
                chrome: ctx.chrome.remove(session),
                footer: ctx.footers.get(session).cloned(),
                body: ctx.chat.take().unwrap_or_else(|| div().into_any_element()),
                landing: ctx
                    .drop
                    .filter(|(target, _)| target == session)
                    .map(|(_, half)| half),
                window: ctx.entity.clone(),
            })
        }
        PaneTree::Split {
            layout, children, ..
        } => {
            let stacked = layout.is_stacked();
            let here = path.clone();
            let areas = ctx.areas.clone();
            let key = path.clone();
            let mut row = div()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .gap_2()
                .when(stacked, |d| d.flex_col())
                .when(!stacked, |d| d.flex_row())
                // The span a drag maps onto, read from the two outer children
                // instead of assumed from the viewport: the footer, the input
                // and the topic strip all sit inside the window, so a share
                // measured against the viewport would drift from the pointer
                // (#2476). Each split records its own box, so a nested rule
                // maps onto the pane it lives in rather than the window.
                .on_children_prepainted(move |bounds, _window, _cx| {
                    let (Some(first), Some(last)) = (bounds.first(), bounds.last()) else {
                        return;
                    };
                    let start_x = f32::from(first.origin.x);
                    let start_y = f32::from(first.origin.y);
                    let end_x = f32::from(last.origin.x + last.size.width);
                    let end_y = f32::from(last.origin.y + last.size.height);
                    areas
                        .borrow_mut()
                        .insert(key.clone(), [start_x, start_y, end_x, end_y]);
                });
            for (ix, child) in children.iter().enumerate() {
                path.push(ix);
                let element = pane_element(child, ctx, path);
                path.pop();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("split-slot-{here:?}-{ix}")))
                        .flex()
                        .flex_col()
                        .flex_shrink()
                        .flex_basis(gpui::relative(child.share()))
                        .min_w_0()
                        .min_h_0()
                        .child(element),
                );
                if ix + 1 < children.len() {
                    // The rule starts the drag and the window root finishes
                    // it, so the pointer can slide off the 12px strip without
                    // the panes snapping back (#2476).
                    let weak = ctx.entity.clone();
                    let drag_path = here.clone();
                    let interactive = ctx.interactive;
                    let grab = move |ev: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
                        if !interactive {
                            return;
                        }
                        let _ = weak.update(cx, |this, cx| {
                            this.begin_divider_drag(&drag_path, ix, ev, cx);
                        });
                    };
                    row = row.child(split_divider(stacked, grab));
                }
            }
            row.into_any_element()
        }
    }
}
