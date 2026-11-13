//! Settings modal state, writes, keyboard handling and row listeners (#2878).
//!
//! Part of the dashboard split (#2822): descriptors live in `settings_view`,
//! GPUI shells in `settings_modal`, and this impl owns the live dashboard state.

use super::*;

impl Dashboard {
    /// ctrl+comma: the settings modal (#2878). It opens over the conversation,
    /// centred like the theme modal, and every change writes at once through
    /// the config file's own locked, revalidating path. Values are read fresh
    /// on open, so a hand edit between opens shows up.
    pub(crate) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings_open {
            self.close_settings(cx);
            return;
        }
        self.settings_cursor = 0;
        self.settings_scroll.scroll_to_item(0);
        self.settings_secret = None;
        self.settings_status = match Config::load() {
            Ok(config) => {
                self.settings_values = settings_view::current_values(&config);
                self.settings_creds = settings_view::credential_rows(&config);
                None
            }
            Err(e) => {
                self.settings_values = Vec::new();
                self.settings_creds = Vec::new();
                Some(format!("config load failed: {e}"))
            }
        };
        self.settings_open = true;
        // The modal owns the arrows and Enter, so the root takes the keyboard
        // rather than a stale editor focus (#2619's rule).
        self.refocus_overlay();
        cx.notify();
    }

    pub(crate) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.settings_secret = None;
        self.refocus_overlay();
        cx.notify();
    }

    /// The flat row list the cursor walks: every setting, then every
    /// credential. Section headers are not rows.
    fn settings_row_count(&self) -> usize {
        self.settings_values.len() + self.settings_creds.len()
    }

    /// Execute one row's write and repaint the row with what the file now
    /// holds. The status line names the key, and for a credential never the
    /// value.
    fn settings_write(
        &mut self,
        index: usize,
        value: settings_view::RowValue,
        cx: &mut Context<Self>,
    ) {
        let Some(spec) = settings_view::SPECS.get(index) else {
            return;
        };
        let Some(op) = settings_view::write_op(spec, &value) else {
            return;
        };
        let result = match op {
            settings_view::WriteOp::SetScalar(v) => Config::write_key(spec.section, spec.key, &v),
            settings_view::WriteOp::SetString(v) => {
                Config::write_key_string(spec.section, spec.key, &v).map(|()| String::new())
            }
            settings_view::WriteOp::Remove => {
                Config::remove_key(spec.section, spec.key).map(|()| String::new())
            }
        };
        match result {
            Ok(_) => {
                let shown = settings_view::value_label(&value);
                self.settings_status = Some(match spec.restart {
                    true => format!(
                        "saved {}.{} = {shown}; restart the window to apply",
                        spec.section, spec.key
                    ),
                    false => format!("saved {}.{} = {shown}", spec.section, spec.key),
                });
                if let Some(slot) = self.settings_values.get_mut(index) {
                    *slot = value;
                }
            }
            Err(e) => {
                self.settings_status = Some(format!("write failed: {e}"));
            }
        }
        cx.notify();
    }

    /// Enter, space, or a row click: flip a toggle, cycle a choice, arm a
    /// credential edit. A number row's click steps up; left/right is the
    /// precise control.
    fn settings_advance(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.settings_values.len() {
            let Some(spec) = settings_view::SPECS.get(index) else {
                return;
            };
            let Some(current) = self.settings_values.get(index).cloned() else {
                return;
            };
            if let Some(next) = settings_view::advance(spec, &current) {
                self.settings_write(index, next, cx);
            }
            return;
        }
        let cred = index - self.settings_values.len();
        if cred < self.settings_creds.len() {
            // Re-arming the row that is already being edited would silently
            // drop the typed buffer, so a click there is a no-op.
            if self
                .settings_secret
                .as_ref()
                .is_some_and(|(armed, _)| *armed == cred)
            {
                return;
            }
            self.settings_secret = Some((cred, String::new()));
            self.settings_status =
                Some("type the key, enter saves, esc cancels; it stays masked".to_string());
            cx.notify();
        }
    }

    fn settings_step(&mut self, index: usize, dir: i32, cx: &mut Context<Self>) {
        let Some(spec) = settings_view::SPECS.get(index) else {
            return;
        };
        let Some(settings_view::RowValue::Number(current)) = self.settings_values.get(index) else {
            return;
        };
        let next = settings_view::step_number(spec, *current, dir);
        self.settings_write(index, settings_view::RowValue::Number(next), cx);
    }

    /// Save the armed credential edit. An empty buffer writes nothing: an
    /// empty api_key is how a provider looks configured but is not, so the
    /// modal refuses to create one.
    fn settings_commit_secret(&mut self, cx: &mut Context<Self>) {
        let Some((row, buffer)) = self.settings_secret.take() else {
            return;
        };
        let Some(cred) = self.settings_creds.get(row) else {
            return;
        };
        let typed = buffer.trim();
        if typed.is_empty() {
            self.settings_status = Some("empty key not written".to_string());
            cx.notify();
            return;
        }
        match Config::write_keys_key(&cred.section, "api_key", typed) {
            Ok(()) => {
                self.settings_status = Some(format!("saved {}.api_key", cred.section));
                if let Some(cred) = self.settings_creds.get_mut(row) {
                    cred.configured = true;
                }
            }
            Err(e) => {
                self.settings_status = Some(format!("write failed: {e}"));
            }
        }
        cx.notify();
    }

    /// The modal owns every key while it is open (#2878), exactly the theme
    /// modal's rule: nothing under it may answer, the composer included.
    pub(crate) fn settings_modal_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        // An armed credential edit takes the whole keyboard: typed characters
        // extend the masked buffer and nothing else in the window listens.
        if self.settings_secret.is_some() {
            match ks.key.as_str() {
                "escape" => {
                    self.settings_secret = None;
                    self.settings_status = None;
                }
                "enter" => self.settings_commit_secret(cx),
                "backspace" => {
                    if let Some((_, buffer)) = self.settings_secret.as_mut() {
                        buffer.pop();
                    }
                }
                "v" if ks.modifiers.platform || ks.modifiers.control => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                        && let Some((_, buffer)) = self.settings_secret.as_mut()
                    {
                        let pasted: String =
                            text.chars().filter(|c| !c.is_control()).collect();
                        buffer.push_str(&pasted);
                    }
                }
                _ => {
                    if let Some(typed) = ks.key_char.as_deref()
                        && !typed.chars().any(|c| c.is_control())
                        && let Some((_, buffer)) = self.settings_secret.as_mut()
                    {
                        buffer.push_str(typed);
                    }
                }
            }
            cx.notify();
            return;
        }
        match ks.key.as_str() {
            "escape" => self.close_settings(cx),
            "up" => {
                self.settings_cursor = self.settings_cursor.saturating_sub(1);
            }
            "down" => {
                let last = self.settings_row_count().saturating_sub(1);
                self.settings_cursor = (self.settings_cursor + 1).min(last);
            }
            "left" => self.settings_step(self.settings_cursor, -1, cx),
            "right" => self.settings_step(self.settings_cursor, 1, cx),
            "enter" | "space" => self.settings_advance(self.settings_cursor, cx),
            _ => {}
        }
        if matches!(ks.key.as_str(), "up" | "down") {
            let item = settings_view::body_index(self.settings_cursor, self.settings_values.len());
            self.settings_scroll.scroll_to_item(item);
        }
        cx.notify();
    }

    /// The modal's rows: agent settings, memory settings, credentials, then
    /// the status line. Listeners live here, the shells in settings_modal.
    pub(crate) fn settings_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let shown = palette::current();
        let mut body: Vec<AnyElement> = Vec::new();
        let mut section = "";
        for (index, spec) in settings_view::SPECS
            .iter()
            .enumerate()
            .take(self.settings_values.len())
        {
            if spec.section != section {
                section = spec.section;
                let title = if section == "agent" { "Agent" } else { "Memory" };
                body.push(settings_modal::section_header(title).into_any_element());
            }
            let selected = self.settings_cursor == index;
            let Some(value) = self.settings_values.get(index).cloned() else {
                continue;
            };
            let control = if matches!(spec.kind, settings_view::SettingKind::Number { .. }) {
                let minus = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    // The row's own click steps up; the buttons must not
                    // bubble into it or one press would step twice.
                    cx.stop_propagation();
                    this.settings_step(index, -1, cx);
                });
                let plus = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.settings_step(index, 1, cx);
                });
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(SharedString::from(format!("settings-minus-{index}")))
                            .cursor_pointer()
                            .px_1()
                            .on_click(minus)
                            .child(text("-", Size::Sm, shown.text_muted)),
                    )
                    .child(text(
                        settings_view::value_label(&value),
                        Size::Sm,
                        shown.text_strong,
                    ))
                    .child(
                        div()
                            .id(SharedString::from(format!("settings-plus-{index}")))
                            .cursor_pointer()
                            .px_1()
                            .on_click(plus)
                            .child(text("+", Size::Sm, shown.text_muted)),
                    )
            } else {
                div().child(text(
                    settings_view::value_label(&value),
                    Size::Sm,
                    shown.accent,
                ))
            };
            let row = settings_modal::row_shell(
                SharedString::from(format!("settings-row-{index}")),
                selected,
                spec.label,
                spec.blurb,
                spec.restart,
                control,
            );
            let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.settings_cursor = index;
                this.settings_advance(index, cx);
            });
            body.push(row.on_click(pick).into_any_element());
        }
        body.push(settings_modal::section_header("Credentials").into_any_element());
        let creds = self.settings_creds.len();
        for row in 0..creds {
            let index = self.settings_values.len() + row;
            let Some(cred) = self.settings_creds.get(row) else {
                continue;
            };
            let editing = self
                .settings_secret
                .as_ref()
                .is_some_and(|(armed, _)| *armed == row);
            let control = if editing {
                let len = self
                    .settings_secret
                    .as_ref()
                    .map(|(_, buffer)| buffer.chars().count())
                    .unwrap_or(0);
                div().child(text(
                    format!("{} (enter saves, esc cancels)", "\u{2022}".repeat(len.max(1))),
                    Size::Sm,
                    shown.accent,
                ))
            } else {
                div().child(text(
                    if cred.configured {
                        "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}".to_string()
                    } else {
                        "not set".to_string()
                    },
                    Size::Sm,
                    if cred.configured {
                        shown.text_muted
                    } else {
                        shown.text_faint
                    },
                ))
            };
            let selected = self.settings_cursor == index;
            let shell = settings_modal::row_shell(
                SharedString::from(format!("settings-cred-{row}")),
                selected,
                &cred.label,
                "api key; enter to replace",
                false,
                control,
            );
            let arm = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.settings_cursor = index;
                this.settings_advance(index, cx);
            });
            body.push(shell.on_click(arm).into_any_element());
        }
        if self.settings_row_count() == 0 {
            body.push(
                text(
                    "The config could not be read; the error is below.",
                    Size::Sm,
                    shown.text_muted,
                )
                .into_any_element(),
            );
        }
        if let Some(status) = &self.settings_status {
            body.push(text(status.clone(), Size::Xs, shown.amber).into_any_element());
        }
        let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.close_settings(cx);
        });
        settings_modal::modal(
            body,
            "up/down move · enter changes · left/right adjust · credentials stay masked",
            &self.settings_scroll,
            dismiss,
        )
        .into_any_element()
    }
}
