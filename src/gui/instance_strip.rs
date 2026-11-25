//! The instance strip: 56 px of column on the far left that says *which crab*
//! this window is looking at.
//!
//! Three nested scopes, one per column, and this is the outermost: the strip
//! picks the instance, the rail lists that instance's sessions, the panes show
//! the selected session. Only the first is new; the other two are the window's
//! own and are reused untouched, because a pick swaps the backend they read
//! from rather than drawing a second rail.
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

use gpui::{
    AnyElement, ClickEvent, Context, Div, IntoElement, Render, SharedString, StyleRefinement,
    WeakEntity, Window, div, prelude::*, rems, rgb,
};

use crate::instances::{Health, InstanceEntry, Instances, badge_title, health_mark};
use crate::palette;

/// The strip's width in rems. 56 px at the window's 16 px root, which is the
/// mockup's number and the same axis the badges are centred on.
pub(crate) const STRIP_REM: f32 = 3.5;

/// A badge's diameter in rems (32 px).
const BADGE_REM: f32 = 2.0;

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
    /// The last thing about the crabs that went wrong, drawn as one warning
    /// mark under the badges with the text on hover. `None` is the normal
    /// state.
    pub(crate) error: Option<SharedString>,
}

impl StripFrame {
    /// Build from the window's model. `now` is the clock the hover title
    /// prints "offline há 5 min" against.
    pub(crate) fn from_model(model: &Instances, now: u64, error: Option<SharedString>) -> Self {
        Self {
            badges: model
                .entries()
                .iter()
                .map(|entry| StripBadge::from_entry(entry, now))
                .collect(),
            selected: model.selected_host_id().map(str::to_string),
            error,
        }
    }
}

/// The strip as an entity, so a cached frame can replay it.
pub(crate) struct InstanceStrip {
    dashboard: WeakEntity<crate::Dashboard>,
    frame: StripFrame,
}

impl InstanceStrip {
    pub(crate) fn new(dashboard: WeakEntity<crate::Dashboard>) -> Self {
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
        self.dashboard
            .update(cx, |dash, cx| dash.instance_picked(host_id.as_ref(), cx))
            .ok();
    }

    fn home(&self, cx: &mut Context<Self>) {
        self.dashboard
            .update(cx, |dash, cx| dash.instance_clear(cx))
            .ok();
    }

    /// The window's mark at the top of the column: this machine's own crab.
    /// A click on it goes home, the same as a second click on the badge that
    /// is open, so the way back is never only the picker that led away.
    fn workspace_mark(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette::current();
        let home = self.frame.selected.is_none();
        let click = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.home(cx));
        div()
            .id("instance-strip-home")
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .w(rems(BADGE_REM))
            .h(rems(1.75))
            .rounded_full()
            .cursor_pointer()
            .bg(rgb(p.panel_hi))
            .border_1()
            .border_color(rgb(if home { p.accent } else { p.track }))
            .on_click(click)
            .child(
                div()
                    .text_size(rems(0.6875))
                    .text_color(rgb(if home { p.accent } else { p.text_title }))
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
        let title = badge.title.clone();
        let click = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.pick(host_id.clone(), cx);
        });

        let mut cell = div()
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
            .tooltip(move |_window, cx| crate::hint::mic_tooltip(title.clone(), cx))
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

    /// The warning mark: one faint `!` whose hover says what failed. Small on
    /// purpose, because the window still works on whichever crab it holds.
    fn error_mark(&self, error: SharedString) -> AnyElement {
        let p = palette::current();
        let title = error.clone();
        div()
            .id("instance-strip-error")
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .w_full()
            .h(rems(1.5))
            .tooltip(move |_window, cx| crate::hint::mic_tooltip(title.clone(), cx))
            .child(
                div()
                    .text_size(rems(0.75))
                    .text_color(rgb(p.warn))
                    .child(SharedString::from("!")),
            )
            .into_any_element()
    }

    /// The column. Deliberately a `flex_col` of fixed-size cells with no
    /// horizontal growth: the strip is the one column that never resizes.
    fn column(&self, cx: &mut Context<Self>) -> Div {
        let p = palette::current();
        let selected = self.frame.selected.clone();
        let mut children: Vec<AnyElement> = Vec::with_capacity(self.frame.badges.len() * 2 + 3);
        children.push(self.workspace_mark(cx));
        children.push(div().h(rems(0.625)).into_any_element());
        for badge in &self.frame.badges {
            let is_selected = selected.as_deref() == Some(badge.host_id.as_str());
            children.push(self.badge(badge, is_selected, cx));
            children.push(div().h(rems(0.5)).into_any_element());
        }
        if let Some(error) = self.frame.error.clone() {
            children.push(self.error_mark(error));
        }

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
