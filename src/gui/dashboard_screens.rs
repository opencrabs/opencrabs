//! The screens the right pane swaps to: usage, mission control, themes,
//! fonts, the directory picker, help and the files list.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// ctrl+u: open the usage ledger, or return to the conversation. The panes
    /// never mix and the chat stays exactly where it was.
    pub(crate) fn toggle_usage(&mut self, cx: &mut Context<Self>) {
        match self.screen {
            Screen::Chat => self.screen = Screen::Usage,
            Screen::Usage => {
                self.screen = Screen::Chat;
                if let Some(id) = self.transcript.session
                    && self.transcript.turns.is_empty()
                    && self.transcript.hidden == 0
                    && self.transcript.error.is_none()
                {
                    self.open_session(id, cx);
                    return;
                }
            }
            Screen::Files => self.screen = Screen::Usage,
            Screen::MissionControl => self.screen = Screen::Usage,
            Screen::Models => self.screen = Screen::Usage,
            // Usage from the wizard: the screen closes first, so the ledger is
            // not left behind a half-filled form.
            Screen::Onboard => {
                self.onboard = None;
                self.screen = Screen::Usage;
            }
            // The skills list is a full screen too: ctrl+shift+u leaves it for
            // the ledger, the same as any other screen.
            Screen::Skills => self.screen = Screen::Usage,
            // The picker is transient: the chord just closes it, the way Esc
            // does, instead of hopping to the ledger.
            Screen::DirectoryPicker => self.screen = Screen::Chat,
        }
        self.refocus_screen();
        cx.notify();
    }

    /// The visible period switcher is another door into the same period
    /// loader as the terminal's t/w/m/a keys, not a second filter path.
    pub(crate) fn usage_period_switcher(&self, cx: &mut Context<Self>) -> Div {
        let mut bar = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(text("Period", Size::Xs, palette::current().text_muted));
        for (period, label, id) in [
            (Period::Today, "Today", "usage-period-today"),
            (Period::Week, "Week", "usage-period-week"),
            (Period::Month, "Month", "usage-period-month"),
            (Period::AllTime, "All time", "usage-period-all-time"),
        ] {
            let active = self.dash.period == period;
            bar = bar.child(
                usage_view::period_tab(label, active)
                    .id(id)
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.set_usage_period(period, cx);
                    })),
            );
        }
        bar
    }

    pub(crate) fn set_usage_period(&mut self, period: Period, cx: &mut Context<Self>) {
        if !self.dash.set_period(period) {
            return;
        }
        logging::info(&format!("period -> {period:?}: fetching"));
        self.pending += 1;
        let pending = self.pending;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            // The fetch runs on its own big-stack thread (see `load_view`):
            // gpui's background workers are dispatch-queue threads with tiny
            // stacks. The previous view stays visible until the result lands.
            let state = load_view(db_path, period).await;
            this.update(cx, |this, cx| {
                if pending == this.pending {
                    this.state = state;
                    logging::info("period fetch applied");
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// `/mission-control`: the window's copy of the terminal's screen. The
    /// panels are drawn from the same services the terminal reads, so the
    /// screen answers with live data rather than a mock row (#2426).
    pub(crate) fn open_mission_control(&mut self, cx: &mut Context<Self>) {
        self.onboard = None;
        self.screen = Screen::MissionControl;
        // A detail left open when the screen closed does not re-open: the
        // screen starts on its panels, and the row is clamped to the snapshot
        // that is on hand.
        self.mc_nav.detail = false;
        self.mc_nav.clamp(self.mc.as_ref());
        // A decision's message belongs to the visit that made it.
        self.mc_notice = None;
        // The four services are read once, on entry, exactly as the terminal
        // does on open. A frame never waits on SQLite.
        self.fetch_mission_control(cx);
        // The root takes the keyboard, as on the ledger: the panel keys are
        // not the composer's, and a stale editor focus would send a message.
        self.refocus_screen();
        cx.notify();
    }

    /// Read the Mission Control snapshot on the bridge's DB thread and apply
    /// it only if the screen is still asking for that same window; a stale
    /// read paints nothing.
    pub(crate) fn fetch_mission_control(&mut self, cx: &mut Context<Self>) {
        if self.mc_loading {
            return;
        }
        self.mc_loading = true;
        self.mc_err = None;
        let db_path = self.db_path.clone();
        let window = self.mc_window;
        cx.spawn(async move |this, cx| {
            let result = mission_control::load(db_path, window).await;
            this.update(cx, |this, cx| {
                this.mc_loading = false;
                if this.mc_window == window {
                    match result {
                        Ok(snapshot) => {
                            this.mc = Some(snapshot);
                            this.mc_err = None;
                        }
                        Err(e) => this.mc_err = Some(e.into()),
                    }
                    // A fresh read can be shorter than the last one; the
                    // selection follows it instead of pointing past the end.
                    this.mc_nav.clamp(this.mc.as_ref());
                } else {
                    // The window moved while this read was in flight, so this
                    // answer is for a window nobody is looking at: read the
                    // one that is showing now.
                    this.fetch_mission_control(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Esc, or `/usage` reached from here: back to the conversation, landing
    /// on its newest row. It used to promise "exactly where it was left",
    /// which is the wrong promise twice over: rows land while a panel is
    /// open, and a reader who stepped away did not ask to be parked in
    /// history when they come back (#2698).
    pub(crate) fn close_mission_control(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Chat;
        self.refocus_screen();
        cx.notify();
    }

    /// Mission Control's bare keys, the terminal's own bindings: Tab and `l`
    /// walk the panels, `h` and Shift-Tab walk them back, `j`/`k` and the
    /// arrows move the row, `g`/`G` jump to the ends, Enter opens the record.
    pub(crate) fn mission_control_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        // Chords belong to the window (zoom, the screen doors), never to a
        // panel: swallowing them would break the keys that leave the screen.
        if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
            return;
        }
        // The log viewer takes every key while it is open, the terminal's own
        // rule: a stray keystroke must not move a selection nobody can see.
        if self.mc_logs.is_some() {
            let key = ks.key.clone();
            let shift = ks.modifiers.shift;
            let close = self
                .mc_logs
                .as_mut()
                .is_some_and(|viewer| mission_control::log_key(viewer, &key, shift));
            if close {
                self.mc_logs = None;
            }
            cx.notify();
            return;
        }
        // Uppercase L opens the log viewer, because lowercase `l` cycles the
        // panels, exactly as the terminal binds it (#1528).
        if ks.modifiers.shift && ks.key == "l" {
            let mut viewer = LogViewerState::open();
            // Land on the newest entries: the thing just reproduced is at the
            // end of the file, not the start of the window.
            viewer.scroll_to_end();
            self.mc_logs = Some(viewer);
            self.mc_nav.detail = false;
            cx.notify();
            return;
        }
        // The window filter the footer advertises: D/W/M/A, the terminal's
        // capitals, so the bare letters stay free for apply and reject. A new
        // window is a new read: the numbers are the services' to produce,
        // never arithmetic done in the frame. Asking again for the window
        // already on screen is not a read at all (#2426).
        if ks.modifiers.shift {
            match mission_control::window_key(self.mc_window, &ks.key) {
                mission_control::WindowKey::Switch(window) => {
                    self.mc_window = window;
                    self.fetch_mission_control(cx);
                    return;
                }
                // The window is already the one asked for: the capital is
                // still the filter's, so it is swallowed rather than falling
                // through to the inbox, where `a` applies a proposal.
                mission_control::WindowKey::Same => return,
                mission_control::WindowKey::Pass => {}
            }
        }
        // Decide a proposal: `a` installs it, `r` archives it, the terminal's
        // own letters. Lowercase on purpose: the capitals are already the
        // analytics window filter, so `A` re-windows and `a` applies.
        let decision = match ks.key.as_str() {
            "a" => Some(mission_control::Decision::Apply),
            "r" => Some(mission_control::Decision::Reject),
            _ => None,
        };
        if let Some(decision) = decision {
            self.decide_selected(decision, cx);
            return;
        }
        match ks.key.as_str() {
            "tab" | "l" => self.mc_nav.focus_next(self.mc.as_ref()),
            "backtab" | "h" => self.mc_nav.focus_prev(self.mc.as_ref()),
            "up" | "k" => self.mc_nav.move_selection(self.mc.as_ref(), -1),
            "down" | "j" => self.mc_nav.move_selection(self.mc.as_ref(), 1),
            "home" => self.mc_nav.home(),
            "end" => self.mc_nav.end(self.mc.as_ref()),
            "g" if ks.modifiers.shift => self.mc_nav.end(self.mc.as_ref()),
            "g" => self.mc_nav.home(),
            "enter" => {
                if !self.mc_nav.detail {
                    self.mc_nav.open_detail(self.mc.as_ref());
                }
            }
            _ => return,
        }
        self.mc_nav.clamp(self.mc.as_ref());
        cx.notify();
    }

    /// `a` / `r`: apply or reject the selected proposal through the same
    /// `RsiProposalsTool` the terminal and the agent use, so an applied
    /// proposal is byte-identical whichever client asked (#2426).
    ///
    /// Both keys are the inbox's, as in the terminal: the other panels hold
    /// no proposal to decide.
    pub(crate) fn decide_selected(
        &mut self,
        decision: mission_control::Decision,
        cx: &mut Context<Self>,
    ) {
        if self.mc_nav.panel != mission_control::Panel::Inbox {
            return;
        }
        let Some(item) = self
            .mc
            .as_ref()
            .and_then(|snapshot| snapshot.inbox.get(self.mc_nav.selected))
            .cloned()
        else {
            return;
        };
        let verb = match decision {
            mission_control::Decision::Apply => "Apply",
            mission_control::Decision::Reject => "Reject",
        };
        let Some(bridge) = agent_bridge::bridge() else {
            self.mc_notice = Some(mission_control::Notice {
                text: format!("{verb} failed: the agent is not running"),
                ok: false,
            });
            cx.notify();
            return;
        };
        let service = bridge.service.clone();
        cx.spawn(async move |this, cx| {
            let result = mission_control::decide(service, item.id, item.kind, decision).await;
            this.update(cx, |this, cx| {
                this.mc_notice = Some(match result {
                    Ok(text) => mission_control::Notice { text, ok: true },
                    Err(e) => mission_control::Notice {
                        text: format!("{verb} failed: {e}"),
                        ok: false,
                    },
                });
                // A decided record must not leave its popup describing the
                // row that took its place: the overlay closes, and the list
                // plus the notice say what happened.
                this.mc_nav.detail = false;
                // The inbox is one row shorter now, so every panel is read
                // again: the selection follows the list the row left behind.
                this.mc_loading = false;
                this.fetch_mission_control(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// ctrl+shift+t: the theme modal (#2619). It opens over the conversation,
    /// centred, so the choice is made against real content. Picking a card
    /// saves it at once, the way Telegram's theme picker does; there is no
    /// draft to keep or discard.
    pub(crate) fn toggle_themes(&mut self, cx: &mut Context<Self>) {
        if self.themes_open {
            self.close_themes(cx);
            return;
        }
        // The wheel opens on the accent in force, so a second visit starts
        // where the first left off.
        let accent = palette::saved_mode()
            .1
            .as_deref()
            .and_then(gui_theme::parse_accent)
            .unwrap_or(gui_theme::DEFAULT_ACCENT);
        let (hue, sat, val) = gui_theme::rgb_to_hsv(accent);
        self.themes_hue = hue;
        self.themes_sat = sat.max(0.25);
        self.themes_val = val.max(0.35);
        self.themes_card = match palette::saved_mode().0 {
            GuiTheme::Orgn => 0,
            GuiTheme::Light => 1,
            GuiTheme::Custom => 2,
            GuiTheme::Auto => 3,
        };
        self.themes_drag = None;
        self.themes_open = true;
        // The window's root takes the keyboard, not the composer: the modal
        // owns the arrows and Enter, so a stale editor focus would send a
        // message instead.
        self.refocus_overlay();
        cx.notify();
    }

    /// Leave the theme modal. A pick was already saved, so closing only puts
    /// the screen back; a card the arrows merely previewed is undone here, so
    /// the window never keeps a theme the config does not hold.
    pub(crate) fn close_themes(&mut self, cx: &mut Context<Self>) {
        self.themes_drag = None;
        self.themes_open = false;
        self.fonts_open = false;
        palette::adopt(palette::saved());
        self.refocus_overlay();
        cx.notify();
    }

    /// The accent the wheel is painting right now.
    pub(crate) fn themes_accent(&self) -> u32 {
        gui_theme::hsv_to_rgb(self.themes_hue, self.themes_sat, self.themes_val)
    }

    /// The modal's own keys. The arrows walk the four cards without writing
    /// anything, so the window repaints in the card under the keyboard and the
    /// choice is made by looking. Enter saves the card it stands on; Escape
    /// closes and leaves the saved theme alone. Every other key is swallowed,
    /// because the window behind the modal is not listening (#2619).
    pub(crate) fn theme_modal_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        const CARDS: [GuiTheme; 4] = [
            GuiTheme::Orgn,
            GuiTheme::Light,
            GuiTheme::Custom,
            GuiTheme::Auto,
        ];
        match ks.key.as_str() {
            "escape" => {
                // The font list is the innermost surface, so Escape closes it
                // before the modal (#2786).
                if self.fonts_open {
                    self.fonts_open = false;
                    cx.notify();
                } else {
                    self.close_themes(cx);
                }
            }
            "left" | "up" => {
                self.themes_card = (self.themes_card + 3) % 4;
                let card = CARDS[self.themes_card];
                self.preview_card(card, cx);
            }
            "right" | "down" | "tab" => {
                self.themes_card = (self.themes_card + 1) % 4;
                let card = CARDS[self.themes_card];
                self.preview_card(card, cx);
            }
            "enter" => {
                let card = CARDS[self.themes_card.min(3)];
                self.pick_theme(card, cx);
                // Custom is the only card with something left to do under it:
                // the wheel edits the accent, so picking it keeps the modal.
                if card != GuiTheme::Custom {
                    self.close_themes(cx);
                } else {
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// Repaint the window in a card's theme without saving it. The arrows walk
    /// the four cards this way, so the choice is made by looking; nothing is
    /// written until a pick, and closing puts the saved theme back.
    pub(crate) fn preview_card(&mut self, mode: GuiTheme, cx: &mut Context<Self>) {
        let accent = if mode == GuiTheme::Custom {
            Some(gui_theme::format_accent(self.themes_accent()))
        } else {
            palette::saved_mode().1
        };
        palette::adopt(gui_theme::resolve(
            mode,
            palette::system_is_dark(),
            accent.as_deref(),
        ));
        cx.notify();
    }

    /// Turn a point in window coordinates into the accent control under it and
    /// start dragging it. A press that lands on none of them is ignored, so a
    /// click on the card's padding does not move the colour.
    pub(crate) fn themes_press_at(&mut self, x: f32, y: f32) -> bool {
        let boxed = |area: &std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>| *area.borrow();
        if let Some(bounds) = boxed(&self.themes_wheel)
            && let Some(hit) = accent::wheel_hit(bounds, x, y)
        {
            self.themes_hue = hit.hue;
            // Past the rim the drag pins to full saturation instead of
            // wrapping, so a fast flick outward keeps the colour it had.
            self.themes_sat = hit.saturation;
            self.themes_drag = Some(accent::Drag::Wheel);
            return true;
        }
        if let Some(bounds) = boxed(&self.themes_sat_track)
            && accent::inside(bounds, x, y, accent::SLACK)
        {
            self.themes_sat = accent::slider_value(bounds, x);
            self.themes_drag = Some(accent::Drag::Saturation);
            return true;
        }
        if let Some(bounds) = boxed(&self.themes_val_track)
            && accent::inside(bounds, x, y, accent::SLACK)
        {
            self.themes_val = accent::slider_value(bounds, x);
            self.themes_drag = Some(accent::Drag::Value);
            return true;
        }
        false
    }

    /// Move the control a drag already holds. The pointer is followed even
    /// once it leaves the painted box, the way a slider normally behaves, so a
    /// fast drag cannot drop the value halfway.
    pub(crate) fn themes_drag_to(&mut self, x: f32, y: f32) -> bool {
        let drag = match self.themes_drag {
            Some(drag) => drag,
            None => return false,
        };
        match drag {
            accent::Drag::Wheel => {
                // A wheel is polar, so a drag reads the angle from wherever the
                // pointer went and pins the saturation at the rim.
                if let Some(hit) = (*self.themes_wheel.borrow())
                    .and_then(|bounds| accent::wheel_drag(bounds, x, y))
                {
                    self.themes_hue = hit.hue;
                    self.themes_sat = hit.saturation;
                }
                true
            }
            accent::Drag::Saturation => {
                if let Some(bounds) = *self.themes_sat_track.borrow() {
                    self.themes_sat = accent::slider_value(bounds, x);
                }
                true
            }
            accent::Drag::Value => {
                if let Some(bounds) = *self.themes_val_track.borrow() {
                    self.themes_val = accent::slider_value(bounds, x);
                }
                true
            }
        }
    }

    /// Pick a card: apply the theme and save it in one step (#2619).
    pub(crate) fn pick_theme(&mut self, mode: GuiTheme, cx: &mut Context<Self>) {
        let hex = gui_theme::format_accent(self.themes_accent());
        let name = match mode {
            GuiTheme::Orgn => "orgn",
            GuiTheme::Light => "light",
            GuiTheme::Custom => "custom",
            GuiTheme::Auto => "auto",
        };
        match Config::write_key_string("gui", "theme", name) {
            Ok(()) => logging::info(&format!("themes: kept '{name}' as [gui] theme")),
            Err(e) => logging::error(&format!("themes: could not save '{name}': {e}")),
        }
        // Only `custom` carries an accent; the other three keep whatever was
        // saved, so switching away and back does not lose the wheel's colour.
        let accent = if mode == GuiTheme::Custom {
            match Config::write_key_string("gui", "custom_accent", &hex) {
                Ok(()) => logging::info(&format!("themes: kept accent {hex}")),
                Err(e) => logging::error(&format!("themes: could not save accent {hex}: {e}")),
            }
            Some(hex)
        } else {
            palette::saved_mode().1
        };
        palette::remember_mode(mode, accent.clone());
        palette::adopt(gui_theme::resolve(
            mode,
            palette::system_is_dark(),
            accent.as_deref(),
        ));
        cx.notify();
    }

    /// Repaint the window with the accent the wheel is showing, without
    /// saving it: a drag paints live, and the save lands on release.
    pub(crate) fn preview_accent(&mut self, cx: &mut Context<Self>) {
        let hex = gui_theme::format_accent(self.themes_accent());
        palette::preview(GuiTheme::Custom, Some(&hex));
        cx.notify();
    }

    /// Save the accent a drag settled on (#2619). A release with no drag under
    /// it is not a save: it is a click that missed every control.
    pub(crate) fn keep_accent(&mut self, cx: &mut Context<Self>) {
        if self.themes_drag.is_none() {
            return;
        }
        self.themes_drag = None;
        let hex = gui_theme::format_accent(self.themes_accent());
        match Config::write_key_string("gui", "custom_accent", &hex) {
            Ok(()) => logging::info(&format!("themes: kept accent {hex}")),
            Err(e) => logging::error(&format!("themes: could not save accent {hex}: {e}")),
        }
        // The wheel only shows under `custom`, so the mode in force is the one
        // to keep; the accent rides along with it.
        palette::remember_mode(palette::saved_mode().0, Some(hex));
        cx.notify();
    }

    /// The theme modal itself: a centred card over the window, the shape of
    /// Telegram's picker. A card applies and saves on the click, so there is no
    /// Keep button; Escape and a click on the scrim close it. Custom keeps the
    /// modal open, because the wheel beneath the cards is the point of picking
    /// it (#2619).
    ///
    /// It is attached last in the root's paint order, so while it is open it is
    /// the surface the user is looking at and the only one the pointer reaches.
    pub(crate) fn themes_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let saved = palette::saved_mode().0;
        let accent = palette::current().accent;
        let mut cards: Vec<Stateful<Div>> = Vec::with_capacity(themes_view::CARDS.len());
        for (index, &(mode, label, hint)) in themes_view::CARDS.iter().enumerate() {
            let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.themes_card = index;
                this.pick_theme(mode, cx);
                // Custom needs the wheel, so its click leaves the modal
                // standing; the other three are complete on their own.
                if mode != GuiTheme::Custom {
                    this.close_themes(cx);
                }
            });
            cards.push(
                themes_view::card(mode, label, hint, self.themes_card == index, accent)
                    .on_click(pick),
            );
        }
        // The wheel is the only draggable thing in the window: a press seeds
        // the hue and the drag repaints live, with the save landing on release,
        // so a drag never writes the config once per frame.
        let press = cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
            let x = f32::from(ev.position.x);
            let y = f32::from(ev.position.y);
            if this.themes_press_at(x, y) {
                this.preview_accent(cx);
            }
        });
        let slide = cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
            let x = f32::from(ev.position.x);
            let y = f32::from(ev.position.y);
            if this.themes_drag_to(x, y) {
                this.preview_accent(cx);
            }
        });
        let release = cx.listener(|this, _ev: &MouseUpEvent, _window, cx| {
            this.keep_accent(cx);
        });
        // A drag that ends outside the row still saves: `keep_accent` ignores a
        // release with no drag under it.
        let release_out = cx.listener(|this, _ev: &MouseUpEvent, _window, cx| {
            this.keep_accent(cx);
        });
        let accent_row = (saved == GuiTheme::Custom).then(|| {
            let areas = themes_view::AccentAreas {
                wheel: self.themes_wheel.clone(),
                saturation: self.themes_sat_track.clone(),
                value: self.themes_val_track.clone(),
            };
            let hex = gui_theme::format_accent(self.themes_accent());
            let shown = palette::current();
            div()
                .id("theme-accent-row")
                .on_mouse_down(MouseButton::Left, press)
                .on_mouse_move(slide)
                .on_mouse_up(MouseButton::Left, release)
                .on_mouse_up_out(MouseButton::Left, release_out)
                .child(themes_view::accent_row(
                    self.themes_hue,
                    self.themes_sat,
                    self.themes_val,
                    &hex,
                    shown,
                    areas,
                ))
                .into_any_element()
        });
        // The font row (#2786): the family in force, and the machine's
        // families under it once the row is opened. The rows are built here
        // because only this `cx` can mint a click listener, and only while the
        // list is open: the catalog is the whole machine, so a closed row must
        // not build three hundred elements on every frame.
        let shown = palette::current();
        let current_font = fonts::current_ui();
        let font_picks: Vec<Stateful<Div>> = if self.fonts_open {
            fonts::picker_rows()
                .into_iter()
                .map(|family| {
                    let selected = family.eq_ignore_ascii_case(&current_font);
                    // The id and the label are read before the family moves
                    // into the listener, which is the only thing that needs it
                    // after the row is built.
                    let id = SharedString::from(format!("theme-font-{family}"));
                    let label = SharedString::from(gui_font::row_label(&family));
                    let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        this.pick_font(&family, cx);
                    });
                    div()
                        .id(id)
                        .flex()
                        .flex_row()
                        .items_center()
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(shown.panel_hi)))
                        .text_xs()
                        .text_color(rgb(if selected {
                            shown.accent
                        } else {
                            shown.text_normal
                        }))
                        .child(label)
                        .on_click(pick)
                })
                .collect()
        } else {
            Vec::new()
        };
        let font_toggle = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.toggle_fonts(cx);
        });
        let font_row =
            themes_view::font_row(&current_font, self.fonts_open, font_toggle, font_picks);
        let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.close_themes(cx);
        });
        themes_view::modal(
            cards,
            accent_row,
            font_row,
            "A click saves the card at once; the arrows walk them and Enter picks.",
            dismiss,
        )
        .into_any_element()
    }

    /// Open or close the font list under the theme cards (#2786).
    pub(crate) fn toggle_fonts(&mut self, cx: &mut Context<Self>) {
        self.fonts_open = !self.fonts_open;
        cx.notify();
    }

    /// Keep a family from the font list (#2786). A pick saves at once, the way
    /// a theme card does, and closes the list so the modal stays the size it
    /// was. The whole window repaints in the new family on the next frame.
    pub(crate) fn pick_font(&mut self, family: &str, cx: &mut Context<Self>) {
        match fonts::keep_ui(family) {
            Ok(()) => logging::info(&format!(
                "fonts: kept '{}' as [gui] font",
                fonts::current_ui()
            )),
            Err(e) => logging::error(&format!("fonts: could not save '{family}': {e}")),
        }
        self.fonts_open = false;
        cx.notify();
    }

    /// `/cd` with no argument: the directory picker screen (#2527). The
    /// terminal opens its dialog on the session's working directory; the
    /// window opens the same screen on the same seed, so both surfaces
    /// answer the same key with the same screen. Needs an open conversation
    /// like every other session-bound command here.
    pub(crate) fn open_cd_picker(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.transcript.session else {
            self.live
                .notices
                .push("No conversation is open. Type /new first.".to_string());
            cx.notify();
            return;
        };
        let here = agent_bridge::bridge()
            .map(|b| b.service.get_working_directory_for_session(session))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        self.cd_dir = here;
        self.cd_show_hidden = false;
        self.cd_selected = 0;
        self.cd_entries = cd_entries_for(&self.cd_dir, false);
        self.screen = Screen::DirectoryPicker;
        // The screen takes the keyboard, the same rule as Themes: on this
        // screen the arrows walk the list, so a stale editor focus would
        // send a message instead.
        self.refocus_screen();
        cx.notify();
    }

    /// Leave the picker: nothing is applied, the conversation comes back.
    pub(crate) fn close_cd_picker(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Chat;
        self.refocus_screen();
        cx.notify();
    }

    /// Tab/Space on a row: make it the session's working directory. The
    /// conversation comes back FIRST and the apply follows in a spawned
    /// task, the ordering the terminal's picker was just fixed to use
    /// (#2526): a slow persistence write must never hold the screen open.
    pub(crate) fn cd_select_here(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.transcript.session else {
            self.close_cd_picker(cx);
            return;
        };
        let dir = self.cd_dir.clone();
        self.screen = Screen::Chat;
        self.refocus_screen();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let applied = match agent_bridge::bridge() {
                Some(b) => b.service.apply_operator_cd(session, &dir).await,
                None => dir.clone(),
            };
            let _ = this.update(cx, |this, cx| {
                this.live
                    .notices
                    .push(text_commands::cd_reply(&applied.display().to_string()));
                cx.notify();
            });
        })
        .detach();
    }

    /// Enter on a row: descend into it. `..` climbs back up one level.
    /// Reads are synchronous and local, the way the terminal's dialog does
    /// them, so the row list can never drift from what the screen shows.
    pub(crate) fn cd_descend(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(next) = self.cd_entries.get(index) else {
            return;
        };
        self.cd_dir = next.clone();
        self.cd_selected = 0;
        self.cd_entries = cd_entries_for(&self.cd_dir, self.cd_show_hidden);
        cx.notify();
    }

    /// `.` in the picker: show or hide the dotfile directories, the same
    /// toggle the terminal's dialog binds.
    pub(crate) fn cd_toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.cd_show_hidden = !self.cd_show_hidden;
        self.cd_selected = 0;
        self.cd_entries = cd_entries_for(&self.cd_dir, self.cd_show_hidden);
        cx.notify();
    }

    /// ctrl+h, or Cmd+? on a Mac: the key reference, the window's door to the
    /// rows the terminal prints via `/help` (#2320). It is read-only, so it
    /// can sit above every other affordance: closing it leaves the screen
    /// exactly as it was.
    pub(crate) fn toggle_help(&mut self, cx: &mut Context<Self>) {
        self.help_open = !self.help_open;
        if !self.help_open {
            // Closing the reference is a return to the conversation, so the
            // caret goes back where typing is expected.
            self.refocus_screen();
        }
        cx.notify();
    }

    /// ctrl+g: the open conversation's tracked files, or back to the chat. A
    /// no-op without an open conversation, because a files browser for no
    /// session is a lie. The TUI reaches the same list with `f` on the
    /// sessions browser (`AppMode::SessionFiles`).
    pub(crate) fn toggle_files(&mut self, cx: &mut Context<Self>) {
        if self.screen != Screen::Chat {
            self.screen = Screen::Chat;
            self.refocus_screen();
            cx.notify();
            return;
        }
        let Some(sid) = self.transcript.session else {
            logging::info("ctrl-g ignored: no open conversation");
            return;
        };
        self.screen = Screen::Files;
        self.files.clear();
        self.files_err = None;
        self.files_for = Some(sid);
        self.fetch_files(sid, cx);
        self.refocus_screen();
        cx.notify();
    }

    /// Fetch the session's tracked files on the bridge's DB thread and apply
    /// them only if the user still sits on the files screen of the same
    /// session; a stale fetch paints nothing.
    pub(crate) fn fetch_files(&mut self, sid: Uuid, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = agent_bridge::load_files(db_path, sid).await;
            this.update(cx, |this, cx| {
                if this.files_for == Some(sid) && this.screen == Screen::Files {
                    match result {
                        Ok(files) => this.files = files,
                        Err(e) => this.files_err = Some(e.into()),
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Put the rail's find field in front of the keyboard (#2812).
    ///
    /// The row the keyboard stands on is armed, so Enter has a target, and the
    /// overlay takes the keys so the composer's Enter and arrows stop answering
    /// while the field says it is listening (#2454). `ctrl+f` and the first
    /// typed character both come through here, so the two ways into the field
    /// cannot drift apart.
    pub(crate) fn arm_rail_filter(&mut self, cx: &mut Context<Self>) {
        self.sessions.filter_active = true;
        if self.sessions.cursor.is_none() || !self.sessions.cursor_visible() {
            self.sessions.cursor = self.sessions.cursor_step(None, 1);
        }
        self.refocus_overlay();
        cx.notify();
    }

    /// Leave the rail filter. The query is a transient find aid, so every
    /// exit clears it: the rail returns to the full list and never hides a
    /// filter behind a closed field.
    pub(crate) fn close_rail_filter(&mut self, cx: &mut Context<Self>) {
        self.sessions.filter.clear();
        self.sessions.filter_active = false;
        // The deep hits clear with the field, and the generation bumps so a
        // query still in flight for the old text bows out instead of landing
        // on an empty rail (#2720).
        self.sessions.content_matches.clear();
        self.rail_find_gen = self.rail_find_gen.wrapping_add(1);
        self.refocus_screen();
        cx.notify();
    }

    /// Drop the rail cursor when the row it points at is no longer shown,
    /// so the keyboard never arms a ghost (#2351). Clearing the filter only
    /// ever adds rows back, so it needs no call here.
    pub(crate) fn normalize_cursor(&mut self) {
        if !self.sessions.cursor_visible() {
            self.sessions.cursor = None;
        }
    }

    /// The session-files browser body: one row per tracked file, name on the
    /// left, tilde path and size on the right. Real rows from
    /// `FileService::list_files_for_session`, never placeholders.
    pub(crate) fn files_view(&self) -> Div {
        let mut body = div().flex().flex_col().gap(rems(0.75));
        if let Some(err) = &self.files_err {
            return body.child(
                div()
                    .text_sm()
                    .text_color(rgb(palette::current().text_muted))
                    .child(err.clone()),
            );
        }
        if self.files.is_empty() {
            return body.child(
                div()
                    .text_sm()
                    .text_color(rgb(palette::current().text_faint))
                    .child("no files tracked yet"),
            );
        }
        for f in &self.files {
            let name = f
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| f.path.display().to_string());
            let mut meta = tilde_path(&f.path);
            if let Some(size) = f.size {
                meta.push_str(&format!(" · {}", human_size(size.max(0) as usize)));
            }
            body = body.child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(rems(1.0))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(rgb(palette::current().text_normal))
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child(SharedString::from(meta)),
                    ),
            );
        }
        body
    }

    /// A file drag is over the window right now. The moves arrive at the poll
    /// cadence, so a few missed frames means the pointer left: no drag, no
    /// overlay, and the next real drag re-arms it.
    pub(crate) fn file_drag_active(&self) -> bool {
        self.drag_seen
            .is_some_and(|seen| seen.elapsed() < std::time::Duration::from_millis(400))
    }

    /// Send typed text as an agent turn (or queue it behind the running
    /// one). Called from the editor's Sent subscription.
    /// A message typed in a pane's own composer (#2530). The window runs one
    /// turn at a time and that turn follows the open conversation, so the pane
    /// hands its session to the window and sends there, the same repair the
    /// terminal makes on a split send. The pane's session becomes the chat, so
    /// the pane closes with it: one session never sits in two leaves (#2472).
    /// Whether the window draws more than one pane (#2530). Window-level
    /// chrome steps aside for the per-pane copies only then, so an unsplit
    /// window keeps the layout it has always had.
    pub(crate) fn window_is_split(&self) -> bool {
        self.panes
            .as_ref()
            .is_some_and(|tree| tree.leaves().len() > 1)
    }
}
