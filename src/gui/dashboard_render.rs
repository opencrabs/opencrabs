//! The window's frame: what each screen draws and what sits above it.
//!
//! `render` is the entry point gpui calls. Every overlay is attached at the
//! window root, after both panes, so a card added later paints over one added
//! earlier (#2690).

use super::*;
use opencrabs::ui_core::hints::{CONTEXT_TIGHT, Hint, HintState, dismissible};

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Advance from whatever repaint came in. The animation receipt is
        // cleared only by its own callback, not by an unrelated repaint.
        self.tick_middle_scroll(cx);
        // The frame the window is showing, read where the window is in hand
        // (#3082). Cheap: one comparison unless a drag moved it.
        self.observe_window(window);
        // Native alpha and the root's opacity are two halves of the same
        // setting (#2922). Change the platform mode only when it flips; the
        // opacity itself is cheap to paint on every frame.
        let transparent = self.window_transparency > 0;
        if transparent != self.window_background_transparent {
            window.set_background_appearance(if transparent {
                WindowBackgroundAppearance::Transparent
            } else {
                WindowBackgroundAppearance::Opaque
            });
            self.window_background_transparent = transparent;
        }
        // The machine's font catalog is read once, on the first paint (#2786):
        // `all_font_names` walks CoreText on macOS and keeps no cache of its
        // own, so asking it per frame would re-enumerate every installed
        // family on every paint.
        if !self.fonts_loaded {
            self.fonts_loaded = true;
            fonts::remember_installed(cx.text_system().all_font_names());
        }
        // Focus follows the keyboard owner, decided here because this is the
        // only place holding a window to focus through. A transition that
        // forgot to ask would leave the composer answering under an open
        // field, which is the bug class #2455 came from, so the owner is
        // reconciled every frame instead of trusted from each call site.
        //
        // A task list whose last task ended lets go first, so the keyboard is
        // back on the composer in the same frame the card disappears.
        self.prune_task_state();
        let owner = keyboard_owner(&self.open_overlays(), self.screen);
        if self.last_owner != Some(owner) {
            // A receipt in the daily log, the same shape as `pane focus ->`
            // (#2549): a switch that never moves the owner is exactly the
            // case the reconciler below cannot repair (#2741).
            logging::debug(&format!("keyboard owner -> {owner:?}"));
            self.last_owner = Some(owner);
            self.refocus = Some(focus_target(owner));
        }
        // Applied before the tree is built, so the frame that shows the new
        // screen is the frame that can be typed into.
        if let Some(target) = self.refocus.take() {
            match target {
                FocusTarget::Composer => {
                    // The keyboard follows the pane, not the window (#2610):
                    // a rotated caret that left focus on the chat leaf typed
                    // into a field the split was not drawing.
                    let composer = self.composer_entity(cx);
                    let handle = composer.read(cx).focus_handle.clone();
                    // `before` is true when gpui turned the request into a
                    // no-op, because it returns early on the handle that is
                    // already focused. `now` names who holds the keys after
                    // the call, so "the caret never came back" is answerable
                    // from the log instead of guessed from a screenshot
                    // (#2741).
                    let before = handle.is_focused(window);
                    window.focus(&handle);
                    logging::debug(&format!(
                        "caret -> composer before={before} now={}",
                        self.caret_owner_name(window, cx)
                    ));
                }
                FocusTarget::Root => {
                    let before = self.focus.is_focused(window);
                    window.focus(&self.focus);
                    logging::debug(&format!(
                        "caret -> root before={before} now={}",
                        self.caret_owner_name(window, cx)
                    ));
                }
            }
        }
        // A press on plain chrome takes the caret without moving the owner:
        // the root tracks focus, so every element that does not sits under it
        // and the press lands there, while the derived owner is still the
        // composer. The transition above cannot see that, so the drift is read
        // off the window instead and repaired before the user types (#2741).
        if owner == KeyboardOwner::Composer {
            let composer = self.composer_entity(cx);
            let drifted = !composer.read(cx).focus_handle.is_focused(window)
                && window.focused(cx).is_none_or(|handle| handle == self.focus);
            if drifted {
                let handle = composer.read(cx).focus_handle.clone();
                window.focus(&handle);
                logging::debug(&format!(
                    "caret -> composer repaired now={}",
                    self.caret_owner_name(window, cx)
                ));
            }
        } else {
            // The mirror of the repair above, and the half that was missing
            // (#2767): an overlay or a screen took the keys while the composer
            // kept the caret, so everything typed into the prompt also landed
            // in the message field and Enter submitted it. A field the overlay
            // owns is not this drift; only the composer is.
            let composer = self.composer_entity(cx);
            let leaked = composer.read(cx).focus_handle.is_focused(window);
            if leaked {
                window.focus(&self.focus);
                logging::debug(&format!(
                    "caret -> root repaired now={}",
                    self.caret_owner_name(window, cx)
                ));
            }
        }
        // Coming back to the conversation is a return, not a resume: a panel
        // that closes lands on the newest row instead of the history the
        // reader left behind, the same rule the terminal applies when a
        // dialog hands the chat back (#2698).
        if self.screen == Screen::Chat && self.last_screen != Screen::Chat {
            self.rearm_transcript_follow();
        }
        self.last_screen = self.screen;
        // Keep every transcript on its newest row (#2685). Run before the
        // tree is built so the snap lands in this frame's layout, and judged
        // from each viewport's own offset so a reader parked in history is
        // never yanked back down.
        if !self.middle_scroll_controls_chat() {
            self.follow_chat();
        }
        for session in self.panes.as_ref().map(|t| t.leaves()).unwrap_or_default() {
            if !self.middle_scroll_controls_pane(session) {
                self.follow_pane(session, cx);
            }
        }
        // The rail is an overlay only while it holds the keys. A path that
        // focuses the composer directly (a drop, a pasted image, the
        // paperclip's deferred request) never asks `refocus_screen`, so the
        // claim is repaired here: an armed rail over a focused input would
        // steal the caret back on the next transition (#2665).
        if self.rail_focused
            && self.rail_open
            && self
                .composer_entity(cx)
                .read(cx)
                .focus_handle
                .is_focused(window)
        {
            self.rail_focused = false;
        }
        // The plan strip follows the open conversation. Where a plan lives
        // depends on what the session is bound to, so the lookup is a DB read
        // and runs once per selection, off the paint path; the strip itself
        // then only stats the file.
        if self.plan_for != self.chat_session {
            self.plan_for = self.chat_session;
            match self.chat_session {
                Some(sid) => {
                    self.plan.set_path(None);
                    // The lookup reads the DB through the pool, and deadpool
                    // needs a Tokio reactor; the gpui executor is not one, so
                    // this never runs inline (smoke-panic receipt in the gate
                    // log, 2026-10-31). Same pattern as every bridge call.
                    agent_bridge::spawn_plan_path(self.db_path.clone(), sid, self.sink.clone());
                }
                None => self.plan.set_path(None),
            }
        }
        self.plan.refresh();
        // A task that just settled rings once (#2691). This is the window's
        // tick: the strip was re-read by mtime just above, and the pane
        // strips are re-read again when the split body is built below, so a
        // pane that settles between frames is caught on the next one.
        self.settle_plan_tasks(cx);

        // The expanded rail is a cached view of its own (#2933). The closed
        // rail never resolves that frame: it keeps only the logo and reopen
        // button, so no session maps are built behind the compact strip.
        let rail = if self.rail_open || self.screen == Screen::Onboard {
            self.rail_element(cx)
        } else {
            self.collapsed_rail_element(cx)
        };

        // One label drives the in-window chrome and the OS titlebar: the old
        // static "usage" title sat over an open conversation. The owner
        // wanted the usage screen's whole identity on the one line the
        // chrome already draws: `OpenCrabs Usage · All Time · v0.5.15`, the
        // middle word tracking the period switch (t/w/m/a).
        let label = match self.screen {
            Screen::Usage => format!(
                "OpenCrabs Usage \u{b7} {} \u{b7} v{}",
                period_word(self.dash.period),
                env!("CARGO_PKG_VERSION")
            ),
            Screen::Files => "OpenCrabs \u{b7} files".to_string(),
            Screen::Models => "OpenCrabs \u{b7} models".to_string(),
            Screen::Skills => "OpenCrabs \u{b7} skills".to_string(),
            Screen::Projects => "OpenCrabs \u{b7} projects".to_string(),
            Screen::DirectoryPicker => "OpenCrabs \u{b7} directory".to_string(),
            Screen::Onboard => "OpenCrabs \u{b7} onboarding".to_string(),
            Screen::MissionControl => "OpenCrabs \u{b7} mission control".to_string(),
            Screen::Chat => format!(
                "OpenCrabs \u{b7} {}",
                self.session_title().unwrap_or_else(|| "chat".to_string())
            ),
        };
        window.set_window_title(&label);
        // The header is its own surface: a panel one step above the chat and a
        // divider under it, so the title does not read as part of the history.
        let chrome = window_controls::drag_handle(div())
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .bg(rgb(palette::current().panel))
            .border_b_1()
            .border_color(rgb(palette::current().panel_hi))
            // The header is the window's own bar on Linux, where the
            // compositor draws none (#2813), so a press on it moves the
            // window instead of selecting text. On the chat screen there is no
            // header (#2596): the rail's brand row is the handle there (#3014).
            .child(icon::banner_logo())
            .child(div().text_lg().child(SharedString::from(label)));

        // One click listener per turn anchor, minted here because only the
        // view's own `cx` can build them; the transcript takes them by value.
        let mut fold_handlers: std::collections::HashMap<String, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for anchor in self.chat_state.transcript.anchors() {
            let key = anchor.clone();
            let handler = cx.listener(move |this, _ev, _window, cx| {
                this.toggle_turn(&key, cx);
            });
            fold_handlers.insert(anchor, std::rc::Rc::new(handler));
        }
        // The live card's own click, minted here for the same reason as the
        // folds: only this `cx` can build one, and the card that is on screen
        // is the one a click must fold (#2442).
        let live_click: transcript::ClickHandler =
            std::rc::Rc::new(cx.listener(|this, ev: &gpui::ClickEvent, _window, cx| {
                // A release that ended a drag is the end of a copy, not a
                // request to fold. gpui fires a click on every release with no
                // movement threshold, and the row's own release handler clears
                // the selection before this listener runs, so the positions are
                // the only evidence left: without this, copying a line out of
                // the card also closed the card the line was in (#2678).
                if let gpui::ClickEvent::Mouse(click) = ev
                    && (click.up.position - click.down.position).magnitude() > 2.0
                {
                    return;
                }
                this.chat_state.transcript.live_expanded =
                    !this.chat_state.transcript.live_expanded;
                cx.notify();
            }));
        // The marker above the transcript's drawn window loads the next one
        // (#2805). Minted here because the transcript state lives on this view.
        let older_click: transcript::ClickHandler =
            std::rc::Rc::new(cx.listener(|this, _ev: &gpui::ClickEvent, _window, cx| {
                if this.chat_state.transcript.load_older() {
                    cx.notify();
                }
            }));
        // One click listener per attachment tile, minted here for the same
        // reason as the folds: only this `cx` can build one, and each tile has
        // to open its own file (#2395).
        let mut attachment_handlers: std::collections::HashMap<String, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for key in transcript::attachment_paths_with_live(
            &self.chat_state.transcript,
            // The window holds exactly the visible conversation's turn: it was
            // parked and handed back by `swap_visible_turn`, so a live card
            // here always belongs to the session on screen (#2616).
            &self.turn.live,
        ) {
            let path = std::path::PathBuf::from(key.clone());
            let handler = cx.listener(move |this, _ev, _window, cx| {
                this.open_preview(path.clone(), cx);
            });
            attachment_handlers.insert(key, std::rc::Rc::new(handler));
        }

        // One hook per stored row (#2613): a right press opens that row's
        // menu, and while select mode is on a left click toggles it. Minted
        // per frame like the tiles above, because only this `cx` can build a
        // listener and each row has to answer for itself.
        let msg_order: Vec<Uuid> = self.chat_state.transcript.message_ids();
        let select_on = self.msg_select.is_active();
        let msg_picked: std::collections::HashSet<Uuid> = msg_order
            .iter()
            .copied()
            .filter(|id| self.msg_select.contains(id))
            .collect();
        let mut message_click: std::collections::HashMap<Uuid, transcript::ClickHandler> =
            std::collections::HashMap::new();
        let mut message_press: std::collections::HashMap<Uuid, transcript::PressHandler> =
            std::collections::HashMap::new();
        // The copy behind one answer's hover button (#2750). Only answers get
        // one: the reader's own rows already carry a pencil, and two buttons in
        // one corner would fight for the same pixel.
        let mut message_copy: std::collections::HashMap<Uuid, transcript::ClickHandler> =
            std::collections::HashMap::new();
        // The row itself goes with the press (#2827), so the menu's actions
        // read the message that was right-clicked rather than looking an id up
        // in whatever transcript happens to be on screen by then.
        let msg_rows: Vec<Message> = msg_order
            .iter()
            .filter_map(|id| {
                self.chat_state
                    .transcript
                    .rows
                    .iter()
                    .find(|m| m.id == *id)
                    .cloned()
            })
            .collect();
        for row in msg_rows {
            let id = row.id;
            if select_on {
                let handler = cx.listener(move |this, ev: &ClickEvent, _window, cx| {
                    this.msg_toggle(id, ev.modifiers().shift, cx);
                });
                message_click.insert(id, std::rc::Rc::new(handler));
            } else if row.role == "user"
                && opencrabs::ui_core::message_reader::opens_in_reader(&row.content)
            {
                // A prompt long enough to stop reading well in the column folds
                // to its opening lines, and both the row and its document card
                // open the whole message in the reader (#3053, #3064). Nothing
                // is lost: the body is stored whole, copy and edit read all of
                // it, and the card is one click from the rest.
                let title = opencrabs::ui_core::message_reader::title(&row.content);
                let content = row.content.clone();
                let handler = cx.listener(move |this, ev: &ClickEvent, _window, cx| {
                    // A drag is a copy gesture, not a request to open (#2678).
                    if let gpui::ClickEvent::Mouse(click) = ev
                        && (click.up.position - click.down.position).magnitude() > 2.0
                    {
                        return;
                    }
                    this.open_message_preview(id, title.clone(), content.clone(), cx);
                });
                message_click.insert(id, std::rc::Rc::new(handler));
            }
            if row.role == "assistant" {
                let content = row.content.clone();
                let handler = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.msg_copy_markdown(id, content.clone(), cx);
                });
                message_copy.insert(id, std::rc::Rc::new(handler));
            }
            let handler = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                this.msg_menu_open(row.clone(), None, ev.position, cx);
            });
            message_press.insert(id, std::rc::Rc::new(handler));
        }
        // Exact live entries get their own captured rows (#2925). Keying by the
        // row they draw keeps the handler attached as later lines arrive, and
        // it covers the reader's own rows too: the question and every message
        // the agent took mid-turn are drawn from the turn, not from `rows`.
        let mut live_press: std::collections::HashMap<
            transcript::LiveRow,
            transcript::PressHandler,
        > = std::collections::HashMap::new();
        // The pencil behind those same rows (#2925). A turn in flight is
        // exactly when the reader wants to take back what he just said, so the
        // one row on screen with no pencil would be his own.
        let mut live_edit: std::collections::HashMap<
            transcript::LiveSlot,
            transcript::ClickHandler,
        > = std::collections::HashMap::new();
        if let Some(session) = self.chat_session {
            for (target, row) in transcript::live_action_rows(&self.turn.live, session) {
                if let transcript::LiveRow::User(slot) = target {
                    let text = row.content.clone();
                    let edit = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.msg_edit_begin_live(slot, text.clone(), cx);
                    });
                    live_edit.insert(slot, std::rc::Rc::new(edit));
                }
                let handler = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.live_menu_open(row.clone(), None, ev.position, cx);
                });
                live_press.insert(target, std::rc::Rc::new(handler));
            }
        }
        // One toggle per chip on screen (#2825). Minted only for the pictures
        // the loaded rows actually carry, so a row with no reactions costs
        // nothing here. The pairs are collected first for the same reason the
        // row ids are: a listener is built from `cx`, not from the state.
        let reaction_pairs: Vec<(Uuid, String)> = self
            .chat_state
            .transcript
            .reactions
            .iter()
            .flat_map(|(id, list)| list.iter().map(move |(name, _)| (*id, name.clone())))
            .collect();
        let mut reaction_click: std::collections::HashMap<
            (Uuid, String),
            transcript::ClickHandler,
        > = std::collections::HashMap::new();
        for (id, name) in reaction_pairs {
            let picked = name.clone();
            let handler = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                // A chip on the window's own row reacts on that row, whatever
                // surface the last row menu happened to be opened in (#2827).
                this.msg_menu_leaf = None;
                this.msg_menu_row = None;
                this.msg_react_pick(id, picked.clone(), cx);
            });
            reaction_click.insert((id, name), std::rc::Rc::new(handler));
        }
        // One edit per stored user row (#2826): the pencil opens a field in
        // that row's own card, with a check that sends the rewrite and a cross
        // that closes it. Answers are left out, so the map holds only the rows
        // a rewrite makes sense for.
        let edit_rows: Vec<(Uuid, i32)> = self
            .chat_state
            .transcript
            .rows
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| (m.id, m.sequence))
            .collect();
        let mut msg_edit: std::collections::HashMap<Uuid, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for (id, sequence) in edit_rows {
            let handler = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.msg_edit_begin(id, sequence, cx);
            });
            msg_edit.insert(id, std::rc::Rc::new(handler));
        }
        // A left press anywhere in the transcript dismisses the message menu
        // (#2613): the press is elsewhere, so the menu goes with it, the same
        // rule the rail applies to its own menu (#2354). The card and the rows
        // stop this from firing on their own presses.
        let msg_dismiss: transcript::PressHandler =
            Rc::new(cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                let closed_menu = this.msg_menu.take().is_some();
                let closed_picker = this.react_menu.take().is_some();
                if closed_picker {
                    this.react_menu_at = None;
                }
                if closed_menu || closed_picker {
                    this.msg_menu_row = None;
                    this.msg_menu_leaf = None;
                    cx.notify();
                }
            }));
        // A wheel tick over the conversation is the reader's own hand on it
        // (#2698): GPUI moves the viewport, and this only says whether the
        // follow goes with it. A tick up releases it, and the frame loop
        // re-arms it the moment the reader is back at the bottom.
        let wheel: transcript::ScrollHandler =
            Rc::new(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                // The card is pinned to a window point, not to its row, so a
                // scroll leaves it floating over a row it no longer names
                // (#2690). The wheel closes it.
                if this.msg_menu.take().is_some() {
                    this.msg_menu_row = None;
                    this.msg_menu_leaf = None;
                    cx.notify();
                }
                if this.react_menu.take().is_some() {
                    this.react_menu_at = None;
                    cx.notify();
                }
                if !wheel_releases_follow(wheel_dy(&ev.delta)) {
                    return;
                }
                this.release_transcript_follow();
                cx.notify();
            }));
        let middle_click: transcript::PressHandler =
            Rc::new(cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                if let Some(session) = this.chat_session {
                    this.middle_scroll_down(
                        middle_scroll::MiddleScrollTarget::Chat(session),
                        ev.position,
                        cx,
                    );
                }
            }));

        // The transcript plans its attachment tiles from the width the pane
        // actually has, read from the live viewport rather than a constant, so
        // a row can never be planned wider than the box it is drawn into
        // (#2395). The usage screen loses its side panel from the same number.
        //
        // The width the conversation's column really has (#3119): the viewport
        // less the rail and the gutter the column spends on its own sides. The
        // column is padded with `px_4`, one rem a side, in a split and on every
        // screen but the unsplit chat; the unsplit chat runs edge to edge
        // (#3112). A fixed 48 points of chrome, the safe side for a plan, was
        // 16 too many here, and in a split those points made every planned pane
        // narrower than the box it was drawn into: the rows stopped short of
        // the plan card under them, which the pane sizes from the box itself.
        let column_gutter = if self.screen == Screen::Chat && !self.window_is_split() {
            0.0
        } else {
            2.0 * self.rem
        };
        let pane_width = attachment_view::column_width(
            f32::from(window.viewport_size().width),
            self.visible_rail_width() * self.rem,
            column_gutter,
        );
        let picker_layout = model_layout::model_layout(
            pane_width,
            self.rem,
            BASE_REM,
            self.screen == Screen::Onboard,
        );

        // The chat's share of the pane area comes from the tree: a leaf under
        // stacked splits spans the full width, and a leaf beside a sibling
        // takes its share of it (#2472, #2473). A row is still never planned
        // wider than its box. The measure is read here rather than inside the
        // chat arm because the conversation and the chrome under it have to
        // agree on one width (#2588).
        let chat_share = self
            .panes
            .as_ref()
            .zip(self.chat_session)
            .and_then(|(tree, id)| tree.leaf_width_share(id))
            .unwrap_or(1.0);
        let primary_width = if chat_share < 1.0 {
            split_pane_width(pane_width, false, chat_share)
        } else {
            pane_width
        };
        // A single pane draws the conversation at a readable measure and
        // centres it; a split window is exempt and keeps every pane at the span
        // the tree hands it (#2586). The count is resolved once: it also
        // decides whether the window footer row draws at all (#2641).
        let pane_count = self.panes.as_ref().map_or(1, |tree| tree.leaves().len());
        let split_window = pane_count > 1;
        let chat_measure_px = chat_measure(primary_width, split_window);
        // Only a pane wider than the measure is wrapped, and a split pane is
        // never wrapped: its measure is already its own width.
        let centred_chat = chat_measure_px < primary_width;

        let (middle, footer): (AnyElement, AnyElement) = match self.screen {
            // The theme modal is an overlay over the conversation, not a
            // screen of its own (#2619): the chat draws as usual and the
            // modal is attached at the root, above the panes.
            Screen::Chat => {
                // The chat is the primary pane, and a split draws its session
                // beside or below it (#2438). Two panes share the content
                // width, less the gap_2 that separates them, so a row is
                // never planned wider than the box it is painted into.
                // The chat's share of the pane area comes from the tree: a
                // leaf under stacked splits spans the full width, and a leaf
                // beside a sibling takes its share of it (#2472, #2473). A
                // row is still never planned wider than its box.
                let measure = chat_measure_px;
                // Minted before the view borrows the transcript (#2826): the
                // two clicks are listeners, so they need `self` mutably.
                let inline_edit = self.inline_edit_hooks(cx);
                // Click resolution needs the conversation's own directory
                // (#2846). A window launched from a desktop entry has no
                // shell cwd, so a relative name in the transcript had no base
                // and the click did nothing.
                let click_roots = self
                    .chat_session
                    .and_then(|id| {
                        agent_bridge::bridge()
                            .map(|b| b.service.get_working_directory_for_session(id))
                    })
                    .into_iter()
                    .collect();
                self.copy_drag.set_roots(click_roots);
                // The rows that follow the last turn ride INSIDE the
                // conversation now (#3139), so they are built before the
                // view that draws them: a plan waiting on approval has to
                // scroll away with the chat instead of holding the bottom
                // of the window.
                let trailing = self.chat_trailing(window, cx);
                let primary = transcript::view(
                    &self.chat_state.transcript,
                    &self.chat_state.scroll,
                    // The visible conversation's turn, never another's: the
                    // window keeps exactly one and swaps them on a switch
                    // (#2616).
                    &self.turn.live,
                    self.turn.busy,
                    measure,
                    f32::from(window.rem_size()),
                    fold_handlers,
                    transcript::ViewHooks {
                        attachments: attachment_handlers,
                        drag: Some(self.copy_drag.clone()),
                        wheel: Some(wheel),
                        middle_click: Some(middle_click),
                        ctx_used: self.turn.ctx_tokens,
                        ctx_max: self.ctx_max,
                        live_click: Some(live_click.clone()),
                        message_click: message_click.clone(),
                        message_press: message_press.clone(),
                        live_press,
                        live_edit,
                        selection: select_on.then(|| msg_picked.clone()),
                        menu_dismiss: Some(msg_dismiss.clone()),
                        older: Some(older_click.clone()),
                        reactions: self.chat_state.transcript.reactions.clone(),
                        reaction_click: reaction_click.clone(),
                        edit: msg_edit.clone(),
                        // The field a rewrite draws in its own card (#2826),
                        // minted here because its two clicks are listeners.
                        edit_row: inline_edit,
                        // The copy on an answer's hover (#2750) and the row
                        // whose check is up, so a copy confirms itself without
                        // moving the row.
                        copy: message_copy.clone(),
                        copied: self.copied_row(),
                        // Whether this transcript is keeping its own bottom
                        // (#3027). Only the reader's place can say whether a
                        // row that runs past the box is a frame to correct or
                        // a place they chose to be.
                        following: self.chat_state.follow.is_following(),
                        // Whether a clip is being made for this conversation
                        // right now (#3092). The set is the window's, keyed by
                        // session, so a split leaf and the chat can never
                        // disagree about which answer is being spoken.
                        voice_pending: self
                            .chat_session
                            .is_some_and(|id| self.voice_pending.contains(&id)),
                        // The hint, the offers and the live plan card,
                        // drawn under the last turn inside the scroll
                        // (#3139).
                        trailing,
                    },
                );
                // The conversation takes the measure and the column centres it
                // (#2586). The cap goes on the transcript itself instead of on
                // a wrapper element: that wrapper was the one node a single
                // pane had and a split never did, and it is what the black
                // chat came from (#2981). Nothing here adds an element to the
                // surface's tree, it only gives the surface its width. The
                // width is set while the view is still its own type, because
                // an erased element takes no style.
                let primary = if centred_chat {
                    primary.w(px(measure)).into_any_element()
                } else {
                    primary.into_any_element()
                };
                // The panes come from the tree (#2472): one leaf is the
                // window as it was, and each split adds a divider and the
                // panes its children hold.
                let body = self.panes_body(primary, pane_width, true, window, cx);
                (
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .gap_2()
                        // A capped surface is centred by its parent, so the
                        // slack falls to both margins and the chrome under it
                        // keeps the same axis. A split never reaches this: its
                        // measure is its own width.
                        .when(centred_chat, |d| d.items_center())
                        .child(body)
                        .into_any_element(),
                    if window_footer_yields(pane_count) {
                        // A split draws one footer per pane (#2593), so the
                        // window row stands down and yields its height, the way
                        // the terminal's status bar does (#2575, #2641). Two
                        // rows printed the model twice, and the window one
                        // could only ever name the chat's pair while the pane
                        // above it named another.
                        div().into_any_element()
                    } else {
                        // Identity left, the one key hint right. The ctx
                        // budget used to sit at the right edge too, and it was
                        // the same number the tool block's own footer already
                        // carries: read twice, on every turn (#2631).
                        // Who is answering, from where and on what branch: the
                        // same fields the TUI's footer carries. Session,
                        // provider, model and directory all read from the left
                        // edge; the Seeker pill takes the right (#3143).
                        // The open conversation's own pair, not the window's
                        // bootstrap provider: a pick pins the model per
                        // session, so the global slot never moves and this row
                        // kept naming the pair it started with (#2634).
                        let (provider, model) = match agent_bridge::bridge() {
                            Some(b) => match self.chat_session {
                                Some(sid) => {
                                    let (provider, model) =
                                        b.service.provider_pair_for_session(sid);
                                    let label =
                                        model.rsplit('/').next().unwrap_or(&model).to_string();
                                    (provider, label)
                                }
                                None => (b.service.provider_name(), b.service.provider_model()),
                            },
                            None => ("-".to_string(), "-".to_string()),
                        };
                        // The session name leads, as in the TUI footer. The row
                        // across the top that used to carry it was removed
                        // (#2596), and in a split this footer stands down while
                        // each pane's border names its own conversation, so the
                        // name is not printed twice anywhere (#2578).
                        // The place the footer names is the open
                        // conversation's own, never the directory the process
                        // was started in (#3036): the launch directory never
                        // moves, so a `/cd` could change where the agent works
                        // and leave this row pointing at the old project. The
                        // branch is read from that same directory, and only
                        // when it changes, because `git` is a subprocess.
                        let (cwd, branch) = self.footer_place();
                        let identity = format!(
                            "{} · {}",
                            self.session_title().unwrap_or_else(|| "chat".to_string()),
                            status_identity(&provider, &model, &cwd, &branch)
                        );
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .w_full()
                            // The single-session dock owns this row now: it is
                            // the bottom half of the composer card, under the
                            // hairline. It needs its own padding because the
                            // chat column's `gap_3` no longer separates it
                            // from the input, and a bare text row would sit
                            // on the card's rounded bottom edge. The split
                            // arm above yields an empty row, so a pane footer
                            // keeps the chrome it already had.
                            //
                            // The row pads the card's column minus the hover
                            // pill's inset, and each label pads the rest, so
                            // every first glyph starts on
                            // `CONVERSATION_INSET_REM` (#3114). That is the
                            // column the field's own pad and the transcript's
                            // gutter + bubble put their words on; the card is
                            // already half a rem in from the session's edge, so
                            // this row's step is the card's, not the pane's.
                            .px(transcript::card_row_inset())
                            .py_2()
                            // The live identity leads the row: session,
                            // provider, model, directory and branch all start
                            // on the left edge, where state is read from
                            // (#3143 follow-up). It used to open second,
                            // behind the Seeker pill, which pushed live state
                            // off that edge for a permanent chord label.
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    // Same 4 as the row's other labels: the
                                    // truncated text starts and ends on the
                                    // composer's column, not on the raw pad
                                    // (#3114).
                                    .px(transcript::hover_pill_inset())
                                    .text_xs()
                                    .text_color(rgb(palette::current().text_faint))
                                    .child(SharedString::from(identity)),
                            )
                            // The Seeker opens with a chord nothing on
                            // screen named (#2747). The hint is permanent and
                            // clickable: a hover tooltip is invisible to
                            // exactly the people who need it, and the chord
                            // printed matches the platform the window runs on.
                            // It closes the row on the right edge now, so the
                            // identity keeps the left one (#3143).
                            //
                            // No sidebar chord label on this row (#3143).
                            // It carries live state and the one door that has
                            // no other mouse path, the Seeker pill. The chord
                            // is taught by the hint line, and the rail already
                            // has two click doors of its own, so a permanent
                            // label here bought nothing. The Usage and Files
                            // footers keep their labels: neither has a Seeker
                            // pill or a rail of its own to click.
                            //
                            // The pill rests on the row's own ink, not the
                            // brighter muted token (#3146). It shares this row
                            // with live state, and a chord label is advice: at
                            // `text_muted` it was the loudest thing in a footer
                            // of faint chrome, so the eye landed on the chord
                            // before the session it was describing.
                            .child(
                                div()
                                    .id("seeker-hint")
                                    .flex()
                                    .flex_none()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .rounded_sm()
                                    .px(transcript::hover_pill_inset())
                                    .text_xs()
                                    .text_color(rgb(palette::current().text_faint))
                                    .cursor_pointer()
                                    .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                                    .on_click(cx.listener(
                                        |this, _ev: &gpui::ClickEvent, _window, cx| {
                                            this.open_finder(cx);
                                        },
                                    ))
                                    .child(SharedString::from(
                                        opencrabs::ui_core::palette::seeker_chord_label(),
                                    ))
                                    .child("· seek"),
                            )
                            .into_any_element()
                    },
                )
            }
            Screen::DirectoryPicker => {
                // The terminal's /cd dialog, as a screen (#2527): the path on
                // top, the rows under it, one footer line naming the keys.
                // Flat surfaces, the same styling law the rest of the window
                // follows (#2528).
                let shown = palette::current();
                let last = self.cd_entries.len().saturating_sub(1);
                let current = self.cd_selected.min(last);
                let mut list = div().flex().flex_col().gap_1();
                for (index, path) in self.cd_entries.iter().enumerate() {
                    let name = if path.ends_with("..") {
                        "..".to_string()
                    } else {
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("?")
                            .to_string()
                    };
                    let selected = index == current;
                    let mut row = div()
                        .id(SharedString::from(format!("cd-row-{index}")))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px_2()
                        .py_1();
                    if selected {
                        row = row.bg(rgb(shown.panel_hi));
                    }
                    let row = row
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.cd_selected = index;
                            this.cd_descend(index, cx);
                        }))
                        .child(text(name, Size::Sm, shown.text))
                        .child(text(
                            if path.ends_with("..") { "up" } else { "open" },
                            Size::Xs,
                            shown.text_muted,
                        ));
                    list = list.child(row);
                }
                let scroller = div()
                    .id("cd-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list);
                let header = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(text(
                        SharedString::from(self.cd_dir.display().to_string()),
                        Size::Sm,
                        shown.text,
                    ))
                    .child(text(
                        if self.cd_show_hidden {
                            "hidden on"
                        } else {
                            "hidden off"
                        },
                        Size::Xs,
                        shown.text_muted,
                    ));
                let mut panel = div()
                    .id("cd-panel")
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded(px(CARD_RADIUS))
                    .border_1()
                    .border_color(rgb(shown.panel_hi))
                    .bg(glass_fill(shown.panel))
                    .child(header)
                    .child(scroller);
                if self.cd_entries.is_empty() {
                    panel = panel.child(text("no subdirectories", Size::Sm, shown.text_muted));
                }
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(div().text_xs().text_color(rgb(shown.text_muted)).child(
                        format!("{}/{}", current + 1, self.cd_entries.len()),
                    ))
                    .child(div().text_xs().text_color(rgb(shown.text_muted)).child(
                        "up/down \u{b7} walk \u{b7} enter \u{b7} open \u{b7} . \u{b7} hidden \u{b7} tab/space \u{b7} select here \u{b7} esc \u{b7} back",
                    ));
                (panel.into_any_element(), footer.into_any_element())
            }
            Screen::Usage => {
                let body: Div = if self.dash.cache_detail_open {
                    let cache = match &self.state {
                        LoadState::Loaded(vm) => vm.cache.as_ref(),
                        LoadState::Failed(_) => None,
                    };
                    breakdown::panel(cache)
                } else {
                    let content = match &self.state {
                        LoadState::Loaded(vm) => usage_view::dashboard(vm, self.dash.focused_card),
                        LoadState::Failed(err) => div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_lg()
                                    .text_color(rgb(palette::current().red))
                                    .child("Usage data failed to load"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(palette::current().text_muted))
                                    .child(err.clone()),
                            ),
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(self.usage_period_switcher(cx))
                        .child(content)
                };
                // The scrollable middle: an overflow-scroll element's minimum size
                // is zero, so `flex_1` bounds it to the space left over and the
                // content inside scrolls. The footer sits outside it, so the key
                // hints never scroll away.
                let scroller = div()
                    .id("usage-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body);
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(breakdown::footer(&self.dash))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child("alt+space · seek help")
                            .child("ctrl+s · sidebar"),
                    );
                (scroller.into_any_element(), footer.into_any_element())
            }
            Screen::Files => {
                // One row per tracked file, the same door the TUI opens with
                // `f` on its sessions browser. Empty without any, errored
                // with the reason, never a mock row.
                let scroller = div()
                    .id("files-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.files_view());
                let count = self.files.len();
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child(SharedString::from(format!(
                                "{count} file{} tracked",
                                if count == 1 { "" } else { "s" }
                            ))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child("alt+space · seek help")
                            .child("ctrl+s · sidebar"),
                    );
                (scroller.into_any_element(), footer.into_any_element())
            }
            // The shared modal paints Projects once, above the window chrome.
            Screen::Projects => (div().flex_1().into_any_element(), div().into_any_element()),
            Screen::MissionControl => {
                let shown = palette::current();
                // The cards reflow with the width the pane actually has, read
                // from the live window so a resize lands on the same frame
                // (#2459). The rail is subtracted only when it is drawn.
                let viewport = f32::from(window.viewport_size().width);
                let rail = self.visible_rail_width() * self.rem;
                let cols = mission_control::grid_columns(viewport - rail, self.rem);
                // The cards draw the snapshot the four services produced on
                // entry; the frame itself never reads SQLite (#2426).
                let body = mission_control_body(
                    self.mc.as_ref(),
                    &self.mc_nav,
                    self.mc_err.as_ref(),
                    self.mc_loading,
                    shown,
                    cols,
                );
                // A decision is never silent: the tool's own sentence sits
                // above the panels, green when it landed and red when it did
                // not, until the screen is left.
                let body = match self.mc_notice.clone() {
                    Some(notice) => {
                        let color = if notice.ok { shown.green } else { shown.red };
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .child(
                                div()
                                    .px_4()
                                    .py_1()
                                    .text_xs()
                                    .text_color(rgb(color))
                                    .child(notice.text),
                            )
                            .child(body)
                            .into_any_element()
                    }
                    None => body.into_any_element(),
                };
                // The terminal's footer, in its own words: the keys the screen
                // answers, so a hand that knows the terminal is already home.
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_1()
                    .child(text("Tab/h/l switch panel", Size::Xs, shown.text_muted))
                    .child(text("↑↓ navigate", Size::Xs, shown.text_muted))
                    .child(text("Enter detail", Size::Xs, shown.text_muted))
                    .child(text("a apply", Size::Xs, shown.text_muted))
                    .child(text("r reject", Size::Xs, shown.text_muted))
                    .child(text("D/W/M/A filter", Size::Xs, shown.text_muted))
                    .child(text("L logs", Size::Xs, shown.text_muted))
                    .child(text("Esc close", Size::Xs, shown.text_muted));
                (body.into_any_element(), footer.into_any_element())
            }
            Screen::Onboard => match self.onboard.as_mut() {
                None => {
                    // Unreachable through the door: `open_onboard` builds the
                    // wizard before it shows the screen. A window in this state
                    // says so instead of drawing a blank pane.
                    let shown = palette::current();
                    (
                        div()
                            .flex_1()
                            .p_6()
                            .child(text(
                                "The wizard is closed. Type /onboard to open it.",
                                Size::Sm,
                                shown.text_muted,
                            ))
                            .into_any_element(),
                        div().into_any_element(),
                    )
                }
                Some(ob) => {
                    let shown = palette::current();
                    let mut model_footer = None;
                    let (number, total) = ob.progress();
                    // The rail is the flow the current mode walks, with the
                    // step the user stands on marked. Channel sub-steps are
                    // reached from Channels and are not stops.
                    let mut rail = div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w(px(picker_layout.onboarding_rail_width))
                        .flex_none()
                        .min_w_0();
                    if picker_layout.onboarding_rail_width > 0.0 {
                        rail = rail.pr_5();
                    } else {
                        rail = rail.hidden();
                    }
                    for (n, title, here) in ob.rail() {
                        rail = rail.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .child(text(
                                    format!("{n:>2}"),
                                    Size::Xs,
                                    if here { shown.amber } else { shown.text_faint },
                                ))
                                .child(
                                    text(
                                        title,
                                        Size::Sm,
                                        if here {
                                            shown.text_strong
                                        } else {
                                            shown.text_muted
                                        },
                                    )
                                    .flex_1()
                                    .min_w_0()
                                    .truncate(),
                                ),
                        );
                    }
                    let mut column = div().flex().flex_col().gap_3().flex_1().min_w_0().min_h_0();
                    column = column.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(text(
                                format!("step {number} of {total}"),
                                Size::Xs,
                                shown.text_faint,
                            ))
                            .child(text(ob.title(), Size::Xl, shown.text_title)),
                    );

                    if ob.step() == onboard::OnboardingStep::ProviderAuth {
                        // The pair this step is pointed at, above the list: the
                        // one line that answers "what will this window answer
                        // with" without reading two lists.
                        column = column.child(text(
                            format!("{} · {}", ob.provider_id(), ob.model()),
                            Size::Xs,
                            shown.text_faint,
                        ));
                        // The provider list, the help the terminal shows under
                        // it, then the fields that provider needs. Every row is
                        // a real click: nothing here is a label pretending.
                        // A window over the list, not the whole list. The
                        // terminal windows this exact list (`visible_window`)
                        // because every provider painted pushed the fields
                        // below it off screen. The arrows walk all of them and
                        // the counts say what is out of view.
                        let selected = ob.wizard.ps.selected_provider;
                        let (provider_rows, hidden_above, hidden_below) = ob.provider_rows(14);
                        let mut list = div().flex().flex_col().gap_1().w_full();
                        if hidden_above > 0 {
                            list = list.child(text(
                                format!("\u{2191} {hidden_above} more"),
                                Size::Xs,
                                shown.text_faint,
                            ));
                        }
                        for (index, provider_name) in provider_rows {
                            let here = index == selected;
                            let mut row = div()
                                .id(SharedString::from(format!("onboard-provider-{index}")))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .child(text(
                                    if here { "\u{25cf}" } else { " " },
                                    Size::Sm,
                                    if here { shown.amber } else { shown.text_muted },
                                ))
                                .child(text(
                                    provider_name,
                                    Size::Sm,
                                    if here { shown.text_strong } else { shown.text },
                                ));
                            if here {
                                row = row.bg(rgb(shown.panel_hi));
                            }
                            list = list.child(row.on_click(cx.listener(
                                move |this, _ev, _window, cx| {
                                    if let Some(ob) = this.onboard.as_mut() {
                                        ob.select_provider(index);
                                        ob.wizard.ps.model_filter.clear();
                                    }
                                    this.models_query.clear();
                                    this.models_index = 0;
                                    this.refresh_onboard_models();
                                    cx.notify();
                                },
                            )));
                        }
                        column = column.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(text("provider", Size::Xs, shown.text_faint))
                                .child(div().id("onboard-providers").w_full().child(list).child(
                                    if hidden_below > 0 {
                                        text(
                                            format!("\u{2193} {hidden_below} more below"),
                                            Size::Xs,
                                            shown.text_faint,
                                        )
                                    } else {
                                        text("", Size::Xs, shown.text_faint)
                                    },
                                )),
                        );

                        let mut help = div().flex().flex_col().gap_1();
                        for line in ob.help_lines() {
                            help = help.child(text(*line, Size::Xs, shown.text_muted));
                        }
                        column = column.child(help);

                        if let Some(key_field) = ob.key_field() {
                            let focused = ob.field == key_field;
                            let saved = ob.wizard.ps.api_key_input
                                == opencrabs::tui::onboarding::EXISTING_KEY_SENTINEL;
                            let value = if ob.wizard.ps.api_key_input.is_empty() {
                                "paste the key".to_string()
                            } else if saved {
                                "a key is already saved \u{2014} type to replace it".to_string()
                            } else {
                                ob.wizard.ps.api_key_input.clone()
                            };
                            column = column.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text(
                                        ob.key_label().to_string(),
                                        Size::Xs,
                                        shown.text_faint,
                                    ))
                                    .child(
                                        div()
                                            .id("onboard-key")
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .px_3()
                                            .py_2()
                                            .rounded(px(6.0))
                                            .bg(rgb(if focused {
                                                shown.panel_hi
                                            } else {
                                                shown.panel
                                            }))
                                            .border_1()
                                            .border_color(rgb(if focused {
                                                shown.focus
                                            } else {
                                                shown.track
                                            }))
                                            .child(text(
                                                value,
                                                Size::Sm,
                                                if ob.wizard.ps.api_key_input.is_empty() || saved {
                                                    shown.text_faint
                                                } else {
                                                    shown.text
                                                },
                                            ))
                                            .child(text(
                                                if focused { "\u{258f}" } else { "" },
                                                Size::Sm,
                                                shown.amber,
                                            ))
                                            .on_click(cx.listener(
                                                move |this, _ev, _window, cx| {
                                                    if let Some(ob) = this.onboard.as_mut() {
                                                        ob.field = key_field;
                                                    }
                                                    cx.notify();
                                                },
                                            )),
                                    ),
                            );
                        }

                        if ob.wizard.ps.is_custom() {
                            let base = 2 + usize::from(ob.key_needed());
                            let fields = [
                                ("name", ob.wizard.ps.custom_name.clone(), base),
                                ("base url", ob.wizard.ps.base_url.clone(), base + 1),
                                ("model", ob.wizard.ps.custom_model.clone(), base + 2),
                            ];
                            for (label, value, field) in fields {
                                let focused = ob.field == field;
                                column = column.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(text(label, Size::Xs, shown.text_faint))
                                        .child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "onboard-custom-{field}"
                                                )))
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .px_3()
                                                .py_2()
                                                .rounded(px(6.0))
                                                .bg(rgb(if focused {
                                                    shown.panel_hi
                                                } else {
                                                    shown.panel
                                                }))
                                                .border_1()
                                                .border_color(rgb(if focused {
                                                    shown.focus
                                                } else {
                                                    shown.track
                                                }))
                                                .child(text(
                                                    if value.is_empty() {
                                                        "\u{2014}".to_string()
                                                    } else {
                                                        value
                                                    },
                                                    Size::Sm,
                                                    if focused {
                                                        shown.text
                                                    } else {
                                                        shown.text_normal
                                                    },
                                                ))
                                                .child(text(
                                                    if focused { "\u{258f}" } else { "" },
                                                    Size::Sm,
                                                    shown.amber,
                                                ))
                                                .on_click(cx.listener(
                                                    move |this, _ev, _window, cx| {
                                                        if let Some(ob) = this.onboard.as_mut() {
                                                            ob.field = field;
                                                        }
                                                        cx.notify();
                                                    },
                                                )),
                                        ),
                                );
                            }
                        }

                        let active_provider = if ob.wizard.ps.is_custom() {
                            ob.wizard.ps.custom_name.clone()
                        } else {
                            ob.provider_id().to_string()
                        };
                        let rendered = model_picker::render(
                            cx,
                            model_picker::ModelPickerView {
                                rows: &self.models_rows,
                                query: &self.models_query,
                                selected: self.models_index,
                                fetching: self.models_fetching,
                                pending: self.models_pending.len(),
                                error: self.models_fetch_error.as_deref(),
                                active_provider: &active_provider,
                                onboarding: true,
                                search_focused: ob.field == ob.search_field(),
                                max_models: 14,
                                row_layout: picker_layout.model_row,
                                provider_label_width: picker_layout.provider_label_width,
                                show_provider_labels: true,
                            },
                        );
                        model_footer = Some(rendered.footer);
                        column = column.child(rendered.body);
                    } else if ob.step() == onboard::OnboardingStep::ModeSelect {
                        // The first choice the terminal asks: how much of the
                        // flow to walk. Same two options, same step counts, so
                        // the promise here matches the progress counter.
                        let current = ob.mode();
                        let mut choices = div().flex().flex_col().gap_2();
                        for (mode, title, note) in [
                            (
                                onboard::WizardMode::QuickStart,
                                "QuickStart",
                                format!(
                                    "sensible defaults, {} steps",
                                    onboard::OnboardingStep::quick_total()
                                ),
                            ),
                            (
                                onboard::WizardMode::Advanced,
                                "Advanced",
                                format!(
                                    "full control, all {} steps",
                                    onboard::OnboardingStep::total()
                                ),
                            ),
                        ] {
                            let here = current == mode;
                            // The arrows land here first on a fresh install, so
                            // the row they are on has to be visible (#2432).
                            let focused =
                                ob.focused_choice() == Some(onboard::ChoiceRow::Mode(mode));
                            choices = choices.child(
                                div()
                                    .id(SharedString::from(format!("onboard-mode-{title}")))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .rounded(px(6.0))
                                    .bg(rgb(if here { shown.panel_hi } else { shown.panel }))
                                    .border_1()
                                    .border_color(rgb(if focused {
                                        shown.amber
                                    } else if here {
                                        shown.focus
                                    } else {
                                        shown.track
                                    }))
                                    .child(text(
                                        if focused { "\u{25b8}" } else { " " },
                                        Size::Xs,
                                        if focused {
                                            shown.amber
                                        } else {
                                            shown.text_faint
                                        },
                                    ))
                                    .child(text(
                                        if here { "\u{25cf}" } else { "\u{25cb}" },
                                        Size::Sm,
                                        if here { shown.amber } else { shown.text_faint },
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(text(
                                                title,
                                                Size::Sm,
                                                if here { shown.text_strong } else { shown.text },
                                            ))
                                            .child(text(note, Size::Xs, shown.text_muted)),
                                    )
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        if let Some(ob) = this.onboard.as_mut() {
                                            ob.set_mode(mode);
                                        }
                                        cx.notify();
                                    })),
                            );
                        }
                        column = column.child(choices);
                    } else if ob.step() == opencrabs::tui::onboarding::OnboardingStep::HealthCheck {
                        // The terminal spreads these checks over ticks so a
                        // Pending row is visible for one frame. The window
                        // resolves them on the click and paints the verdicts,
                        // which is the part anyone came for.
                        let results = ob.health().to_vec();
                        let complete = ob.health_complete();
                        let passed = ob.all_health_passed();
                        let mut rows = div().flex().flex_col().gap_1();
                        if results.is_empty() {
                            rows = rows.child(text(
                                "The checks have not run yet.",
                                Size::Sm,
                                shown.text_muted,
                            ));
                        }
                        for (label, status) in results {
                            let (mark, colour, detail) = match status {
                                opencrabs::tui::onboarding::HealthStatus::Pending => {
                                    ("\u{25cb}", shown.text_faint, None)
                                }
                                opencrabs::tui::onboarding::HealthStatus::Running => {
                                    ("\u{25cb}", shown.amber, None)
                                }
                                opencrabs::tui::onboarding::HealthStatus::Pass => {
                                    ("\u{2713}", shown.green, None)
                                }
                                opencrabs::tui::onboarding::HealthStatus::Fail(reason) => {
                                    ("\u{2717}", shown.red, Some(reason))
                                }
                            };
                            let mut row = div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .bg(rgb(shown.panel))
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .child(text(mark, Size::Sm, colour))
                                        .child(text(label, Size::Sm, shown.text)),
                                );
                            if let Some(reason) = detail {
                                row = row.child(text(reason, Size::Xs, shown.text_muted));
                            }
                            rows = rows.child(row);
                        }
                        column = column.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(text("checks", Size::Xs, shown.text_faint))
                                .child(rows),
                        );
                        // The terminal ends the list with a verdict, so a
                        // clean run reads as one sentence and not as six
                        // marks the user has to add up.
                        if complete {
                            let (verdict, colour) = if passed {
                                ("All checks passed!", shown.green)
                            } else {
                                ("Some checks failed.", shown.red)
                            };
                            column = column.child(text(verdict, Size::Sm, colour));
                        }
                        column = column.child(
                            div()
                                .id("onboard-health-run")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .bg(rgb(shown.panel_hi))
                                .child(text(
                                    if complete {
                                        "run the checks again"
                                    } else {
                                        "run the checks"
                                    },
                                    Size::Sm,
                                    shown.text_strong,
                                ))
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    if let Some(ob) = this.onboard.as_mut() {
                                        ob.run_health();
                                    }
                                    cx.notify();
                                })),
                        );
                    } else {
                        if ob.is_channel_step() {
                            // The setup steps, verbatim from the terminal, so
                            // a window user is told where to create the bot
                            // instead of hunting for it.
                            let help_lines = ob.channel_help();
                            if !help_lines.is_empty() {
                                let mut help = div().flex().flex_col().gap_1();
                                for line in help_lines {
                                    help = help.child(text(*line, Size::Xs, shown.text_muted));
                                }
                                column = column.child(help);
                            }
                            // The terminal's own channel list. Toggling a row
                            // here is the same flip the terminal makes, and the
                            // fields each enabled channel asks for are painted
                            // underneath from the shared slot table.
                            let mut list = div().flex().flex_col().gap_1();
                            for (index, (name, description, on)) in
                                ob.channels().into_iter().enumerate()
                            {
                                // The row the keyboard stands on is the row
                                // Space flips, so it has to be visible.
                                let focused = ob.field == index;
                                let mut row = div()
                                    .id(SharedString::from(format!("onboard-channel-{index}")))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .rounded(px(6.0))
                                    .child(text(
                                        if focused { "\u{25b8}" } else { " " },
                                        Size::Xs,
                                        if focused {
                                            shown.amber
                                        } else {
                                            shown.text_faint
                                        },
                                    ))
                                    .child(text(
                                        if on { "\u{2713}" } else { "\u{25cb}" },
                                        Size::Sm,
                                        if on { shown.green } else { shown.text_faint },
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(text(
                                                name,
                                                Size::Sm,
                                                if on { shown.text_strong } else { shown.text },
                                            ))
                                            .child(text(description, Size::Xs, shown.text_muted)),
                                    );
                                if focused {
                                    row = row.bg(rgb(shown.panel_hi));
                                }
                                list = list.child(row.on_click(cx.listener(
                                    move |this, _ev, _window, cx| {
                                        if let Some(ob) = this.onboard.as_mut() {
                                            ob.field = index;
                                            ob.toggle_channel(index);
                                        }
                                        cx.notify();
                                    },
                                )));
                            }
                            column = column.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text("channels", Size::Xs, shown.text_faint))
                                    .child(list),
                            );
                        }

                        if ob.step() == onboard::OnboardingStep::VoiceSetup {
                            // The terminal asks this step as a radio choice first
                            // and only then shows the fields that choice needs. Same
                            // options, same labels, same order.
                            let mut rows = div().flex().flex_col().gap_1();
                            for (provider, label, here) in ob.stt_options() {
                                // The row the arrows and Space act on, so it has
                                // to be visible: caret when focused, filled dot
                                // when picked (#2432).
                                let focused =
                                    ob.focused_choice() == Some(onboard::ChoiceRow::Stt(provider));
                                rows = rows.child(
                                    div()
                                        .id(SharedString::from(format!("onboard-stt-{label}")))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(6.0))
                                        .border_1()
                                        .border_color(rgb(if focused {
                                            shown.amber
                                        } else {
                                            shown.track
                                        }))
                                        .bg(rgb(if here { shown.panel_hi } else { shown.panel }))
                                        .child(text(
                                            if focused { "\u{25b8}" } else { " " },
                                            Size::Xs,
                                            if focused {
                                                shown.amber
                                            } else {
                                                shown.text_faint
                                            },
                                        ))
                                        .child(text(
                                            if here { "\u{25cf}" } else { "\u{25cb}" },
                                            Size::Sm,
                                            if here { shown.amber } else { shown.text_faint },
                                        ))
                                        .child(text(
                                            label,
                                            Size::Sm,
                                            if here { shown.text_strong } else { shown.text },
                                        ))
                                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                                            if let Some(ob) = this.onboard.as_mut() {
                                                ob.set_stt_provider(provider);
                                            }
                                            cx.notify();
                                        })),
                                );
                            }
                            column = column.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text("transcription", Size::Xs, shown.text_faint))
                                    .child(rows),
                            );

                            let mut rows = div().flex().flex_col().gap_1();
                            for (provider, label, here) in ob.tts_options() {
                                let focused =
                                    ob.focused_choice() == Some(onboard::ChoiceRow::Tts(provider));
                                rows = rows.child(
                                    div()
                                        .id(SharedString::from(format!("onboard-tts-{label}")))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(6.0))
                                        .border_1()
                                        .border_color(rgb(if focused {
                                            shown.amber
                                        } else {
                                            shown.track
                                        }))
                                        .bg(rgb(if here { shown.panel_hi } else { shown.panel }))
                                        .child(text(
                                            if focused { "\u{25b8}" } else { " " },
                                            Size::Xs,
                                            if focused {
                                                shown.amber
                                            } else {
                                                shown.text_faint
                                            },
                                        ))
                                        .child(text(
                                            if here { "\u{25cf}" } else { "\u{25cb}" },
                                            Size::Sm,
                                            if here { shown.amber } else { shown.text_faint },
                                        ))
                                        .child(text(
                                            label,
                                            Size::Sm,
                                            if here { shown.text_strong } else { shown.text },
                                        ))
                                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                                            if let Some(ob) = this.onboard.as_mut() {
                                                ob.set_tts_provider(provider);
                                            }
                                            cx.notify();
                                        })),
                                );
                            }
                            column = column.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text("speech", Size::Xs, shown.text_faint))
                                    .child(rows),
                            );

                            // OpenAI TTS is ten voices behind one radio list
                            // in the terminal, so the window paints the same list
                            // instead of asking anyone to type a voice name blind.
                            if ob.wizard.tts_provider
                                == opencrabs::tui::onboarding::TtsProvider::OpenAi
                            {
                                let mut voices = div().flex().flex_col().gap_1();
                                for (voice, here) in ob.tts_voice_options() {
                                    voices = voices.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "onboard-tts-voice-{voice}"
                                            )))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap_2()
                                            .px_3()
                                            .py_1()
                                            .rounded(px(6.0))
                                            .bg(rgb(if here {
                                                shown.panel_hi
                                            } else {
                                                shown.panel
                                            }))
                                            .child(text(
                                                if here { "\u{25cf}" } else { "\u{25cb}" },
                                                Size::Sm,
                                                if here { shown.amber } else { shown.text_faint },
                                            ))
                                            .child(text(
                                                voice,
                                                Size::Sm,
                                                if here { shown.text_strong } else { shown.text },
                                            ))
                                            .on_click(cx.listener(
                                                move |this, _ev, _window, cx| {
                                                    if let Some(ob) = this.onboard.as_mut() {
                                                        ob.set_tts_voice(voice);
                                                    }
                                                    cx.notify();
                                                },
                                            )),
                                    );
                                }
                                column = column.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(text("voice", Size::Xs, shown.text_faint))
                                        .child(voices),
                                );
                            }

                            for note in ob.voice_notes() {
                                column = column.child(text(note, Size::Xs, shown.text_muted));
                            }
                        }

                        if ob.step() == onboard::OnboardingStep::ImageSetup {
                            // The terminal names the house provider above the
                            // switches, so the window says the same thing.
                            column = column.child(text(
                                "provider \u{b7} google \u{b7} nano banana",
                                Size::Xs,
                                shown.text_faint,
                            ));
                            // Two switches the terminal asks for on this step: read
                            // images sent to a channel, and generate images on
                            // request. Both are stored as their own config keys.
                            for (label, note, on, target) in [
                                (
                                    "understand images",
                                    "look at a picture sent in a chat",
                                    ob.image_vision(),
                                    onboard::ImageToggle::Vision,
                                ),
                                (
                                    "generate images",
                                    "draw a picture from a prompt",
                                    ob.image_generation(),
                                    onboard::ImageToggle::Generation,
                                ),
                            ] {
                                // The switch the arrows and Space act on (#2432).
                                let focused =
                                    ob.focused_choice() == Some(onboard::ChoiceRow::Image(target));
                                column = column.child(
                                    div()
                                        .id(SharedString::from(format!("onboard-image-{label}")))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(6.0))
                                        .border_1()
                                        .border_color(rgb(if focused {
                                            shown.amber
                                        } else {
                                            shown.track
                                        }))
                                        .bg(rgb(if focused { shown.panel_hi } else { shown.panel }))
                                        .child(text(
                                            if focused { "\u{25b8}" } else { " " },
                                            Size::Xs,
                                            if focused {
                                                shown.amber
                                            } else {
                                                shown.text_faint
                                            },
                                        ))
                                        .child(text(
                                            if on { "\u{2713}" } else { "\u{25cb}" },
                                            Size::Sm,
                                            if on { shown.green } else { shown.text_faint },
                                        ))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(text(label, Size::Sm, shown.text))
                                                .child(text(note, Size::Xs, shown.text_muted)),
                                        )
                                        .on_click(cx.listener(move |_this, _ev, _window, cx| {
                                            if let Some(ob) = _this.onboard.as_mut() {
                                                ob.toggle_image(target);
                                            }
                                            cx.notify();
                                        })),
                                );
                            }
                        }

                        if ob.step() == opencrabs::tui::onboarding::OnboardingStep::Daemon {
                            let on = ob.install_daemon();
                            let focused = ob.focused_choice() == Some(onboard::ChoiceRow::Daemon);
                            column = column.child(
                                div()
                                    .id("onboard-daemon")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .rounded(px(6.0))
                                    .border_1()
                                    .border_color(rgb(if focused {
                                        shown.amber
                                    } else {
                                        shown.track
                                    }))
                                    .bg(rgb(if focused { shown.panel_hi } else { shown.panel }))
                                    .child(text(
                                        if focused { "\u{25b8}" } else { " " },
                                        Size::Xs,
                                        if focused { shown.amber } else { shown.text_faint },
                                    ))
                                    .child(text(
                                        if on { "\u{2713}" } else { "\u{25cb}" },
                                        Size::Sm,
                                        if on { shown.green } else { shown.text_faint },
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(text(
                                                format!("install as {}", ob.daemon_platform()),
                                                Size::Sm,
                                                shown.text,
                                            ))
                                            .child(text(
                                                "so OpenCrabs keeps answering with no window or terminal open",
                                                Size::Xs,
                                                shown.text_muted,
                                            )),
                                    )
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        if let Some(ob) = this.onboard.as_mut() {
                                            ob.toggle_daemon();
                                        }
                                        cx.notify();
                                    })),
                            );
                        }

                        // Text rows come from the wizard's own slot table, so a
                        // row's label and the string it edits cannot drift.
                        let mut fields = div().flex().flex_col().gap_2();
                        for (label, target) in ob.slots() {
                            let focused = ob.focused_text() == Some(target);
                            let value = ob.text_mut(target).clone();
                            let saved = value == opencrabs::tui::onboarding::EXISTING_KEY_SENTINEL;
                            let (painted, faint) = if saved {
                                (
                                    "a key is already saved, type to replace it".to_string(),
                                    true,
                                )
                            } else if value.is_empty() {
                                // An empty generation model is not "nothing":
                                // the step falls back to a specific model, so
                                // the row names it instead of playing blank.
                                match target {
                                    onboard::TextTarget::ImageModel => (
                                        format!(
                                            "{} (default)",
                                            onboard::Onboard::default_image_model()
                                        ),
                                        true,
                                    ),
                                    _ => ("type here".to_string(), true),
                                }
                            } else {
                                (value, false)
                            };
                            fields = fields.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text(label, Size::Xs, shown.text_faint))
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "onboard-field-{label}"
                                            )))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .px_3()
                                            .py_2()
                                            .rounded(px(6.0))
                                            .bg(rgb(if focused {
                                                shown.panel_hi
                                            } else {
                                                shown.panel
                                            }))
                                            .border_1()
                                            .border_color(rgb(if focused {
                                                shown.focus
                                            } else {
                                                shown.track
                                            }))
                                            .child(text(
                                                painted,
                                                Size::Sm,
                                                if faint { shown.text_faint } else { shown.text },
                                            ))
                                            .child(text(
                                                if focused { "\u{258f}" } else { "" },
                                                Size::Sm,
                                                shown.amber,
                                            ))
                                            .on_click(cx.listener(
                                                move |this, _ev, _window, cx| {
                                                    if let Some(ob) = this.onboard.as_mut() {
                                                        ob.focus_text(target);
                                                    }
                                                    cx.notify();
                                                },
                                            )),
                                    ),
                            );
                        }
                        column = column.child(fields);
                    }

                    if ob.step() == opencrabs::tui::onboarding::OnboardingStep::BrainSetup {
                        // The step does work before it can move, so it says
                        // where that work stands instead of looking like a
                        // text field that does nothing on Enter.
                        let (mark, colour, note) = if ob.brain_generating() {
                            ("\u{25cb}", shown.amber, "writing your brain files")
                        } else if ob.brain_generated() {
                            ("\u{2713}", shown.green, "your brain files are written")
                        } else if let Some(why) = ob.brain_error() {
                            ("\u{2717}", shown.red, why)
                        } else {
                            (
                                "\u{25cb}",
                                shown.text_faint,
                                "enter writes them from the words above",
                            )
                        };
                        column = column.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .bg(rgb(shown.panel))
                                .child(text(mark, Size::Sm, colour))
                                .child(text(note.to_string(), Size::Sm, colour)),
                        );
                    }

                    if let Some(err) = ob.error() {
                        column = column.child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .bg(rgb(shown.panel))
                                .border_1()
                                .border_color(rgb(shown.red))
                                .child(text(err.to_string(), Size::Sm, shown.red)),
                        );
                    }

                    let primary_hint = if ob.is_channel_step() {
                        // Space is the key that flips a channel here, so the
                        // hint names it instead of "save".
                        "space \u{b7} toggle \u{b7} enter \u{b7} continue \u{b7} esc \u{b7} back"
                    } else if ob.step() == opencrabs::tui::onboarding::OnboardingStep::BrainSetup {
                        "enter \u{b7} write my brain files \u{b7} esc \u{b7} back"
                    } else if ob.wizard.quick_jump {
                        "enter \u{b7} save \u{b7} esc \u{b7} close"
                    } else {
                        "enter \u{b7} continue \u{b7} esc \u{b7} back"
                    };
                    let horizontal_hints =
                        matches!(picker_layout.footer, model_layout::FooterLayout::Row);
                    let mut footer = div().flex().w_full().gap_2().min_w_0();
                    footer = match picker_layout.footer {
                        model_layout::FooterLayout::Row => {
                            footer.flex_row().items_center().justify_between()
                        }
                        model_layout::FooterLayout::Column => footer.flex_col().items_start(),
                    };
                    let footer = footer
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(shown.text_muted))
                                .min_w_0()
                                .whitespace_normal()
                                .when(horizontal_hints, |d| d.flex_1())
                                .when(!horizontal_hints, |d| d.w_full())
                                .child(primary_hint),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(shown.text_muted))
                                .min_w_0()
                                .whitespace_normal()
                                .when(horizontal_hints, |d| d.flex_none())
                                .when(!horizontal_hints, |d| d.w_full())
                                .child("tab \u{b7} next field \u{b7} arrows \u{b7} walk"),
                        );
                    let footer = if let Some(model_footer) = model_footer {
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .w_full()
                            .min_w_0()
                            .child(model_footer)
                            .child(footer)
                    } else {
                        footer
                    };
                    (
                        div()
                            .flex()
                            .flex_row()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(rail)
                            .child(
                                div()
                                    .id("onboard-body")
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(column),
                            )
                            .into_any_element(),
                        footer.into_any_element(),
                    )
                }
            },
            // The window-level modal owns this surface. Keeping the old full
            // screen here as well duplicated its controls and element IDs.
            Screen::Models => (div().flex_1().into_any_element(), div().into_any_element()),
            Screen::Skills => {
                // The same list `/skills` opens in the terminal, from the same
                // loader. A skill is run by its row: Enter or a click submits
                // the prompt the slash would have submitted (#2466).
                let shown = palette::current();
                let last = self.skills_rows.len().saturating_sub(1);
                let current = self.skills_index.min(last);
                let mut list = div().flex().flex_col().gap_1();
                for (index, skill) in self.skills_rows.iter().enumerate() {
                    let selected = index == current;
                    let name = skill.slash_name.clone();
                    let description = skill.description.clone();
                    let tag = skills_view::source_label(skill);
                    let mut head = div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(text(name, Size::Sm, shown.text))
                        .child(text(tag, Size::Xs, shown.text_muted));
                    if skill.review_gate {
                        // The gate is visible before it runs: a skill that will
                        // stop for approval says so on its row.
                        head = head.child(text("review gate", Size::Xs, shown.amber));
                    }
                    let mut entry = div()
                        .id(SharedString::from(format!("skill-row-{index}")))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .rounded(px(6.0))
                        .child(head)
                        .child(text(description, Size::Xs, shown.text_muted));
                    if selected {
                        entry = entry.bg(rgb(shown.panel_hi));
                    }
                    list =
                        list.child(entry.on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.skills_index = index;
                            this.run_skill(index, cx);
                        })));
                }
                let scroller = div()
                    .id("skills-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list);
                let count = self.skills_rows.len();
                // The typed filter is shown, so a short list never looks like a
                // loader failure: the row says what is being narrowed by.
                let counted = if self.skills_query.is_empty() {
                    format!("{count} skill{}", if count == 1 { "" } else { "s" })
                } else {
                    format!(
                        "{count} of {} skills \u{b7} filter: {}",
                        self.skills_all.len(),
                        self.skills_query
                    )
                };
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(shown.text_muted))
                            .child(SharedString::from(counted)),
                    )
                    .child(div().text_xs().text_color(rgb(shown.text_muted)).child(
                        "enter \u{b7} run \u{b7} type \u{b7} filter \u{b7} esc \u{b7} back",
                    ));
                (scroller.into_any_element(), footer.into_any_element())
            }
        };

        // The input box and the live turn panel belong to the chat screen.
        // An editor parked under the usage ledger is the mixing the owner
        // called out, so usage draws the footer hint and nothing else.
        // The bar sits above the transcript, at the top of the chat (#2394).
        let mut topic_row: Option<(topic_tabs::Edge, AnyElement)> = None;
        // Topics live in the chat: the `...` button sits above the
        // conversation for a session that has them on (#2303).
        let is_topic = self
            .chat_session
            .is_some_and(|id| self.sessions.parents.contains_key(&id));
        // A split draws the strip inside each pane (#2687): one window row
        // above a wall of panes named the chat's topics over conversations
        // that had none, and sat outside the box it described.
        let split_draws_its_own = self
            .panes
            .as_ref()
            .is_some_and(|tree| tree.leaves().len() > 1);
        if self.screen == Screen::Chat
            && !split_draws_its_own
            && (self.open_chat_topics_on() || is_topic)
        {
            // The tabs (#2400): the family's parent, then its topics. The
            // leading button moves the whole row to another side (#2402).
            // The `...` menu is NOT here any more (#2480): it lives at the
            // conversation's top right, over the chat it acts on.
            let (tabs, pill) = self.topic_tabs(None, cx);
            let toggle_edge = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.cycle_topic_edge(cx);
            });
            let strip = topic_strip(
                "window",
                self.topic_edge,
                toggle_edge,
                tabs,
                self.chat_topics_expanded,
                pill,
            )
            // Where the row actually landed, recorded for the pick's scrim
            // (#3097). Measured from the strip's own children, because a side
            // row is centred beside the conversation and no constant knows
            // where that puts it. The value read this frame is the last
            // paint's: prepaint runs after the tree is built, and the row
            // does not move between frames.
            .on_children_prepainted({
                let slot = self.topic_strip_rect.clone();
                let edge = self.topic_edge;
                move |bounds, _window, _cx| {
                    // The union of every child, not the first and the last: a
                    // fold pill is shorter than the cells beside it, and a
                    // band cut to the last child's edge would dim the bottom
                    // of the row it exists to leave bright.
                    let mut start = f32::INFINITY;
                    let mut end = f32::NEG_INFINITY;
                    for child in &bounds {
                        let (near, far) = if edge.is_vertical() {
                            (
                                f32::from(child.origin.x),
                                f32::from(child.origin.x + child.size.width),
                            )
                        } else {
                            (
                                f32::from(child.origin.y),
                                f32::from(child.origin.y + child.size.height),
                            )
                        };
                        start = start.min(near);
                        end = end.max(far);
                    }
                    if start.is_finite() && end > start {
                        *slot.borrow_mut() = Some(topic_tabs::StripRect { edge, start, end });
                    }
                }
            });
            topic_row = Some((self.topic_edge, strip.into_any_element()));
        }
        // Where a split pick's scrim leaves the chat bright (#3097). The row
        // is the shortest way to the topic a split wants, and the scrim blocks
        // the mouse for everything behind it, so a row under the scrim cannot
        // answer the pick at all. No row means the chat dims whole.
        //
        // A split draws no window row (#2687), but every pane draws its own
        // strip on the top edge at one height, so a constant covers all of
        // them. The window's row is measured instead of derived: a side strip
        // is centred beside the conversation, so its distance from the rail
        // moves with the window's width and the text measure, and a constant
        // put the band in the wrong place for every edge but the top.
        let pick_hole = if split_draws_its_own {
            Some(topic_tabs::StripRect {
                edge: topic_tabs::Edge::Top,
                start: 0.0,
                end: topic_tabs::CELL_REMS * self.rem + 14.0,
            })
        } else if topic_row.is_some() {
            *self.topic_strip_rect.borrow()
        } else {
            None
        };
        // The dock owns the chrome, so the field must not paint a second card
        // inside it. A split pane renders the same editor bare and keeps its own
        // card, which is why this is a flag on the field and not a deleted style
        // (#2987). It is recomputed on every frame rather than latched: the
        // window's composer IS the focused pane's editor once the chat splits
        // (`dashboard_panes.rs`), so a sticky flag would strip the box off the
        // pane the moment it took the chat over.
        self.composer.editor.update(cx, |editor, _cx| {
            editor.docked = self.screen == Screen::Chat && !self.window_is_split();
        });
        let mut right = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .gap_3();
        // The unsplit chat's ground IS the shot (#3112): it runs edge to edge
        // and top to bottom, with no gutter around it and no corner of its own.
        // The owner's read was that the screen kept a band of empty background
        // above the first row and below the field, and a rounded box floating
        // in the middle of the pane. Every other screen keeps the window's
        // gutter, and so does a split: those panes are cards with a focus ring,
        // and a ring needs room around it.
        if self.screen != Screen::Chat || self.window_is_split() {
            // Less room on every side than the old 24px: the header lines up
            // with the sessions title, the input is not floating in empty
            // space above the window edge, and the sides drop to 16px so the
            // gutter stops reading as a band beside the rail (#3001).
            right = right.px_4().pt_3().pb_4();
        }
        // The chat screen draws no title row (#2596). The conversation's
        // name already prints in the footer, so a second copy across the top
        // only cost a band of the window to repeat it. Every other screen
        // keeps the chrome it has always had.
        if self.screen != Screen::Chat {
            right = right.child(chrome);
        }
        // The row takes the side it was saved on: above the transcript,
        // under the input, or beside the chat (#2402).
        let (mut topic_top, mut topic_bottom, mut topic_side) = (None, None, None);
        if let Some((edge, strip)) = topic_row {
            match edge {
                topic_tabs::Edge::Top => topic_top = Some(strip),
                topic_tabs::Edge::Bottom => topic_bottom = Some(strip),
                topic_tabs::Edge::Left | topic_tabs::Edge::Right => {
                    topic_side = Some((edge, strip))
                }
            }
        }
        // The topic bar is a row of the chat's own stack, so it takes the same
        // measure as the text under it (#2590). It is not attached here: the
        // chat arm places it inside the session surface, above the history, so
        // the whole session is one card (#3110). It is only ever built for the
        // chat screen, which is why no other arm needs it.
        // The conversation joins the column inside the arms below, not here:
        // the chat puts it inside the session surface (#3110), which has to own
        // the history, the strips and the composer together, and every other
        // screen takes it as the plain body it always was.
        right = match self.screen {
            Screen::Chat => {
                // The running checklist sits between the conversation and the
                // input, the same place the TUI's plan card holds. The key
                // hints go back UNDER the input where they were: the owner
                // called the move above the plan card out.
                let mut col = right;
                // The strips that ride between the conversation and the input
                // share one box that is allowed to shrink and clip (#2980).
                // Each of them is `flex_none` on its own, so together they
                // could ask for more height than the window has, and the stack
                // was painted past the bottom edge, under the composer and off
                // the screen. Inside this box they give up the room instead,
                // and the box cuts what does not fit. The composer and the key
                // hints stay outside it: neither may ever be clipped.
                let mut above = div()
                    .flex()
                    .flex_col()
                    .flex_shrink()
                    .min_h_0()
                    .overflow_hidden()
                    // The same 8 the session uses (#3114): the strips between
                    // the history and the field are steps of one stack, not a
                    // second, tighter grid parked inside it.
                    .gap_2();
                let mut above_used = false;
                // A split already is one card per pane, and each pane's leaf
                // box paints its own fill, its own radius and the clip that
                // keeps a scrolled row off the corner (#2612). A second wrapper
                // around the arrangement would round the whole thing and leave
                // the panes square inside it, so a split keeps the panes bare.
                if self.window_is_split() {
                    col = col.child(middle);
                } else {
                    // The session is one surface (#3110). The history, the plan
                    // card, the queue strip, the suggested options, the
                    // running-task row, the bottom topic row and the composer all
                    // live inside a single surface, so the lower half of the
                    // window reads as one session instead of a stack of unrelated
                    // strips parked under the chat. The cards inside it stay cards,
                    // because they are still separate objects, but they sit on this
                    // fill rather than beside it, and the composer stops floating
                    // over the gap with a surface of its own.
                    //
                    // The fill is the transcript's, so the two agree to the pixel
                    // where the inner surface ends and no seam shows between them.
                    // The clip stays here too: the transcript scrolls, so it cannot
                    // clip its own rows without losing the scroll, and one scrolled
                    // row painted past the surface's edge before the clip moved up.
                    //
                    // Built here, before the strips, because it is the ground they
                    // sit on: `middle` is its first child and everything that rides
                    // between the history and the field is appended in the order it
                    // stacks.
                    // The ground is the shot (#3112). It used to be a card the
                    // size of the conversation's measure, rounded like the
                    // bubbles and parked inside the window's gutter, which left
                    // a band of empty background above the first row and below
                    // the field. The owner's read: the background should take
                    // the full height and the full width with no radius at all,
                    // so the screen is one surface top to bottom. What keeps its
                    // measure is the CONTENT, not the ground: the transcript
                    // caps itself and centres, and every strip inside goes
                    // through `session_row`, which does the same.
                    let mut session = div()
                        .flex()
                        .flex_col()
                        // One rhythm for the whole lower half (#3114): 8, the
                        // step the transcript already gives between two of its
                        // own rows. It was 12 here on top of the transcript's
                        // own bottom 8, so the gap under the last card came to
                        // 20 against 8 between the cards above it, and the
                        // conversation closed on a band twice as wide as the
                        // conversation. The cards inside still keep their own
                        // shape; only the step between them changed.
                        .gap_2()
                        // Height, not `flex_1`: the card is a child of a row
                        // (see the wrapper below), and in a row `flex_1` grows
                        // the width, which would cancel the measure it is
                        // about to be given (#3110).
                        .h_full()
                        .min_h_0()
                        .w_full()
                        .bg(rgb(transcript::surface_fill(palette::current().bg)))
                        .overflow_hidden();
                    // The topic bar opens the card, then the history. Both are
                    // rows of the session, so the strip stops floating over a
                    // surface it does not belong to (#3110).
                    if let Some(bar) = topic_top {
                        session = session.child(central(bar, centred_chat, chat_measure_px));
                    }
                    session = session.child(middle);
                    // The hint, the offers and the live plan card are
                    // rows of the conversation now (#3139): they are built
                    // in `chat_trailing` and handed to the transcript, so
                    // they scroll with the turns instead of holding the
                    // bottom of the window. What stays here is what names
                    // something the window is doing right now, one line at
                    // a time: the queue and the detached command.
                    let waiting = self.queued_for_strip();
                    if !waiting.is_empty() {
                        above = above.child(session_row(
                            queue_strip(&waiting, self.composer.queue_expanded)
                                .id("queue-strip-chat")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this: &mut Self, _, _, cx| {
                                        if this.queued_for_strip().len() > 2 {
                                            this.composer.queue_expanded =
                                                !this.composer.queue_expanded;
                                            cx.notify();
                                        }
                                    }),
                                )
                                .into_any_element(),
                            centred_chat,
                            chat_measure_px,
                        ));
                        above_used = true;
                    }
                    // A detached command sits directly above the editor: the same
                    // place the TUI puts it (on the input box's top border), which
                    // is where the eye goes while waiting.
                    if let Some(label) = self.running_tasks_label()
                        && let Some(sid) = self.chat_session
                        && let Some(area) = self.task_area(sid, &label, cx)
                    {
                        // An open list is a card of the messages' size, so it
                        // takes the strips' half-rem gutter. A closed list is
                        // one bare line, like the hint strip and the footer, and
                        // it already pads its own: handing it the gutter as well
                        // put its right edge a second 8 in from the column every
                        // other row ends on, which is the misalignment the owner
                        // predicted for the detached-task row before he saw it
                        // (#3114). `central` only centres and caps; it adds no
                        // inset of its own.
                        above = above.child(if self.tasks_open.contains(&sid) {
                            session_row(area, centred_chat, chat_measure_px)
                        } else {
                            central(area, centred_chat, chat_measure_px)
                        });
                        above_used = true;
                    }
                    // The strips take their room from the conversation before
                    // the composer does: the box goes in above the input, and
                    // an empty box adds no row at all.
                    if above_used {
                        session = session.child(above);
                    }
                    // The unsplit chat uses the same composer swap as a split
                    // leaf (#2924). Without this arm the forward state changed
                    // but its destination picker was never rendered here.
                    // The bottom topic row sits immediately ABOVE the dock,
                    // in Telegram's forum arrangement. It is never between
                    // the composer and the footer, because the footer now
                    // lives inside the same card as the composer.
                    if let Some(strip) = topic_bottom {
                        session = session.child(central(strip, centred_chat, chat_measure_px));
                    }
                    let composer_replacement =
                        if self.forward_pick.is_some() && self.forward_leaf.is_none() {
                            Some(self.forward_picker(cx))
                        } else if self.msg_select.is_active() {
                            Some(self.select_bar(cx))
                        } else {
                            None
                        };
                    let composer: AnyElement = if let Some(view) = composer_replacement {
                        // The forward picker and the select bar stand in for the
                        // input, and they keep the dock so the key hints stay put
                        // instead of jumping to a separate row (#2924).
                        view
                    } else {
                        // The slash-command list sits over the input while a name is
                        // not being spelled, the terminal's own placement. The name
                        // prompt itself is the overlay below (#2398). The list floats
                        // over the composer, so it lines up with it: same measure,
                        // still the child directly above the editor, so the order it
                        // paints in does not change (#2590).
                        if self.topic_draft.is_none() {
                            let popup = self.composer.editor.read(cx).command_popup();
                            if let Some((names, selected)) = popup {
                                session = session.child(session_row(
                                    command_popup_view(&names, selected).into_any_element(),
                                    centred_chat,
                                    chat_measure_px,
                                ));
                            } else if let Some((items, selected)) =
                                self.composer.editor.read(cx).emoji_popup()
                            {
                                session = session.child(session_row(
                                    emoji_popup_view(&items, selected).into_any_element(),
                                    centred_chat,
                                    chat_measure_px,
                                ));
                            }
                        }
                        self.composer.editor.clone().into_any_element()
                    };
                    // The dock is the composer's column: the field, the
                    // hairline, and the key hints under it. The surface, border
                    // and radius live on the shell around it, so the field and
                    // its footer read as one box instead of two stacked flush,
                    // which would each round their own corners and leave a
                    // notch where they meet.
                    //
                    // The card is the messages' size again (#3114). It ran edge
                    // to edge after #3112, and the owner called that out on a
                    // single pane: a full-bleed band under measure-wide cards
                    // read as a slab from another window, and its words had to
                    // be pushed back onto the conversation's column by hand.
                    // The width comes from `session_row` below, the helper the
                    // plan card and the queue already use, so the card's edge
                    // lands where a message's edge lands and the rows inside it
                    // pad the card's own column (`card_inset`) instead of the
                    // pane's.
                    let dock = div()
                        .id("composer-dock")
                        .flex()
                        .flex_col()
                        .gap_0()
                        .child(composer)
                        .child(
                            div()
                                .w_full()
                                .h(px(1.))
                                .bg(rgb(palette::current().panel_hi)),
                        );
                    // The composer's shell is the field and the footer, and
                    // nothing else (#3102). The hint that used to ride on top
                    // of it as a strip is now a card of its own above the
                    // composer, where it can be read and dismissed instead of
                    // being welded to the thing you type into.
                    let shell = div()
                        .flex()
                        .flex_col()
                        .gap_0()
                        .w_full()
                        .min_w_0()
                        .bg(glass_fill(palette::current().panel))
                        .border_1()
                        .border_color(rgb(palette::current().panel_hi))
                        .rounded(px(transcript::MESSAGE_RADIUS))
                        .overflow_hidden();
                    // The composer closes the session as its last row, a card of
                    // the conversation's own size (#3110, #3114). `session_row`
                    // spends the half rem on each side, so the card's edge is
                    // where a message's edge is, and `pb_2` is that same step
                    // below: a rounded card flush on the window's bottom edge is
                    // a card whose bottom you cannot see.
                    //
                    // The footer is the last row of the card and it takes the
                    // card's column like the field above it (#3114).
                    session = session.child(div().w_full().min_w_0().pb_2().child(session_row(
                        shell.child(dock.child(footer)).into_any_element(),
                        centred_chat,
                        chat_measure_px,
                    )));
                    // The session card takes the conversation's measure and
                    // centres as a whole, so its own edges line up with the
                    // text instead of the card being full width with the text
                    // inset (#2586). `central` cannot do this job: its row is
                    // `flex_none`, so a card that has to fill the height would
                    // collapse inside it. This row grows, and the card is what
                    // it centres.
                    col = col.child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .justify_center()
                            .child(session),
                    );
                }
                col
            }
            // Every other screen is the body and then its footer, unchanged:
            // only the chat has a session surface to build (#3110).
            _ => right.child(middle).child(footer),
        };

        let mut root = div()
            .flex()
            .flex_row()
            .size_full()
            // Every child, including the transcript and every overlay, gets
            // the same opacity. The native window accepts alpha above (#2922).
            .opacity(gui_theme::window_opacity(self.window_transparency))
            .bg(rgb(palette::current().bg))
            .text_color(rgb(palette::current().text))
            // One family for the window (#2786): `font_family` sets the family
            // of this element and its children, so the composer, the
            // transcript and every bare label inherit the pick instead of each
            // one naming a face. The transcript's own `TextStyle`s name it
            // explicitly, because `TextStyle::default()` fills the field with
            // the platform font and would pin it.
            .font_family(fonts::current_ui())
            .track_focus(&self.focus)
            // Capture exits even when a focused editor would consume the key
            // before the dashboard's bubble handler sees it (#2964).
            .capture_key_down(cx.listener(|this, _ev: &KeyDownEvent, _window, cx| {
                this.cancel_middle_scroll(cx);
            }))
            .on_key_down(cx.listener(Self::on_key))
            // Capture sees the second middle-click before the marker's
            // full-window hitbox, and every other press exits too (#2964).
            .capture_any_mouse_down(cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                this.cancel_middle_scroll(cx);
            }))
            // A context menu closes when its row is left alone: any left
            // press in the window dismisses it, the rail rows do the same
            // in their own press listener first (#2354).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                    // A press anywhere is a press on the conversation unless a
                    // key put the rail in front (#2798). Arming from the
                    // pointer's x made every uncommitted press in the column
                    // take the keyboard, so the composer needed an Esc before
                    // it would type again.
                    this.rail_hands_back();
                    // A press anywhere but the card is a press on the
                    // conversation, so the offer's ring steps aside and the
                    // keyboard goes back to wherever the press landed (#3101).
                    if this.options_focus.take().is_some() {
                        cx.notify();
                    }
                    if this.sessions.rail_menu.take().is_some() {
                        cx.notify();
                    }
                    if this.topic_menu.take().is_some() {
                        cx.notify();
                    }
                    // A message card is pinned to a window point now, so it is
                    // not the transcript's to dismiss any more: every card
                    // closes on the same press, wherever it landed (#2690).
                    if this.msg_menu.take().is_some() {
                        this.msg_menu_row = None;
                        this.msg_menu_leaf = None;
                        cx.notify();
                    }
                    this.close_composer_menus(cx);
                }),
            )
            // While the marker owns the hit test, the transcript never sees
            // the wheel. The root still does, so the wheel exits cleanly.
            .on_scroll_wheel(cx.listener(|this, _ev: &ScrollWheelEvent, _window, cx| {
                this.cancel_middle_scroll(cx);
            }))
            // The drag lives on the root so it survives the pointer leaving
            // the handle: a 4px column cannot track a fast drag on its own.
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                this.move_middle_scroll(ev.position, cx);
                if this.dragging_rail
                    && let Some((from_x, from_w)) = this.drag_from
                {
                    let dx = f32::from(ev.position.x) - from_x;
                    let next = (from_w + dx) / this.rem;
                    if next < RAIL_MIN_REM {
                        // Dragged past the floor: close it, the same gesture
                        // the owner asked for instead of leaving a sliver. The
                        // close goes through the one transition, so a hidden
                        // filter, an open chip panel and the keyboard focus do
                        // not outlive a rail that is no longer drawn (#2845).
                        // A drag and ctrl+s now close the same way.
                        this.set_rail_open(false, cx);
                        this.dragging_rail = false;
                        this.drag_from = None;
                    } else {
                        this.rail_width = next.min(RAIL_MAX_REM);
                    }
                    cx.notify();
                }
                // The divider drag, mapped onto the pane span recorded at paint
                // time. The floor lives in split::share_after_drag, so a fast
                // drag narrows a pane but never shuts it (#2476).
                if let Some(drag) = this.split_drag.clone() {
                    let area = this.split_areas.borrow().get(&drag.path).copied();
                    if let Some([x0, y0, x1, y1]) = area {
                        let span = if drag.stacked { y1 - y0 } else { x1 - x0 };
                        let now = if drag.stacked {
                            f32::from(ev.position.y)
                        } else {
                            f32::from(ev.position.x)
                        };
                        let travelled = now - drag.last;
                        if travelled != 0.0 {
                            if let Some(tree) = this.panes.as_mut() {
                                tree.drag_divider(&drag.path, drag.divider, travelled, span);
                            }
                            if let Some(active) = this.split_drag.as_mut() {
                                active.last = now;
                            }
                            cx.notify();
                        }
                    }
                }
                // A rail row slid left far enough: open its menu. The
                // threshold keeps a wobbling click from revealing anything.
                if let Some((id, from_x)) = this.rail_swipe
                    && sessions::slide_reveals(from_x, f32::from(ev.position.x))
                    && this.sessions.rail_menu != Some(id)
                {
                    this.sessions.rail_menu = Some(id);
                    this.rail_menu_at = Some(ev.position);
                    this.rail_slide_opened = Some(id);
                    cx.notify();
                }
            }))
            // Light the target while a Finder drag is over the window. The
            // moves are what carry the flag; the drop consumes it.
            .on_drag_move::<ExternalPaths>(cx.listener(
                |this, _ev: &DragMoveEvent<ExternalPaths>, _window, cx| {
                    this.drag_seen = Some(std::time::Instant::now());
                    cx.notify();
                },
            ))
            // The drop itself: every path that is a real file becomes a chip
            // in the input, through the same attach path the paperclip uses.
            .on_drop::<ExternalPaths>(cx.listener(|this, ev: &ExternalPaths, window, cx| {
                let paths = ev.paths().to_vec();
                this.drag_seen = None;
                // A dropped file belongs to the composer the keyboard owns,
                // which in a split is the focused pane's own field (#2610).
                let composer = this.composer_entity(cx);
                let added = composer.update(cx, |editor, cx| {
                    let added = editor.attach_paths(paths);
                    if added > 0 {
                        cx.notify();
                        // A drop is a request to type, so the caret goes
                        // to the input rather than leaving the window with
                        // no focused element.
                        editor.focus_handle.focus(window);
                    }
                    added
                });
                if added > 0 {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, _window, _cx| {
                    this.dragging_rail = false;
                    this.drag_from = None;
                    this.rail_swipe = None;
                    // A pane drag ends here whether or not it landed anywhere
                    // (#2683): the band belongs to the drag, so it goes with it.
                    this.pane_drop = None;
                    // A divider dragged to a new size is a layout the user
                    // chose, so the release is when it gets written (#2675).
                    if this.split_drag.take().is_some() {
                        this.persist_layout();
                    }
                }),
            )
            // A release ends the slide even when the element under the pointer
            // swallows the bubble. A topic chip, the "+N" pill and the collapse
            // pill all stop propagation in their click listeners, so the
            // handler above never ran for them: the armed swipe outlived the
            // click and the next pointer move tripped `slide_reveals`, opening
            // the card's menu on a plain left click (#2911). Capture runs root
            // first, so no child can swallow this one.
            .capture_any_mouse_up(cx.listener(|this, _ev: &MouseUpEvent, _window, _cx| {
                this.rail_swipe = None;
            }));
        if self.rail_open {
            let width = self.rail_width;
            let grab = cx.listener(move |this, _ev: &MouseDownEvent, window, _cx| {
                this.dragging_rail = true;
                this.drag_from = Some((f32::from(window.mouse_position().x), width * this.rem));
            });
            root = root.child(rail).child(splitter(grab));
        } else if self.screen != Screen::Onboard {
            root = root.child(rail);
        }
        // The panes go in before every overlay. gpui paints children in the
        // order they are added, so a pane added after an overlay covers it:
        // the key reference then drew under the chat and under the rail.
        // A side strip rides the conversation's measure (#2948). The top and
        // bottom strips are wrapped in `central` (#2590), but a side strip is a
        // root sibling of the pane column, so it never got that wrapper: it sat
        // against the window edge while the text was centred at the measure,
        // and the whole centring slack opened between the two. The strip and
        // the column are centred as one block instead, so the topics sit beside
        // the conversation they belong to and the slack falls outside both.
        match topic_side {
            Some((edge, strip)) => {
                // The column pads itself with `px_4` on each side, so its own
                // width is the measure plus 2rem. Capping it there puts its
                // children back on the axis they always centred on.
                let pane_px = chat_measure_px + 2.0 * self.rem;
                let pane = div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .when(centred_chat, |d| d.max_w(px(pane_px)))
                    .child(right);
                let mut row = div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .justify_center();
                row = if edge == topic_tabs::Edge::Left {
                    row.child(strip).child(pane)
                } else {
                    row.child(pane).child(strip)
                };
                root = root.child(row);
            }
            None => root = root.child(right),
        }
        // One dialog for every delete path, painted above both panes. The
        // conversation is only named so the user knows what they are about
        // to lose; nothing is erased until the confirm button is clicked.
        if let Some(id) = self.confirm_delete {
            let title = self
                .sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| "(untitled)".to_string());
            let confirm = cx.listener(move |this, _ev, _window, cx| {
                this.confirm_delete = None;
                this.refocus_screen();
                this.delete_session(id, cx);
            });
            let cancel = cx.listener(|this, _ev, _window, cx| {
                this.confirm_delete = None;
                this.refocus_screen();
                cx.notify();
            });
            root = root.child(confirm_delete_overlay(title, confirm, cancel));
        }
        // Turning topics off folds the family into the conversation (#2863).
        // Same shape as the delete dialog: the menu arms it, the dialog's own
        // button is what writes, and the topic count is named so the fold is
        // never a surprise.
        if let Some(id) = self.confirm_disable_topics {
            let title = self
                .sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| "(untitled)".to_string());
            // Live topics only: `parents` already drops archived rows, so
            // this is the number the fold will actually move.
            let topics = self
                .sessions
                .parents
                .values()
                .filter(|parent| **parent == id)
                .count();
            let confirm = cx.listener(move |this, _ev, _window, cx| {
                this.confirm_disable_topics = None;
                this.refocus_screen();
                this.disable_topics(id, cx);
            });
            let cancel = cx.listener(|this, _ev, _window, cx| {
                this.confirm_disable_topics = None;
                this.refocus_screen();
                cx.notify();
            });
            root = root.child(confirm_disable_topics_overlay(
                title, topics, confirm, cancel,
            ));
        }
        // The Group topics picker: a multi select card painted above both
        // panes. Rows toggle on click, the keyboard cursor and Space mirror
        // the same selection, and only the group button writes anything.
        if self.group_target.is_some() {
            let parent_title = self
                .group_target
                .and_then(|id| self.sessions.items.iter().find(|s| s.id == id))
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| "(untitled)".to_string());
            // The rows the field lets through (#2864). Built fresh each
            // render so a keystroke narrows the list without touching the
            // selection underneath it.
            let visible = self.group_visible();
            let mut list = div().id("group-pick-list").flex().flex_col().gap_0p5();
            for (idx, (sid, title, date)) in visible.iter().enumerate() {
                let sid = *sid;
                let checked = self.group_selected.contains(&sid);
                let cursored = idx == self.group_cursor;
                // A row that still carries topics is offered, and says how
                // many: grouping it folds them into it first, which is the
                // only way a family becomes a topic at depth one (#2864).
                let topic_count = match self
                    .sessions
                    .parents
                    .values()
                    .filter(|parent| **parent == sid)
                    .count()
                {
                    0 => None,
                    n => Some(n),
                };
                let toggle = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    this.group_cursor = idx;
                    if !this.group_selected.insert(sid) {
                        this.group_selected.remove(&sid);
                    }
                    // A new selection re-asks the fold question (#2864).
                    this.group_warned = false;
                    cx.notify();
                });
                let mark = div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(rems(1.0))
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(rgb(palette::current().text_muted))
                    .when(checked, |d| {
                        d.bg(rgb(palette::current().accent))
                            .border_color(rgb(palette::current().accent))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette::current().panel))
                                    .child(SharedString::from("\u{2713}")),
                            )
                    });
                let row = div()
                    .id(SharedString::from(format!("group-pick-row-{sid}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .when(cursored, |d| d.bg(rgb(palette::current().selection)))
                    .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                    .on_click(toggle)
                    .child(mark)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(rgb(palette::current().text))
                            .truncate()
                            .child(SharedString::from(title.clone())),
                    )
                    .when_some(topic_count, |d, count| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette::current().text_muted))
                                .child(SharedString::from(if count == 1 {
                                    "1 topic".to_string()
                                } else {
                                    format!("{count} topics")
                                })),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child(SharedString::from(date.clone())),
                    );
                list = list.child(row);
            }
            // A blank list is never left unexplained (#2864): an empty field
            // with nothing to offer and a search that matched nothing read
            // the same before, and neither said which one it was.
            if visible.is_empty() {
                list = list.child(
                    div()
                        .id("group-pick-empty")
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(if self.group_filter.is_empty() {
                            "no session is available to group".to_string()
                        } else {
                            "no session matches that".to_string()
                        })),
                );
            }
            let scroller = div()
                .id("group-pick-body")
                .max_h(rems(20.0))
                .overflow_y_scroll()
                .child(list);
            let cancel = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.close_group_topics();
                cx.notify();
            });
            let confirm = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.submit_group_topics(cx);
            });
            let count = self.group_selected.len();
            let card = div()
                .id("group-pick")
                .flex()
                .flex_col()
                .gap_3()
                .w(rems(28.0))
                .p_4()
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(rgb(palette::current().panel_hi))
                .bg(glass_fill(palette::current().panel))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette::current().text))
                        .child(SharedString::from(format!(
                            "Group topics under {parent_title}"
                        ))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(
                            "Pick sessions to attach as topics. Type to filter, Space \
                             toggles, Enter groups.",
                        )),
                )
                // The field (#2864). It shows what was typed instead of
                // holding a caret: the dialog owns the keyboard, so the
                // characters land here from the key handler, not from a
                // focusable text input.
                .child(
                    div()
                        .id("group-pick-filter")
                        .px_2()
                        .py_1()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(palette::current().panel_hi))
                        .bg(rgb(palette::current().panel_hi))
                        .text_sm()
                        .text_color(if self.group_filter.is_empty() {
                            rgb(palette::current().text_faint)
                        } else {
                            rgb(palette::current().text)
                        })
                        .child(SharedString::from(if self.group_filter.is_empty() {
                            "type to filter...".to_string()
                        } else {
                            self.group_filter.clone()
                        })),
                )
                // A selected family keeps its own topics, and grouping it
                // folds them in first (#2864). The first attempt names that
                // instead of writing; Enter again goes ahead.
                .when(self.group_warned, |d| {
                    d.child(
                        div()
                            .id("group-pick-warning")
                            .text_xs()
                            .text_color(rgb(palette::current().amber))
                            .child(SharedString::from(
                                "The selected sessions have topics of their \
                                 own. Grouping folds those into them first, \
                                 and that cannot be undone. Enter again to go \
                                 ahead, or Esc to leave them as they are.",
                            )),
                    )
                })
                .when_some(self.group_error.clone(), |d, e| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().red))
                            .child(SharedString::from(e)),
                    )
                })
                .child(scroller)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .id("group-pick-cancel")
                                .px_3()
                                .py_1()
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgb(palette::current().text_normal))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                                .child(SharedString::from("cancel"))
                                .on_click(cancel),
                        )
                        .child(
                            div()
                                .id("group-pick-yes")
                                .px_3()
                                .py_1()
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgb(palette::current().accent))
                                .cursor_pointer()
                                .border_1()
                                .border_color(rgb(palette::current().accent))
                                .hover(|d| d.bg(rgb(palette::current().selection)))
                                .child(SharedString::from(if self.group_busy {
                                    "grouping...".to_string()
                                } else if self.group_warned {
                                    format!("fold and group ({count})")
                                } else {
                                    format!("group ({count})")
                                }))
                                .on_click(confirm),
                        ),
                );
            let scrim = div()
                .id("group-pick-scrim")
                .occlude()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .bg(rgba(0x0a0a0acc))
                .child(card);
            root = root.child(scrim);
        }
        // A pick that named a topic says why no prompt opened (#2605). The
        // card is the delete dialog's shape with one answer, so a stopped
        // pick can never be mistaken for a progress notice.
        if let Some(title) = self.topic_warning.clone() {
            let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.topic_warning = None;
                this.refocus_screen();
                cx.notify();
            });
            let body = format!(
                "\"{title}\" is a topic, not a conversation. A topic cannot hold topics, so pick the conversation it sits under, then name the new topic."
            );
            root = root.child(warning_overlay(&body, dismiss));
        }
        // A pick that named a conversation already open in a pane says so
        // instead of moving it (#2819). One card at a time, so the topic
        // warning keeps the slot when both are somehow armed.
        if self.topic_warning.is_none()
            && let Some(title) = self.pane_warning.clone()
        {
            let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.pane_warning = None;
                this.refocus_screen();
                cx.notify();
            });
            let body = format!(
                "\"{title}\" is already open in another pane. Close that pane first, or pick a different conversation."
            );
            root = root.child(warning_overlay(&body, dismiss));
        }
        // A preview sits over both panes, and over everything else on the
        // window: it is where the user went to look at something, and the
        // transcript behind it must not take a click meant for it (#2395).
        if let Some(open) = self.preview.clone() {
            let close = cx.listener(|this, _ev, _window, cx| {
                // The other half of the pair: an open with no close after it
                // stayed on screen, an open and a close in the same gesture is
                // the overlay dismissing itself (#2395).
                logging::info("preview: closed");
                this.close_preview();
                this.refocus_screen();
                cx.notify();
            });
            let window_size = window.viewport_size();
            let viewport = (f32::from(window_size.width), f32::from(window_size.height));
            let document_height = (viewport.1 - 300.0).clamp(120.0, 620.0);
            let body = self.preview_body(&open, document_height, cx);
            let detail =
                preview::detail_line(&open, self.preview_zoom, self.preview_video.as_ref());
            let document_open = self.preview_document.is_some();
            root = root.child(preview::overlay(
                &open,
                detail,
                body,
                document_open,
                viewport,
                close,
            ));
        }
        // The key reference, above the panes and above the delete dialog: it
        // is read-only, so nothing behind it can be armed while it is up.
        if self.help_open {
            let view = HelpView {
                version: opencrabs::VERSION,
                update: self.update_available.clone(),
                filter: self.help_filter.clone(),
                scroll: self.help_scroll.clone(),
                narrow: f32::from(window.viewport_size().width) < 760.0,
                commands: &self.help_commands,
            };
            root = root.child(help_screen(&view));
        }
        // One task's full output, over everything in the chat: it is where
        // the reader went to look, so nothing behind it takes a click.
        if self.task_detail.is_some() {
            let size = window.viewport_size();
            let viewport = (f32::from(size.width), f32::from(size.height));
            if let Some(modal) = self.task_detail_overlay(viewport, cx) {
                root = root.child(modal);
            }
        }
        // The topic info panel sits above the panes, like the key reference (#2304).
        if let Some(info) = self.topic_info.clone() {
            let close = cx.listener(|this, _ev, _window, cx| {
                this.topic_info = None;
                cx.notify();
            });
            root = root.child(topic_info_overlay(&info, close));
        }
        // While a split waits for its session the chat dims, and a card says what
        // to do. The sidebar is left bright so its rows can be picked (#2438),
        // and so is the topic row, on whichever edge it sits, for the same
        // reason (#3097).
        if let Some(layout) = self.split_pick {
            let rail_px = px(self.visible_rail_width() * self.rem);
            root = root.child(split_pick_overlay(layout, rail_px, pick_hole));
        }
        // The name prompt takes the whole window: the scrim darkens everything
        // else, and a click outside the panel cancels, as Esc does (#2398).
        if let Some(draft) = self.topic_draft.clone() {
            // The heading follows what the prompt will rename: a child session
            // is a topic, a root one is a conversation. Reading it from the
            // pending rename alone labelled every conversation rename as a
            // topic edit (#2668).
            let label = match self.topic_rename {
                Some(id) if self.sessions.parents.contains_key(&id) => "Rename topic",
                Some(_) => "Rename conversation",
                None => "New topic",
            };
            let cancel = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.topic_draft = None;
                this.topic_rename = None;
                cx.notify();
            });
            let icon = self
                .topic_draft_icon
                .clone()
                .unwrap_or_else(|| DEFAULT_TOPIC_ICON.to_string());
            // The pack ships in two families, so the prompt lists them one
            // after another instead of one 98-cell wall (#2824, #2897). A pick
            // is a picture, not a bullet (#2768): at the old 1.5rem the badges
            // were specks and picking one was guesswork. They render at 4.0rem
            // (#2789), and the 256x256 sources stay crisp at 2x.
            let mut families: Vec<(&'static str, Vec<Stateful<Div>>)> = topic_icons::TOPIC_FAMILIES
                .iter()
                .map(|&(caption, prefix)| {
                    let cells: Vec<Stateful<Div>> = topic_icons::family_icons(prefix)
                        .map(|(name, bytes)| {
                            let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                                this.topic_draft_icon = Some(topic_icons::store_name(name));
                                cx.notify();
                            });
                            div()
                                .id(SharedString::from(format!("topic-icon-{name}")))
                                .p_1()
                                .rounded(px(6.0))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                                .on_click(pick)
                                .child(topic_icons::picture(bytes, 4.0))
                        })
                        .collect();
                    (caption, cells)
                })
                .collect();
            // One extra cell opens the platform file picker for a custom
            // image, Telegram style (#2790). The tile matches the embedded
            // picks' geometry so the grid keeps its rhythm, and it keeps its
            // own caption so the + never trails a 50-icon family unseen
            // (#2824).
            let add_image = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.pick_icon_image(cx);
            });
            families.push((
                "Your image",
                vec![
                    div()
                        .id("topic-icon-add-image")
                        .p_1()
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(add_image)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(rems(4.0))
                                .rounded(px(8.0))
                                .border_1()
                                .border_color(rgb(palette::current().panel_hi))
                                .text_color(rgb(palette::current().text_muted))
                                .text_size(rems(1.6))
                                .child("+"),
                        ),
                ],
            ));
            // The badge previews a custom pick from the same cache the rail
            // and tabs draw from (#2790).
            let custom_picture = self.custom_icon_picture(&icon, 3.2);
            // The session image itself opens the picker, the same handler
            // the add-image cell uses (#2790).
            let pick_image = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.pick_icon_image(cx);
            });
            root = root.child(topic_prompt_overlay(
                label,
                &draft,
                &icon,
                families,
                custom_picture,
                pick_image,
                cancel,
            ));
        }
        // The update dialog (#2406): two stages, and /evolve runs only after the second yes.
        if let Some(second) = self.update_prompt {
            let version = self.update_available.clone().unwrap_or_default();
            let confirm = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                if second {
                    this.update_prompt = None;
                    this.run_command(
                        commands::Outcome::Run(commands::GuiAction::Evolve),
                        this.chat_session,
                        cx,
                    );
                } else {
                    this.update_prompt = Some(true);
                }
                cx.notify();
            });
            let cancel = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.update_prompt = None;
                cx.notify();
            });
            root = root.child(update_prompt_overlay(&version, second, confirm, cancel));
        }
        // An open Mission Control record sits above the screen it belongs to, the
        // way the preview does: Esc or a click on the scrim closes it (#2426).
        if self.screen == Screen::MissionControl && self.mc_nav.detail {
            let close = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.mc_nav.detail = false;
                cx.notify();
            });
            root = root.child(mission_control_detail(
                self.mc.as_ref(),
                &self.mc_nav,
                close,
            ));
        }
        // The log reader covers the screen while it is open, like the
        // terminal's, so no key lands on a panel nobody can see.
        if self.screen == Screen::MissionControl && self.mc_logs.is_some() {
            let close = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.mc_logs = None;
                cx.notify();
            });
            if let Some(viewer) = self.mc_logs.as_mut() {
                root = root.child(mission_control_logs(viewer, close));
            }
        }
        // The drop target: a Finder drag tints the whole window so the target
        // is never guessed. It paints above both panes and vanishes the
        // moment the drag ends.
        if self.file_drag_active() {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .rounded(px(CARD_RADIUS))
                    .border_2()
                    .border_color(rgb(palette::current().accent))
                    .bg(rgba(0x101010dd))
                    .child(div().text_xl().child("📎"))
                    .child(div().text_lg().child("Drop to attach"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child("files and images ride with your next message"),
                    ),
            );
        }
        // The Seeker card paints above every other surface, because it is
        // the one thing that takes every key while it is up (#2458). One
        // handler per printed row, built here where the context lives.
        if let Some(state) = self.finder.as_ref() {
            let (start, printed) = command_palette::window_range(state);
            let handlers: Vec<command_palette::PickHandler> = (0..printed)
                .map(|ix| -> command_palette::PickHandler {
                    Box::new(
                        cx.listener(move |this: &mut Self, _ev: &ClickEvent, _window, cx| {
                            this.pick_finder_row(start + ix, cx);
                        }),
                    )
                })
                .collect();
            let entity = cx.entity().downgrade();
            let chip: command_palette::ChipHandler =
                Box::new(move |want: Option<PaletteKind>, cx: &mut App| {
                    let _ = entity.update(cx, |this, cx| this.set_finder_kind_filter(want, cx));
                });
            let wheel = cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                let lines = match ev.delta {
                    ScrollDelta::Lines(lines) => lines.y,
                    // A pixel delta is a trackpad: a hundredth of a notch
                    // per pixel keeps a flick from jumping whole pages.
                    ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / 100.0,
                };
                let rows = if lines > 0.0 {
                    lines.ceil() as i32
                } else if lines < 0.0 {
                    lines.floor() as i32
                } else {
                    0
                };
                this.finder_scroll(rows, cx);
            });
            let idle_revealed = state.idle_ready(std::time::Instant::now());
            root = root.child(command_palette::overlay(
                command_palette::FinderOverlayState {
                    state,
                    idle_revealed,
                    caret_on: self.finder_caret,
                    view: cx.entity().downgrade(),
                    paint: self.finder_paint.clone(),
                    voice: self.seeker_voice_view(cx),
                },
                handlers,
                chip,
                wheel,
                find::channel_hint(state.query(), &self.finder_channels),
                self.seeker_voice_error.clone(),
            ));
        }
        // The window controls are the frame's, not a screen's (#2813): drawn
        // over the top-right corner so no screen's layout owns them, and before
        // the menus and modals below so a scrim that owns the pointer also owns
        // the buttons. Where the platform draws a caption, the cluster is not
        // drawn at all: a second set of buttons in the same corner is exactly
        // the bug #3083 reports, and the app's set was the one that went dead.
        let decorations = effective_decorations(
            window.window_decorations(),
            platform_draws_caption(TITLEBAR_APPEARS_TRANSPARENT),
        );
        // Whichever platform draws the cluster also runs it. Only Windows has a
        // path where the platform runs the buttons instead, and that is the
        // path where the cluster is not drawn (see `platform_draws_caption`).
        let on_control: Option<window_controls::Handler> =
            Some(Rc::new(|control, _ev, window, cx| match control {
                window_controls::Control::Minimize => window.minimize_window(),
                window_controls::Control::Maximize => window.zoom_window(),
                window_controls::Control::Close => cx.quit(),
            }));
        if let Some(cluster) = window_controls::cluster(decorations, on_control) {
            root = root.child(cluster);
        }
        // Both context menus live here, at the window root, never inside the
        // row or column that asked for them (#2690). Their cards are placed at
        // the press point in window coordinates, and a parent's own frame would
        // translate those coordinates to its corner. Deferred, so they paint
        // over the cards and the conversation they open above.
        if let Some(menu) = self.chat_menu(cx) {
            root = root.child(gpui::deferred(menu).with_priority(2));
        }
        if let Some(id) = self.sessions.rail_menu
            && let Some(menu) = self.rail_menu_popover_for(id, cx)
        {
            root = root.child(gpui::deferred(menu).with_priority(2));
        }
        // The message menu is a window-level card too (#2690): it used to be a
        // child of its row, so a long message pushed it far from the press and
        // a split pane's frame moved it again.
        if self.msg_menu.is_some()
            && let Some(at) = self.msg_menu_at
        {
            let items = self.msg_menu_items(cx);
            let dismiss = cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                this.msg_menu_close(cx);
            });
            root = root.child(
                gpui::deferred(transcript::message_menu_popover(&items, at, dismiss))
                    .with_priority(2),
            );
        }
        // The crab pack is a window-level card for the same reason (#2825): it
        // opens at the point the row's menu did, and a row's own frame would
        // move that point.
        if self.react_menu.is_some()
            && let Some(at) = self.react_menu_at
        {
            let cells = self.react_cells(cx);
            let dismiss = cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                this.msg_menu_close(cx);
            });
            root = root.child(
                gpui::deferred(transcript::react_picker_popover(&cells, at, dismiss))
                    .with_priority(2),
            );
        }
        // These window-level cards share Theme's scrim and pointer ownership.
        // Their screen state remains the compatibility keyboard owner.
        if self.screen == Screen::Models {
            root = root.child(self.models_modal(cx));
        }
        if self.screen == Screen::Projects {
            root = root.child(self.projects_modal(cx));
        }
        // The theme modal is attached last, so it paints over every other
        // surface: while it is open it is what the user is looking at, and the
        // scrim is what the pointer meets (#2619).
        // The built-in editor, over the panes and the other overlays but
        // under the settings and theme modals: it is where the user went to
        // write, so a keystroke or a click meant for the buffer must not
        // reach the panes behind it (#3163). Only the focused conversation's
        // dialog draws; the other conversations' editors wait their turn,
        // exactly like every other session state in this window.
        if let Some(id) = self.editor_focus
            && let Some(dialog) = self.editor_dialogs.get_mut(&id)
        {
            // The window's answers to the editor's gestures travel as one
            // value, so the draw call stays three arguments deep instead of
            // eight (#3163).
            let handlers = editor_dialog::EditorHandlers {
                dismiss: Box::new(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                    this.close_editor(cx);
                })),
                toggle_mode: Box::new(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                    this.toggle_editor_mode(cx);
                })),
                mouse_down: Box::new(cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    this.editor_mouse_down(ev, cx);
                })),
                mouse_move: Box::new(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                    this.editor_mouse_move(ev, cx);
                })),
                mouse_up: Box::new(cx.listener(|this, ev: &MouseUpEvent, _window, cx| {
                    this.editor_mouse_up(ev, cx);
                })),
                wheel: Box::new(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                    this.editor_wheel(ev, cx);
                })),
            };
            // The window is only read here: the card is measured from the
            // viewport size it carries (#3163).
            root = root.child(editor_dialog::overlay(dialog, window, handlers));
        }
        if self.themes_open {
            root = root.child(self.themes_modal(cx));
        }
        // The settings modal follows the same rule (#2878).
        if self.settings_open {
            root = root.child(self.settings_modal(cx));
        }
        // The auto-scroll marker is the final surface while active, and it
        // owns the pointer: gpui gates every mouse listener on the element
        // being hovered, and this overlay blocks the hit test for everything
        // beneath it, so a listener left on the root never runs while the
        // marker is up (#2964). The gesture's three listeners ride the one
        // element the pointer is over, and stopping propagation in the
        // capture phase keeps the press from reaching the rows underneath.
        if let Some(gesture) = self.middle_scroll.as_ref() {
            root = root.child(
                middle_scroll::overlay(gesture)
                    .capture_any_mouse_down(cx.listener(
                        |this, _ev: &MouseDownEvent, _window, cx| {
                            this.cancel_middle_scroll(cx);
                            cx.stop_propagation();
                        },
                    ))
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                        this.move_middle_scroll(ev.position, cx);
                    }))
                    .on_scroll_wheel(cx.listener(|this, _ev: &ScrollWheelEvent, _window, cx| {
                        this.cancel_middle_scroll(cx);
                        cx.stop_propagation();
                    })),
            );
        }
        // A stationary pointer still scrolls. Entering the dead zone lets the
        // one outstanding animation callback drain out.
        self.schedule_middle_scroll_frame(window, cx);
        root
    }
}

/// The facts the hint row is decided from, read off state the app already
/// keeps for its own gauge, footer and update badge.
///
/// Nothing here is bookkeeping kept only for the hint: `ctx_tokens` and
/// `ctx_max` are the numbers the context gauge prints, `busy` is what the
/// composer shows while a turn runs, and `update_available` is the version the
/// badge already names.
impl Dashboard {
    pub(crate) fn hint_state(&self) -> HintState {
        let mut state = HintState {
            processing: self.turn.busy,
            context_used: u64::from(self.turn.ctx_tokens.unwrap_or(0)),
            context_max: u64::from(self.ctx_max),
            clear_base: 0,
            update_version: self.update_available.clone(),
            turns: self.chat_state.transcript.turns.len(),
            // An offer waiting above the composer is the most actionable thing
            // on screen, so the row says how to reach it (#3101). Read from the
            // session on screen, the same one the card is drawn for.
            offers: self
                .chat_session
                .map(|sid| agent_bridge::options_snapshot(sid).len())
                .unwrap_or(0),
            // The rail holding the keyboard changes what a typed character
            // does, so the row says so instead of naming commands the field
            // will never receive.
            rail_focused: self.rail_open && self.rail_focused,
            // A split waiting for its second session reads the same keys for a
            // different promise, and one word apart is the difference between a
            // shortcut and a surprise (#3102).
            rail_split: self.split_target.is_some(),
            // Filled from the dismissal store by the window itself; the engine
            // stays free of the database.
            dismissed: self.hint_dismissed.clone(),
            // The same predicate the field's cold line uses, so the tip and the
            // prompt never disagree about whether there is an engine to record
            // with.
            voice: crate::composer_view::voice_ready(),
        };
        // What a `/clear` leaves behind is the system prompt and the tool
        // schemas. Counting it tokenises every tool schema, so it is measured
        // only when the context rung is live: this runs on every frame, and
        // the other rungs never print the number.
        if state.context_percent() >= CONTEXT_TIGHT {
            state.clear_base = agent_bridge::bridge()
                .map(|b| u64::from(b.service.base_context_tokens_raw()))
                .unwrap_or(0);
        }
        state
    }
}

/// The rows that follow the last turn, inside the conversation (#3139).
///
/// The hint, the follow-up offers and the live plan card used to be children of
/// the `above` box, which is a sibling of the scroll container: each of them
/// took its height from the conversation, so a live plan plus a waiting offer
/// plus the hint was most of the lower half of the window with no way to scroll
/// any of it away. They are rows of the conversation now, handed to
/// `transcript::view` and drawn under the turn they belong to, so they move
/// with the chat and the reader gets that height back.
///
/// The order is the one the owner asked for: the hint comments on the turn that
/// just finished, so it comes first; then the offers waiting on a click; then
/// the plan the work belongs to. The queue strip and the detached-task row stay
/// outside, because each is one line and each names something the window is
/// doing right now rather than something the conversation said.
///
/// A split is the exception (#3145): the chat leaf is a pane there, and a pane
/// already draws its plan card and its offers in its own chrome, compact for
/// the width it has and pinned so the taps cannot scroll away. Only the hint
/// is built here in that case, because no pane draws one.
impl Dashboard {
    fn chat_trailing(&mut self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(hint) = self.task_hint() {
            rows.push(hint_line(&hint, cx).into_any_element());
        }
        // The offers and the plan card belong to the chrome in a split
        // (#3145). The chat leaf draws both from its pane chrome there, the
        // same compact strip every other leaf draws (#2624), pinned to the
        // pane's bottom so a waiting plan's taps are never scrolled out of
        // reach (#2755). Building them here as well put two of each on the
        // chat leaf of every split. The hint has no pane copy, so it stays.
        let split = self.window_is_split();
        if !split && let Some(sid) = self.chat_session {
            let offers = agent_bridge::options_snapshot(sid);
            if !offers.is_empty() {
                let buttons = self.option_rows(sid, offers, cx);
                // The keyboard's ring, if this is the session it stands on
                // (#3101). A ring left over from another session must not
                // appear on this one's card.
                let focus = self
                    .options_focus
                    .filter(|(session, _)| *session == sid)
                    .map(|(_, at)| at);
                rows.push(options_strip(&buttons, focus).into_any_element());
            }
        }
        // In a split every pane carries its own plan, queue and composer
        // (#2530), so the window-level copies stand down: drawing both would
        // give the chat two composers.
        if !split {
            let open = self.chat_session;
            let actions = open.map(|session| self.plan_actions(session, cx));
            self.plan.refresh();
            let card = self
                .plan
                .editing_view(actions.as_ref(), window)
                .or_else(|| self.plan.view(window));
            if let Some(plan_view) = card {
                rows.push(plan_view.into_any_element());
            }
        }
        rows
    }
}

/// One card, on the bubbles' own column (#3110, #3114).
///
/// The ground runs edge to edge now, so the cards cannot simply take its
/// width: they would be wider than every message above them. What they match is
/// the CONTENT column, which is the conversation's measure when the pane is
/// wider than it, and the pane itself when it is not. `px_2` is the transcript's
/// own padding step, and it is a half rem rather than a pixel count, so the
/// strip's left and right edges land exactly where a message bubble's do at any
/// zoom level: both sit half a rem inside that column, and both are as wide as
/// what the transcript hands its rows (`measure - rem`). That is what makes a
/// plan card or an options card read as the same size as the messages above it
/// instead of as a band cut out of the background with only its top and bottom
/// visible.
fn session_row(view: AnyElement, centred: bool, measure: f32) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .justify_center()
        .w_full()
        .min_w_0()
        .child(
            div()
                .w_full()
                .min_w_0()
                .px_2()
                .when(centred, |d| d.w(px(measure)))
                .child(view),
        )
        .into_any_element()
}

/// The hint line under the conversation (#3102, #3121, #3124, #3139).
///
/// It carries one line about the work: what the screen is asking for right now.
/// The keys that act on the field itself stay inside the field, where the caret
/// is, so the two never compete for the same glance.
///
/// It is a line, not a card (#3139). The card it used to be drew a fill, an
/// edge and a curve around one sentence, which made a tip read as a third
/// surface competing with the conversation it comments on. The ink already
/// carries it: the accent at three quarters is louder than the muted chrome
/// around it, so the box only added another border to look at. Hovering it
/// still offers the one control it needs, a way to make it stop.
///
/// The hover state lives in the shared thread-local the player blocks use, so
/// a repaint costs one map lookup and no view rebuild.
fn hint_line(hint: &Hint, cx: &mut Context<Dashboard>) -> impl IntoElement {
    let shown = palette::current();
    let hover_id = format!("hint-line-{}", hint.id);
    let over = crate::composer_orb::hovering(&hover_id);
    // The id is what makes the line stateful, and `on_hover` only exists on a
    // stateful element (#3121), so the line returns the element it builds
    // rather than a plain `Div` it can no longer be.
    let mut line = div()
        .id("hint-line")
        .flex()
        .flex_row()
        // The sentence breaks onto a second or third row in a narrow pane
        // (#3162), so the row's children sit at the top of it: centred, the
        // label and the dismiss control would drift down the middle of a
        // three-row sentence.
        .items_start()
        .justify_between()
        .gap_2()
        .w_full()
        .min_w_0()
        // The conversation's column (#3124): the transcript already spends half
        // a rem of gutter on each side, so this adds the other 16 and the words
        // land on the messages' 24. No vertical padding and no surface: the
        // transcript's own 8 is the whole gap to the turn above and the offer
        // below.
        .px(transcript::card_inset())
        .on_hover(move |is_over: &bool, window, _cx| {
            // Only a change is worth a frame: a pointer that rests on the line
            // would otherwise repaint the window on every mouse move.
            if crate::composer_orb::note_hover(&hover_id, *is_over) {
                window.refresh();
            }
        })
        .child(
            div()
                .flex()
                .flex_row()
                // The sentence may take more than one row now (#3162), so the
                // label stays on the first row of it instead of floating in
                // the middle of the block.
                .items_start()
                .gap_2()
                // The row takes the width the line has left, so the sentence
                // has a width to break against. Without a bounded width the
                // text would take its natural size and the pane would scroll
                // instead of wrapping.
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .child(
                    // The line names itself (#3124). A bare sentence over the
                    // composer read as the app talking to itself, and the owner
                    // asked what the line was supposed to be. The label is
                    // faint and never shrinks, so the tip keeps the row.
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from("Hint")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        // The sentence is never cut (#3162): it breaks onto a
                        // second or third row in a narrow pane, and the whole
                        // line stays readable. The old truncate put an
                        // ellipsis where the tail of the hint used to be, and
                        // the tail is where the second command and the joke
                        // live.
                        .whitespace_normal()
                        .text_xs()
                        // The theme's accent at full strength shouted for a
                        // line that is advice, not a warning (#3124). It is
                        // mixed a quarter of the way into the surface it sits on,
                        // which is the same ink at 75%, and the accent itself
                        // is untouched: focus rings, the selection wash and
                        // every other accent surface stay exactly as they were.
                        .text_color(rgb(gui_theme::mix(
                            shown.accent,
                            transcript::surface_fill(shown.bg),
                            25,
                            100,
                        )))
                        .child(SharedString::from(hint.text.clone())),
                ),
        );
    // Only a hint the reader may hide gets the control, and only while the
    // pointer is on the line: a permanent × on every line would be a button
    // asking to be pressed by accident. The class comes from the engine, which
    // keeps the warnings (a context about to overflow) undismissable: hiding
    // the one line that says the conversation is running out of room is not a
    // preference, it is a way to lose work.
    if over && dismissible(hint.id) {
        let id = hint.id;
        line = line.child(
            div()
                .id("hint-dismiss")
                .flex_none()
                .px_1()
                .rounded(px(4.0))
                .text_xs()
                .text_color(rgb(shown.text_muted))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(shown.track)).text_color(rgb(shown.text_normal)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut Dashboard, _ev, _window, cx| {
                        cx.stop_propagation();
                        this.dismiss_hint(id, cx);
                    }),
                )
                .child(SharedString::from("×")),
        );
    }
    line
}
