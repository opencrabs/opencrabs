//! The instance strip: 56 px of column on the far left that says *which crab*
//! this window is looking at.
//!
//! Three nested scopes, one per column, and this is the outermost: the strip
//! picks the instance, the rail lists that instance's sessions, the panel shows
//! the selected session. Only the first is new; the other two already existed
//! and are reused untouched, which is the whole reason the strip is a strip and
//! not a redesign.
//!
//! Like the rail (#2933) this is a cached view: the window resolves a
//! [`StripFrame`] of plain data and hands it over, and a frame that compares
//! equal replays the last prepaint instead of rebuilding badges for a keystroke
//! typed in the composer.
//!
//! Two rules the paint follows, both from the mockup that was approved:
//!
//! 1. **The accent belongs to the selection alone.** Ring, number and the edge
//!    bar are the only orange in the column; a crab that is merely online gets a
//!    green dot, not a glow.
//! 2. **Health never moves a crab.** Order is the store's, so the badge under
//!    the pointer cannot slide away between the press and the release.

use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, Div, IntoElement, Render, SharedString, StyleRefinement,
    WeakEntity, Window, div, prelude::*, rems, rgb,
};

use crate::instances::{Health, InstanceEntry, badge_title, health_mark};
use crate::palette;

/// The strip's width in rems. 56 px at the window's 16 px root, which is the
/// mockup's number and the same axis the badges are centred on.
pub(crate) const STRIP_REM: f32 = 3.5;

/// A badge's diameter in rems (32 px), and the smaller chip below it (24 px).
const BADGE_REM: f32 = 2.0;
const CHIP_REM: f32 = 1.5;

/// One crab, resolved to what the paint reads.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StripBadge {
    pub(crate) host_id: String,
    pub(crate) label: SharedString,
    /// The full label, URL and probe state, for the hover title.
    pub(crate) title: SharedString,
    pub(crate) health: Health,
}

impl StripBadge {
    fn from_entry(entry: &InstanceEntry, now: u64) -> Self {
        Self {
            host_id: entry.host_id().to_string(),
            label: SharedString::from(entry.strip_label()),
            title: SharedString::from(badge_title(entry, now)),
            health: entry.health,
        }
    }
}

/// Everything the strip paints, resolved by the window.
///
/// `PartialEq` is load-bearing: an equal frame notifies nothing, so the probe
/// cadence and the selection are the only things that can wake this view.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StripFrame {
    pub(crate) badges: Vec<StripBadge>,
    pub(crate) selected: Option<String>,
    /// The pairing modal is open, so the add chip draws pressed.
    pub(crate) pairing: bool,
}

impl StripFrame {
    /// Build from the window's model. `now` is the same clock the panel prints,
    /// so "offline há 5 min" cannot disagree between the two.
    pub(crate) fn from_model(model: &crate::instances::Instances, now: u64, pairing: bool) -> Self {
        Self {
            badges: model
                .entries()
                .iter()
                .map(|entry| StripBadge::from_entry(entry, now))
                .collect(),
            selected: model.selected_host_id().map(str::to_string),
            pairing,
        }
    }
}

/// The strip as an entity, so a cached frame can replay it.
pub(crate) struct InstanceStrip {
    dashboard: WeakEntity<crate::Dashboard>,
    frame: StripFrame,
}

impl InstanceStrip {
    pub(crate) fn new(dashboard: WeakEntity<crate::Dashboard>, cx: &mut Context<Self>) -> Self {
        Self {
            dashboard,
            frame: StripFrame::default(),
        }
    }

    /// Hand the view a frame. An equal frame returns without notifying, which
    /// is what keeps a typing composer from repainting the column.
    pub(crate) fn set_frame(&mut self, frame: StripFrame, cx: &mut Context<Self>) {
        if self.frame == frame {
            return;
        }
        self.frame = frame;
        cx.notify();
    }

    fn pick(&self, host_id: SharedString, cx: &mut Context<Self>) {
        let dashboard = self.dashboard.clone();
        dashboard
            .update(cx, |dash, cx| dash.instance_picked(host_id.as_ref(), cx))
            .ok();
    }

    fn add(&self, cx: &mut Context<Self>) {
        let dashboard = self.dashboard.clone();
        dashboard
            .update(cx, |dash, cx| dash.instance_add_clicked(cx))
            .ok();
    }

    /// The window's mark at the top of the column. It is not an instance and
    /// never selects anything; it is there so the column has a head and the
    /// badges below it read as a list rather than a floating stack.
    fn workspace_mark(&self) -> AnyElement {
        let p = palette::current();
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .w(rems(BADGE_REM))
            .h(rems(1.75))
            .rounded_full()
            .bg(rgb(p.panel_hi))
            .border_1()
            .border_color(rgb(p.track))
            .child(
                div()
                    .text_size(rems(0.6875))
                    .text_color(rgb(p.text_title))
                    .child(SharedString::from("C")),
            )
            .into_any_element()
    }

    fn badge(&self, badge: &StripBadge, selected: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette::current();
        let dot = match badge.health {
            Health::Up { .. } => p.green,
            Health::Down { .. } => p.red,
            Health::Unknown => p.text_faint,
        };
        let text = if selected { p.accent } else { p.text_muted };
        let host_id = SharedString::from(badge.host_id.clone());
        let click = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.pick(host_id.clone(), cx);
        });

        let mut cell = div()
            // A click needs an element id in this gpui: `on_click` lives on
            // `StatefulInteractiveElement`, which is only reachable through
            // `.id()`. The badge's own host id keeps it stable across frames.
            .id(SharedString::from(format!(
                "instance-badge-{}",
                badge.host_id
            )))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .w_full()
            .h(rems(BADGE_REM))
            .cursor_pointer()
            .on_click(click)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .w(rems(BADGE_REM))
                    .h(rems(BADGE_REM))
                    .rounded_full()
                    .bg(rgb(if selected { p.panel_hi } else { p.panel }))
                    .when(selected, |d| d.border_1().border_color(rgb(p.accent)))
                    .child(
                        div()
                            .text_size(rems(0.6875))
                            .when(selected, |d| d.font_weight(gpui::FontWeight::BOLD))
                            .text_color(rgb(text))
                            .child(badge.label.clone()),
                    ),
            )
            // The dot is the only thing health changes, and it sits on the
            // badge's shoulder so it never widens the cell.
            .child(
                div()
                    .absolute()
                    .bottom(rems(0.0625))
                    .right(rems(0.5))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .w(rems(0.5))
                    .h(rems(0.5))
                    .rounded_full()
                    .bg(rgb(dot))
                    .child(
                        div()
                            .text_size(rems(0.4375))
                            .text_color(rgb(p.bg))
                            .child(SharedString::from(health_mark(badge.health))),
                    ),
            );

        // The selection bar is flush with the column's left edge, which is what
        // makes "you are here" readable at a glance without a glow.
        if selected {
            cell = cell.child(
                div()
                    .absolute()
                    .left_0()
                    .top(rems(0.5))
                    .w(rems(0.125))
                    .h(rems(1.0))
                    .rounded_full()
                    .bg(rgb(p.accent)),
            );
        }

        cell.into_any_element()
    }

    fn add_chip(&self, pressed: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette::current();
        let click = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.add(cx));
        div()
            .id("instance-strip-add")
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .w_full()
            .h(rems(CHIP_REM))
            .cursor_pointer()
            .on_click(click)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .w(rems(CHIP_REM))
                    .h(rems(CHIP_REM))
                    .rounded_md()
                    .bg(rgb(if pressed { p.panel_hi } else { p.bg }))
                    .border_1()
                    .border_color(rgb(p.track))
                    .child(
                        div()
                            .text_size(rems(0.75))
                            .text_color(rgb(if pressed { p.text_muted } else { p.text_faint }))
                            .child(SharedString::from("+")),
                    ),
            )
            .into_any_element()
    }

    /// The column. Deliberately a `flex_col` of fixed-size cells with no
    /// horizontal growth: the strip is the one column that never resizes.
    fn column(&self, cx: &mut Context<Self>) -> Div {
        let p = palette::current();
        let selected = self.frame.selected.clone();
        let mut children: Vec<AnyElement> = Vec::with_capacity(self.frame.badges.len() + 3);
        children.push(self.workspace_mark());
        children.push(div().h(rems(0.625)).into_any_element());
        for badge in &self.frame.badges {
            let is_selected = selected.as_deref() == Some(badge.host_id.as_str());
            children.push(self.badge(badge, is_selected, cx));
            children.push(div().h(rems(0.5)).into_any_element());
        }
        children.push(self.add_chip(self.frame.pairing, cx));

        div()
            .flex()
            .flex_col()
            .items_center()
            .w_full()
            .h_full()
            .pt(rems(0.625))
            .bg(rgb(p.bg))
            .border_r_1()
            .border_color(rgb(p.track))
            .children(children)
    }
}

impl Render for InstanceStrip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.column(cx)
    }
}

/// The box the cached view is laid out in. A cached view is laid out as a ROOT,
/// where `flex_1` sizes nothing, so the width is spelled out here and nowhere
/// else: the reserved box and the drawn one cannot drift (#2805).
pub(crate) fn strip_root_style() -> StyleRefinement {
    let mut boxed = div()
        .flex_none()
        .w(rems(STRIP_REM))
        .h_full()
        .min_w_0()
        .min_h_0();
    boxed.style().clone()
}

/// The width the window reserves for the strip, in rems.
pub(crate) fn strip_width_rem() -> f32 {
    STRIP_REM
}

/// How often a health probe may run. The window polls at 20 Hz, so the strip
/// asks for a probe every `PROBE_EVERY_TICKS` ticks rather than every frame.
pub(crate) const PROBE_EVERY_TICKS: u64 = 100;

/// Is it time to probe? Split out so the cadence is testable without a window.
pub(crate) fn probe_due(tick: u64) -> bool {
    tick != 0 && tick % PROBE_EVERY_TICKS == 0
}

/// The probe's timeout, kept beside the cadence it has to fit inside.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
