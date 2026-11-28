//! The session rail as an entity of its own (#2933).
//!
//! The rail used to be built inline in `Dashboard::render`: every row, every
//! topic chip, every badge and the filter menu were minted on the window's
//! frame, and every chip re-measured its label against the text system. A
//! transcript streaming at 5 Hz paid all of that per frame whether or not
//! anything in the rail had moved.
//!
//! It is a view now, inserted through gpui's cached path (`AnyView::cached`,
//! `gpui-0.2.2/src/view.rs:103`) the way a split pane is (#2805). The window
//! hands it a frame of plain data and stops there: an identical frame notifies
//! nothing, so a repaint the rail has no stake in replays its last prepaint
//! instead of rebuilding it. The rail's own clock drives its spinner, so an
//! animation asks for a rail repaint and leaves the transcripts beside it
//! replayed from their own prepaint.
//!
//! Two rules come with the cached path, both learned on the panes:
//!
//! - the view is laid out as a ROOT (`element.layout_as_root(bounds.size)`),
//!   where `flex_1` sizes nothing, so its root fills the box with `size_full`
//!   and the box itself is spelled out in [`rail_root_style`];
//! - a cached view replays until its entity is notified, so everything the
//!   paint reads has to reach it through the frame. Nothing here reaches back
//!   into the window's state while the window may be drawing it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Image, IntoElement, MouseButton, MouseDownEvent,
    Render, SharedString, Stateful, StyleRefinement, WeakEntity, Window, div, prelude::*, rems,
    rgb,
};

use crate::palette;
use crate::rail_activity;
use crate::sessions;
use crate::topic_icons;

/// The rail, drawn from the frame the window resolved for it.
pub(crate) struct RailView {
    /// The window this rail belongs to. A rail listener never acts on its own:
    /// it asks the window, which is where the session state, the split pick and
    /// the menus live.
    dashboard: Link,
    /// The scroll container's handle, shared with the window so the keyboard
    /// walk keeps scrolling the rail it already owns.
    scroll: gpui::ScrollHandle,
    /// The rail's own monotonic origin. One clock for the whole column, so a
    /// list of busy rows blinks as one spinner instead of one clock per row.
    clock: Instant,
    /// Poll ticks since the rail opened, so the spinner advances on the
    /// repaint cadence rather than on the poll's.
    tick: u64,
    /// The motion the last paint drew (#2842). A tick that would draw the same
    /// it already drew asks for nothing, which is how an idle rail sleeps even
    /// while it holds a badge.
    painted: rail_activity::Motion,
    frame: RailFrame,
}

/// Everything the rail paints, resolved by the window (#2933).
///
/// Plain data, and deliberately `PartialEq`: the frame is what decides whether
/// the rail is notified, so two frames that would paint the same pixels have to
/// compare equal. That is why no element, no `Session` and no clock phase
/// crosses this boundary.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RailFrame {
    /// The column's width, in rems, and the window's root size.
    pub(crate) width_rem: f32,
    pub(crate) rem: f32,
    /// The find field, the ledger error and the hit count.
    pub(crate) head: sessions::RailHead,
    pub(crate) rows: Vec<RailRow>,
    /// The channel chip at the end of the column (#2452), when a configured
    /// channel has no live owner.
    pub(crate) footer: Option<RailFooter>,
    /// The filter menu's state (#2719, #2787).
    pub(crate) filter: sessions::RailFilter,
    pub(crate) filter_open: bool,
    /// The crabs listed under the logo (#3178). `None` is `[gui]
    /// instance_rail` off: the rail mounts nothing new, not even the arrow.
    pub(crate) crabs: Option<CrabsFrame>,
}

/// The rail's list of crabs: this machine's own and every paired one.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CrabsFrame {
    /// The logo was clicked and the list is unfolded.
    pub(crate) open: bool,
    /// The crabs, the pick and the last error, as the window resolved them.
    pub(crate) list: crate::instance_strip::StripFrame,
}

/// One card in the rail.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RailRow {
    pub(crate) card: sessions::RailCard,
    /// Whether the pin draws filled (#2803).
    pub(crate) favorite: bool,
    pub(crate) flags: RailRowFlags,
    /// The topic chips this card carries, when it is a parent.
    pub(crate) chips: Option<RailChips>,
}

/// The data half of [`sessions::RowFlags`]: what the card prints, with the two
/// elements it also needs (the pin control and the icon) left to the rail,
/// which is the only place a listener can be minted.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RailRowFlags {
    pub(crate) selected: bool,
    pub(crate) cursored: bool,
    pub(crate) indented: bool,
    pub(crate) closed: bool,
    /// Whether this row has work in flight and whether its plan waits (#2636).
    /// The glyph itself is the rail's, because it moves with the rail's clock.
    pub(crate) mark: rail_activity::RowState,
    pub(crate) preview: Option<SharedString>,
    pub(crate) activity: Option<SharedString>,
    pub(crate) plan: Option<(usize, usize)>,
    pub(crate) flash: bool,
    pub(crate) running: Option<SharedString>,
    pub(crate) cost: f64,
    pub(crate) stamp_at: chrono::DateTime<chrono::Utc>,
    pub(crate) icon: Option<RailIcon>,
}

/// The stored icon of a card, resolved to the shape the paint needs (#2790).
///
/// The window owns the decoded-image cache, so it decides which of the three
/// resolvers applies and the rail only draws the answer.
#[derive(Clone, Debug)]
pub(crate) enum RailIcon {
    /// An embedded mark or a crab icon, by its stored value.
    Mark(String),
    /// A custom image the window already decoded.
    Picture(Arc<Image>),
    /// A legacy glyph, drawn as text.
    Glyph(String),
}

impl PartialEq for RailIcon {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Mark(a), Self::Mark(b)) | (Self::Glyph(a), Self::Glyph(b)) => a == b,
            // One handle per stored value (#2790), so two rows sharing a picture
            // share the pointer. A re-decode after a pick is a different
            // picture and has to repaint.
            (Self::Picture(a), Self::Picture(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// A parent card's topic strip (#2701).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RailChips {
    /// Whether the "+N" pill has been opened.
    pub(crate) expanded: bool,
    /// Every topic under the card, in the order the strip paints them.
    pub(crate) kids: Vec<RailTopic>,
}

/// One topic chip: a child conversation painted as a pill on its parent.
///
/// The paint-side twin of `sessions::RailChip`, which carries the ledger facts
/// the strip is ordered by. This one carries only what the pill draws, and no
/// clock: the spinner comes from the rail's own.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RailTopic {
    pub(crate) id: uuid::Uuid,
    pub(crate) title: SharedString,
    pub(crate) mark: rail_activity::RowState,
    pub(crate) selected: bool,
    pub(crate) plan: Option<(usize, usize)>,
}

/// Whether a chip says work is still going in that topic (#3020, #3106).
///
/// A running turn is a badge and a plan waiting for approval is a ring, so both
/// keep the chip visible when the strip folds. A plan progress bar is neither:
/// it is state, not work in flight, and it has no freshness gate, so counting
/// it here pinned every topic with an abandoned plan file into the folded strip
/// for as long as that file lived. A plan that is actually running already sets
/// the badge, so nothing live is lost by dropping the bar.
pub(crate) fn chip_in_flight(kid: &RailTopic) -> bool {
    kid.mark.badge.is_some() || kid.mark.ring
}

/// Which chips a folded strip paints (#3020).
///
/// A folded strip shows three chips, but position is not the rule: a chip with
/// work in flight is the one the reader needs, so it keeps its place wherever
/// it sits in the ledger order. The other slots go to the leading chips that
/// are not already in, so a busy chip at position four keeps the three idle
/// chips in front of it instead of replacing one, and the painted order stays
/// the ledger's. A busy chip never jumps when a neighbour folds.
pub(crate) fn folded_mask(kids: &[RailTopic]) -> Vec<bool> {
    /// The three chips a folded strip always shows.
    const BUDGET: usize = 3;
    let mut shown = vec![false; kids.len()];
    let mut kept = 0;
    // 1. Every chip with work in flight is in, wherever it sits.
    for (i, kid) in kids.iter().enumerate() {
        if chip_in_flight(kid) {
            shown[i] = true;
            kept += 1;
        }
    }
    // 2. The leading chips that are not in yet fill the rest of the budget,
    // in ledger order, so the strip still reads from the top of the list.
    for slot in shown.iter_mut() {
        if kept >= BUDGET {
            break;
        }
        if !*slot {
            *slot = true;
            kept += 1;
        }
    }
    shown
}

/// The channel chip at the end of the rail (#2452).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RailFooter {
    pub(crate) line: SharedString,
    pub(crate) error: Option<SharedString>,
    /// "Start daemon", or "starting..." while the installer runs.
    pub(crate) action: SharedString,
}

/// The window handle a rail listener needs.
///
/// Cloned into every closure. A listener runs with `&mut Context<RailView>`,
/// which is no place to act on the window's session state, so it asks the
/// window to do it: one upgrade, one call, and a rail that outlives the window
/// simply stops asking.
#[derive(Clone)]
struct Link(WeakEntity<crate::Dashboard>);

impl Link {
    /// Run `f` on the window, if it is still open.
    fn act<R>(
        &self,
        cx: &mut App,
        f: impl FnOnce(&mut crate::Dashboard, &mut Context<crate::Dashboard>) -> R,
    ) {
        if let Some(dashboard) = self.0.upgrade() {
            let _ = dashboard.update(cx, f);
        }
    }
}

/// The size of a crab's square in the rail's list, in rems.
const CRAB_MARK_REM: f32 = 1.25;

impl RailView {
    /// The logo, made the door to the crabs (#3178): a click folds or
    /// unfolds the list, and the arrow beside it says which way it goes.
    fn crabs_toggle(&self, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let pal = palette::current();
        let link = self.dashboard.clone();
        let click = cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
            link.act(cx, |dash, cx| dash.toggle_crab_list(cx));
        });
        header_button(
            "rail-crabs-toggle",
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_0p5()
                .child(crate::icon::banner_logo())
                .child(
                    div()
                        .text_size(rems(0.625))
                        .text_color(rgb(pal.text_muted))
                        .child(SharedString::from(if open {
                            "\u{25be}"
                        } else {
                            "\u{25b8}"
                        })),
                ),
        )
        .on_click(click)
        .into_any_element()
    }

    /// The unfolded list: this machine's crab, every paired one in the
    /// store's order, the last error, and "emparelhar um crab". A click opens
    /// a crab, a right-click its own card (abrir, esquecer, nova morada).
    fn crabs_list(&self, crabs: &CrabsFrame, cx: &mut Context<Self>) -> AnyElement {
        let pal = palette::current();
        let selected = crabs.list.selected.clone();
        let mut col = div()
            .id("rail-crabs")
            .flex()
            .flex_col()
            .w_full()
            .gap_0p5()
            .px_2()
            .pb_2()
            .border_b_1()
            .border_color(rgb(pal.panel_hi));

        let home = {
            let link = self.dashboard.clone();
            cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                link.act(cx, |dash, cx| dash.instance_clear(cx));
            })
        };
        col = col.child(
            crab_row(
                SharedString::from("rail-crab-home"),
                crate::icon::banner_logo().into_any_element(),
                SharedString::from("este crab"),
                selected.is_none(),
            )
            .on_click(home),
        );

        for badge in &crabs.list.badges {
            let is_selected = selected.as_deref() == Some(badge.host_id.as_str());
            let pick = {
                let link = self.dashboard.clone();
                let host_id = badge.host_id.clone();
                cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                    let host_id = host_id.clone();
                    link.act(cx, move |dash, cx| dash.instance_picked(&host_id, cx));
                })
            };
            let manage = {
                let link = self.dashboard.clone();
                let badge = badge.clone();
                cx.listener(move |_rail, _ev: &MouseDownEvent, _window, cx| {
                    let badge = badge.clone();
                    link.act(cx, move |dash, cx| dash.instance_manage(&badge, cx));
                })
            };
            let title = badge.title.clone();
            col = col.child(
                crab_row(
                    SharedString::from(format!("rail-crab-{}", badge.host_id)),
                    crate::instance_strip::badge_mark(badge, is_selected, CRAB_MARK_REM),
                    badge.label.clone(),
                    is_selected,
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(rems(0.5625))
                        .text_color(rgb(crate::instance_strip::health_dot(badge.health)))
                        .child(SharedString::from(crate::instances::health_mark(
                            badge.health,
                        ))),
                )
                .tooltip(move |_window, cx| crate::hint::mic_tooltip(title.clone(), cx))
                .on_click(pick)
                .on_mouse_down(MouseButton::Right, manage),
            );
        }

        if let Some(error) = crabs.list.error.clone() {
            col = col.child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(rgb(pal.warn))
                    .child(error),
            );
        }

        let pair = {
            let link = self.dashboard.clone();
            cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                link.act(cx, |dash, cx| dash.instance_pair(cx));
            })
        };
        col = col.child(
            crab_row(
                SharedString::from("rail-crab-pair"),
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(rems(CRAB_MARK_REM))
                    .h(rems(CRAB_MARK_REM))
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(pal.track))
                    .text_size(rems(0.75))
                    .text_color(rgb(pal.text_muted))
                    .child(SharedString::from("+"))
                    .into_any_element(),
                SharedString::from("emparelhar um crab"),
                false,
            )
            .on_click(pair),
        );
        col.into_any_element()
    }
}

/// One row of the crabs list: its square, its name, and room for a dot.
fn crab_row(
    id: SharedString,
    mark: AnyElement,
    label: SharedString,
    selected: bool,
) -> Stateful<Div> {
    let pal = palette::current();
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |d| d.bg(rgb(pal.panel_hi)))
        .hover(|d| d.bg(rgb(pal.panel_hi)))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .w(rems(CRAB_MARK_REM))
                .h(rems(CRAB_MARK_REM))
                .child(mark),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_sm()
                .text_color(rgb(if selected { pal.accent } else { pal.text }))
                .child(label),
        )
}

/// One icon button in the rail header (#2969).
///
/// The padding is the hit box: an icon alone is a 16px target, and the header
/// row is the one place in the rail where a miss costs a whole repaint of the
/// list underneath it.
pub(crate) fn header_button(id: &'static str, glyph: impl IntoElement) -> Stateful<Div> {
    // The buttons live on the window's drag handle (#3014), so a press on one
    // stays a press on the button instead of starting a window move.
    crate::window_controls::inside_drag_handle(div().id(id))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .p_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .child(glyph)
}

impl RailView {
    pub(crate) fn new(dashboard: WeakEntity<crate::Dashboard>, scroll: gpui::ScrollHandle) -> Self {
        Self {
            dashboard: Link(dashboard),
            scroll,
            clock: Instant::now(),
            tick: 0,
            painted: rail_activity::Motion::STILL,
            frame: RailFrame::default(),
        }
    }

    /// Hand the rail the frame the window resolved for it (#2933).
    ///
    /// A frame that compares equal notifies nothing, and a view that was not
    /// notified replays its last prepaint. That comparison is the whole saving,
    /// which is why it is derived over the frame rather than a stamp the window
    /// has to keep in step with every field the paint reads.
    pub(crate) fn set_frame(&mut self, frame: RailFrame, cx: &mut Context<Self>) {
        if self.frame == frame {
            return;
        }
        self.frame = frame;
        cx.notify();
    }

    /// Advance the rail's clock (#2933).
    ///
    /// Called from the window's poll, which already runs at 20 Hz. Only a frame
    /// that actually moves asks for the next paint, and it asks the rail alone:
    /// the window is not notified, so the transcripts beside the rail are
    /// replayed rather than rebuilt for a glyph.
    ///
    /// The paint is owed when the motion would differ from the one already on
    /// screen, never merely because a tick came round (#2842). A badge that
    /// holds still, which is what a plan task in flight draws, asks for nothing
    /// and the idle window stops repainting altogether. The cadence gate stays
    /// so a comparison can never turn the 20 Hz poll into a 20 Hz paint.
    pub(crate) fn tick(&mut self, cx: &mut Context<Self>) {
        self.tick = self.tick.wrapping_add(1);
        if !crate::busy_repaint_due(self.tick) {
            return;
        }
        let motion = self.frame.motion(self.clock.elapsed());
        if motion == self.painted {
            return;
        }
        self.painted = motion;
        cx.notify();
    }
}

impl RailFrame {
    /// What the rail's own clock draws right now (#2842).
    ///
    /// The spinner glyph and the waiting-plan ring are the rail's animations:
    /// the plan bar's settle flash is not, because the window resolves its lit
    /// half from its own clock and the frame carries the answer.
    ///
    /// Every animating surface reads the same phase, so one glyph and one ring
    /// alpha describe the whole column however many rows carry them. The
    /// decision lives in `rail_activity::motion` so it can be tested without a
    /// window; this only gathers the marks it reads.
    pub(crate) fn motion(&self, phase: Duration) -> rail_activity::Motion {
        rail_activity::motion(
            self.rows.iter().flat_map(|row| {
                std::iter::once(row.flags.mark).chain(
                    row.chips
                        .iter()
                        .flat_map(|chips| chips.kids.iter().map(|kid| kid.mark)),
                )
            }),
            phase,
        )
    }
}

/// The rail's root, the box the cached view lays out as a ROOT.
///
/// `size_full` and not `flex_1`: on the cached path the view is laid out
/// against the box the parent reserved, and a flex share sizes nothing there.
/// The pane learned this the visible way (#2805), with a leaf drawn taller than
/// its box over the strips beside it.
pub(crate) fn rail_root() -> Div {
    div().flex().flex_col().size_full().min_w_0().min_h_0()
}

/// The box the window reserves for the rail (#2933), handed to the cached view
/// path so the reserved box and the drawn one cannot drift (#2805).
///
/// The width is the window's, because the rail is a fixed column beside its
/// splitter rather than a share of the row.
pub(crate) fn rail_root_style(width_rem: f32) -> StyleRefinement {
    let mut boxed = div()
        .flex_none()
        .w(rems(width_rem))
        .h_full()
        .min_w_0()
        .min_h_0();
    boxed.style().clone()
}

impl Render for RailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // One phase for the whole column, read once: every spinner in the rail
        // blinks on the same frame instead of one behind its neighbour.
        let phase = self.clock.elapsed();
        let mut rows: Vec<AnyElement> = Vec::with_capacity(self.frame.rows.len());
        for row in &self.frame.rows {
            rows.push(self.row_element(row, phase, window, cx));
        }
        // The channel chip (#2452): quiet notice plus the one action, drawn at
        // the end of the rail only while a configured channel has no live owner.
        let footer = self
            .frame
            .footer
            .as_ref()
            .map(|footer| self.footer_chip(footer, cx));
        let control = self.filter_control(cx);
        let actions = self.header_buttons(cx);
        // With the flag off both stay `None` and the brand row is main's.
        let crabs = self.frame.crabs.clone();
        let toggle = crabs
            .as_ref()
            .map(|crabs| self.crabs_toggle(crabs.open, cx));
        let list = crabs
            .as_ref()
            .filter(|crabs| crabs.open)
            .map(|crabs| self.crabs_list(crabs, cx));
        rail_root().child(sessions::rail(
            &self.frame.head,
            rows,
            self.frame.width_rem,
            &self.scroll,
            footer,
            sessions::RailBrand {
                actions: Some(actions),
                crabs_toggle: toggle,
                crabs_list: list,
            },
            control,
        ))
    }
}

impl RailView {
    /// One card: its pin, its stored icon, its chips, and the three handlers
    /// that reach the window.
    fn row_element(
        &self,
        row: &RailRow,
        phase: Duration,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.card.id;
        let link = self.dashboard.clone();
        // Right-click asks for the row's menu: pin and delete without the slide
        // gesture or a chord (#2354). The card takes the pointer, not the
        // keyboard, so the composer keeps its caret while it is open (#2798).
        let ask_menu = {
            let link = link.clone();
            cx.listener(move |_rail, ev: &MouseDownEvent, _window, cx| {
                let at = ev.position;
                link.act(cx, move |dash, cx| dash.rail_row_menu(id, at, cx));
            })
        };
        // The visible pin and the context menu share one favorite writer
        // (#2803). Stopping the press keeps the row closed and prevents a slide.
        let toggle_pin = {
            let link = link.clone();
            cx.listener(move |_rail, _ev: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                link.act(cx, move |dash, cx| dash.rail_toggle_pin(id, cx));
            })
        };
        let pin_toggle =
            sessions::pin_toggle(id, row.favorite).on_mouse_down(MouseButton::Left, toggle_pin);
        let flags = sessions::RowFlags {
            selected: row.flags.selected,
            cursored: row.flags.cursored,
            pin_toggle: Some(pin_toggle),
            indented: row.flags.indented,
            closed: row.flags.closed,
            // One glyph cell, from the rail's own clock, so a list of busy rows
            // blinks as one spinner instead of ten (#2636). Only a live turn
            // spins; a plan badge holds still (#2842).
            badge: row
                .flags
                .mark
                .badge
                .map(|badge| rail_activity::glyph(badge, phase)),
            preview: row.flags.preview.clone(),
            activity: row.flags.activity.clone(),
            plan: row.flags.plan,
            flash: row.flags.flash,
            running: row.flags.running.clone(),
            cost: row.flags.cost,
            stamp_at: row.flags.stamp_at,
            // The stored icon leads the row (#2790), resolved by the window
            // because it owns the decoded-image cache. The paint never reads a
            // file.
            icon: row.flags.icon.as_ref().and_then(|icon| match icon {
                RailIcon::Mark(stored) => topic_icons::tab_icon(stored, sessions::RAIL_MARK_REMS),
                RailIcon::Picture(image) => Some(topic_icons::cached_picture(
                    image.clone(),
                    sessions::RAIL_MARK_REMS,
                )),
                RailIcon::Glyph(stored) => {
                    Some(topic_icons::tab_glyph(stored, sessions::RAIL_MARK_REMS))
                }
            }),
        };
        let chips = row
            .chips
            .as_ref()
            .map(|chips| self.chip_strip(row, chips, phase, window, cx));
        // No menu is passed in: the card is built once at the window root,
        // because a child of the row would inherit the row's frame and the card
        // is placed in window coordinates (#2742).
        let r = sessions::row(&row.card, flags, chips);
        // The press starts the slide that opens the card's menu.
        let press = {
            let link = link.clone();
            cx.listener(move |_rail, _ev: &MouseDownEvent, window, cx| {
                let from = f32::from(window.mouse_position().x);
                link.act(cx, move |dash, cx| dash.rail_row_press(id, from, cx));
            })
        };
        let pick = {
            let link = link.clone();
            cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                link.act(cx, move |dash, cx| dash.rail_row_pick(id, cx));
            })
        };
        let r = r.on_mouse_down(MouseButton::Right, ask_menu);
        let r = r.on_mouse_down(MouseButton::Left, press);
        // A plan waiting for approval rings its row until it is opened. The
        // border lives on this wrapper, never inside `sessions::row`: the row
        // stays the square, edge-to-edge tile #2540 pins, and the ring is a
        // frame around it rather than a card. Wrapped after the click is
        // attached, so the row keeps the handlers it was built with and only
        // gains a frame.
        rail_activity::ring(sessions::on_click(r, pick), row.flags.mark.ring, phase)
            .into_any_element()
    }

    /// A parent card's topic chips (#2701): three by default, a "+N" pill
    /// expands the rest, a collapse pill folds them back. Each chip opens its
    /// child and offers the shared context menu on right-click, with its own
    /// live glyph.
    fn chip_strip(
        &self,
        row: &RailRow,
        chips: &RailChips,
        phase: Duration,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = row.card.id;
        // Which chips the strip paints (#3020). Expanded shows every topic; a
        // folded strip keeps every chip with work in flight and fills the rest
        // of the three-chip budget with idle ones, in ledger order.
        let shown = if chips.expanded {
            vec![true; chips.kids.len()]
        } else {
            folded_mask(&chips.kids)
        };
        // Each chip is measured as it is built: the strip places the chips
        // itself so the first row can stop short of the pin while the rows
        // below keep the full card width (#2809).
        let mut els: Vec<(Stateful<Div>, f32)> = Vec::new();
        for (kid, shown) in chips.kids.iter().zip(shown.iter()) {
            if !*shown {
                continue;
            }
            let kid_id = kid.id;
            let title = &kid.title;
            let live = kid
                .mark
                .badge
                .map(|badge| rail_activity::glyph(badge, phase));
            let link = self.dashboard.clone();
            let open = {
                let link = link.clone();
                cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    link.act(cx, move |dash, cx| dash.rail_chip_pick(kid_id, cx));
                })
            };
            let ask = {
                let link = link.clone();
                cx.listener(move |_rail, ev: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    let at = ev.position;
                    link.act(cx, move |dash, cx| dash.rail_chip_menu(kid_id, at, cx));
                })
            };
            let chip =
                sessions::topic_chip(kid_id, title, live, kid.mark.ring, kid.selected, kid.plan)
                    .on_click(open)
                    .on_mouse_down(MouseButton::Right, ask);
            let width =
                sessions::chip_width(window, title, live.is_some() || kid.mark.ring, kid.selected);
            els.push((chip, width));
        }
        let hidden = shown.iter().filter(|shown| !**shown).count();
        if hidden > 0 {
            let label = SharedString::from(format!("+{hidden}"));
            let width = sessions::count_chip_width(window, &label);
            let expand = {
                let link = self.dashboard.clone();
                cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    link.act(cx, move |dash, cx| dash.rail_expand_chips(id, cx));
                })
            };
            els.push((sessions::count_chip(id, label).on_click(expand), width));
        } else if chips.expanded && chips.kids.len() > 3 {
            let label = SharedString::from("\u{2212}");
            let width = sessions::count_chip_width(window, &label);
            let collapse = {
                let link = self.dashboard.clone();
                cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    link.act(cx, move |dash, cx| dash.rail_collapse_chips(id, cx));
                })
            };
            els.push((sessions::count_chip(id, label).on_click(collapse), width));
        }
        // The pin floats at the strip's right edge, so the first row hands it
        // the space and every row below runs edge to edge (#2809). The strip is
        // packed here, where the widths and the card's own width are both known.
        let widths: Vec<f32> = els.iter().map(|(_, width)| *width).collect();
        let rem = self.frame.rem;
        let (first, rest) = sessions::strip_budgets(rem, self.frame.width_rem * rem);
        let counts = sessions::pack_rows(&widths, rem * 0.25, first, rest);
        let mut chips = els.into_iter().map(|(chip, _)| chip);
        let rows: Vec<Vec<Stateful<Div>>> = counts
            .into_iter()
            .map(|count| chips.by_ref().take(count).collect())
            .collect();
        sessions::topic_chips(id, rows)
    }

    /// The channel chip: the notice, the installer's error and the one action.
    fn footer_chip(&self, footer: &RailFooter, cx: &mut Context<Self>) -> AnyElement {
        let link = self.dashboard.clone();
        let start = cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
            link.act(cx, move |dash, cx| dash.rail_start_daemon(cx));
        });
        let shown = palette::current();
        let mut chip = div()
            .id("channel-chip")
            .flex()
            .flex_col()
            .gap_1()
            .mt_auto()
            .w_full()
            .min_w_0()
            .flex_none()
            .p_1p5()
            .rounded_md()
            .border_1()
            .border_color(rgb(shown.track))
            .bg(rgb(shown.panel_hi))
            .text_xs()
            .text_color(rgb(shown.text_muted))
            .child(footer.line.clone());
        if let Some(err) = &footer.error {
            chip = chip.child(div().text_color(rgb(shown.red)).child(err.clone()));
        }
        chip.child(
            div()
                .id("channel-chip-start")
                .text_color(rgb(shown.accent))
                .cursor_pointer()
                .hover(|s| s.text_color(rgb(shown.text)))
                .child(footer.action.clone())
                .on_click(start),
        )
        .into_any_element()
    }

    /// The buttons the rail header carries (#2969): the sidebar toggle and the
    /// menu, side by side at the end of the brand row. One line of chrome, so
    /// the rail's first row starts where the brand ends instead of under a
    /// band of empty width.
    ///
    /// The menu button wears the accent while a non-default filter hides rows,
    /// so a closed panel never silently filters.
    fn header_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let rail_filter = self.frame.filter;
        let pal = palette::current();
        let link = self.dashboard.clone();
        let toggle = {
            let link = link.clone();
            cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                link.act(cx, move |dash, cx| dash.rail_toggle_filter_menu(cx));
            })
        };
        let hide_rail = {
            let link = self.dashboard.clone();
            cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                link.act(cx, move |dash, cx| dash.toggle_rail(cx));
            })
        };
        div()
            .id("rail-header-buttons")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_0p5()
            .child(
                header_button(
                    "rail-header-sidebar",
                    sessions::sidebar_icon(1.0, pal.text_muted),
                )
                .on_click(hide_rail),
            )
            .child(
                header_button(
                    "rail-filter-toggle",
                    sessions::menu_icon(
                        1.0,
                        if sessions::filter_is_active(&rail_filter) {
                            pal.accent
                        } else {
                            pal.text_muted
                        },
                    ),
                )
                .on_click(toggle),
            )
            .into_any_element()
    }

    /// The rail filter menu (#2719, #2787): the chip row the header's menu
    /// button opens, the plan-state chips plus the cron/RSI toggle. It sits
    /// directly under the brand row and paints only while the panel is open.
    fn filter_control(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.frame.filter_open {
            return None;
        }
        let rail_filter = self.frame.filter;
        let link = self.dashboard.clone();
        let plan_chips: [(&'static str, &'static str, sessions::PlanFilter); 4] = [
            ("rail-filter-all", "all", sessions::PlanFilter::All),
            ("rail-filter-active", "active", sessions::PlanFilter::Active),
            ("rail-filter-done", "done", sessions::PlanFilter::Done),
            (
                "rail-filter-noplan",
                "no plan",
                sessions::PlanFilter::NoPlan,
            ),
        ];
        let mut chips = div().flex().flex_row().items_center().gap_0p5();
        for (id, label, plan) in plan_chips {
            let pick = {
                let link = link.clone();
                cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                    link.act(cx, move |dash, cx| dash.rail_set_plan_filter(plan, cx));
                })
            };
            chips = chips
                .child(sessions::filter_chip(id, label, rail_filter.plan == plan).on_click(pick));
        }
        let auto = {
            let link = link.clone();
            cx.listener(move |_rail, _ev: &ClickEvent, _window, cx| {
                link.act(cx, move |dash, cx| dash.rail_toggle_automation(cx));
            })
        };
        chips = chips.child(
            sessions::filter_chip("rail-filter-auto", "auto", rail_filter.show_automation)
                .on_click(auto),
        );
        Some(
            div()
                .id("rail-filter-menu")
                .flex()
                .flex_col()
                .items_end()
                .gap_0p5()
                .child(chips)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rail_activity::Badge;

    /// One chip: `badge`, `ring` and `plan` are the three in-flight signals.
    fn kid(id: u128, badge: bool, ring: bool, plan: bool) -> RailTopic {
        RailTopic {
            id: uuid::Uuid::from_u128(id),
            title: SharedString::from(format!("topic-{id}")),
            mark: rail_activity::RowState {
                badge: badge.then_some(Badge::Live),
                ring,
            },
            selected: false,
            plan: plan.then_some((1, 4)),
        }
    }

    fn shown(mask: &[bool]) -> Vec<usize> {
        mask.iter()
            .enumerate()
            .filter(|(_, shown)| **shown)
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn a_folded_strip_shows_the_three_leading_chips() {
        // #3020: the plain case is still three, so a card with nothing running
        // reads exactly as it did before the fold rule existed.
        let kids: Vec<RailTopic> = (0..5).map(|i| kid(i, false, false, false)).collect();
        assert_eq!(shown(&folded_mask(&kids)), vec![0, 1, 2]);
    }

    #[test]
    fn a_folded_strip_keeps_a_busy_chip_past_the_budget() {
        // #3020: the chip the reader needs is the one with work in flight, so
        // it keeps its place even when three idle chips sit in front of it.
        // The budget is three chips in total, so the busy fourth takes a slot
        // from the idle third instead of widening the strip (#3020).
        let kids = vec![
            kid(0, false, false, false),
            kid(1, false, false, false),
            kid(2, false, false, false),
            kid(3, true, false, false),
            kid(4, false, false, false),
        ];
        assert_eq!(shown(&folded_mask(&kids)), vec![0, 1, 3]);
    }

    #[test]
    fn a_folded_strip_keeps_every_live_chip_and_folds_idle_ones() {
        // #3020: topics with work in flight all stay visible, and the idle
        // chips on either side are what the "+N" pill hides. A plan bar alone
        // is not work in flight (#3106), so it folds with the idle ones.
        let kids = vec![
            kid(0, false, false, false),
            kid(1, true, false, false),
            kid(2, false, true, false),
            kid(3, false, false, true),
            kid(4, true, false, false),
            kid(5, false, false, false),
        ];
        assert_eq!(shown(&folded_mask(&kids)), vec![1, 2, 4]);
    }

    #[test]
    fn every_live_signal_keeps_a_chip_and_a_plan_bar_does_not() {
        // #3020: a spinner and a plan waiting for approval are the two reasons
        // the reader may need the chip. #3106: a progress bar is state, not
        // work in flight, so on its own it never keeps a chip.
        assert!(chip_in_flight(&kid(0, true, false, false)), "a live turn");
        assert!(
            chip_in_flight(&kid(1, false, true, false)),
            "a waiting plan"
        );
        assert!(
            !chip_in_flight(&kid(2, false, false, true)),
            "a plan bar alone is not work in flight"
        );
        assert!(
            !chip_in_flight(&kid(3, false, false, false)),
            "an idle topic folds with the rest"
        );
    }

    #[test]
    fn the_mask_answers_for_every_chip_it_is_given() {
        // #3020: `chip_strip` zips the mask with the kids, so a mask shorter
        // than the list would silently drop chips from a folded strip and one
        // longer would be ignored. The length is the contract.
        let kids: Vec<RailTopic> = (0..6).map(|i| kid(i, false, false, false)).collect();
        let folded = folded_mask(&kids);
        assert_eq!(folded.len(), kids.len());
        assert_eq!(shown(&folded), vec![0, 1, 2]);
    }
}
