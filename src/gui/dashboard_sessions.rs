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
        if agent_bridge::bridge().is_none() {
            // Still booting: key it to the session it finished on, so it
            // resumes when the stack is ready (#2629).
            agent_bridge::queue_push(
                session_id,
                agent_bridge::PendingMessage::system(context_text, display_text),
            );
            return;
        }
        self.turn.live.begin(display_text.clone());
        // A new turn re-arms the follow, the terminal's own rule (#2698).
        self.rearm_transcript_follow();
        self.chat_state.transcript.live_expanded = false;
        self.turn.settle.begin(self.chat_state.row_total());
        self.turn.busy = true;
        let cancel = tokio_util::sync::CancellationToken::new();
        self.turn.cancel = Some(cancel.clone());
        let attached = editor::marker_paths(&context_text);
        agent_bridge::spawn_turn(
            self.backend.clone(),
            session_id,
            context_text,
            Some(display_text.clone()),
            // A turn the window mints for itself is text: an approved plan and
            // a command's reply are read, never spoken (#3096).
            false,
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.backend.clone(), session_id, attached);
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
        if agent_bridge::bridge().is_none() {
            // Still booting: key it to the session it finished on, so it
            // resumes when the stack is ready (#2629).
            agent_bridge::queue_push(
                session_id,
                agent_bridge::PendingMessage::system(context_text, display_text),
            );
            return;
        }
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
            self.backend.clone(),
            session_id,
            context_text,
            Some(display_text.clone()),
            // A turn the window mints for itself is text: an approved plan and
            // a command's reply are read, never spoken (#3096).
            false,
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.backend.clone(), session_id, attached);
        }
        // The rail says the session is working from the first frame, the same
        // signal a visible turn gives (#2666, #2636).
        self.refresh_rail(cx);
        cx.notify();
    }

    /// The hint the empty field shows: the two keys that act on the field
    /// itself (#3102).
    ///
    /// These did not move to the card above the composer. Esc stops the turn
    /// being written into the field, the chord records a message into it, and
    /// both read best where the caret already is.
    pub(crate) fn composer_hint(&self) -> SharedString {
        crate::composer_view::composer_hint_for(
            self.turn.busy,
            self.queued_for_strip().len(),
            crate::composer_view::voice_ready(),
        )
    }

    /// Paint the field's hint on the window's composer and every split pane's.
    ///
    /// A no-op when the line did not change, so a tick cannot repaint every
    /// field in the window for nothing (#2630).
    pub(crate) fn sync_composer_hint(&mut self, cx: &mut Context<Self>) {
        let hint = self.composer_hint();
        let panes = self.pane_editors(cx);
        self.composer.sync_composer_hint(hint, &panes, cx);
    }

    /// The line the card under the conversation paints right now (#3121).
    ///
    /// It has an intent: it names what the screen is asking for at this moment,
    /// and nothing else. The keys that act on the field itself stay inside the
    /// field, so the two never compete for the same glance.
    ///
    /// It comes back with its id, because the card can now be dismissed and a
    /// dismissal needs to know which line the reader hid. `None` when the work
    /// has nothing to say, which paints no card at all rather than an empty
    /// one.
    pub(crate) fn task_hint(&self) -> Option<opencrabs::ui_core::hints::Hint> {
        crate::composer_view::task_hint_for(&self.hint_state())
    }

    /// Hide the hint the card is showing, for seven days.
    ///
    /// The window stops drawing it immediately and the ledger write happens on
    /// its own thread, so the click never waits on a database open. Seven days
    /// is the temporary half of the promise the card makes: long enough that
    /// the reader stops being nagged, short enough that a hint they later want
    /// comes back on its own instead of being lost for good.
    pub(crate) fn dismiss_hint(&mut self, hint_id: &str, cx: &mut Context<Self>) {
        if !self.hint_dismissed.iter().any(|id| id == hint_id) {
            self.hint_dismissed.push(hint_id.to_string());
        }
        let backend = self.backend.clone();
        let hint_id = hint_id.to_string();
        crate::hint_dismiss::dismiss(backend, hint_id);
        cx.notify();
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
                        // A click is the pointer's version of Enter, so the
                        // keyboard's ring steps aside with the offer (#3101).
                        this.options_focus = None;
                        this.send_turn_to(session, label.clone(), None, cx);
                    },
                ));
                (item, pick)
            })
            .collect()
    }

    /// Up from a composer with an offer waiting hands the keyboard to the card
    /// above it (#3101). With nothing to walk the field keeps the caret, and Up
    /// keeps its history: an empty card must never eat the key.
    pub(crate) fn focus_options(&mut self, session: Uuid, cx: &mut Context<Self>) {
        if agent_bridge::options_snapshot(session).is_empty() {
            return;
        }
        self.options_focus = Some((session, 0));
        self.refocus_overlay();
        cx.notify();
    }

    /// One key while the offer holds the keyboard: the arrows walk it, Enter
    /// sends the row the ring stands on, Esc hands the keys back to the field
    /// without sending anything, and so does Down past the last row, because a
    /// list whose bottom is a wall has no exit the screen ever names (#3114).
    /// Any other key hands them back too, because it means the user went back
    /// to typing and the field is where typing lands.
    pub(crate) fn options_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        let Some((session, at)) = self.options_focus else {
            return;
        };
        let offers = agent_bridge::options_snapshot(session);
        // The offer can leave under the keyboard: a stop takes it, and a new
        // turn takes it. Hand the keys back rather than walking a list that is
        // no longer on screen.
        if offers.is_empty() {
            self.options_focus = None;
            self.request_composer_caret(cx);
            return;
        }
        match ks.key.as_str() {
            "up" => self.options_focus = Some((session, at.saturating_sub(1))),
            // Down walks the list and then walks out of it (#3114). The last
            // row used to be a wall: the ring stayed on the card, and only Esc
            // or a key that meant "back to typing" got the caret back, so a
            // reader who followed the hint down had no way out that the screen
            // ever named. Up from the field reaches the card, so Down from its
            // last row hands the keys back the same way.
            "down" if at + 1 >= offers.len() => {
                self.options_focus = None;
                self.request_composer_caret(cx);
            }
            "down" => self.options_focus = Some((session, at + 1)),
            "enter" => {
                let Some(item) = offers.get(at) else {
                    self.options_focus = None;
                    self.request_composer_caret(cx);
                    return;
                };
                let label = item.label.clone();
                // The offer belongs to the turn that made it, so it is taken as
                // the send lands: a second press must not send a stale one.
                agent_bridge::options_take(session);
                self.options_focus = None;
                self.request_composer_caret(cx);
                self.send_turn_to(session, label, None, cx);
                return;
            }
            "escape" => {
                self.options_focus = None;
                self.request_composer_caret(cx);
            }
            // Anything else is the user going back to typing, so the ring
            // steps aside and the field takes the caret again. The key itself
            // is dropped rather than guessed at: the card never had a caret to
            // put it in, and typing a stray letter into a message behind the
            // ring would be the worse surprise.
            _ => {
                self.options_focus = None;
                self.request_composer_caret(cx);
            }
        }
        cx.notify();
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
        if agent_bridge::bridge().is_none() {
            // Still booting: keep the text. Clearing it here dropped whatever
            // the user typed whenever the window polled before the stack came
            // up (#2629).
            return;
        }
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
            let cancel = tokio_util::sync::CancellationToken::new();
            self.turn.cancel = Some(cancel.clone());
            let attached = editor::marker_paths(&joined.context_text);
            agent_bridge::spawn_turn(
                self.backend.clone(),
                sid,
                joined.context_text,
                Some(joined.display_text),
                // A flushed queue is typed text, like the window's own (#3096).
                false,
                self.sink.clone(),
                cancel,
            );
            if !attached.is_empty() {
                agent_bridge::spawn_track_files(self.backend.clone(), sid, attached);
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
        let (provider, model) = match self.backend.facts(active) {
            Some(facts) => (Some(facts.provider), Some(facts.model)),
            None => (None, None),
        };
        agent_bridge::spawn_create_session(
            self.backend.clone(),
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
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_favorite(backend, id, !pinned).await {
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
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_topics(backend.clone(), id, true).await {
                logging::error(&format!("enable topics {id}: {e}"));
                this.update(cx, |this, cx| {
                    this.turn.live.notices.push(format!("enable topics: {e}"));
                    cx.notify();
                })
                .ok();
                return;
            }
            let result = sessions::load_topics(backend).await;
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
        let backend = self.backend.clone();
        cx.spawn(
            async move |this, cx| match sessions::merge_topics(backend, id).await {
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
            },
        )
        .detach();
        cx.notify();
    }

    /// Re-read the pinned ids. A separate fetch from the rows because the
    /// sidecar can change on its own (the TUI pins too).
    pub(crate) fn refresh_favorites(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = sessions::load_favorites(backend).await;
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

    /// A transcript reports its resolved target here so local files share the
    /// attachment overlay and only external destinations reach the OS opener.
    pub(crate) fn open_transcript_target(
        &mut self,
        target: opencrabs::ui_core::links::ClickTarget,
        cx: &mut Context<Self>,
    ) {
        match target {
            opencrabs::ui_core::links::ClickTarget::File(path) => self.open_preview(path, cx),
            opencrabs::ui_core::links::ClickTarget::Url(url) => {
                preview::launch(&preview::url_command(&url));
            }
        }
    }

    /// Open a file a tile or link stands for. Images and clips keep their
    /// existing path-driven behavior; supported documents start a guarded
    /// background read and parse below (#3019).
    pub(crate) fn open_preview(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let open = preview::Preview::of(path);
        let path = open.path.clone();
        let generation = self.preview_document_generation.wrapping_add(1);
        self.preview_document_generation = generation;
        let document_kind = opencrabs::ui_core::local_document::DocumentKind::of_path(&path);
        self.preview_document = document_kind
            .map(|_| preview_document::DocumentState::loading(path.clone(), generation));
        // A clip's poster frame and length are still probed on the click, as
        // before. Document bytes and Markdown parsing use the async path below.
        self.preview_video = if open.kind == preview::PreviewKind::Video {
            Some(preview::VideoFacts::probe(
                &self.backend,
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

        if let Some(kind) = document_kind {
            cx.spawn(async move |this, cx| {
                let load_path = path.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { preview_document::parse(&load_path, kind) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    let visible = this.preview.as_ref().is_some_and(|open| open.path == path);
                    let current = this
                        .preview_document
                        .as_ref()
                        .is_some_and(|document| document.matches(&path, generation));
                    if visible && current {
                        if let Some(document) = this.preview_document.as_mut() {
                            document.complete(result);
                        }
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    pub(crate) fn close_preview(&mut self) {
        self.preview_document_generation = self.preview_document_generation.wrapping_add(1);
        self.preview = None;
        self.preview_video = None;
        self.preview_document = None;
    }

    /// Read a long message in the document reader (#3053). When the window has
    /// written the row's markdown to the conversation's project files, the
    /// reader opens THAT file (#3064): the card on the row names it, so reading
    /// the file is the card keeping its word. A row the window has no file for
    /// falls back to the text it holds, because a reader that refuses what the
    /// row already shows is worse than one with no file behind it.
    pub(crate) fn open_message_preview(
        &mut self,
        id: uuid::Uuid,
        title: String,
        content: String,
        cx: &mut Context<Self>,
    ) {
        let generation = self.preview_document_generation.wrapping_add(1);
        self.preview_document_generation = generation;
        self.preview = Some(preview::Preview::message(title));
        self.preview_document = Some(preview_document::DocumentState::text(generation));
        self.preview_video = None;
        self.preview_zoom = 1.0;
        let path = self.prompt_docs.get(&id).cloned();
        let path = path.map(|path| self.backend.localize(&path));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let parsed = cx
                .background_executor()
                .spawn(async move {
                    // A file the window wrote is the same bytes as the row, so
                    // the read is only about which copy is authoritative. It
                    // stays on this half of the work: a failed read falls back
                    // to the row's own text instead of an error the reader
                    // cannot do anything with.
                    let body = match path {
                        Some(path) => std::fs::read_to_string(path).unwrap_or(content),
                        None => content,
                    };
                    // Verbatim is what the row itself draws, so the reader
                    // shows the same line breaks the reader typed.
                    preview_document::parse_text(&body, crate::markdown::MdFlow::Verbatim)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let current = this
                    .preview_document
                    .as_ref()
                    .is_some_and(|document| document.matches_generation(generation));
                if current {
                    if let Some(document) = this.preview_document.as_mut() {
                        document.complete(Ok(parsed));
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The inside of the preview: the part that differs per kind. The shell
    /// (title, kind, esc) is the same for all four, so a picture, a clip, a
    /// document and a message cannot drift apart in how they open or close.
    pub(crate) fn preview_body(
        &self,
        open: &preview::Preview,
        document_height: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // A message has no file behind it, so the two file actions would be
        // dead buttons (#3053).
        let actions = if open.kind == preview::PreviewKind::Message {
            None
        } else {
            // A crab on another machine is not trusted by this one: the
            // system opener only ever gets what `openable` answers (the
            // local crab's own path, or a remote crab's kept copy of a kind
            // that is shown rather than run). Reveal shows the folder of the
            // path the window holds here, which for a remote crab is its
            // cache directory, never a place the crab named.
            let target = self.backend.openable(&open.path);
            let exists = self.backend.localize(&open.path).exists();
            let refused = match &target {
                Err(notice) if exists => Some(*notice),
                _ => None,
            };
            let open_file = cx.listener(move |_this, _ev, _window, _cx| match &target {
                Ok(path) => preview::launch(&preview::open_command(path)),
                Err(notice) => logging::warn(&format!("preview: {notice}")),
            });
            let reveal_file = {
                let path = self.backend.revealable(&open.path);
                cx.listener(move |_this, _ev, _window, _cx| {
                    preview::launch(&preview::reveal_command(&path));
                })
            };
            let row = preview::actions_row(
                open.open_label(),
                exists && refused.is_none(),
                open_file,
                reveal_file,
            );
            Some(match refused {
                Some(notice) => div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child(notice),
                    )
                    .child(row)
                    .into_any_element(),
                None => row,
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
        let middle_click = cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
            this.middle_scroll_down(middle_scroll::MiddleScrollTarget::Preview, ev.position, cx);
        });
        let document = self
            .preview_document
            .as_ref()
            .map(|state| preview::DocumentBody {
                state,
                height: document_height,
            });
        preview::body(
            &self.backend,
            open,
            actions,
            self.preview_zoom,
            wheel,
            self.preview_video.as_ref(),
            document,
            middle_click,
        )
    }

    /// Delete a session, then let the rail fetch the truth back from the
    /// ledger. The row disappears because the row is gone, not because a
    /// local list was filtered.
    pub(crate) fn delete_session(&mut self, id: Uuid, cx: &mut Context<Self>) {
        agent_bridge::spawn_delete_session(self.backend.clone(), id, self.sink.clone());
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
        // The old name is there to edit, not to select: the caret lands at
        // the end so typing appends the way it did before, and everything
        // else the prompt can now do starts from a real field (#2892).
        self.topic_draft = Some(TextField::from_text(&current));
        // The dialog opens on the session's own icon, so a rename that never
        // touches the picker keeps it (#2791). No stored icon means None, and
        // the default the badge shows is never written back over nothing.
        self.topic_draft_icon = self.sessions.topic_icons.get(&id).cloned();
        cx.notify();
    }

    /// Load the info panel for a session. The panel is read-only: it shows
    /// the id, the parent, the created time, and the message count.
    pub(crate) fn open_topic_info(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(
            async move |this, cx| match sessions::load_topic_info(backend, id).await {
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
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_closed(backend, id, !closed).await {
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
