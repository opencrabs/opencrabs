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

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, Div, IntoElement, Keystroke, MouseButton,
    MouseDownEvent, Render, SharedString, StyleRefinement, WeakEntity, Window, anchored, deferred,
    div, point, prelude::*, px, rems, rgb,
};
use opencrabs::config::{A2aConfig, Config, gui_theme};
use opencrabs::remote::instance_client;
use opencrabs::remote::instance_repoint;
use opencrabs::remote::instance_store::{self, InstanceFile, InstanceStore, KnownInstance, now};
use opencrabs::remote::pairing::{CODE_TTL_SECS, PairingStore};
use opencrabs::remote::scopes::BASE_SCOPES;
use opencrabs::remote::tunnel;
use opencrabs::ui_core::text_field::{self, TextField};

use crate::instances::{Health, InstanceEntry, Instances, badge_tint, badge_title, health_mark};
use crate::{modal_shell, palette};

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
    /// What the badge itself draws: one initial, an emoji, or a picture name.
    pub(crate) glyph: SharedString,
    /// The full label, URL and probe state, for the hover title.
    pub(crate) title: SharedString,
    pub(crate) health: Health,
}

impl StripBadge {
    fn from_entry(entry: &InstanceEntry, now: u64) -> Self {
        Self {
            host_id: entry.host_id().to_string(),
            label: SharedString::from(entry.strip_label()),
            // One initial or a picture, not a cut label: the badge is 32 px
            // and the full name lives in `title`, where the hover shows it
            // whole. `label` stays for the cards, which name the crab.
            glyph: SharedString::from(entry.glyph()),
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
    /// The card the `+` opened, if any. It lives on the strip and not on the
    /// window because it belongs to the column that opened it, and because it
    /// never enters a [`StripFrame`]: a keystroke typed into it repaints this
    /// view alone, never the window around it.
    card: Option<StripCard>,
    /// What the last pairing or minting said went wrong, printed in the card.
    card_error: Option<SharedString>,
    /// Moves on every open and close, so an answer for a card the owner has
    /// already closed does not reopen it or overwrite the one that replaced it.
    card_epoch: u64,
    /// Whether a card is up, shared with the window. The window's overlay
    /// stack reads it without a `cx`, so a card counts as a modal there and
    /// focus leaves the hidden composer: otherwise a character typed into the
    /// card reaches the editor nobody can see (#3133).
    card_up: Rc<Cell<bool>>,
}

impl InstanceStrip {
    pub(crate) fn new(dashboard: WeakEntity<crate::Dashboard>, card_up: Rc<Cell<bool>>) -> Self {
        Self {
            dashboard,
            frame: StripFrame::default(),
            card: None,
            card_error: None,
            card_epoch: 0,
            card_up,
        }
    }

    /// The one writer of `card`, so the cell the window reads never disagrees
    /// with it. The window repaints on the next turn of the loop, which is
    /// when it moves the keyboard to or from the card.
    fn set_card(&mut self, card: Option<StripCard>, cx: &mut Context<Self>) {
        let up = card.is_some();
        self.card = card;
        if self.card_up.replace(up) != up {
            let dashboard = self.dashboard.clone();
            cx.defer(move |cx| {
                dashboard.update(cx, |_, cx| cx.notify()).ok();
            });
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
            // A rounded square, like the rail's session marks and the topic
            // tabs: the strip is part of the same chrome, and a circle in a
            // column of squares read as a different widget (#3133).
            .rounded_md()
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
        // Each crab wears its own hue, derived from its host id, so the column
        // is recognisable before any label is read. The wash and the ring are
        // the same hue at two strengths; the glyph mixes with the palette's own
        // text colour, which keeps it legible in the light theme too.
        let hue = tint_hue(badge_tint(&badge.host_id), &p);
        let wash = gui_theme::mix(p.panel, hue, 30, 100);
        let ring = gui_theme::mix(p.panel, hue, 62, 100);
        let glyph = gui_theme::mix(hue, p.text, 40, 100);
        let text = if selected { p.accent } else { glyph };
        let host_id = SharedString::from(badge.host_id.clone());
        let title = badge.title.clone();
        let click = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.pick(host_id.clone(), cx);
        });
        let manage_badge = badge.clone();
        let ask = cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
            this.open_manage(&manage_badge, cx);
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
            .on_mouse_down(MouseButton::Right, ask)
            .tooltip(move |_window, cx| crate::hint::mic_tooltip(title.clone(), cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .w(rems(BADGE_REM))
                    .h(rems(BADGE_REM))
                    // A rounded square, the shape the rail's session marks and
                    // the topic tabs wear. The column is the same chrome, and
                    // a circle among squares read as a widget from another
                    // window (#3133).
                    .rounded_md()
                    .bg(rgb(wash))
                    .border_1()
                    .border_color(rgb(if selected { p.accent } else { ring }))
                    .child(mark(badge, text, selected)),
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
        let mut children: Vec<AnyElement> = Vec::with_capacity(self.frame.badges.len() * 2 + 4);
        children.push(self.workspace_mark(cx));
        children.push(div().h(rems(0.625)).into_any_element());
        for badge in &self.frame.badges {
            let is_selected = selected.as_deref() == Some(badge.host_id.as_str());
            children.push(self.badge(badge, is_selected, cx));
            children.push(div().h(rems(0.5)).into_any_element());
        }
        children.push(self.add_mark(cx));
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let column = self.column(cx);
        match self.card_overlay(window, cx) {
            Some(card) => column.child(card),
            None => column,
        }
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

// ── The `+` card: pair a crab into this window, or hand this one out ────────

/// Which card the `+` opened.
///
/// No `Debug`, no `Clone`: two of the three states carry a pairing code, and a
/// derive is how a code ends up in a log line nobody meant to write.
pub(crate) enum StripCard {
    /// The other crab's address and code, typed or pasted.
    Pair(PairingForm),
    /// A code is being minted and the address read, off the UI thread.
    Minting,
    /// This crab's address, a fresh code and the QR that carries both.
    Share(ShareView),
    /// A badge was right-clicked: open that crab, or forget it.
    Manage(ManageView),
}

/// The crab a right-click asked about, held by value so the card still names
/// it while the store is being rewritten.
pub(crate) struct ManageView {
    pub(crate) host_id: String,
    pub(crate) label: SharedString,
    pub(crate) title: SharedString,
    /// A forget is in flight; the buttons stop answering until it lands.
    pub(crate) busy: bool,
    /// A new address for this crab, pasted when its tunnel came back
    /// somewhere else. No code: the crab's own token confirms the move.
    pub(crate) address: TextField,
    /// A move to that address is being confirmed.
    pub(crate) moving: bool,
}

impl InstanceStrip {
    /// Whether a card is up. While it is, it owns the keyboard.
    pub(crate) fn card_open(&self) -> bool {
        self.card.is_some()
    }

    /// The `+` was clicked: the pairing form. Nothing touches the network
    /// until it is submitted with an address and a code.
    pub(crate) fn open_pair(&mut self, cx: &mut Context<Self>) {
        self.card_epoch = self.card_epoch.wrapping_add(1);
        self.set_card(Some(StripCard::Pair(PairingForm::default())), cx);
        self.card_error = None;
        cx.notify();
    }

    /// Close whichever card is up. Closing the share card is not a revoke: the
    /// code stays valid until it expires, and saying otherwise would be a
    /// promise the pairing store cannot keep.
    pub(crate) fn close_card(&mut self, cx: &mut Context<Self>) {
        self.card_epoch = self.card_epoch.wrapping_add(1);
        self.set_card(None, cx);
        self.card_error = None;
        cx.notify();
    }

    /// A badge was right-clicked: the card that opens or forgets that crab.
    ///
    /// Forgetting lives behind the right-click and a second, named button,
    /// never on the left-click: the left-click is the pick, and a slip of the
    /// hand there must not cost a pairing.
    pub(crate) fn open_manage(&mut self, badge: &StripBadge, cx: &mut Context<Self>) {
        self.card_epoch = self.card_epoch.wrapping_add(1);
        self.set_card(
            Some(StripCard::Manage(ManageView {
                host_id: badge.host_id.clone(),
                label: badge.label.clone(),
                title: badge.title.clone(),
                busy: false,
                address: TextField::new(),
                moving: false,
            })),
            cx,
        );
        self.card_error = None;
        cx.notify();
    }

    /// The managed crab's "abrir": the same pick a left-click makes.
    fn manage_open(&mut self, cx: &mut Context<Self>) {
        let Some(StripCard::Manage(view)) = self.card.as_ref() else {
            return;
        };
        let host_id = SharedString::from(view.host_id.clone());
        self.close_card(cx);
        if self.frame.selected.as_deref() != Some(host_id.as_ref()) {
            self.pick(host_id, cx);
        }
    }

    /// The managed crab's "esquecer": drop its row from the store off the UI
    /// thread, then hand the window the store as it now reads.
    fn manage_forget(&mut self, cx: &mut Context<Self>) {
        let epoch = self.card_epoch;
        let host_id = match self.card.as_mut() {
            Some(StripCard::Manage(view)) if !view.busy && !view.moving => {
                view.busy = true;
                view.host_id.clone()
            }
            _ => return,
        };
        self.card_error = None;
        cx.notify();
        let forget = host_id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { forget_instance(&forget) })
                .await;
            this.update(cx, |strip, cx| match result {
                Ok(file) => {
                    if strip.card_epoch == epoch {
                        strip.close_card(cx);
                    }
                    strip
                        .dashboard
                        .update(cx, |dash, cx| dash.instance_forgotten(&file, &host_id, cx))
                        .ok();
                }
                Err(e) => {
                    if strip.card_epoch != epoch {
                        return;
                    }
                    if let Some(StripCard::Manage(view)) = strip.card.as_mut() {
                        view.busy = false;
                    }
                    strip.card_error = Some(SharedString::from(format!("esquecer: {e:#}")));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn manage_view(&mut self) -> Option<&mut ManageView> {
        match self.card.as_mut() {
            Some(StripCard::Manage(view)) => Some(view),
            _ => None,
        }
    }

    /// The right-click card's keyboard: its one field takes the typing, Enter
    /// moves the crab to the address in it, Esc closes.
    fn manage_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) {
        if self
            .manage_view()
            .is_some_and(|view| view.busy || view.moving)
        {
            return;
        }
        match ks.key.as_str() {
            "escape" => self.close_card(cx),
            "enter" => self.manage_repoint(cx),
            _ => {
                if let Some(edit) = text_field::editing_chord(
                    ks.key.as_str(),
                    ks.modifiers.control,
                    ks.modifiers.platform,
                    ks.modifiers.alt,
                    ks.modifiers.shift,
                ) {
                    let Some(view) = self.manage_view() else {
                        return;
                    };
                    match text_field::apply(&mut view.address, edit) {
                        Some(text_field::ClipboardRequest::Write(text)) => {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                        Some(text_field::ClipboardRequest::Read) => {
                            if let Some(text) =
                                cx.read_from_clipboard().and_then(|item| item.text())
                                && let Some(view) = self.manage_view()
                            {
                                view.address.insert_text(text.trim());
                            }
                        }
                        None => {}
                    }
                    cx.notify();
                    return;
                }
                if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
                    return;
                }
                let typed = ks.key_char.as_ref().and_then(|s| s.chars().next());
                if let Some(ch) = typed
                    && !ch.is_whitespace()
                    && let Some(view) = self.manage_view()
                {
                    view.address.insert_text(&ch.to_string());
                    cx.notify();
                }
            }
        }
    }

    /// The managed crab's "mudar endereço": ask the pasted address who it is
    /// with this crab's own token, off the UI thread, and move the crab there
    /// only if it is the same crab. A refusal is printed in the card and
    /// changes nothing.
    fn manage_repoint(&mut self, cx: &mut Context<Self>) {
        let epoch = self.card_epoch;
        let Some(view) = self.manage_view() else {
            return;
        };
        if view.busy || view.moving {
            return;
        }
        let host_id = view.host_id.clone();
        let address = view.address.trimmed().to_string();
        if address.is_empty() {
            self.card_error = Some(SharedString::from(
                "endereço: cola primeiro o endereço novo deste crab",
            ));
            cx.notify();
            return;
        }
        if let Some(view) = self.manage_view() {
            view.moving = true;
        }
        self.card_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repoint_instance(&host_id, &address) })
                .await;
            this.update(cx, |strip, cx| {
                match result {
                    Ok(known) => {
                        if strip.card_epoch == epoch {
                            strip.close_card(cx);
                        }
                        strip
                            .dashboard
                            .update(cx, |dash, cx| dash.instance_repointed(&known, cx))
                            .ok();
                    }
                    Err(e) => {
                        if strip.card_epoch != epoch {
                            return;
                        }
                        if let Some(view) = strip.manage_view() {
                            view.moving = false;
                        }
                        strip.card_error =
                            Some(SharedString::from(format!("endereço recusado: {e:#}")));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Mint a code for this crab and build the card that carries it.
    ///
    /// The code is minted in this process by the same [`PairingStore`] the
    /// A2A gateway reads, so no terminal and no `opencrabs connect` is needed.
    /// The config read and the store write both happen on the background
    /// executor: the disk is not touched on the UI thread.
    pub(crate) fn open_share(&mut self, cx: &mut Context<Self>) {
        self.card_epoch = self.card_epoch.wrapping_add(1);
        let epoch = self.card_epoch;
        self.set_card(Some(StripCard::Minting), cx);
        self.card_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let minted = cx
                .background_executor()
                .spawn(async move { mint_share() })
                .await;
            this.update(cx, |strip, cx| {
                if strip.card_epoch != epoch {
                    return;
                }
                match minted {
                    Ok(share) => strip.set_card(Some(StripCard::Share(share)), cx),
                    Err(e) => {
                        strip.card_error = Some(SharedString::from(format!("código: {e:#}")));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pair_form(&mut self) -> Option<&mut PairingForm> {
        match self.card.as_mut() {
            Some(StripCard::Pair(form)) => Some(form),
            _ => None,
        }
    }

    /// The card's keyboard: Tab walks the three fields, Enter pairs, Esc
    /// closes. Every key is the card's while it is up, so a character typed
    /// here can never land in the composer underneath.
    pub(crate) fn card_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) {
        if matches!(self.card, Some(StripCard::Manage(_))) {
            self.manage_key(ks, cx);
            return;
        }
        if self.pair_form().is_some_and(|form| form.busy) {
            // A submit in flight owns nothing: the answer comes back into the
            // form, and typing into a request already sent would lie.
            return;
        }
        match ks.key.as_str() {
            "escape" => self.close_card(cx),
            "enter" => self.pair_submit(cx),
            "tab" => {
                if let Some(form) = self.pair_form() {
                    form.field = if ks.modifiers.shift {
                        (form.field + 2) % 3
                    } else {
                        (form.field + 1) % 3
                    };
                    cx.notify();
                }
            }
            _ => {
                if let Some(edit) = text_field::editing_chord(
                    ks.key.as_str(),
                    ks.modifiers.control,
                    ks.modifiers.platform,
                    ks.modifiers.alt,
                    ks.modifiers.shift,
                ) {
                    self.apply_pair_edit(edit, cx);
                    return;
                }
                if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
                    return;
                }
                let typed = if ks.key == "space" {
                    Some(' ')
                } else {
                    ks.key_char.as_ref().and_then(|s| s.chars().next())
                };
                if let Some(ch) = typed
                    && let Some(form) = self.pair_form()
                {
                    form.focused().insert_text(&ch.to_string());
                    cx.notify();
                }
            }
        }
    }

    /// One editing chord on the focused field, clipboard included, exactly as
    /// the composer does it. A pasted pairing link fills all three fields.
    fn apply_pair_edit(&mut self, edit: text_field::TextFieldEdit, cx: &mut Context<Self>) {
        let Some(form) = self.pair_form() else {
            return;
        };
        let in_url = form.field == 0;
        match text_field::apply(form.focused(), edit) {
            Some(text_field::ClipboardRequest::Write(text)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Some(text_field::ClipboardRequest::Read) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                    && let Some(form) = self.pair_form()
                    && !(in_url && form.absorb_link(&text))
                {
                    form.focused().insert_text(&text);
                }
            }
            None => {}
        }
        cx.notify();
    }

    /// Pair a crab: POST the code, store the token, hand the window the file.
    ///
    /// The exchange and both disk writes run on the background executor, on
    /// the remote client's runtime (`reqwest` panics outside a Tokio reactor,
    /// and gpui's executor is not one). Only the host id and the store as it
    /// now reads come back, and the window picks the new crab from those.
    pub(crate) fn pair_submit(&mut self, cx: &mut Context<Self>) {
        let epoch = self.card_epoch;
        let Some(form) = self.pair_form() else {
            return;
        };
        if form.busy {
            return;
        }
        let url = form.url.trimmed().to_string();
        let code = form.code.trimmed().to_string();
        let label = form.label.trimmed().to_string();
        if url.is_empty() || code.is_empty() {
            self.card_error = Some(SharedString::from(
                "emparelhar: preciso do endereço e do código",
            ));
            cx.notify();
            return;
        }
        form.busy = true;
        self.card_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { pair_and_store(&url, &code, &label) })
                .await;
            this.update(cx, |strip, cx| {
                let current = strip.card_epoch == epoch;
                match result {
                    Ok((host_id, file)) => {
                        // A pairing that went through is kept even when its
                        // card was closed meanwhile: the token is already on
                        // disk, and a strip that hid it would be lying.
                        if current {
                            strip.set_card(None, cx);
                            strip.card_error = None;
                        }
                        strip
                            .dashboard
                            .update(cx, |dash, cx| dash.instance_paired(&file, &host_id, cx))
                            .ok();
                    }
                    Err(e) => {
                        if current && let Some(form) = strip.pair_form() {
                            // The code is single use either way: clear it so
                            // the retry asks for a fresh one.
                            form.busy = false;
                            form.code = TextField::new();
                            form.field = 1;
                            strip.card_error =
                                Some(SharedString::from(format!("emparelhar: {e:#}")));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The `+` under the badges: the way a crab gets into the strip.
    fn add_mark(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette::current();
        let click = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.open_pair(cx));
        div()
            .id("instance-strip-add")
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .w(rems(BADGE_REM))
            .h(rems(BADGE_REM))
            .rounded_full()
            .cursor_pointer()
            .border_1()
            .border_color(rgb(p.track))
            .hover(|d| d.bg(rgb(p.panel_hi)))
            .on_click(click)
            .tooltip(|_window, cx| {
                crate::hint::mic_tooltip(SharedString::from("emparelhar um crab"), cx)
            })
            .child(
                div()
                    .text_size(rems(0.875))
                    .text_color(rgb(p.text_muted))
                    .child(SharedString::from("+")),
            )
            .into_any_element()
    }

    /// The card, painted over the whole window.
    ///
    /// The strip is a 56 px cached view, and the card must not be: it is
    /// deferred and anchored at the window's origin in a box the size of the
    /// viewport, so the shared modal's scrim covers what it covers when the
    /// window mounts it at the root, without the window holding its state.
    fn card_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, title, subtitle, footer, body) = match self.card.as_ref()? {
            StripCard::Pair(form) => {
                let form = form.clone();
                (
                    "instance-pair-modal",
                    "Emparelhar um crab",
                    "o outro crab mostra o endereço e um código de uso único",
                    "tab campos · enter emparelha · esc fecha",
                    self.pair_body(&form, cx),
                )
            }
            StripCard::Minting => (
                "instance-share-modal",
                "Partilhar este crab",
                "a gerar um código de uso único",
                "esc fecha",
                self.minting_body(cx),
            ),
            StripCard::Manage(view) => {
                let body = self.manage_body(view, cx);
                (
                    "instance-manage-modal",
                    "Este crab",
                    "abrir mostra as sessões dele; esquecer apaga o emparelhamento",
                    "enter muda o endereço · esc fecha",
                    body,
                )
            }
            StripCard::Share(share) => {
                let share = share.clone();
                (
                    "instance-share-modal",
                    "Partilhar este crab",
                    "o outro crab lê o QR ou cola o link",
                    "esc fecha · fechar não revoga o código",
                    self.share_body(&share, cx),
                )
            }
        };
        let rem = f32::from(window.rem_size());
        let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.close_card(cx));
        let modal = modal_shell::render(
            modal_shell::ModalView {
                id,
                title: title.to_string(),
                subtitle: subtitle.to_string(),
                actions: None,
                header_close: true,
                body,
                footer: Some(modal_shell::footer(footer.to_string()).into_any_element()),
                width: 26.0 * rem,
                max_height: 36.0 * rem,
            },
            dismiss,
        );
        let viewport = window.viewport_size();
        Some(
            deferred(
                anchored().position(point(px(0.0), px(0.0))).child(
                    div()
                        .relative()
                        .w(viewport.width)
                        .h(viewport.height)
                        .child(modal),
                ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    fn card_button(
        id: &'static str,
        label: impl Into<SharedString>,
        accent: bool,
    ) -> gpui::Stateful<Div> {
        let p = palette::current();
        div()
            .id(id)
            .px_3()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .bg(rgb(if accent { p.accent } else { p.panel_hi }))
            .text_size(rems(0.75))
            .text_color(rgb(if accent { p.bg } else { p.text }))
            .child(label.into())
    }

    fn card_error_line(&self) -> Option<AnyElement> {
        let p = palette::current();
        self.card_error.clone().map(|err| {
            div()
                .text_size(rems(0.75))
                .text_color(rgb(p.warn))
                .child(err)
                .into_any_element()
        })
    }

    /// The pairing form: address, code, and an optional name for the strip.
    fn pair_body(&self, form: &PairingForm, cx: &mut Context<Self>) -> AnyElement {
        let p = palette::current();
        let mut body: Vec<AnyElement> = Vec::new();
        for (index, (label, field)) in form.fields().iter().enumerate() {
            let here = form.field == index;
            // The same span split the composer uses: the window never slices
            // a string, the core does.
            let (before, selected, after) = field.render_spans();
            // A click moves the caret to the row, the same state Tab sets: a
            // field only Tab can reach is a field half the owners never find.
            let focus_here = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                if let Some(form) = this.pair_form() {
                    form.field = index;
                }
                cx.notify();
            });
            body.push(
                div()
                    .id(SharedString::from(format!("instance-pair-field-{index}")))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .cursor_pointer()
                    .on_click(focus_here)
                    .child(
                        div()
                            .text_size(rems(0.6875))
                            .text_color(rgb(if here { p.text_normal } else { p.text_faint }))
                            .child(SharedString::from(*label)),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            // A pasted address is longer than the card is
                            // wide: one line, clipped at the border, never
                            // drawn past it.
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .flex()
                            .flex_row()
                            .items_center()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(p.panel))
                            .border_1()
                            .border_color(rgb(if here { p.accent } else { p.track }))
                            .text_size(rems(0.8125))
                            .text_color(rgb(if field.is_empty() {
                                p.text_faint
                            } else {
                                p.text
                            }))
                            .when(field.is_empty() && !here, |d| {
                                d.child(SharedString::from(match index {
                                    0 => "https://… ou o link opencrabs://pair",
                                    1 => "8 caracteres",
                                    _ => "o nome do outro crab",
                                }))
                            })
                            .child(SharedString::from(before.to_string()))
                            .when(!selected.is_empty(), |d| {
                                d.child(
                                    div()
                                        .bg(rgb(p.selection))
                                        .child(SharedString::from(selected.to_string())),
                                )
                            })
                            .when(here, |d| {
                                d.child(div().flex_none().w(px(1.5)).h(rems(1.0)).bg(rgb(p.accent)))
                            })
                            .child(SharedString::from(after.to_string())),
                    )
                    .into_any_element(),
            );
        }
        if let Some(err) = self.card_error_line() {
            body.push(err);
        }
        let submit = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.pair_submit(cx));
        let share = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.open_share(cx));
        body.push(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    Self::card_button(
                        "instance-pair-submit",
                        if form.busy {
                            "a emparelhar…"
                        } else {
                            "emparelhar"
                        },
                        true,
                    )
                    .when(form.busy, |d| d.opacity(0.6))
                    .on_click(submit),
                )
                .child(
                    Self::card_button("instance-pair-share", "partilhar este crab", false)
                        .on_click(share),
                )
                .into_any_element(),
        );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(body)
            .into_any_element()
    }

    /// The share card while the code is being minted, or why it was not.
    /// The right-click card: who the crab is, "abrir", and "esquecer" named
    /// after the crab so nobody forgets the wrong one.
    fn manage_body(&self, view: &ManageView, cx: &mut Context<Self>) -> AnyElement {
        let p = palette::current();
        let mut body: Vec<AnyElement> = vec![
            div()
                .text_size(rems(0.75))
                .text_color(rgb(p.text_muted))
                .child(view.title.clone())
                .into_any_element(),
        ];
        body.push(self.manage_address_field(view, cx));
        if let Some(err) = self.card_error_line() {
            body.push(err);
        }
        let open = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.manage_open(cx));
        let forget = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.manage_forget(cx));
        let forget_label = if view.busy {
            SharedString::from("a esquecer…")
        } else {
            SharedString::from(format!("esquecer {}", view.label))
        };
        body.push(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(Self::card_button("instance-manage-open", "abrir", true).on_click(open))
                .child(
                    Self::card_button("instance-manage-forget", forget_label, false)
                        .on_click(forget),
                )
                .into_any_element(),
        );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(body)
            .into_any_element()
    }

    /// The new-address field and its button. Clipped at its border the way
    /// the pairing field is: a tunnel URL is longer than the card is wide.
    fn manage_address_field(&self, view: &ManageView, cx: &mut Context<Self>) -> AnyElement {
        let p = palette::current();
        let field = &view.address;
        let (before, selected, after) = field.render_spans();
        let repoint = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.manage_repoint(cx));
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(rems(0.6875))
                    .text_color(rgb(p.text_faint))
                    .child(SharedString::from(
                        "mudou de endereço? cola o novo (sem código)",
                    )),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(p.panel))
                    .border_1()
                    .border_color(rgb(p.accent))
                    .text_size(rems(0.8125))
                    .text_color(rgb(if field.is_empty() {
                        p.text_faint
                    } else {
                        p.text
                    }))
                    .child(SharedString::from(before.to_string()))
                    .when(!selected.is_empty(), |d| {
                        d.child(
                            div()
                                .bg(rgb(p.selection))
                                .child(SharedString::from(selected.to_string())),
                        )
                    })
                    .child(div().flex_none().w(px(1.5)).h(rems(1.0)).bg(rgb(p.accent)))
                    .child(SharedString::from(after.to_string()))
                    .when(field.is_empty(), |d| {
                        d.child(SharedString::from("https://….trycloudflare.com"))
                    }),
            )
            .child(
                div().flex().flex_row().child(
                    Self::card_button(
                        "instance-manage-repoint",
                        if view.moving {
                            "a confirmar…"
                        } else {
                            "mudar endereço"
                        },
                        false,
                    )
                    .when(view.moving, |d| d.opacity(0.6))
                    .on_click(repoint),
                ),
            )
            .into_any_element()
    }

    fn minting_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette::current();
        let mut body: Vec<AnyElement> = Vec::new();
        match self.card_error_line() {
            Some(err) => {
                body.push(err);
                let again = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.open_share(cx));
                body.push(
                    Self::card_button("instance-share-retry", "tentar outra vez", true)
                        .on_click(again)
                        .into_any_element(),
                );
            }
            None => body.push(
                div()
                    .text_size(rems(0.75))
                    .text_color(rgb(p.text_muted))
                    .child(SharedString::from("a gerar o código…"))
                    .into_any_element(),
            ),
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(body)
            .into_any_element()
    }

    /// The share card: the QR, the address, the code, and one copy for all.
    fn share_body(&self, share: &ShareView, cx: &mut Context<Self>) -> AnyElement {
        let p = palette::current();
        let mut body: Vec<AnyElement> = Vec::new();
        body.push(qr_element(share));
        body.push(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(rgb(p.text_faint))
                        .child(SharedString::from("endereço deste crab")),
                )
                .child(
                    div()
                        .text_size(rems(0.8125))
                        .text_color(rgb(p.text))
                        .child(SharedString::from(share.url.clone())),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(rems(0.6875))
                                .text_color(rgb(p.text_faint))
                                .child(SharedString::from("código")),
                        )
                        .child(
                            div()
                                .text_size(rems(1.1))
                                .text_color(rgb(p.accent))
                                .child(SharedString::from(share.code.clone())),
                        ),
                )
                .child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(rgb(p.text_faint))
                        .child(SharedString::from(format!(
                            "vale {} minutos e só serve uma vez",
                            CODE_TTL_SECS / 60
                        ))),
                )
                .into_any_element(),
        );
        if let Some(warning) = share.warning.clone() {
            body.push(
                div()
                    .text_size(rems(0.75))
                    .text_color(rgb(p.warn))
                    .child(SharedString::from(warning))
                    .into_any_element(),
            );
        }
        // The block that opens this machine, printed whole so the owner
        // pastes it instead of assembling keys from the warning (upstream
        // 05cd8a6de). One line per key, in the card's own faint text.
        if let Some(snippet) = share.snippet.clone() {
            let copy_snippet = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                if let Some(StripCard::Share(share)) = this.card.as_ref()
                    && let Some(snippet) = share.snippet.clone()
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(snippet));
                }
            });
            body.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(p.panel))
                    .border_1()
                    .border_color(rgb(p.track))
                    .children(snippet.lines().map(|line| {
                        div()
                            .text_size(rems(0.6875))
                            .text_color(rgb(p.text_muted))
                            .child(SharedString::from(line.to_string()))
                            .into_any_element()
                    }))
                    .child(
                        div()
                            .id("instance-share-copy-snippet")
                            .text_size(rems(0.6875))
                            .text_color(rgb(p.accent))
                            .cursor_pointer()
                            .on_click(copy_snippet)
                            .child(SharedString::from("copiar o bloco")),
                    )
                    .into_any_element(),
            );
        }
        // One click, one paste, whole pairing: the link carries address, code
        // and name together, and the far side's form fills all three from it.
        let copy_all = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            if let Some(StripCard::Share(share)) = this.card.as_ref() {
                cx.write_to_clipboard(ClipboardItem::new_string(share.link.clone()));
            }
        });
        let fresh = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.open_share(cx));
        let back = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.open_pair(cx));
        body.push(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    Self::card_button("instance-share-copy-all", "copiar tudo", true)
                        .on_click(copy_all),
                )
                .child(
                    Self::card_button("instance-share-fresh", "novo código", false).on_click(fresh),
                )
                .child(
                    Self::card_button("instance-share-pair", "emparelhar um crab", false)
                        .on_click(back),
                )
                .into_any_element(),
        );
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(body)
            .into_any_element()
    }
}

/// One QR module's side in pixels. 4 px over a 33-module code is 132 px: it
/// fits the card and a phone camera still locks on.
const QR_MODULE_PX: f32 = 4.0;

/// The QR, white on black whatever the theme is: a scanner needs contrast,
/// and dark modules on a dark card are a square no camera reads.
fn qr_element(share: &ShareView) -> AnyElement {
    if share.width == 0 {
        let p = palette::current();
        return div()
            .text_size(rems(0.75))
            .text_color(rgb(p.warn))
            .child(SharedString::from(
                "não consegui desenhar o QR: usa «copiar tudo»",
            ))
            .into_any_element();
    }
    let mut rows: Vec<AnyElement> = Vec::with_capacity(share.width);
    for y in 0..share.width {
        let mut cells: Vec<AnyElement> = Vec::with_capacity(share.width);
        for x in 0..share.width {
            let dark = share.qr.get(y * share.width + x).copied().unwrap_or(false);
            cells.push(
                div()
                    .w(px(QR_MODULE_PX))
                    .h(px(QR_MODULE_PX))
                    .bg(rgb(if dark { 0x000000 } else { 0xffffff }))
                    .into_any_element(),
            );
        }
        rows.push(div().flex().flex_row().children(cells).into_any_element());
    }
    div()
        .flex()
        .flex_row()
        .justify_center()
        .w_full()
        .child(
            div()
                .flex()
                .flex_col()
                .p_2()
                .rounded_md()
                .bg(rgb(0xffffff))
                .children(rows),
        )
        .into_any_element()
}

/// The pairing form's state. The three fields are real [`TextField`]s, so a
/// typo can be fixed and a paste replaces a selection.
///
/// `Debug` is written by hand: the code field holds a single-use credential,
/// and it prints as its length, never as itself.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct PairingForm {
    pub(crate) url: TextField,
    pub(crate) code: TextField,
    pub(crate) label: TextField,
    /// A submit is in flight; the button says so and refuses a second one.
    pub(crate) busy: bool,
    /// Which field the keyboard is in: 0 address, 1 code, 2 name.
    pub(crate) field: usize,
}

impl std::fmt::Debug for PairingForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = format!("<{} chars>", self.code.text().chars().count());
        f.debug_struct("PairingForm")
            .field("url", &self.url.text())
            .field("code", &code)
            .field("label", &self.label.text())
            .field("busy", &self.busy)
            .field("field", &self.field)
            .finish()
    }
}

impl PairingForm {
    /// The three fields, in the order Tab walks them.
    pub(crate) fn fields(&self) -> [(&'static str, &TextField); 3] {
        [
            ("endereço do crab", &self.url),
            ("código de emparelhamento", &self.code),
            ("nome (opcional)", &self.label),
        ]
    }

    /// The field the keyboard is in.
    pub(crate) fn focused(&mut self) -> &mut TextField {
        match self.field {
            0 => &mut self.url,
            1 => &mut self.code,
            _ => &mut self.label,
        }
    }

    /// Fill the form from a pairing link, which is what the other crab's card
    /// copies and its QR carries. Returns whether the text was one.
    pub(crate) fn absorb_link(&mut self, text: &str) -> bool {
        let Some((url, code, label)) = parse_pair_link(text) else {
            return false;
        };
        self.url.set_text(&url);
        self.code.set_text(&code);
        if let Some(label) = label {
            self.label.set_text(&label);
        }
        self.field = 2;
        true
    }
}

/// The share card's model, built once when the code is minted. The code exists
/// in plaintext only here and as a hash in the store, so the card is rebuilt
/// for a new code rather than re-read.
///
/// `Debug` by hand, for the same reason as [`PairingForm`]: neither the code
/// nor the link that carries it is ever printed.
#[derive(Clone, PartialEq)]
pub(crate) struct ShareView {
    /// The address another crab should dial.
    pub(crate) url: String,
    /// The single-use code.
    pub(crate) code: String,
    /// `opencrabs://pair?…`, which is what the QR carries.
    pub(crate) link: String,
    /// Row-major modules, `width` per side. Empty when the link did not fit.
    pub(crate) qr: Vec<bool>,
    pub(crate) width: usize,
    /// Why this address will not work from another machine, when it will not.
    pub(crate) warning: Option<String>,
    /// The `[a2a]` block that makes this machine reachable, for the owner to
    /// paste into `config.toml`. `None` when nothing is missing.
    pub(crate) snippet: Option<String>,
}

impl std::fmt::Debug for ShareView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShareView")
            .field("url", &self.url)
            .field("code", &"<redacted>")
            .field("link", &"<redacted>")
            .field("width", &self.width)
            .field("warning", &self.warning)
            .field("snippet", &self.snippet)
            .finish()
    }
}

impl ShareView {
    pub(crate) fn new(
        url: String,
        code: String,
        label: Option<&str>,
        warning: Option<String>,
    ) -> Self {
        let link = pair_link(&url, &code, label);
        let (qr, width) = qr_modules(&link);
        Self {
            url,
            code,
            link,
            qr,
            width,
            warning,
            snippet: None,
        }
    }

    /// The card with the config block it should print under the warning.
    pub(crate) fn with_snippet(mut self, snippet: Option<String>) -> Self {
        self.snippet = snippet;
        self
    }
}

/// Mint a code and read the address to hand out. Runs off the UI thread.
fn mint_share() -> anyhow::Result<ShareView> {
    let scopes: Vec<String> = BASE_SCOPES.iter().map(|s| (*s).to_string()).collect();
    let code = PairingStore::new(PairingStore::default_path()).mint_code(&scopes)?;
    // A tunnel that is already running is the truth about how another crab
    // reaches this one: its public URL beats the bind address it forwards.
    let running = tunnel::status(&tunnel::default_state_dir()).ok().flatten();
    let (url, warning, snippet) = match Config::load() {
        Ok(config) => share_address(&config.a2a, running.as_ref()),
        Err(e) => (
            format!("http://127.0.0.1:{}", A2aConfig::default().port),
            Some(format!("não consegui ler a config: {e}")),
            None,
        ),
    };
    let label = this_host_label();
    Ok(ShareView::new(url, code, label.as_deref(), warning).with_snippet(snippet))
}

/// The address to hand out, why it may not work, and the config block that
/// would make it work.
///
/// Pure on purpose (upstream `share_address`): a tunnel that is up is an
/// address another machine can actually dial, and the bind address it
/// forwards is not. The warning and the block exist only for the case where
/// nothing is published, so a live tunnel clears both instead of leaving two
/// stale paragraphs on the card.
pub(crate) fn share_address(
    a2a: &A2aConfig,
    tunnel: Option<&tunnel::TunnelState>,
) -> (String, Option<String>, Option<String>) {
    if let Some(state) = tunnel {
        return (state.url.clone(), None, None);
    }
    let (url, warning) = share_target(a2a);
    (url, warning, share_config_snippet(a2a))
}

/// The `[a2a]` block this machine needs before another crab can reach it, as
/// text the owner can paste into `config.toml`.
///
/// A warning that names two keys still leaves the owner to assemble them in
/// the right section with the right quotes, and a typo there is a window that
/// will not start. What is missing is filled in with this machine's real
/// address, what is already right is left alone, and a machine that needs
/// nothing gets `None`.
pub(crate) fn share_config_snippet(a2a: &A2aConfig) -> Option<String> {
    let host = lan_ip().unwrap_or_else(|| "o-ip-desta-maquina".to_string());
    let loopback = matches!(a2a.bind.as_str(), "127.0.0.1" | "localhost" | "::1");
    let no_key = a2a
        .api_key
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty();
    if a2a.enabled && !loopback && !no_key {
        return None;
    }
    let mut lines = vec!["[a2a]".to_string()];
    if !a2a.enabled {
        lines.push("enabled = true".to_string());
    }
    if loopback {
        lines.push("bind = \"0.0.0.0\"".to_string());
    }
    if a2a
        .advertise_url
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        lines.push(format!("advertise_url = \"http://{host}:{}\"", a2a.port));
    }
    if no_key {
        lines.push("api_key = \"<uma-chave-longa-que-so-tu-sabes>\"".to_string());
    }
    Some(lines.join("\n"))
}

/// Exchange the code, store the token, and read the store back. Runs off the
/// UI thread; the token never leaves this function except into the store.
fn pair_and_store(url: &str, code: &str, label: &str) -> anyhow::Result<(String, InstanceFile)> {
    let paired = instance_client::block_on(instance_store::pair(url, code, label))?;
    let store = InstanceStore::new(InstanceStore::default_path());
    // A crab paired again keeps the mark its owner chose for it.
    let glyph = store.find(&paired.host_id)?.and_then(|known| known.glyph);
    store.upsert(KnownInstance {
        host_id: paired.host_id.clone(),
        label: paired.label,
        url: paired.url,
        token: paired.token,
        added_at: now(),
        last_ok: 0,
        glyph,
        routes: paired.routes,
    })?;
    Ok((paired.host_id, store.load()?))
}

/// The address to hand out, and the reason it may be useless.
///
/// `advertise_url` wins because it is the owner's explicit answer to "how do I
/// reach this box". Without it the bind decides: a loopback gateway cannot be
/// reached from anywhere else, and handing out a code while the listener is on
/// 127.0.0.1 is a pairing that could never have worked.
pub(crate) fn share_target(a2a: &A2aConfig) -> (String, Option<String>) {
    if !a2a.enabled {
        return (
            format!("http://127.0.0.1:{}", a2a.port),
            Some(
                "o gateway A2A está desligado nesta máquina: põe [a2a] enabled = true e volta a abrir a janela."
                    .to_string(),
            ),
        );
    }
    if let Some(url) = a2a
        .advertise_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        return (url.trim_end_matches('/').to_string(), None);
    }
    if matches!(a2a.bind.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return (
            format!("http://127.0.0.1:{}", a2a.port),
            Some(format!(
                "esta máquina só aceita ligações locais, por isso outro crab não chega aqui. Para abrir à rede: [a2a] bind = \"0.0.0.0\" e advertise_url = \"http://{}:{}\", depois fecha e reabre a janela.",
                lan_ip().unwrap_or_else(|| "o-ip-desta-máquina".to_string()),
                a2a.port
            )),
        );
    }
    let host = lan_ip().unwrap_or_else(|| "0.0.0.0".to_string());
    let no_key = a2a
        .api_key
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty();
    let warning = no_key.then(|| {
        "sem api_key, qualquer crab na tua rede pode chamar /a2a/v1. Põe uma em [a2a] api_key."
            .to_string()
    });
    (format!("http://{host}:{}", a2a.port), warning)
}

/// This machine's address on the LAN. A UDP socket that never sends a packet:
/// `connect` only picks the route, and the local address it reports is the
/// interface the kernel would use.
fn lan_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

/// What this crab calls itself in the link. `None` leaves the name to the far
/// side, which then uses this crab's agent-card name.
fn this_host_label() -> Option<String> {
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|h| h.trim().to_string())
        .find(|h| !h.is_empty())
}

/// The link as QR modules, row-major, and the side length. A link that does
/// not fit comes back empty rather than as a half-drawn square.
pub(crate) fn qr_modules(link: &str) -> (Vec<bool>, usize) {
    match qrcode::QrCode::new(link.as_bytes()) {
        Ok(code) => {
            let width = code.width();
            let dark = code
                .to_colors()
                .into_iter()
                .map(|color| color == qrcode::Color::Dark)
                .collect();
            (dark, width)
        }
        Err(_) => (Vec::new(), 0),
    }
}

/// `opencrabs://pair?url=…&code=…&label=…`, the link the share card copies and
/// its QR carries. [`parse_pair_link`] is its inverse.
pub(crate) fn pair_link(url: &str, code: &str, label: Option<&str>) -> String {
    let mut link = format!(
        "opencrabs://pair?url={}&code={}",
        percent_encode(url),
        percent_encode(code)
    );
    if let Some(label) = label.filter(|l| !l.is_empty()) {
        link.push_str("&label=");
        link.push_str(&percent_encode(label));
    }
    link
}

/// Read a pairing link back into `(url, code, label)`.
pub(crate) fn parse_pair_link(text: &str) -> Option<(String, String, Option<String>)> {
    let rest = text.trim().strip_prefix("opencrabs://pair?")?;
    let mut url = None;
    let mut code = None;
    let mut label = None;
    for pair in rest.split('&') {
        let (key, value) = pair.split_once('=')?;
        let value = percent_decode(value);
        match key {
            "url" => url = Some(value),
            "code" => code = Some(value),
            "label" => label = Some(value),
            _ => {}
        }
    }
    Some((url?, code?, label.filter(|l| !l.is_empty())))
}

/// Percent-encode everything outside the unreserved set.
pub(crate) fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Percent-decode a query value, byte by byte, without slicing the string.
fn percent_decode(value: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        char::from(byte)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    }
    let bytes: Vec<u8> = value.bytes().collect();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let escaped = if byte == b'%' {
            let hi = bytes.get(i + 1).copied().and_then(hex);
            let lo = bytes.get(i + 2).copied().and_then(hex);
            hi.zip(lo)
        } else {
            None
        };
        match (byte, escaped) {
            (_, Some((hi, lo))) => {
                out.push(hi * 16 + lo);
                i += 3;
            }
            (b'+', None) => {
                out.push(b' ');
                i += 1;
            }
            (other, None) => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Drop one crab from the store and read the store back, the pair a forget
/// hands the window. A host that was already gone is not an error: the strip
/// still reloads, so a stale badge left by another window disappears too.
fn forget_instance(host_id: &str) -> anyhow::Result<InstanceFile> {
    let store = InstanceStore::new(InstanceStore::default_path());
    store.remove(host_id)?;
    store.load()
}

/// Move one crab to the address the owner pasted, and hand back its row as it
/// now reads. Runs off the UI thread. The check is
/// [`instance_repoint::repoint_to`]'s: the address must answer this crab's
/// own token and announce its host id, or nothing is written.
fn repoint_instance(host_id: &str, address: &str) -> anyhow::Result<KnownInstance> {
    let store = InstanceStore::new(InstanceStore::default_path());
    instance_client::block_on(instance_repoint::repoint_to(&store, host_id, address))
}
/// The hue a badge's colour slot wears in the live palette.
///
/// The slots are palette colours rather than computed hues, so every crab's
/// tint follows the theme: switch to the light theme and the washes lighten
/// with it instead of burning a hole in the paper.
fn tint_hue(slot: usize, p: &palette::Palette) -> u32 {
    match slot % crate::instances::BADGE_TINTS {
        0 => p.blue,
        1 => p.teal,
        2 => p.green,
        3 => p.amber,
        4 => p.accent,
        _ => p.red,
    }
}

/// What a badge draws: the owner's picture when the mark is one, the mark's
/// characters otherwise.
///
/// A mark can be a letter, an emoji, or one of the embedded pictures the topic
/// tabs pick from (`topic:`/`img:` names). Resolving it here, instead of
/// storing two kinds of value, keeps one field in the store and one column on
/// screen: a crab's icon and a topic's icon are the same pick (#3133).
fn mark(badge: &StripBadge, text: u32, selected: bool) -> AnyElement {
    if let Some(picture) = crate::topic_icons::tab_icon(&badge.glyph, BADGE_REM * 0.62) {
        return picture;
    }
    div()
        .text_size(rems(0.6875))
        .when(selected, |d| d.font_weight(gpui::FontWeight::BOLD))
        .text_color(rgb(text))
        .child(badge.glyph.clone())
        .into_any_element()
}
