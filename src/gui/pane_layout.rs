//! The pane walk: the window's tree of panes, drawn into one element (#2472).
//!
//! A leaf is one pane: its header, its ×, and the transcript it holds. A split
//! lays its children along its own axis with a divider between each pair, and
//! every share comes from the tree rather than from a single window-wide
//! ratio, so a nested split divides only the pane it grew from. A tree of one
//! leaf draws the chat untouched, which is the unsplit window the window has
//! always had.
//!
//! Split out of the crate root (#2817 follow-up): the walk reads the state the
//! window keeps and owns none of it, so it takes a [`PanesCtx`] built in
//! `panes_body` and draws. A leaf has no `Context`, so every listener it needs
//! is closed over the window's weak entity here rather than run from a click
//! handler the leaf could not build.

use gpui::{
    AnyElement, App, ClickEvent, IntoElement, MouseDownEvent, ScrollWheelEvent, SharedString,
    Window, div, prelude::*, px, rgb,
};
use opencrabs::ui_core::card::CARD_RADIUS;
use opencrabs::ui_core::pane_tree::{DropHalf, PaneTree, drop_half};
use opencrabs::ui_core::scroll::wheel_releases_follow;
use opencrabs::ui_core::split::Layout;
use std::collections::HashMap;
use std::rc::Rc;
use uuid::Uuid;

use crate::pane_chrome::{
    UNFOCUSED_PANE_OPACITY, drop_indicator, focus_ring_color, pane_footer, pane_header,
    split_divider, split_pane_width,
};
use crate::wheel_dy;
use crate::transcript;
use crate::{Dashboard, PaneDrag, SplitPane};

/// Everything the pane walk needs to draw one leaf or one split: the chat
/// element (drawn once, at its own leaf), the state each other pane keeps, the
/// caret, the tree's measured areas, and the window handle the per-pane
/// handlers report through (#2472).
pub(crate) struct PanesCtx<'a> {
    pub(crate) chat: Option<AnyElement>,
    pub(crate) chat_session: Option<Uuid>,
    pub(crate) chat_title: SharedString,
    pub(crate) states: &'a HashMap<Uuid, SplitPane>,
    pub(crate) focus: Option<Uuid>,
    pub(crate) content: f32,
    pub(crate) areas: &'a Rc<std::cell::RefCell<HashMap<Vec<usize>, [f32; 4]>>>,
    pub(crate) entity: gpui::WeakEntity<Dashboard>,
    /// Whether the dividers take the pointer. The themes preview draws the
    /// same panes but never resizes them (#2438).
    pub(crate) interactive: bool,
    /// One compact footer per pane session (#2593), precomputed before the
    /// walk so a draw pass never reads the bridge or the plan file.
    pub(crate) footers: &'a HashMap<Uuid, SharedString>,
    /// Each pane's own plan strip, queue and composer (#2530). A leaf has no
    /// `Context`, so this is built up front in `panes_body`, where the plan
    /// files can be re-read and an editor entity read, and the walk only has
    /// to hand each leaf its own. The chat leaf gets the window's chrome, so
    /// an unsplit window never draws its composer twice.
    pub(crate) chrome: &'a mut HashMap<Uuid, AnyElement>,
    /// Where a dragged pane would land (#2683): the pane under the pointer and
    /// the half of it. Read by each leaf to draw its landing band.
    pub(crate) drop: Option<(Uuid, DropHalf)>,
    /// Each pane's own topic strip (#2686, #2687), drawn between its header
    /// and its conversation. Built in `panes_body` because a leaf has no
    /// `Context` to run a click listener from.
    pub(crate) tops: &'a mut HashMap<Uuid, AnyElement>,
}

/// Draw the pane tree (#2472). A leaf is one pane: its header, its ×, and the
/// transcript it holds. A split lays its children along its own axis with a
/// divider between each pair, and every share comes from the tree rather than
/// from a single window-wide ratio, so a nested split divides only the pane it
/// grew from. A tree of one leaf draws the chat untouched, which is the
/// unsplit window the window has always had.
pub(crate) fn pane_element(
    tree: &PaneTree,
    ctx: &mut PanesCtx,
    path: &mut Vec<usize>,
    width_share: f32,
) -> AnyElement {
    match tree {
        PaneTree::Leaf { session, .. } => {
            let is_chat = Some(*session) == ctx.chat_session;
            let focused = if is_chat {
                ctx.focus.is_none()
            } else {
                ctx.focus == Some(*session)
            };
            let title = if is_chat {
                ctx.chat_title.clone()
            } else {
                match ctx.states.get(session) {
                    Some(pane) => SharedString::from(pane.title.clone()),
                    None => SharedString::from("(untitled)"),
                }
            };
            // The pane's own wheel, so a tick up releases only this pane's
            // follow and never the window's (#2698).
            let wheel_weak = ctx.entity.clone();
            let wheel_session = *session;
            let wheel = move |ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut App| {
                if !wheel_releases_follow(wheel_dy(&ev.delta)) {
                    return;
                }
                let _ = wheel_weak.update(cx, |this, cx| {
                    this.release_pane_follow(wheel_session);
                    cx.notify();
                });
            };
            let body: AnyElement = if is_chat {
                ctx.chat.take().unwrap_or_else(|| div().into_any_element())
            } else {
                match ctx.states.get(session) {
                    Some(pane) => div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .child(transcript::view(
                            &pane.transcript,
                            &pane.scroll,
                            &pane.live,
                            false,
                            if width_share < 1.0 {
                                split_pane_width(ctx.content, false, width_share)
                            } else {
                                ctx.content
                            },
                            HashMap::new(),
                            transcript::ViewHooks {
                                // The pane's own drag-to-copy (#2623): the
                                // gesture was dead in every split because this
                                // was `default()`, which leaves `drag: None`.
                                drag: Some(pane.copy_drag.clone()),
                                wheel: Some(Rc::new(wheel)),
                                ..Default::default()
                            },
                        ))
                        .into_any_element(),
                    None => div().into_any_element(),
                }
            };
            let weak = ctx.entity.clone();
            let close_session = *session;
            let close = move |_ev: &ClickEvent, _window: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| this.close_pane(close_session, cx));
            };
            // Clicking anywhere in a pane takes the keyboard, not only the dim
            // composer (#2610). The pane under the pointer is the pane the next
            // keystroke belongs to, and a click that only highlighted a row
            // left the caret where it was.
            let reach = ctx.entity.clone();
            let reach_session = *session;
            let take_keyboard = move |_ev: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
                let next = if is_chat { None } else { Some(reach_session) };
                let _ = reach.update(cx, |this, cx| {
                    if this.pane_focus != next {
                        this.pane_focus = next;
                        // A click that takes the keyboard changes which pane
                        // the next launch puts the caret on (#2675).
                        this.persist_layout();
                        this.refocus_screen();
                        cx.notify();
                    }
                });
            };
            let interactive = ctx.interactive;
            // Where a dragged pane would land if released here (#2683). Only
            // the target draws a band, and only while a pane is in flight.
            let landing = ctx
                .drop
                .filter(|(target, _)| target == session)
                .map(|(_, half)| half);
            let hover_weak = ctx.entity.clone();
            let hover_session = *session;
            let drop_weak = ctx.entity.clone();
            let drop_session = *session;
            let mut node = div()
                .id(SharedString::from(format!("pane-{session}")))
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                // The leaf that owns the keyboard wears the accent all the way
                // round, and the leaves that do not are drawn back so a split
                // reads as one live pane beside disabled ones (#2612). Same
                // border width on both states, so focus never shifts a row.
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(rgb(focus_ring_color(focused)))
                .when(!focused, |d| d.opacity(UNFOCUSED_PANE_OPACITY))
                .when(interactive, |d| {
                    // Capture, not bubble. A transcript row stops the press
                    // on its way up (`cx.stop_propagation()` in the text-row
                    // handler), so a bubble-phase listener never ran for a
                    // click on text, which is most of a pane: the caret
                    // stayed on another leaf and ctrl+| split that one while
                    // the window called the pane just clicked focused
                    // (#2758).
                    d.capture_any_mouse_down(take_keyboard)
                        // Every pane is a drop target (#2683). The half the
                        // pointer is nearest picks the axis and the side while
                        // the drag is still in flight, so the band under it is
                        // the layout the release will produce.
                        .on_drag_move::<PaneDrag>(move |ev, _window, cx| {
                            let from = ev.drag(cx).session;
                            let b = ev.bounds;
                            let bounds = [
                                f32::from(b.origin.x),
                                f32::from(b.origin.y),
                                f32::from(b.origin.x + b.size.width),
                                f32::from(b.origin.y + b.size.height),
                            ];
                            let x = f32::from(ev.event.position.x);
                            let y = f32::from(ev.event.position.y);
                            // gpui delivers `on_drag_move` to EVERY pane while
                            // a drag is live, hovered or not (the dispatch has
                            // no `is_hovered` gate, unlike `on_mouse_move`), so
                            // each pane used to answer with its own bounds and
                            // the last pane painted won: the band landed on a
                            // pane the pointer was nowhere near, and its half
                            // described that pane's nearest edge instead of the
                            // one under the pointer (#2759).
                            let inside = x >= bounds[0]
                                && x <= bounds[2]
                                && y >= bounds[1]
                                && y <= bounds[3];
                            let _ = hover_weak.update(cx, |this, cx| {
                                if inside {
                                    // A pane dropped on itself is not a move,
                                    // so it never shows a landing band of its
                                    // own.
                                    let next = (from != hover_session)
                                        .then(|| (hover_session, drop_half(bounds, x, y)));
                                    if this.pane_drop != next {
                                        this.pane_drop = next;
                                        cx.notify();
                                    }
                                } else if this
                                    .pane_drop
                                    .is_some_and(|(target, _)| target == hover_session)
                                {
                                    // Clear only a band this pane owns, so a
                                    // pane hearing the move does not wipe the
                                    // band the pane under the pointer set.
                                    this.pane_drop = None;
                                    cx.notify();
                                }
                            });
                        })
                        .on_drop::<PaneDrag>(move |drag, _window, cx| {
                            let from = drag.session;
                            let _ = drop_weak.update(cx, |this, cx| {
                                // The half the last move recorded here, or the
                                // right half when the pointer arrived and left
                                // without a move of its own.
                                let half = this
                                    .pane_drop
                                    .filter(|(target, _)| *target == drop_session)
                                    .map(|(_, half)| half)
                                    .unwrap_or(DropHalf {
                                        layout: Layout::Side,
                                        after: true,
                                    });
                                this.pane_drop = None;
                                let moved = this
                                    .panes
                                    .as_mut()
                                    .is_some_and(|tree| tree.move_leaf(from, drop_session, half));
                                if moved {
                                    // A move is the layout the user chose, so
                                    // the release is when it gets written
                                    // (#2683, #2675).
                                    this.persist_layout();
                                }
                                cx.notify();
                            });
                        })
                })
                .child(pane_header(
                    SharedString::from(format!("pane-close-{session}")),
                    title,
                    focused,
                    interactive,
                    *session,
                    close,
                ))
                // The pane's topics sit inside its box, under its name
                // (#2687): a strip above the whole wall described the chat
                // while the panes it hung over held something else.
                .when_some(ctx.tops.remove(session), |d, top| {
                    d.child(div().flex().flex_none().min_w_0().child(top))
                })
                // The transcript takes the slack, the strips and the composer
                // below it keep their height (#2624). Without the wrapper the
                // conversation's own min height won the shrink and the plan
                // strip was squeezed to nothing in a short pane, while the
                // composer survived because its field has a min size: the plan
                // card vanished exactly where the panes got small.
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .child(body),
                );
            // The pane's own plan, queue and composer sit between its
            // transcript and its footer, where the window-level ones sit in
            // an unsplit window (#2530). `flex_none` keeps every one of them
            // at its own height: a shrinkable stack lost its top rows first.
            if let Some(chrome) = ctx.chrome.remove(session) {
                node = node.child(div().flex().flex_col().flex_none().min_w_0().child(chrome));
            }
            node.child(pane_footer(
                SharedString::from(format!("pane-footer-{session}")),
                ctx.footers.get(session).cloned(),
                focused,
            ))
            // Painted last, so the landing band covers the pane's own rows
            // instead of hiding under its transcript (#2683).
            .when_some(landing, |d, half| d.child(drop_indicator(half)))
            .into_any_element()
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
                let child_width = if stacked {
                    width_share
                } else {
                    width_share * child.share()
                };
                let element = pane_element(child, ctx, path, child_width);
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
