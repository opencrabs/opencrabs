//! Message selection, the forward picker, and the transcript reload that
//! runs after a turn lands.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// The bar that stands where the composer stands in select mode (#2613):
    /// how many rows are picked, Forward, and Cancel. It borrows the
    /// composer's box so the swap reads as one surface changing state, not a
    /// second panel appearing.
    pub(crate) fn select_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette::current();
        let count = self.msg_select.count();
        let cancel = cx.listener(|this, _ev, _window, cx| {
            cx.stop_propagation();
            this.msg_select_cancel(cx);
        });
        let forward = cx.listener(|this, _ev, _window, cx| {
            cx.stop_propagation();
            // A count of zero forwards nothing, so the click is refused where
            // it happens rather than opening an empty picker.
            if this.msg_select.is_empty() {
                return;
            }
            this.msg_forward_begin(cx);
        });
        // Copy applies to the same set Forward would take (#2613): the bar is
        // the bulk-action surface, so it carries both bulk actions.
        let copy = cx.listener(|this, _ev, _window, cx| {
            cx.stop_propagation();
            if this.msg_select.is_empty() {
                return;
            }
            this.msg_copy_picks(cx);
        });
        let armed = count > 0;
        div()
            .id("message-select-bar")
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .w_full()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.panel_hi))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.text_normal))
                    .child(SharedString::from(if count == 1 {
                        "1 message selected".to_string()
                    } else {
                        format!("{count} messages selected")
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("message-select-copy")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .when(armed, |d| {
                                d.cursor_pointer()
                                    .bg(rgb(palette.panel_hi))
                                    .text_color(rgb(palette.text_normal))
                                    .hover(|d| d.bg(rgb(palette.track)))
                            })
                            .when(!armed, |d| d.text_color(rgb(palette.text_faint)))
                            .child(SharedString::from("Copy"))
                            .on_click(copy),
                    )
                    .child(
                        div()
                            .id("message-select-cancel")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgb(palette.text_muted))
                            .hover(|d| d.bg(rgb(palette.panel_hi)))
                            .child(SharedString::from("Cancel"))
                            .on_click(cancel),
                    )
                    .child(
                        div()
                            .id("message-select-forward")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .when(armed, |d| {
                                d.cursor_pointer()
                                    .bg(rgb(palette.teal))
                                    .text_color(rgb(palette.bg))
                                    .hover(|d| d.opacity(0.9))
                            })
                            .when(!armed, |d| {
                                d.bg(rgb(palette.track)).text_color(rgb(palette.text_faint))
                            })
                            .child(SharedString::from("Forward"))
                            .on_click(forward),
                    ),
            )
            .into_any_element()
    }

    /// The destination picker (#2608): the rows that will be forwarded, the
    /// query field, and the sessions a forward can land in. It replaces the
    /// composer so the choice is made where a message would have been typed.
    pub(crate) fn forward_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette::current();
        let count = self.forward_pick.as_ref().map_or(0, Vec::len);
        let targets = self.forward_destinations();
        let cancel = cx.listener(|this, _ev, _window, cx| {
            cx.stop_propagation();
            this.forward_cancel(cx);
        });
        let mut rows: Vec<AnyElement> = Vec::new();
        for (id, label) in targets.iter().take(FORWARD_PICKER_ROWS) {
            let target = *id;
            let picked = self.forward_cursor == Some(target);
            let pick = cx.listener(move |this, _ev, _window, cx| {
                cx.stop_propagation();
                this.forward_into(target, cx);
            });
            rows.push(
                div()
                    .id(SharedString::from(format!("forward-to-{id}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .when(picked, |d| d.bg(rgb(palette.selection)))
                    .hover(|d| d.bg(rgb(palette.panel_hi)))
                    .child(SharedString::from(label.clone()))
                    .child(div().text_xs().text_color(rgb(palette.text_faint)).child(
                        SharedString::from(target.to_string().chars().take(8).collect::<String>()),
                    ))
                    .on_click(pick)
                    .into_any_element(),
            );
        }
        let empty = if targets.is_empty() {
            Some(
                div()
                    .text_sm()
                    .text_color(rgb(palette.text_faint))
                    .child(SharedString::from("No session to forward to")),
            )
        } else {
            None
        };
        div()
            .id("message-forward-picker")
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.panel_hi))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().text_color(rgb(palette.text_normal)).child(
                        SharedString::from(match count {
                            1 => "Forward 1 message to".to_string(),
                            n => format!("Forward {n} messages to"),
                        }),
                    ))
                    .child(
                        div()
                            .id("message-forward-cancel")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgb(palette.text_muted))
                            .hover(|d| d.bg(rgb(palette.panel_hi)))
                            .child(SharedString::from("Cancel"))
                            .on_click(cancel),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(palette.track))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if self.forward_query.is_empty() {
                                palette.text_faint
                            } else {
                                palette.text
                            }))
                            .child(SharedString::from(if self.forward_query.is_empty() {
                                "Type to filter sessions".to_string()
                            } else {
                                self.forward_query.clone()
                            })),
                    ),
            )
            .children(empty)
            .children(rows)
            .into_any_element()
    }

    /// The rail's own label for a session (#2608): the topic name outranks the
    /// stored one, resolved by the same helper the rail paints with, so a
    /// forward names the source the way the user sees it.
    pub(crate) fn session_label(&self, id: Uuid) -> String {
        let stored = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.as_deref());
        rail::rail_title(
            stored,
            self.sessions.topic_titles.get(&id).map(String::as_str),
        )
    }

    /// The session the rows being forwarded came from (#2608).
    pub(crate) fn forward_source(&self) -> Option<Uuid> {
        self.forward_pick
            .as_ref()
            .and_then(|rows| rows.first())
            .map(|m| m.session_id)
    }

    /// The sessions a forward can land in, in the rail's own order: every row
    /// the user can see except the source, minus closed sessions that refuse
    /// turns, narrowed by the picker's query. A forward into the chat it came
    /// from is not a forward, so the source is never offered.
    pub(crate) fn forward_destinations(&self) -> Vec<(Uuid, String)> {
        let source = self.forward_source();
        let query = self.forward_query.trim().to_lowercase();
        self.sessions
            .ordered()
            .filter(|s| Some(s.id) != source && !self.sessions.is_closed(s.id))
            .map(|s| (s.id, self.session_label(s.id)))
            .filter(|(id, label)| {
                query.is_empty()
                    || label.to_lowercase().contains(&query)
                    || id.to_string().starts_with(&query)
            })
            .collect()
    }

    /// Open the message menu on a stored row (#2613), at the press that
    /// asked for it (#2690).
    pub(crate) fn msg_menu_open(
        &mut self,
        id: Uuid,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        // One menu at a time: the rail's open menu closes, and so does any
        // other row's.
        self.sessions.close_rail_menu();
        self.msg_menu = Some(id);
        self.msg_menu_at = Some(at);
        // And the crab pack closes too (#2825): it belongs to the row whose
        // menu was open, so a new press on another row ends it.
        self.react_menu = None;
        self.react_menu_at = None;
        cx.notify();
    }

    /// Open the crab pack for one row (#2825), at the point the row's own menu
    /// already opened at. One card at a time: the menu that carried the React
    /// item closes as the picker takes its place.
    pub(crate) fn msg_react_open(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.msg_menu = None;
        self.react_menu = Some(id);
        self.react_menu_at = self.msg_menu_at;
        cx.notify();
    }

    /// The picker's cells: every crab in the pack, one click each (#2825).
    /// Empty unless a picker is open, so a closed picker mints nothing.
    pub(crate) fn react_cells(
        &self,
        cx: &mut Context<Self>,
    ) -> Vec<(String, transcript::ClickHandler)> {
        let Some(id) = self.react_menu else {
            return Vec::new();
        };
        crate::topic_icons::CRAB_ICONS
            .iter()
            .map(|(name, _)| {
                let name = (*name).to_string();
                let picked = name.clone();
                let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.msg_react_pick(id, picked.clone(), cx);
                });
                (name, Rc::new(pick) as transcript::ClickHandler)
            })
            .collect()
    }

    /// Put one picture on one row, or take it back (#2825). The write runs off
    /// the frame; the chip follows the answer, so a write that failed leaves
    /// the row as it was instead of showing a reaction nobody stored.
    pub(crate) fn msg_react_pick(&mut self, id: Uuid, name: String, cx: &mut Context<Self>) {
        self.react_menu = None;
        self.react_menu_at = None;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = transcript::toggle_reaction(db_path, id, name.clone()).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(reacted) => {
                        let empty = {
                            let list = this.chat_state.transcript.reactions.entry(id).or_default();
                            if reacted {
                                if !list.iter().any(|(known, _)| *known == name) {
                                    list.push((name.clone(), 1));
                                }
                            } else {
                                list.retain(|(known, _)| *known != name);
                            }
                            list.is_empty()
                        };
                        if empty {
                            this.chat_state.transcript.reactions.remove(&id);
                        }
                    }
                    Err(e) => logging::info(&format!("reaction toggle failed: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Put one stored user row back in the composer to be edited (#2826).
    ///
    /// The row's text replaces whatever the field held and the row is
    /// remembered, so the next send replaces it and drops the conversation
    /// after it. Only the window's own chat mints the button, so this always
    /// arms the window's composer.
    pub(crate) fn msg_edit_begin(&mut self, id: Uuid, sequence: i32, cx: &mut Context<Self>) {
        let Some(session) = self.chat_state.session() else {
            return;
        };
        let Some(text) = self
            .chat_state
            .transcript
            .rows
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.content.clone())
        else {
            return;
        };
        // Whatever was waiting in the field belongs to the message that was
        // about to be written, not to the one being rewritten.
        self.composer.edit = Some(chat_leaf::EditTarget {
            id,
            session,
            sequence,
        });
        let editor = self.composer.editor.clone();
        editor.update(cx, |editor, cx| {
            editor.restore_draft(text, cx);
            editor.show_editing(cx);
        });
        // The caret goes to the field being edited: the user asked to rewrite
        // this text, so the next keystroke is theirs.
        self.refocus_screen();
        logging::info(&format!("editing row {id} at sequence {sequence}"));
        cx.notify();
    }

    /// Copy one stored row's text (#2613). Selection-aware, as the issue asks:
    /// with rows picked this copies the whole selection in transcript order,
    /// and with nothing picked it copies the row the menu was opened on.
    pub(crate) fn msg_copy(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let texts: Vec<String> = if self.msg_select.is_empty() {
            self.chat_state
                .transcript
                .rows
                .iter()
                .filter(|m| m.id == id)
                .map(|m| m.content.clone())
                .collect()
        } else {
            self.msg_picked_rows()
                .into_iter()
                .map(|m| m.content)
                .collect()
        };
        self.copy_texts(&texts, cx);
        self.msg_menu = None;
        cx.notify();
    }

    /// Join rows the way the drag selector joins them and put the result on the
    /// clipboard, so a copied selection and a copied drag read the same (#2613).
    pub(crate) fn copy_texts(&mut self, texts: &[String], cx: &mut Context<Self>) {
        if texts.is_empty() {
            return;
        }
        if let Some(text) = opencrabs::ui_core::copy_select::copied_text(texts, 0, texts.len() - 1)
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// The selection bar's Copy (#2613): every picked row, transcript order.
    pub(crate) fn msg_copy_picks(&mut self, cx: &mut Context<Self>) {
        let texts: Vec<String> = self
            .msg_picked_rows()
            .into_iter()
            .map(|m| m.content)
            .collect();
        self.copy_texts(&texts, cx);
    }

    /// The items a row's menu shows (#2613), resolved once per frame. Only
    /// the actions that work end to end are listed: a menu entry with nothing
    /// behind it is worse than an absent one.
    pub(crate) fn msg_menu_items(
        &self,
        id: Uuid,
        cx: &mut Context<Self>,
    ) -> Vec<(String, transcript::ClickHandler)> {
        let mut items: Vec<(String, transcript::ClickHandler)> = Vec::new();
        let copy = cx.listener(move |this, _ev, _window, cx| {
            cx.stop_propagation();
            this.msg_copy(id, cx);
        });
        items.push((MessageAction::CopyText.label().to_string(), Rc::new(copy)));
        // React opens the pack at the same point the row menu opened at
        // (#2825): the menu it replaces is the thing that was clicked, so a
        // second placement rule would move the picker away from the pointer.
        let react = cx.listener(move |this, _ev, _window, cx| {
            cx.stop_propagation();
            this.msg_react_open(id, cx);
        });
        items.push((MessageAction::React.label().to_string(), Rc::new(react)));
        // Forward needs somewhere to go: with no other session on the rail the
        // item would open an empty picker, so it is left out instead.
        if !self.forward_destinations_for(id).is_empty() {
            let forward = cx.listener(move |this, _ev, _window, cx| {
                cx.stop_propagation();
                this.msg_forward_one(id, cx);
            });
            items.push((MessageAction::Forward.label().to_string(), Rc::new(forward)));
        }
        let select = cx.listener(move |this, _ev, _window, cx| {
            cx.stop_propagation();
            this.msg_select_row(id, cx);
        });
        items.push((MessageAction::Select.label().to_string(), Rc::new(select)));
        items
    }

    /// Whether a forward of `id` has a destination at all (#2608). The menu
    /// asks before offering Forward, and it asks about the row's own session,
    /// not about whatever pick happens to be open.
    pub(crate) fn forward_destinations_for(&self, id: Uuid) -> Vec<(Uuid, String)> {
        let Some(source) = self
            .chat_state
            .transcript
            .rows
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.session_id)
        else {
            return Vec::new();
        };
        self.sessions
            .ordered()
            .filter(|s| s.id != source && !self.sessions.is_closed(s.id))
            .map(|s| (s.id, self.session_label(s.id)))
            .collect()
    }

    /// Toggle one row in select mode (#2613). A shift-click extends from the
    /// anchor instead, the same gesture the transcript's text selection uses
    /// for a run of rows.
    pub(crate) fn msg_toggle(&mut self, id: Uuid, shift: bool, cx: &mut Context<Self>) {
        if !self.msg_select.is_active() {
            self.msg_select.begin();
        }
        let order: Vec<Uuid> = self.chat_state.transcript.message_ids();
        if shift {
            self.msg_select.extend(&order, &id);
        } else {
            self.msg_select.toggle(id);
        }
        self.msg_menu = None;
        cx.notify();
    }

    /// "Select" from a row's menu: enter select mode with that row picked, so
    /// a run of rows starts at the one the user pointed at.
    pub(crate) fn msg_select_row(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.msg_select.begin();
        self.msg_select.toggle(id);
        self.msg_menu = None;
        cx.notify();
    }

    /// Leave select mode and drop the picks (#2613).
    pub(crate) fn msg_select_cancel(&mut self, cx: &mut Context<Self>) {
        self.msg_select.cancel();
        self.msg_menu = None;
        cx.notify();
    }

    /// The rows the current picks stand for, in transcript order (#2608).
    /// Order matters: the destination reads them the way the source showed
    /// them, not the order they happened to be clicked.
    pub(crate) fn msg_picked_rows(&self) -> Vec<Message> {
        let order: Vec<Uuid> = self.chat_state.transcript.message_ids();
        self.msg_select
            .picked_in_order(&order)
            .into_iter()
            .filter_map(|id| {
                self.chat_state
                    .transcript
                    .rows
                    .iter()
                    .find(|m| m.id == id)
                    .cloned()
            })
            .collect()
    }

    /// "Forward" on a single row: pick it and go straight to the destination
    /// picker, so one message does not need select mode first (#2613).
    pub(crate) fn msg_forward_one(&mut self, id: Uuid, cx: &mut Context<Self>) {
        // A single-row forward never enters select mode: there is nothing to
        // count, so the bar would be a step with one option.
        let rows: Vec<Message> = self
            .chat_state
            .transcript
            .rows
            .iter()
            .filter(|m| m.id == id)
            .cloned()
            .collect();
        self.msg_menu = None;
        self.forward_open(rows, cx);
    }

    /// Forward the picks: open the destination picker, or say why it cannot
    /// open (#2608).
    pub(crate) fn msg_forward_begin(&mut self, cx: &mut Context<Self>) {
        let rows = self.msg_picked_rows();
        self.forward_open(rows, cx);
    }

    /// Hand the rows to the picker. The composer is swapped for it, so the
    /// destination is chosen where a message would have been typed.
    pub(crate) fn forward_open(&mut self, rows: Vec<Message>, cx: &mut Context<Self>) {
        if rows.is_empty() {
            return;
        }
        self.forward_pick = Some(rows);
        self.forward_query.clear();
        self.forward_cursor = self.forward_destinations().first().map(|(id, _)| *id);
        self.msg_menu = None;
        cx.notify();
    }

    /// Back out of the picker, keeping the picks: the user who opened it by
    /// mistake lands back in select mode with the same rows still chosen.
    pub(crate) fn forward_cancel(&mut self, cx: &mut Context<Self>) {
        self.forward_pick = None;
        self.forward_query.clear();
        self.forward_cursor = None;
        cx.notify();
    }

    /// Walk the destination list (#2608), wrapping nowhere: the ends clamp,
    /// like the rail's cursor.
    pub(crate) fn forward_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let targets: Vec<Uuid> = self
            .forward_destinations()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        if targets.is_empty() {
            self.forward_cursor = None;
            cx.notify();
            return;
        }
        let at = self
            .forward_cursor
            .and_then(|id| targets.iter().position(|t| *t == id));
        let next = match (at, delta.signum()) {
            (Some(ix), 1) => (ix + 1).min(targets.len() - 1),
            (Some(ix), -1) => ix.saturating_sub(1),
            (None, 1) => 0,
            _ => targets.len() - 1,
        };
        self.forward_cursor = Some(targets[next]);
        cx.notify();
    }

    /// Write the rows into the destination (#2608): a click on a row, or Enter
    /// on the picker's cursor.
    pub(crate) fn forward_commit(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self
            .forward_cursor
            .or_else(|| self.forward_destinations().first().map(|(id, _)| *id))
        else {
            return;
        };
        self.forward_into(target, cx);
    }

    /// Copy the picked rows into `target`. The write runs off the UI thread;
    /// the destination is only written to, never prompted, so a forward can
    /// never start a turn the user did not ask for.
    pub(crate) fn forward_into(&mut self, target: Uuid, cx: &mut Context<Self>) {
        let Some(rows) = self.forward_pick.clone() else {
            return;
        };
        let Some(source) = self.forward_source() else {
            return;
        };
        if target == source {
            return;
        }
        let label = self.session_label(source);
        let db_path = self.db_path.clone();
        self.forward_pick = None;
        self.forward_query.clear();
        self.forward_cursor = None;
        self.msg_select.cancel();
        self.msg_menu = None;
        cx.spawn(async move |this, cx| {
            let result = transcript::forward_rows(db_path, rows, target, label).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(landed) => {
                        // A forward into the chat on screen has to show up:
                        // the rows are written, so a reload finds them. Any
                        // other destination is announced only, because the
                        // window runs one conversation at a time.
                        if this.chat_state.session() == Some(target) {
                            this.reload_transcript(target, cx);
                        }
                        logging::info(&format!("forward: {landed} row(s) into {target}"));
                    }
                    Err(e) => logging::error(&format!("forward into {target}: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Refetch the turn's session transcript from the DB, and drop the live
    /// turn only once the persisted rows are actually there.
    ///
    /// Clearing first was the bug behind an "empty session after the turn":
    /// the live copy vanished, the reload had not landed yet (or the rows
    /// were not written yet), and the pane showed nothing at all. The live
    /// turn is the only thing keeping a silent or slow turn visible, so it
    /// stays until the fetch returns a transcript that includes it.
    pub(crate) fn reload_transcript(&mut self, sid: Uuid, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        // The turn that just finished should add at least one row to the
        // session. A reload that comes back short is a write that has not
        // landed yet. The count is the session's own row total, so a
        // conversation past the display window settles like any other (#2705).
        let expect = self.turn.settle.expect;
        let attempt = self.turn.settle.retries;
        // The budget starts at the first read that found the rows short, not
        // when the question was sent: it measures the write, which only exists
        // once the turn has ended.
        let deadline = *self
            .turn
            .settle
            .deadline
            .get_or_insert_with(|| std::time::Instant::now() + SETTLE_BUDGET);
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, sid).await;
            this.update(cx, |this, cx| {
                if this.chat_state.session() != Some(sid) {
                    return;
                }
                match result {
                    Ok(loaded) => {
                        let (hidden, turns) = transcript::turns_with_origins(
                            &loaded.rows,
                            loaded.total,
                            &this.metrics.by_anchor,
                            &loaded.origins,
                        );
                        let landed = settle_landed(expect, loaded.total);
                        // Taken before `rows` is moved out below: every retry
                        // log names the row total it was comparing against.
                        let total = loaded.total;
                        this.chat_state.transcript.hidden = hidden;
                        this.chat_state.transcript.turns = turns;
                        // The rows now carry the question the card opened the
                        // turn with, so this exchange has been identified
                        // against the DB once. Record it, because the
                        // persisted turn is skipped while the card draws it:
                        // dropping the card's own question here would leave
                        // the prompt on neither side of the seam (#2705).
                        let question_landed = this
                            .chat_state.transcript
                            .turns
                            .last()
                            .is_some_and(|turn| transcript::newest_live_turn_matches(turn, &this.turn.live));
                        if question_landed {
                            this.turn.live.landed = true;
                        }
                        // A turn that is still running owns the live card.
                        // Reading the conversation under it (a boot resume, or
                        // opening a session mid-turn) must not wipe the card
                        // the work is painting into (#2627).
                        let in_flight = this.turn.busy;
                        // The ordered ids the selection resolves its ranges and
                        // its copy order against, which the visible turns alone
                        // cannot answer once they are folded (#2613).
                        this.chat_state.transcript.rows = loaded.rows;
                        this.chat_state.transcript.reactions = loaded.reactions;
                        if landed {
                            // The rows are in: the newest turn is the one
                            // that just answered, so it can claim the metrics
                            // recorded at `Done` (#2324).
                            if !in_flight {
                                this.attach_pending_metrics(sid);
                                this.turn.live.clear();
                            }
                            this.turn.settle.expect = None;
                            this.turn.settle.retries = 0;
                            this.turn.settle.deadline = None;
                            this.turn.settle.rechecks = 0;
                        } else if in_flight {
                            // Short of the count while a turn is running is not
                            // a missed write, it is a turn still writing.
                            this.turn.settle.expect = None;
                            this.turn.settle.retries = 0;
                            this.turn.settle.deadline = None;
                        } else if std::time::Instant::now() < deadline {
                            // The row is still in flight: retry shortly, the
                            // live turn keeps the answer on screen meanwhile.
                            // The row total travels with the attempt so the
                            // next incident is diagnosable from the log alone.
                            this.turn.settle.retries = attempt + 1;
                            logging::info(&format!(
                                "transcript short of the finished turn, retry {} (rows {total}, want {})",
                                attempt + 1,
                                expect.map_or("?".to_string(), |n| n.to_string()),
                            ));
                            this.reload_transcript(sid, cx);
                        } else if this.turn.settle.rechecks < SETTLE_RECHECKS {
                            // The budget ran out, but the payload is usually
                            // written by now and only this read missed it. One
                            // more look after a pause turns a permanently
                            // pinned live card into a bounded delay (the
                            // 2026-11-08 incident sat there until the session
                            // was reopened).
                            this.turn.settle.rechecks += 1;
                            this.turn.settle.expect = expect;
                            this.turn.settle.retries = 0;
                            this.turn.settle.deadline = None;
                            logging::info(&format!(
                                "transcript short after {SETTLE_BUDGET:?}; recheck {} (rows {total}, want {})",
                                this.turn.settle.rechecks,
                                expect.map_or("?".to_string(), |n| n.to_string()),
                            ));
                            let view = cx.entity().downgrade();
                            cx.spawn(async move |_, cx| {
                                cx.background_executor().timer(SETTLE_RECHECK).await;
                                view.update(cx, |this, cx| {
                                    if this.chat_state.session() == Some(sid) {
                                        this.reload_transcript(sid, cx);
                                    }
                                })
                                .ok();
                            })
                            .detach();
                        } else {
                            // Bounded: the live turn keeps the answer, so the
                            // pane is never empty even if the write never lands.
                            logging::info(&format!(
                                "transcript never caught up; keeping the live turn (rows {total}, want {})",
                                expect.map_or("?".to_string(), |n| n.to_string()),
                            ));
                            this.turn.settle.expect = None;
                            this.turn.settle.retries = 0;
                            this.turn.settle.deadline = None;
                        }
                    }
                    Err(e) => this.chat_state.transcript.error = Some(e.into()),
                }
                // The turn has landed: follow it only if the reader is still
                // at the bottom, the terminal's own rule (#2685, #2698).
                this.follow_chat();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Key the just-settled turn's metrics by the anchor its persisted rows
    /// produced, now that they exist. Only called once a reload proves the
    /// turn landed, so the newest turn on screen IS the one that answered;
    /// the map is what carries the numbers onto a later reload of the same
    /// conversation.
    pub(crate) fn attach_pending_metrics(&mut self, sid: Uuid) {
        let Some(metrics) = self.metrics.pending.remove(&sid) else {
            return;
        };
        let Some(turn) = self.chat_state.transcript.turns.last_mut() else {
            return;
        };
        turn.metrics = Some(metrics);
        // The persisted row can carry no ledger at all, and the card must not
        // claim the turn did nothing (#2632).
        turn.summary.tool_calls = turn.summary.tool_calls.max(metrics.tool_calls);
        let anchor = turn.anchor.clone();
        self.metrics.by_anchor.insert(anchor.clone(), metrics);
        // The card the user was watching keeps its rows once it settles
        // (#2632): a turn that folds its ledger away the moment it finishes
        // reads as if the work never happened. The chevron still folds it, and
        // older turns keep the terminal's folded default.
        self.chat_state.transcript.expanded.insert(anchor);
    }

    /// Give a freshly loaded pane the numbers its turn recorded at settle: the
    /// last turn of those rows is the one that just answered, and the anchor it
    /// landed under is what keeps the numbers on a later reload (#2632).
    pub(crate) fn claim_pane_metrics(
        &mut self,
        session: Uuid,
        mut turns: Vec<transcript::Turn>,
    ) -> Vec<transcript::Turn> {
        let Some(metrics) = self.metrics.pending.remove(&session) else {
            return turns;
        };
        if let Some(turn) = turns.last_mut() {
            turn.metrics = Some(metrics);
            turn.summary.tool_calls = turn.summary.tool_calls.max(metrics.tool_calls);
            self.metrics.by_anchor.insert(turn.anchor.clone(), metrics);
        }
        turns
    }
}
