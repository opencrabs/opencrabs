//! One split pane as an entity of its own (#2805).
//!
//! Every pane's state used to sit in a `HashMap<Uuid, SplitPane>` on the
//! window, so the window was the only thing that could repaint: a row landing
//! in one leaf, a plan task settling in another, or a scroll tick in a third
//! all went through `Dashboard::notify` and redrew the whole wall. Lifting the
//! state into a `PaneView` entity gives each leaf a repaint of its own.
//!
//! The state itself does not change. `title`, `transcript`, `scroll`, `follow`,
//! `editor`, `plan`, `plan_for`, `live`, `busy`, `cancel` and `copy_drag` were
//! per-leaf before this module existed and stay per-leaf after it (#2438,
//! #2530, #2614, #2623, #2685).
//!
//! A leaf cannot see the window's state from its own `Context`, so the two
//! things it needs from the window come in as fields: a weak handle for the
//! wheel hook, and the width the walk measured for it this frame.

use gpui::{
    App, Context, Entity, IntoElement, Render, ScrollHandle, ScrollWheelEvent, WeakEntity, Window,
    div, prelude::*,
};
use opencrabs::ui_core::scroll::{Follow, scrolled_from_top, wheel_releases_follow};
use std::collections::HashMap;
use std::rc::Rc;
use uuid::Uuid;

use crate::Dashboard;
use crate::editor;
use crate::plan_strip;
use crate::transcript::{self, CopyDrag, TranscriptState};
use crate::wheel_dy;

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
    /// This pane's own drag-to-copy state (#2623). A pane transcript is built
    /// with `ViewHooks::default()`, which leaves `drag: None`, so every pane
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
        }
    }

    /// Record the box this leaf was drawn in this frame. The walk measures the
    /// leaf from the tree's shares, so a narrow column wraps its transcript to
    /// its own width instead of the window's (#2624).
    pub(crate) fn set_width(&mut self, width: f32) {
        self.width = width;
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

impl Render for PaneView {
    /// The leaf's conversation, drawn the way the walk drew it before the
    /// state moved here: the transcript fills the slack, carries this pane's
    /// own drag-to-copy state, and reports its wheel ticks to the window so a
    /// scroll in one leaf never releases another's follow (#2623, #2698).
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
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
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(transcript::view(
                &self.transcript,
                &self.scroll,
                &self.live,
                false,
                self.width,
                HashMap::new(),
                transcript::ViewHooks {
                    // The pane's own drag-to-copy (#2623): the gesture was
                    // dead in every split because this was `default()`, which
                    // leaves `drag: None`.
                    drag: Some(self.copy_drag.clone()),
                    wheel: Some(Rc::new(wheel)),
                    ..Default::default()
                },
            ))
    }
}
