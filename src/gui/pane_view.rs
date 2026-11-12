//! One split pane as an entity of its own (#2805).
//!
//! Every pane's state used to sit in a map on the window, so the window was
//! the only thing that could repaint: a row landing in one leaf, a plan task
//! settling in another, or a scroll tick in a third all went through
//! `Dashboard::notify` and redrew the whole wall. Lifting the state into a
//! `PaneView` entity gives each leaf a repaint of its own.
//!
//! The state itself does not change. `title`, `transcript`, `scroll`, `follow`,
//! `editor`, `plan`, `plan_for`, `live`, `busy`, `cancel` and `copy_drag` were
//! per-leaf before this module existed and stay per-leaf after it (#2438,
//! #2530, #2614, #2623, #2685).
//!
//! A leaf cannot reach the window's map from its own `Context`, so what it
//! needs from the window travels one of two ways: state that must be written
//! before the frame (the width the walk measured) arrives as a field, and
//! everything else (which leaf holds the keyboard, whether a queue is folded,
//! which plan is open) is read back through a weak handle to the window. The
//! same handle is what the leaf's listeners report through, so a tap inside a
//! pane is answered by the window that owns it.

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, IntoElement, MouseButton, MouseDownEvent,
    Render, ScrollHandle, ScrollWheelEvent, SharedString, WeakEntity, Window, div, prelude::*, px,
    rgb,
};
use opencrabs::brain::tools::suggest_options::SuggestionItem;
use opencrabs::ui_core::card::CARD_RADIUS;
use opencrabs::ui_core::pane_tree::{DropHalf, drop_half};
use opencrabs::ui_core::scroll::{Follow, scrolled_from_top, wheel_releases_follow};
use opencrabs::ui_core::split::Layout;
use std::collections::HashMap;
use std::rc::Rc;
use uuid::Uuid;

use crate::agent_bridge;
use crate::composer_view::{command_popup_view, emoji_popup_view, options_strip, queue_strip};
use crate::editor;
use crate::pane_chrome::{
    UNFOCUSED_PANE_OPACITY, drop_indicator, focus_ring_color, pane_composer_dim, pane_footer,
    pane_header,
};
use crate::plan_strip;
use crate::task_strip;
use crate::transcript::{self, ClickHandler, CopyDrag, TranscriptState};
use crate::{Dashboard, PaneDrag, wheel_dy};

/// One pane beside the window's own chat: its transcript, its scroll and
/// everything that belongs to the conversation it draws (#2438).
///
/// Where the pane sits, how much space it holds and which session it shows
/// still live in the pane tree, so a leaf carries neither a layout nor a
/// place of its own (#2472). What it carries is the state a conversation
/// keeps while it is on screen.
pub(crate) struct PaneView {
    /// The conversation this leaf draws. The window's map is keyed by the same
    /// id, but a leaf needs it for its own listeners, which run with no map in
    /// hand.
    pub(crate) session: Uuid,
    /// The session's title as the rail shows it, for the pane header.
    pub(crate) title: String,
    pub(crate) transcript: TranscriptState,
    pub(crate) scroll: ScrollHandle,
    /// This pane's own bottom-follow state (#2685), per leaf like every other
    /// piece of pane chrome.
    pub(crate) follow: Follow,
    /// The pane's own composer (#2530). What is typed here stays with the pane
    /// it was typed into, so moving focus never carries a draft across.
    pub(crate) editor: Entity<editor::Editor>,
    /// The pane's own plan strip, read from its session's plan file and cached
    /// by mtime like the window's.
    pub(crate) plan: plan_strip::PlanStripState,
    /// Which session the strip's path was resolved for, so the DB lookup runs
    /// once per pane rather than once per frame.
    pub(crate) plan_for: Option<Uuid>,
    /// The turn this pane is running (#2614). A leaf owns its own stream, so
    /// sending here never repaints the window's conversation.
    pub(crate) live: transcript::LiveTurn,
    /// Whether this pane's turn is in flight: its composer takes a queue
    /// instead of starting a second one, exactly as the window does.
    pub(crate) busy: bool,
    /// The token that cancels THIS pane's turn, so `/stop` in one leaf leaves
    /// the other running.
    pub(crate) cancel: Option<tokio_util::sync::CancellationToken>,
    /// This pane's own drag-to-copy state (#2623). A pane transcript used to
    /// be built with default hooks, which leave `drag: None`, so every pane
    /// had no anchor, no highlight and no release handler: the gesture was
    /// dead the moment a second pane existed. It cannot share the window's
    /// either, because the row text is stored by ordinal and two panes plus
    /// the window all count rows from zero.
    pub(crate) copy_drag: Rc<CopyDrag>,
    /// The window this leaf belongs to. The wheel hook reports through it,
    /// because a tick that releases this pane's follow is a change to the
    /// window's pane map and a leaf's own context cannot reach that.
    window: WeakEntity<Dashboard>,
    /// The width the walk measured for this leaf, written each frame it
    /// changes. `Render` takes no arguments beyond the window and the leaf's
    /// own context, so the number has to arrive as state.
    width: f32,
    /// The window-owned half of this leaf's frame, folded into one number
    /// (#2805).
    ///
    /// A pane is inserted through gpui's cached view path, so a pane that was
    /// not notified replays its last prepaint instead of drawing again. Every
    /// value its draw reads from outside itself, the keyboard owner, the
    /// status row, the drop band, the topic strip, the queue, the offers, the
    /// plan file and its own width, is what the walk hashes here. A difference
    /// is the only thing that redraws a pane the user did not touch.
    stamp: u64,
}

impl PaneView {
    /// Build one leaf's state. `width` starts at the window's content width
    /// and is corrected by the first walk that measures the leaf.
    pub(crate) fn new(
        session: Uuid,
        title: String,
        editor: Entity<editor::Editor>,
        copy_drag: Rc<CopyDrag>,
        width: f32,
        window: WeakEntity<Dashboard>,
    ) -> Self {
        Self {
            session,
            title,
            transcript: TranscriptState {
                session: Some(session),
                ..Default::default()
            },
            scroll: ScrollHandle::new(),
            follow: Follow::new(),
            editor,
            plan: plan_strip::PlanStripState::default(),
            plan_for: Some(session),
            live: transcript::LiveTurn::default(),
            busy: false,
            cancel: None,
            copy_drag,
            window,
            width,
            stamp: 0,
        }
    }

    /// Hand this leaf the frame the walk resolved for it (#2624, #2805).
    ///
    /// The width comes from the tree's shares, so a narrow column wraps its
    /// transcript to its own width instead of the window's. The stamp covers
    /// everything else the draw reads from the window, and a different stamp
    /// is what redraws a pane that was cached. The plan file is re-read here on
    /// its mtime, because a cached pane never reaches the refresh in its own
    /// draw (#2691).
    pub(crate) fn sync_frame(&mut self, width: f32, stamp: u64, cx: &mut Context<Self>) {
        self.width = width;
        let before = self.plan.mtime();
        self.plan.refresh();
        if self.stamp == stamp && self.plan.mtime() == before {
            return;
        }
        self.stamp = stamp;
        cx.notify();
    }

    /// Snap this leaf's transcript to its newest row only while the reader is
    /// still following: a reader who scrolled up keeps their place while rows
    /// land under them (#2685, #2698).
    pub(crate) fn snap_if_following(&mut self) {
        let offset = scrolled_from_top(f32::from(self.scroll.offset().y));
        let max = f32::from(self.scroll.max_offset().height);
        if self.follow.update(offset, max) {
            self.scroll.scroll_to_bottom();
        }
    }
}

impl PaneView {
    /// The strips and the composer this pane draws under its conversation
    /// (#2530, #2805): its plan card, its queue, the follow-up offers made to
    /// it, the detached task running for it, and its own input.
    ///
    /// The window still owns the state these read (which leaf holds the
    /// keyboard, whether a queue is folded, which plan is open), so the pane
    /// reads it through its weak handle and mints its listeners against the
    /// same handle. Nothing here is built by the walk and handed over: the
    /// column, its ids and its handlers all belong to the leaf.
    fn chrome(&mut self, cx: &mut Context<Self>) -> Div {
        let mut col = div().flex().flex_col().flex_none().gap_2();
        let Some(window) = self.window.upgrade() else {
            return col;
        };
        let session = self.session;
        // The window's own answers, read once, so its state is not borrowed
        // while the strips are built.
        let (panes, expanded, queue_expanded, topic_idle, focused, running) = {
            let win = window.read(cx);
            (
                win.panes
                    .as_ref()
                    .map(|tree| tree.leaves().len())
                    .unwrap_or(1),
                win.plan_expanded.contains(&session),
                win.composer.queue_expanded,
                win.topic_draft.is_none(),
                win.split_focus() == Some(session),
                win.running_tasks_label_for(session),
            )
        };
        // How much plan this pane can read (#2624): a wall of panes keeps a
        // bar, a pair keeps the running task and the next one or two. The
        // width is the one the walk measured for this leaf.
        let density = plan_strip::pane_density(panes, self.width);
        // A plan waiting on approval is the one card a pane must not lose in a
        // split (#2755): it is the only place the tap lives.
        let actions = plan_strip::PlanActions {
            on_approve: self.plan_tap("/execute"),
            on_discard: self.plan_tap("/discard"),
        };
        // The plan file is re-read on this path, with its mtime as the cache
        // key: a rewritten plan redraws on the next frame.
        self.plan.refresh();
        if let Some(view) = self
            .plan
            .editing_view(Some(&actions))
            .or_else(|| self.plan.pane_view(density, expanded))
        {
            // A click trades the conversation's room for the whole checklist,
            // per pane (#2624). The handler stops the event so the press does
            // not also hand the leaf the keyboard.
            let weak = self.window.clone();
            col = col.child(
                view.id(SharedString::from(format!("plan-strip-{session}")))
                    .on_mouse_down(
                        MouseButton::Left,
                        move |_ev: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
                            cx.stop_propagation();
                            let _ = weak.update(cx, |this, cx| this.toggle_pane_plan(session, cx));
                        },
                    ),
            );
        }
        let queued = agent_bridge::queue_snapshot(session);
        if !queued.is_empty() {
            // Only a folded queue is worth a click, so a two-message list does
            // not react to a stray tap (#2629).
            let folds = queued.len() > 2;
            let weak = self.window.clone();
            col = col.child(
                queue_strip(&queued, queue_expanded)
                    .id(SharedString::from(format!("queue-strip-{session}")))
                    .on_mouse_down(
                        MouseButton::Left,
                        move |_ev: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
                            cx.stop_propagation();
                            if folds {
                                let _ = weak.update(cx, |this, cx| {
                                    this.composer.queue_expanded =
                                        !this.composer.queue_expanded;
                                    cx.notify();
                                });
                            }
                        },
                    ),
            );
        }
        // The follow-up offer sits with the queue, above the composer it will
        // send into (#2708).
        let offers = agent_bridge::options_snapshot(session);
        if !offers.is_empty() {
            let rows: Vec<(SuggestionItem, ClickHandler)> = offers
                .into_iter()
                .map(|item| {
                    let label = item.label.clone();
                    let weak = self.window.clone();
                    let pick: ClickHandler = Rc::new(
                        move |_ev: &ClickEvent, _window: &mut Window, cx: &mut App| {
                            cx.stop_propagation();
                            agent_bridge::options_take(session);
                            let _ = weak.update(cx, |this, cx| {
                                this.send_turn_to(session, label.clone(), None, cx);
                            });
                        },
                    );
                    (item, pick)
                })
                .collect();
            col = col.child(options_strip(&rows));
        }
        if let Some(label) = running {
            col = col.child(task_strip(label));
        }
        // The pane holding the keyboard draws the real composer; the others
        // show the draft they hold, dimmed and inert, which is the same
        // "disabled while unfocused" language the rest of the split uses. The
        // popups that hang over the input travel with it, and they read the
        // field of the pane that owns the keyboard, not the chat underneath it
        // (#2530, #2610).
        let composer = if focused {
            let mut cell = div().flex().flex_col().flex_none().gap_2();
            let slash_popup = self.editor.read(cx).command_popup();
            let emoji_popup = self.editor.read(cx).emoji_popup();
            if topic_idle && let Some((names, selected)) = slash_popup {
                cell = cell.child(command_popup_view(&names, selected));
            } else if topic_idle && let Some((items, selected)) = emoji_popup {
                cell = cell.child(emoji_popup_view(&items, selected));
            }
            cell.child(self.editor.clone()).into_any_element()
        } else {
            let draft = self.editor.read(cx).content.clone();
            pane_composer_dim(draft, session, false, self.window.clone())
        };
        col.child(composer)
    }

    /// One tap on the approval card (#2755): the pane sends the same command
    /// the terminal and Telegram send, through the window that owns it.
    fn plan_tap(&self, command: &'static str) -> ClickHandler {
        let weak = self.window.clone();
        let session = self.session;
        Rc::new(
            move |_ev: &ClickEvent, _window: &mut Window, cx: &mut App| {
                cx.stop_propagation();
                let _ = weak.update(cx, |this, cx| {
                    this.send_turn_to(session, command.to_string(), None, cx);
                });
            },
        )
    }
}

impl Render for PaneView {
    /// The leaf's whole box: its conversation, the strips and the composer
    /// under it, its name, its topics, its status row and the band a dragged
    /// pane would land on (#2612, #2624, #2683, #2687).
    ///
    /// The walk used to build this frame around the pane's state. It belongs
    /// here because everything in it is the pane's own: which conversation it
    /// shows, whether it holds the keyboard, whether the pointer is dragging a
    /// pane over it. The walk is left with the tree.
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = self.window.clone();
        let session = self.session;
        let wheel = move |ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut App| {
            if !wheel_releases_follow(wheel_dy(&ev.delta)) {
                return;
            }
            let _ = weak.update(cx, |this, cx| {
                this.release_pane_follow(session, cx);
                cx.notify();
            });
        };
        let Some(window) = self.window.upgrade() else {
            return div().into_any_element();
        };
        // The keyboard owner and the status row are the window's answers, so
        // they are read from it rather than guessed here.
        let (focused, footer, landing) = {
            let win = window.read(cx);
            let leaves = win
                .panes
                .as_ref()
                .map(|tree| tree.leaves())
                .unwrap_or_default();
            let count = leaves.len().max(1);
            let index = leaves
                .iter()
                .position(|id| *id == session)
                .map(|i| i + 1)
                .unwrap_or(1);
            (
                win.split_focus() == Some(session),
                SharedString::from(win.pane_footer_text(session, index, count)),
                // Where a dragged pane would land if released here (#2683):
                // only the target draws a band, and only while one is in
                // flight.
                win.pane_drop
                    .filter(|(target, _)| *target == session)
                    .map(|(_, half)| half),
            )
        };
        let body = transcript::view(
            &self.transcript,
            &self.scroll,
            &self.live,
            false,
            self.width,
            HashMap::new(),
            transcript::ViewHooks {
                // The pane's own drag-to-copy (#2623): the gesture was dead in
                // every split because this was `default()`, which leaves
                // `drag: None`.
                drag: Some(self.copy_drag.clone()),
                wheel: Some(Rc::new(wheel)),
                ..Default::default()
            },
        )
        .into_any_element();
        // The topic strip is built against the sessions the window holds, so
        // it is asked for here rather than handed over: a pane that only
        // redraws itself must not lose the strip the window built last frame
        // (#2686, #2687).
        let topics = window.update(cx, |win, cx| win.pane_topics(session, cx));
        let chrome = self.chrome(cx).into_any_element();
        leaf_box(LeafBox {
            session,
            title: SharedString::from(self.title.clone()),
            is_chat: false,
            focused,
            // A pane exists only on the live screen, so its frame is
            // always the interactive one.
            interactive: true,
            topics,
            chrome: Some(chrome),
            footer: Some(footer),
            body,
            landing,
            window: self.window.clone(),
        })
    }
}

/// Everything one leaf's frame draws (#2612, #2683, #2687): the box the pane
/// sits in, the × that closes it, the topic strip under its name, the body
/// that takes the slack, the strips and the composer under that, its status
/// row, and the band a dragged pane would land on.
///
/// The window's own conversation and a split pane wear the same frame, so both
/// arrive here: a pane builds its own box in its render, and `panes_body`
/// builds the chat leaf's, because the chat's state is the window's own.
pub(crate) struct LeafBox {
    pub(crate) session: Uuid,
    pub(crate) title: SharedString,
    /// Whether this leaf is the window's own conversation, which is the leaf
    /// the keyboard falls back to when no pane owns it.
    pub(crate) is_chat: bool,
    pub(crate) focused: bool,
    pub(crate) interactive: bool,
    pub(crate) topics: Option<AnyElement>,
    pub(crate) chrome: Option<AnyElement>,
    pub(crate) footer: Option<SharedString>,
    pub(crate) body: AnyElement,
    /// The half of this leaf a dragged pane would land on, when this leaf is
    /// the one under the pointer (#2683). The caller reads it from the window,
    /// because the window's own conversation is drawn while the window's state
    /// is already borrowed.
    pub(crate) landing: Option<DropHalf>,
    pub(crate) window: WeakEntity<Dashboard>,
}

/// The layout every pane frame starts from (#2805).
///
/// `leaf_box` builds this box in the pane's own draw, and the cached view path
/// lays the pane out from this instead of drawing it, so the two have to
/// agree: the pane fills the slot the walk gives it and may shrink to it. A
/// split child is sized by its slot's `flex_basis`, so the pane's own basis is
/// zero and it takes whatever the slot measured.
pub(crate) fn pane_root_style() -> gpui::StyleRefinement {
    gpui::StyleRefinement::default()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
}

/// Draw one leaf's frame (#2612, #2624, #2683, #2687).
pub(crate) fn leaf_box(leaf: LeafBox) -> AnyElement {
    let LeafBox {
        session,
        title,
        is_chat,
        focused,
        interactive,
        topics,
        chrome,
        footer,
        body,
        landing,
        window,
    } = leaf;
    let close_weak = window.clone();
    let close = move |_ev: &ClickEvent, _window: &mut Window, cx: &mut App| {
        let _ = close_weak.update(cx, |this, cx| this.close_pane(session, cx));
    };
    // Clicking anywhere in a pane takes the keyboard, not only the dim
    // composer (#2610). The pane under the pointer is the pane the next
    // keystroke belongs to, and a click that only highlighted a row left the
    // caret where it was.
    let reach = window.clone();
    let take_keyboard = move |_ev: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
        let next = if is_chat { None } else { Some(session) };
        let _ = reach.update(cx, |this, cx| {
            if this.pane_focus != next {
                this.pane_focus = next;
                // A click that takes the keyboard changes which pane the next
                // launch puts the caret on (#2675).
                this.persist_layout();
                this.refocus_screen();
                cx.notify();
            }
        });
    };
    let hover_weak = window.clone();
    let drop_weak = window.clone();
    let mut node = div()
        .id(SharedString::from(format!("pane-{session}")))
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        // The leaf that owns the keyboard wears the accent all the way round,
        // and the leaves that do not are drawn back so a split reads as one
        // live pane beside disabled ones (#2612). Same border width on both
        // states, so focus never shifts a row.
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(focus_ring_color(focused)))
        .when(!focused, |d| d.opacity(UNFOCUSED_PANE_OPACITY))
        .when(interactive, |d| {
            // Capture, not bubble. A transcript row stops the press on its way
            // up (`cx.stop_propagation()` in the text-row handler), so a
            // bubble-phase listener never ran for a click on text, which is
            // most of a pane: the caret stayed on another leaf and ctrl+|
            // split that one while the window called the pane just clicked
            // focused (#2758).
            d.capture_any_mouse_down(take_keyboard)
                // Every pane is a drop target (#2683). The half the pointer is
                // nearest picks the axis and the side while the drag is still
                // in flight, so the band under it is the layout the release
                // will produce.
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
                    // gpui delivers `on_drag_move` to EVERY pane while a drag
                    // is live, hovered or not (the dispatch has no
                    // `is_hovered` gate, unlike `on_mouse_move`), so each pane
                    // used to answer with its own bounds and the last pane
                    // painted won: the band landed on a pane the pointer was
                    // nowhere near, and its half described that pane's nearest
                    // edge instead of the one under the pointer (#2759).
                    let inside =
                        x >= bounds[0] && x <= bounds[2] && y >= bounds[1] && y <= bounds[3];
                    let _ = hover_weak.update(cx, |this, cx| {
                        if inside {
                            // A pane dropped on itself is not a move, so it
                            // never shows a landing band of its own.
                            let next =
                                (from != session).then(|| (session, drop_half(bounds, x, y)));
                            if this.pane_drop != next {
                                this.pane_drop = next;
                                cx.notify();
                            }
                        } else if this.pane_drop.is_some_and(|(target, _)| target == session) {
                            // Clear only a band this pane owns, so a pane
                            // hearing the move does not wipe the band the pane
                            // under the pointer set.
                            this.pane_drop = None;
                            cx.notify();
                        }
                    });
                })
                .on_drop::<PaneDrag>(move |drag, _window, cx| {
                    let from = drag.session;
                    let _ = drop_weak.update(cx, |this, cx| {
                        // The half the last move recorded here, or the right
                        // half when the pointer arrived and left without a
                        // move of its own.
                        let half = this
                            .pane_drop
                            .filter(|(target, _)| *target == session)
                            .map(|(_, half)| half)
                            .unwrap_or(DropHalf {
                                layout: Layout::Side,
                                after: true,
                            });
                        this.pane_drop = None;
                        let moved = this
                            .panes
                            .as_mut()
                            .is_some_and(|tree| tree.move_leaf(from, session, half));
                        if moved {
                            // A move is the layout the user chose, so the
                            // release is when it gets written (#2683, #2675).
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
            session,
            close,
        ))
        // The pane's topics sit inside its box, under its name (#2687): a
        // strip above the whole wall described the chat while the panes it
        // hung over held something else.
        .when_some(topics, |d, top| {
            d.child(div().flex().flex_none().min_w_0().child(top))
        })
        // The transcript takes the slack, the strips and the composer below it
        // keep their height (#2624). Without the wrapper the conversation's
        // own min height won the shrink and the plan strip was squeezed to
        // nothing in a short pane, while the composer survived because its
        // field has a min size: the plan card vanished exactly where the panes
        // got small.
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(body),
        );
    // The pane's own plan, queue and composer sit between its transcript and
    // its footer, where the window-level ones sit in an unsplit window
    // (#2530). `flex_none` keeps every one of them at its own height: a
    // shrinkable stack lost its top rows first.
    if let Some(chrome) = chrome {
        node = node.child(div().flex().flex_col().flex_none().min_w_0().child(chrome));
    }
    node.child(pane_footer(
        SharedString::from(format!("pane-footer-{session}")),
        footer,
        focused,
    ))
    // Painted last, so the landing band covers the pane's own rows instead of
    // hiding under its transcript (#2683).
    .when_some(landing, |d, half| d.child(drop_indicator(half)))
    .into_any_element()
}
