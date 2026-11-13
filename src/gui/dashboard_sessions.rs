//! Session rows and the work they hold: resuming a detached task, starting
//! one, favorites, delete, rename, and the preview a path opens.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// Settle one detached row in the live turn, if it still holds it.
    pub(crate) fn settle_detached_row(
        &mut self,
        task_id: &str,
        success: bool,
        elapsed_secs: f32,
    ) -> bool {
        transcript::settle_detached(&mut self.turn.live.tools, task_id, success, elapsed_secs)
    }

    /// Run a turn on the open conversation for a finished background task.
    ///
    /// Same bookkeeping as `drain_pending`: the resumed turn must look like any
    /// other turn, so `live.begin` seeds the panel and `settle.begin` counts the
    /// row the answer will land in. Without a service there is nowhere to send
    /// it, so it queues and drains on `Ready` instead of vanishing.
    pub(crate) fn resume_finished_task(
        &mut self,
        session_id: Uuid,
        context_text: String,
        display_text: String,
        cx: &mut Context<Self>,
    ) {
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: key it to the session it finished on, so it
            // resumes when the stack is ready (#2629).
            agent_bridge::queue_push(
                session_id,
                agent_bridge::PendingMessage::system(context_text, display_text),
            );
            return;
        };
        self.turn.live.begin(display_text.clone());
        // A new turn re-arms the follow, the terminal's own rule (#2698).
        self.rearm_transcript_follow();
        self.chat_state.transcript.live_expanded = false;
        self.turn.settle.begin(self.chat_state.row_total());
        self.turn.busy = true;
        let (hint, panes) = (self.composer_hint(), self.pane_editors(cx));
        self.composer.sync_composer_hint(hint, &panes, cx);
        let cancel = tokio_util::sync::CancellationToken::new();
        self.turn.cancel = Some(cancel.clone());
        let attached = editor::marker_paths(&context_text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session_id,
            context_text,
            Some(display_text.clone()),
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), session_id, attached);
        }
        cx.notify();
    }

    /// Whether `session` already has a turn in flight in this window: the
    /// conversation on screen while it is busy, a split leaf still working, or
    /// a turn parked in the stash (#2704).
    ///
    /// A completion for one of those joins the session's queue instead of
    /// starting a second turn over it, which is the rule the terminal applies
    /// to its own per-session state.
    pub(crate) fn running_here(&self, session: Uuid, cx: &App) -> bool {
        if self.chat_session == Some(session) && self.turn.busy {
            return true;
        }
        // Drawn or parked: a leaf switched to another conversation is still
        // running this one's turn (#2854).
        if self
            .pane_view(session)
            .is_some_and(|pane| pane.read(cx).busy)
        {
            return true;
        }
        // A parked card that has ended is not work in flight (#2811). Its
        // entry outlives the turn so the stop survives a switch, and reading
        // presence as work would queue the completion instead of resuming it.
        self.stashed.get(&session).is_some_and(|parked| parked.busy)
    }

    /// Start the turn a completion asks for on a conversation nobody is
    /// looking at, parking its card under that session (#2704).
    ///
    /// The terminal resumes the owning session wherever the user is looking,
    /// so the window does too instead of logging the receipt away. The parked
    /// turn is the same shape a switch leaves behind (#2616): `swap_visible_turn`
    /// brings it back whole when that conversation is opened.
    pub(crate) fn resume_off_screen_task(
        &mut self,
        session_id: Uuid,
        context_text: String,
        display_text: String,
        cx: &mut Context<Self>,
    ) {
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: key it to the session it finished on, so it
            // resumes when the stack is ready (#2629).
            agent_bridge::queue_push(
                session_id,
                agent_bridge::PendingMessage::system(context_text, display_text),
            );
            return;
        };
        let mut live = transcript::LiveTurn::default();
        live.begin(display_text.clone());
        let cancel = tokio_util::sync::CancellationToken::new();
        self.stashed.insert(
            session_id,
            // The rows land under a conversation that is not on screen, so no
            // reload has to be proven here; opening it reads them.
            chat_leaf::Turn::running(live, cancel.clone()),
        );
        let attached = editor::marker_paths(&context_text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session_id,
            context_text,
            Some(display_text.clone()),
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), session_id, attached);
        }
        // The rail says the session is working from the first frame, the same
        // signal a visible turn gives (#2666, #2636).
        self.refresh_rail(cx);
        cx.notify();
    }

    /// What the composer says about the state it is in: nothing running, a turn
    /// running, a queue waiting on it, or a queue already on its way. The words
    /// live under the caret because that is where the user is typing, and the
    /// queue card is a list, not an instruction manual (#2630).
    pub(crate) fn composer_hint(&self) -> SharedString {
        composer_hint_for(self.turn.busy, self.queued_for_strip().len())
    }

    /// Every split pane's own composer. The window's field lives on the chat
    /// leaf, so a hint that has to reach the panes is handed this list rather
    /// than reaching into the split tree itself (#2805).
    pub(crate) fn pane_editors(&self, cx: &App) -> Vec<Entity<editor::Editor>> {
        self.pane_states
            .values()
            .map(|pane| pane.read(cx).editor.clone())
            .collect()
    }

    /// A composer just sent its field. The field holds every message its recall
    /// took, joined into one edit, so the whole text is the one going out and
    /// nothing goes back to wait. Other composers release their whole recall
    /// (#2637, #2696).
    pub(crate) fn finish_recall(&mut self, sent: &Entity<editor::Editor>, cx: &mut Context<Self>) {
        let mut editors = vec![self.composer.editor.clone()];
        editors.extend(self.pane_editors(cx));
        for editor in editors {
            let keep = editor.entity_id() == sent.entity_id();
            let returned = editor.update(cx, |ed, _| ed.release_recall(keep));
            if let Some((session, msgs)) = returned {
                for msg in msgs {
                    agent_bridge::queue_recall_return(session, msg);
                }
            }
        }
    }

    /// What the strip shows, oldest first: the text waiting on the open
    /// session, then anything typed before a session existed. One store backs
    /// both, so what the user reads is exactly what will send (#2629).
    pub(crate) fn queued_for_strip(&self) -> Vec<agent_bridge::PendingMessage> {
        let mut out = self.composer.unsent.clone();
        if let Some(sid) = self.chat_session {
            out.extend(agent_bridge::queue_snapshot(sid));
        }
        out
    }

    /// The offered labels as drawable rows, each minted with the session it
    /// belongs to so a pick sends into that conversation and nowhere else
    /// (#2708). The offer is taken as the click lands: the buttons belong to
    /// the turn that made them, so a second press must not send a stale one.
    pub(crate) fn option_rows(
        &self,
        session: Uuid,
        offers: Vec<SuggestionItem>,
        cx: &mut Context<Self>,
    ) -> Vec<(SuggestionItem, transcript::ClickHandler)> {
        offers
            .into_iter()
            .map(|item| {
                let label = item.label.clone();
                let pick: transcript::ClickHandler = Rc::new(cx.listener(
                    move |this: &mut Self, _ev: &gpui::ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        agent_bridge::options_take(session);
                        this.send_turn_to(session, label.clone(), None, cx);
                    },
                ));
                (item, pick)
            })
            .collect()
    }

    pub(crate) fn drain_pending(&mut self, cx: &mut Context<Self>) {
        if self.turn.busy {
            return;
        }
        // Boot text has no session to live in yet. It joins the open one here,
        // so there is never a second list the agent cannot see (#2629).
        if let Some(sid) = self.chat_session
            && !self.composer.unsent.is_empty()
        {
            for msg in std::mem::take(&mut self.composer.unsent) {
                agent_bridge::queue_push(sid, msg);
            }
        }
        let waiting = match self.chat_session {
            Some(sid) => agent_bridge::queue_snapshot(sid),
            None => self.composer.unsent.clone(),
        };
        if waiting.is_empty() {
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: keep the text. Clearing it here dropped whatever
            // the user typed whenever the window polled before the stack came
            // up (#2629).
            return;
        };
        // No open conversation: keep the queue and open one. The queue drains
        // again when that session lands (`SessionCreated`).
        let Some(sid) = self.chat_session else {
            self.new_session(cx);
            return;
        };
        let msgs = agent_bridge::queue_take(sid);
        if let Some(joined) = agent_bridge::PendingMessage::join(&msgs) {
            // The flush receipt: how many queued messages ride out in this
            // one turn, so a first-only report can be read against the pushes
            // and takes that precede it (#2695).
            logging::info(&format!(
                "queue flush: session {sid}, sending {} queued as one turn",
                msgs.len()
            ));
            self.turn.live.begin(joined.display_text.clone());
            // A flushed queue is a send, so it follows its own answer (#2698).
            self.rearm_transcript_follow();
            self.chat_state.transcript.live_expanded = false;
            self.turn.settle.begin(self.chat_state.row_total());
            self.turn.busy = true;
            let (hint, panes) = (self.composer_hint(), self.pane_editors(cx));
            self.composer.sync_composer_hint(hint, &panes, cx);
            let cancel = tokio_util::sync::CancellationToken::new();
            self.turn.cancel = Some(cancel.clone());
            let attached = editor::marker_paths(&joined.context_text);
            agent_bridge::spawn_turn(
                bridge.service.clone(),
                sid,
                joined.context_text,
                Some(joined.display_text),
                self.sink.clone(),
                cancel,
            );
            if !attached.is_empty() {
                agent_bridge::spawn_track_files(self.db_path.clone(), sid, attached);
            }
        }
        cx.notify();
    }

    /// Create a conversation and open it. The rail is refreshed from the
    /// ledger, never patched by hand: the new row is real data or it does
    /// not appear.
    pub(crate) fn new_session(&mut self, cx: &mut Context<Self>) {
        // Same inheritance rule as the TUI: the row carries the provider and
        // model the service is actually answering with, so a later read of
        // the session cannot report a model that never ran.
        //
        // It inherits the pair from the conversation on screen, not from the
        // global slot. The global slot follows the config `enabled` flags and
        // every config reload swaps it (agent_bridge provider_hook), so
        // reading it here started each new conversation on a provider the
        // user had already switched away from (#2643).
        let active = self.focused_pane();
        let (provider, model) = match agent_bridge::bridge() {
            Some(b) => match active {
                Some(sid) => (
                    Some(b.service.provider_name_for_session(sid)),
                    Some(b.service.provider_model_for_session(sid)),
                ),
                None => (
                    Some(b.service.provider_name()),
                    Some(b.service.provider_model()),
                ),
            },
            None => (None, None),
        };
        agent_bridge::spawn_create_session(
            self.db_path.clone(),
            self.sink.clone(),
            provider,
            model,
        );
        // A new conversation is about to be typed into, so the rail gives the
        // keyboard up even when the click came from its own button (#2665).
        self.rail_focused = false;
        self.refocus_screen();
        cx.notify();
    }

    /// Pin or unpin one conversation, then re-read the pinned set from the
    /// ledger. The star flips on the click so the rail never lags a slow
    /// write, and the refetch replaces the guess with what was stored.
    pub(crate) fn toggle_favorite(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let pinned = self.sessions.is_favorite(id);
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_favorite(db_path, id, !pinned).await {
                logging::error(&format!("favorite {id}: {e}"));
                this.update(cx, |this, cx| {
                    this.turn.live.notices.push(format!("favorite: {e}"));
                    cx.notify();
                })
                .ok();
                return;
            }
            this.update(cx, |this, cx| this.refresh_favorites(cx)).ok();
        })
        .detach();
        cx.notify();
    }

    /// Turn topics on for a session from the rail menu (#2302). The write is
    /// durable: the flag is read back from the database, never kept only here.
    pub(crate) fn enable_topics(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.enable_topics_then(id, |_, _| {}, cx);
    }

    /// Turn topics on, then run `after` once the read-back has landed (#2604).
    ///
    /// The seeker's pick can name a conversation that never had topics on, and
    /// a prompt opened before the flag reaches `sessions.topics` is dropped by
    /// the same guard the rail menu never hits. `after` runs inside the update
    /// that stores the read-back, so it cannot outrun the flag it depends on.
    pub(crate) fn enable_topics_then(
        &mut self,
        id: Uuid,
        after: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_topics(db_path.clone(), id, true).await {
                logging::error(&format!("enable topics {id}: {e}"));
                this.update(cx, |this, cx| {
                    this.turn.live.notices.push(format!("enable topics: {e}"));
                    cx.notify();
                })
                .ok();
                return;
            }
            let result = sessions::load_topics(db_path).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(ids) => {
                        this.sessions.topics = ids.into_iter().collect();
                        after(this, cx);
                    }
                    Err(e) => logging::error(&format!("topics refresh: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Arm the disable dialog (#2863). Turning topics off folds every topic
    /// into this conversation, so it is never one click: the menu arms the
    /// dialog and the dialog's own button does the fold.
    pub(crate) fn request_disable_topics(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm_disable_topics = Some(id);
        cx.notify();
    }

    /// Fold the topics into the session and switch the flag off, then rebuild
    /// what the rail and the strips read (#2863).
    ///
    /// The refresh is the whole point: the folded topics are archived rows and
    /// the parent's flag is now 0, so a rail that only cleared its own dialog
    /// would keep drawing the family it just dissolved.
    pub(crate) fn disable_topics(&mut self, id: Uuid, cx: &mut Context<Self>) {
        // A topic can be the conversation on screen while its family is
        // switched off from the rail, and the fold archives that row. The
        // open chat moves back to the session it was folded into, so the
        // window is never left drawing a row that no longer exists (#2863).
        let open_was_topic = self
            .chat_session
            .is_some_and(|open| self.sessions.parents.get(&open) == Some(&id));
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            match sessions::merge_topics(db_path, id).await {
                Ok(folded) => {
                    this.update(cx, |this, cx| {
                        logging::info(&format!("topics disabled on {id}: folded {folded}"));
                        this.refresh_rail(cx);
                        this.refresh_topic_state(cx);
                        if open_was_topic {
                            this.open_session(id, cx);
                        }
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    logging::error(&format!("disable topics {id}: {e}"));
                    this.update(cx, |this, cx| {
                        this.turn.live.notices.push(format!("disable topics: {e}"));
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Re-read the pinned ids. A separate fetch from the rows because the
    /// sidecar can change on its own (the TUI pins too).
    pub(crate) fn refresh_favorites(&mut self, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = sessions::load_favorites(db_path).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(ids) => this.sessions.favorites = ids.into_iter().collect(),
                    Err(e) => logging::error(&format!("favorites refresh: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Arm the delete dialog. Every delete path (a row's menu, and nothing
    /// else) lands here instead of erasing, so the erase is always the second,
    /// deliberate click.
    pub(crate) fn request_delete(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm_delete = Some(id);
        cx.notify();
    }

    /// Open a file a tile stands for. The overlay decides what to draw from the
    /// path alone, so this stays one assignment: no state is derived at click
    /// time that could disagree with what the overlay renders (#2395).
    pub(crate) fn open_preview(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let open = preview::Preview::of(path);
        // A clip is the only kind whose overlay needs the disk: a poster frame
        // has to be extracted, and its length read. Doing it here, on the
        // click, is what keeps ffmpeg off the render path.
        self.preview_video = if open.kind == preview::PreviewKind::Video {
            Some(preview::VideoFacts::probe(
                &opencrabs::config::opencrabs_home(),
                &open.path,
            ))
        } else {
            None
        };
        // One line per open, so a click that lands and a click that never
        // arrives can be told apart in the GUI log instead of guessed at
        // (#2395). The path is the marker's own, which is the string the
        // overlay will draw.
        logging::info(&format!(
            "preview: open {} ({:?})",
            open.path.display(),
            open.kind
        ));
        self.preview = Some(open);
        self.preview_zoom = 1.0;
        cx.notify();
    }

    /// The inside of the preview: the part that differs per kind. The shell
    /// (title, kind, esc) is the same for all three, so a picture, a clip and
    /// a document cannot drift apart in how they open or close.
    pub(crate) fn preview_body(
        &self,
        open: &preview::Preview,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let path = open.path.clone();
        let open_file = {
            let path = path.clone();
            cx.listener(move |_this, _ev, _window, _cx| {
                preview::launch(&preview::open_command(&path));
            })
        };
        let reveal_file = {
            let path = path.clone();
            cx.listener(move |_this, _ev, _window, _cx| {
                preview::launch(&preview::reveal_command(&path));
            })
        };
        let wheel = cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
            let lines = match ev.delta {
                ScrollDelta::Lines(lines) => lines.y,
                // A pixel delta is a trackpad: a hundredth of a notch per
                // pixel keeps a fast flick from jumping the whole range.
                ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / 100.0,
            };
            let next = preview::zoom_after_scroll(this.preview_zoom, lines);
            if next != this.preview_zoom {
                this.preview_zoom = next;
                cx.notify();
            }
        });
        preview::body(
            open,
            preview::actions_row(
                open.open_label(),
                open.path.exists(),
                open_file,
                reveal_file,
            ),
            self.preview_zoom,
            wheel,
            self.preview_video.as_ref(),
        )
    }

    /// Delete a session, then let the rail fetch the truth back from the
    /// ledger. The row disappears because the row is gone, not because a
    /// local list was filtered.
    pub(crate) fn delete_session(&mut self, id: Uuid, cx: &mut Context<Self>) {
        agent_bridge::spawn_delete_session(self.db_path.clone(), id, self.sink.clone());
        // A deleted conversation must not keep a draft waiting for a composer
        // it can never reach. Its pane's editor leaves with the conversation.
        self.composer.drafts.remove(id);
        self.pane_states.remove(&id);
        // A parked pane of a deleted conversation must not come back to a
        // session that no longer exists (#2854).
        self.parked_panes.remove(&id);
        // The pin is part of the row: it leaves with the row.
        self.sessions.favorites.retain(|f| *f != id);
        cx.notify();
    }

    /// Open the rename prompt for a topic, pre-filled with its title
    /// (#2304). Enter saves the new name; Esc leaves the title as it was.
    ///
    /// The id comes from the caller: the menu acts on the topic its own row
    /// opened it for, which is not always the one on screen.
    pub(crate) fn start_rename(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let current = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_default();
        self.topic_menu = None;
        self.topic_rename = Some(id);
        self.topic_draft = Some(current);
        // The dialog opens on the session's own icon, so a rename that never
        // touches the picker keeps it (#2791). No stored icon means None, and
        // the default the badge shows is never written back over nothing.
        self.topic_draft_icon = self.sessions.topic_icons.get(&id).cloned();
        cx.notify();
    }

    /// Load the info panel for a session. The panel is read-only: it shows
    /// the id, the parent, the created time, and the message count.
    pub(crate) fn open_topic_info(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(
            async move |this, cx| match sessions::load_topic_info(db_path, id).await {
                Ok(Some(info)) => {
                    this.update(cx, |this, cx| {
                        this.topic_info = Some(info);
                        cx.notify();
                    })
                    .ok();
                }
                Ok(None) => {}
                Err(e) => {
                    logging::error(&format!("topic info {id}: {e}"));
                    this.update(cx, |this, cx| {
                        this.turn.live.notices.push(format!("topic info: {e}"));
                        cx.notify();
                    })
                    .ok();
                }
            },
        )
        .detach();
        cx.notify();
    }

    /// Close a topic, or reopen it if it is closed (#2305). The state is
    /// re-read from the database, so the dimming and the send guard follow
    /// what was actually written.
    pub(crate) fn toggle_closed_topic(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let closed = self.sessions.is_closed(id);
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_closed(db_path, id, !closed).await {
                logging::error(&format!("close topic {id}: {e}"));
                this.update(cx, |this, cx| {
                    this.turn.live.notices.push(format!("close topic: {e}"));
                    cx.notify();
                })
                .ok();
                return;
            }
            this.update(cx, |this, cx| this.refresh_topic_state(cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// Whether the open chat is a session with topics turned on.
    pub(crate) fn open_chat_topics_on(&self) -> bool {
        self.chat_session
            .is_some_and(|id| self.sessions.is_topic_enabled(id) && self.family_has_topics(id))
    }

    /// Whether a session still has at least one live topic under it (#2863).
    ///
    /// The flag alone is not the answer: a session can carry `topics_enabled`
    /// with every topic archived or folded, and the strip then draws a bar
    /// whose only chip is `All`. A family with no topics is not a family, so
    /// the strip asks this before it paints anything.
    pub(crate) fn family_has_topics(&self, root: Uuid) -> bool {
        self.sessions.has_live_topics(root)
    }

    /// The session a new topic hangs under (#2686): the leaf that owns the
    /// keyboard, or the chat leaf when the window is unsplit. A split used to
    /// create the topic under the chat's own session whatever pane was focused,
    /// and refused outright when that leaf had topics off.
    pub(crate) fn topic_parent(&self) -> Option<Uuid> {
        self.split_focus().or(self.chat_session)
    }

    /// The conversation a new topic would hang under, or `None` when nothing
    /// on screen can hold one (#2605).
    ///
    /// [`Self::topic_parent`] answers "which pane asked". This answers "can
    /// that pane hold a topic", and the two used to disagree: the menu offered
    /// New topic because the chat leaf had topics on, the draft then bailed
    /// because the focused pane did not, the menu closed, and the typed name
    /// went to the composer and got sent as a message. A prompt that cannot
    /// create anything must never open, so both the row and the prompt resolve
    /// through here.
    pub(crate) fn topic_draft_parent(&self) -> Option<Uuid> {
        if let Some(id) = self.topic_parent()
            && self.sessions.is_topic_enabled(id)
        {
            return Some(id);
        }
        self.chat_session
            .filter(|id| self.sessions.is_topic_enabled(*id))
    }
}
