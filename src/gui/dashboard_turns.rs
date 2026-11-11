//! The turn pipeline: sending from a pane, folding a streamed turn,
//! settling it, failing it, and the poll that drives all of it.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// A pane's own send. `display` is the row the reader gets when the text is
    /// scaffolding the model needs and the user does not (#2756).
    pub(crate) fn send_from_pane(
        &mut self,
        session: Uuid,
        text: String,
        display: Option<String>,
        origin: SendOrigin,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // A send under an open overlay is the composer answering keys nobody
        // gave it (#2767). The caret repair keeps the composer out of the
        // dispatch path, and this refuses the one path that got around it: a
        // name typed into the topic prompt was submitted here instead, so the
        // topic was never created and the name reached the agent as a message.
        // Only a composer's own send is that case. The window mints turns of
        // its own for a named session (the plan card's Approve and Discard,
        // a command's reply turn), and chrome is not theirs to refuse: a plan
        // waiting on approval kept its tap landing nowhere while the overlay
        // that ate it stayed up.
        let overlays = self.open_overlays();
        if origin == SendOrigin::Composer
            && !overlays.is_empty()
            && matches!(self.screen, Screen::Chat)
        {
            logging::debug(&format!(
                "send refused: {:?} owns the keyboard",
                overlays.last()
            ));
            return;
        }
        // A send ends that pane's offer, the same rule the window's own
        // composer follows (#2708).
        agent_bridge::options_take(session);
        // A slash line is a screen command: `/help`, `/models` and `/sessions`
        // read and write the window's own chrome, so they run there and the
        // leaf is left where it is. A prompt is the pane's own turn (#2614).
        if let Some(outcome) = commands::parse_command(&text) {
            // Commands act on the window's chrome, but the reply belongs to
            // the field it was typed in: after the command runs, the notices
            // it wrote move to that pane, so a split never answers in the
            // wrong leaf (#2614).
            let before = self.live.notices.len();
            self.run_command(outcome, Some(session), cx);
            if Some(session) != self.transcript.session
                && let Some(pane) = self.pane_states.get(&session).cloned()
            {
                let split_at = before.min(self.live.notices.len());
                let moved: Vec<String> = self.live.notices.split_off(split_at);
                pane.update(cx, |pane, cx| {
                    pane.live.notices.extend(moved);
                    cx.notify();
                });
            }
            cx.notify();
            return;
        }
        if let Some(cmd) = opencrabs::ui_core::bang::bang_command(&text) {
            if let Some(pane) = self.pane_states.get(&session) {
                pane.update(cx, |pane, cx| {
                    pane.live.notices.push(format!("! {cmd}"));
                    cx.notify();
                });
            }
            bang::spawn(cmd.to_string(), self.sink.clone());
            cx.notify();
            return;
        }
        if let Some(err) = self.agent_error.clone() {
            if let Some(pane) = self.pane_states.get(&session) {
                pane.update(cx, |pane, cx| {
                    pane.live.notices.push(format!("agent offline: {err}"));
                    cx.notify();
                });
            }
            cx.notify();
            return;
        }
        // One turn per leaf: a second send while this pane is working joins
        // the session's queue, the terminal's own path (#2441).
        let Some(pane_busy) = self.pane_states.get(&session).map(|p| p.read(cx).busy) else {
            return;
        };
        if pane_busy {
            agent_bridge::queue_push(session, Self::pending_message(text, display.clone()));
            cx.notify();
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            agent_bridge::queue_push(session, Self::pending_message(text, display.clone()));
            cx.notify();
            return;
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let shown = display.clone().unwrap_or_else(|| text.clone());
        if let Some(pane) = self.pane_states.get(&session) {
            pane.update(cx, |pane, cx| {
                pane.live = transcript::LiveTurn::default();
                pane.live.begin(shown);
                pane.transcript.live_expanded = false;
                pane.busy = true;
                pane.cancel = Some(cancel.clone());
                // Sending in a pane puts its reader back on the end of it
                // (#2698).
                pane.follow.rearm();
                pane.scroll.scroll_to_bottom();
                cx.notify();
            });
        }
        // Attachments are tracked against the pane's session, so the file
        // lands in that conversation's artifacts, exactly as the window's does.
        let attached = editor::marker_paths(&text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session,
            text,
            display,
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), session, attached);
        }
        cx.notify();
    }

    /// What this session's card should say it is doing right now (#2718), from
    /// whichever owner holds its turn: a split leaf, a turn parked under a
    /// conversation the user switched away from, the open turn, or a turn
    /// running with no leaf at all. `None` when nothing is in flight, so the
    /// card keeps the message the ledger holds.
    ///
    /// The four sources are checked in that order because a session can only
    /// be in one of them, and the ones that own a card of their own are more
    /// specific than the rail's own map.
    pub(crate) fn rail_activity_for(&self, id: Uuid, cx: &App) -> Option<SharedString> {
        let line = |live: &transcript::LiveTurn| {
            let text = rail_activity_line(live);
            (!text.is_empty()).then(|| SharedString::from(text))
        };
        if let Some(pane) = self.pane_states.get(&id) {
            let pane = pane.read(cx);
            if pane.busy {
                return line(&pane.live);
            }
        }
        if let Some(turn) = self.stashed.get(&id) {
            return line(&turn.live);
        }
        if self.busy && self.transcript.session == Some(id) {
            return line(&self.live);
        }
        self.rail_live.get(&id).and_then(line)
    }

    /// The progress of a pane's own turn, folded into that leaf's card
    /// (#2614). The window's live state is untouched: a split shows two turns
    /// streaming at once without either one bleeding into the other.
    pub(crate) fn fold_pane_progress(
        &mut self,
        session: Uuid,
        progress: opencrabs::brain::agent::ProgressEvent,
        at: std::time::Instant,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.pane_states.get(&session).cloned() {
            pane.update(cx, |pane, cx| {
                fold_live(&mut pane.live, progress, at);
                // Rows landing under a reader who scrolled up must not drag
                // them back to the bottom (#2685, #2698).
                pane.snap_if_following();
                cx.notify();
            });
            return;
        }
        // A conversation the user switched away from keeps its turn in the
        // stash (#2616). Feeding the card there is what stops it freezing at
        // the frame the switch left: opening it again shows the tool rows and
        // the counter the turn actually reached, not a stale snapshot.
        if let Some(turn) = self.stashed.get_mut(&session) {
            fold_live(&mut turn.live, progress, at);
            cx.notify();
            return;
        }
        // A session with no leaf is what a sub-agent looks like: its
        // progress arrives with its own id and there is no pane to fold it
        // into. The event is the only trace of that turn the window holds,
        // so the rail records it here instead of dropping it (#2636).
        //
        // The card also keeps the event, not just a flag (#2718): with no
        // message persisted yet, a card that only knows "streaming" shows the
        // last thing said in a previous turn, or nothing at all, while the
        // turn is running tool calls. The same fold the panes use gives the
        // card a line that is true right now.
        let first = self.rail.observe_stream(session);
        // Read the line before folding and after it, so the repaint happens
        // when the sentence the card shows actually changes. The first
        // sighting always repaints; a repeat that leaves the line alone does
        // not (#2636), and one that moves the turn from a finished call to the
        // next one does, or the card would freeze on the first tool it saw.
        let before = self
            .rail_live
            .get(&session)
            .map(rail_activity_line)
            .unwrap_or_default();
        match self.rail_live.get_mut(&session) {
            Some(live) => fold_live(live, progress, at),
            None => {
                let mut live = transcript::LiveTurn::default();
                fold_live(&mut live, progress, at);
                self.rail_live.insert(session, live);
            }
        }
        let moved = self
            .rail_live
            .get(&session)
            .is_some_and(|live| rail_activity_line(live) != before);
        if first || moved {
            cx.notify();
        }
    }

    /// The pane's turn finished: its own card settles and its rows come back
    /// from the DB, so the window's conversation never moves (#2614).
    pub(crate) fn settle_pane(
        &mut self,
        session: Uuid,
        tokens_per_second: Option<f64>,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.pane_states.get(&session).cloned() else {
            return;
        };
        let calls = pane.update(cx, |pane, cx| {
            pane.live.finalize(tokens_per_second);
            pane.live.thinking = false;
            pane.busy = false;
            pane.cancel = None;
            // The turn ends where the reader is, not where it ended (#2698).
            pane.snap_if_following();
            cx.notify();
            pane.live.tools.len()
        });
        // The leaf's card keeps the numbers it showed in flight, the same ones
        // the window's card keeps (#2632): the ctx budget every pane shares,
        // and the calls this leaf made.
        self.pending_metrics.insert(
            session,
            transcript::TurnMetrics {
                ctx_used: self.ctx_tokens.unwrap_or(0),
                ctx_max: self.ctx_max,
                tps: tokens_per_second,
                tool_calls: calls,
            },
        );
        cx.notify();
        self.reload_pane(session, cx);
        // The leaf's turn is over: whatever is still queued for its session
        // becomes the next turn, the same flush the window runs (#2695).
        self.drain_pane_pending(session, cx);
    }

    /// The pane's turn failed: the reason lands in that leaf's notices instead
    /// of the window's (#2614).
    pub(crate) fn fail_pane(&mut self, session: Uuid, text: String, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_states.get(&session).cloned() else {
            return;
        };
        pane.update(cx, |pane, cx| {
            pane.live.notices.push(text);
            pane.live.thinking = false;
            pane.busy = false;
            pane.cancel = None;
            cx.notify();
        });
        // A failed turn still hands the queue to the next one: the text the
        // user typed behind it is not part of the failure (#2695).
        self.drain_pane_pending(session, cx);
    }

    /// A pane's turn ended (settled, failed, or stopped): everything still
    /// queued for its session goes out as ONE joined turn, the exact contract
    /// the window's `drain_pending` keeps (#2695). Before this, only the
    /// window's post-batch drain existed and it reads the window's session,
    /// so a leaf's queue survived the turn and sat in the strip until some
    /// later turn happened to drain it mid-flight.
    ///
    /// The busy gate keeps the #2631 ordering: a stop flushes when its
    /// cancellation event lands, never a second spawn into a turn still
    /// unwinding, and a user who already sent the next turn by hand is never
    /// doubled up on.
    pub(crate) fn drain_pane_pending(&mut self, session: Uuid, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_states.get(&session) else {
            return;
        };
        if pane.read(cx).busy {
            return;
        }
        let waiting = agent_bridge::queue_snapshot(session);
        if waiting.is_empty() {
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: keep the text, the same rule the window keeps.
            return;
        };
        let msgs = agent_bridge::queue_take(session);
        let Some(joined) = agent_bridge::PendingMessage::join(&msgs) else {
            return;
        };
        logging::info(&format!(
            "pane queue flush: session {session}, sending {} queued as one turn",
            msgs.len()
        ));
        let cancel = tokio_util::sync::CancellationToken::new();
        if let Some(pane) = self.pane_states.get(&session) {
            pane.update(cx, |pane, cx| {
                pane.live = transcript::LiveTurn::default();
                pane.live.begin(joined.display_text.clone());
                pane.transcript.live_expanded = false;
                pane.busy = true;
                pane.cancel = Some(cancel.clone());
                // A flushed queue is a send too, so the pane follows its answer
                // the way a typed one does (#2698).
                pane.follow.rearm();
                pane.scroll.scroll_to_bottom();
                cx.notify();
            });
        }
        let attached = editor::marker_paths(&joined.context_text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session,
            joined.context_text,
            Some(joined.display_text),
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), session, attached);
        }
        cx.notify();
    }

    /// Re-read one pane's rows from the ledger, the same read `add_pane` makes
    /// when it first draws the leaf (#2614).
    pub(crate) fn reload_pane(&mut self, session: Uuid, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, session).await;
            this.update(cx, |this, cx| match result {
                Ok(loaded) => {
                    let (hidden, turns) = transcript::turns_with_origins(
                        &loaded.rows,
                        loaded.total,
                        &this.turn_metrics,
                        &loaded.origins,
                    );
                    // The turn this leaf's card was watching has landed, so it
                    // takes the numbers it showed in flight before the rows are
                    // drawn (#2632).
                    let turns = this.claim_pane_metrics(session, turns);
                    if let Some(pane) = this.pane_states.get(&session) {
                        pane.update(cx, |pane, cx| {
                            pane.transcript.hidden = hidden;
                            pane.transcript.turns = turns;
                            // The persisted row is on screen: the live copy
                            // retires, exactly as the window's does, unless the
                            // flush already started the next turn (#2695):
                            // clearing then would wipe the card of the turn now
                            // running, the same guard the window's reload keeps.
                            if !pane.busy {
                                pane.live.clear();
                            }
                            // The persisted rows land where the reader is
                            // watching (#2698), never under a reader parked in
                            // history.
                            pane.snap_if_following();
                            cx.notify();
                        });
                    }
                    cx.notify();
                }
                Err(e) => {
                    if let Some(pane) = this.pane_states.get(&session) {
                        pane.update(cx, |pane, cx| {
                            pane.transcript.error = Some(e.into());
                            pane.scroll.scroll_to_bottom();
                            cx.notify();
                        });
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Send one turn to the session it belongs to: the window's chat when that
    /// is the session, otherwise the split leaf drawing it (#2672).
    ///
    /// A command typed into a pane resolved its session at the door and then
    /// the turn it asked for fell through to the window's chat, so `/execute`
    /// in pane B approved B's plan and ran the seeded checklist in A. Both
    /// paths already own the queue and busy gates for their leaf, so the turn
    /// keeps the conversation it was asked for.
    pub(crate) fn send_turn_to(
        &mut self,
        session: Uuid,
        text: String,
        display: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if Some(session) == self.transcript.session {
            self.send_message_showing(text, display, cx);
        } else {
            // The window asked for this turn, not a field: the overlay gate
            // belongs to composer sends and must not eat it (#2795).
            self.send_from_pane(session, text, display, SendOrigin::Window, cx);
        }
    }

    /// Dispatch the one turn a window command asked for, into the session that
    /// ran it (#2672). With no session open the window's own path keeps the
    /// text: it waits in the queue and a conversation is created, exactly as a
    /// composer send with nothing on screen does.
    pub(crate) fn send_command_turn(
        &mut self,
        session: Option<Uuid>,
        text: String,
        display: Option<String>,
        cx: &mut Context<Self>,
    ) {
        match session {
            Some(session) => self.send_turn_to(session, text, display, cx),
            None => self.send_message_showing(text, display, cx),
        }
    }

    pub(crate) fn send_message(&mut self, text: String, cx: &mut Context<Self>) {
        self.send_message_showing(text, None, cx);
    }

    /// The same send, with the row history should keep when the text is a
    /// prompt the user must not read (#2756). A command that composes a turn
    /// passes its receipt here, so the `[SYSTEM: ...]` scaffolding stays in
    /// the model's context and never becomes a bubble.
    pub(crate) fn send_message_showing(
        &mut self,
        text: String,
        display: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // A send ends the offer (#2708): the buttons belonged to the turn that
        // made them, and leaving them up over a new question would send a
        // stale follow-up into it.
        if let Some(sid) = self.transcript.session {
            agent_bridge::options_take(sid);
        }
        // The composer the text came from: when it sends a recalled message,
        // that message leaves the queue with the text while the rest of its
        // walk goes back to wait (#2637).
        let sent_from = self.composer_entity(cx);
        // Commands are read before every other gate, because the two that
        // matter most are needed exactly when the window is busy or broken:
        // `/stop` has nothing to stop on an idle window, and `/help` is what a
        // user reaches for when the agent is offline. A line that starts with
        // a slash never becomes a turn (#2374).
        if let Some(outcome) = commands::parse_command(&text) {
            self.run_command(outcome, self.transcript.session, cx);
            return;
        }
        // `!cmd` runs a shell command here, not a turn, as the terminal does (#2437).
        if let Some(cmd) = opencrabs::ui_core::bang::bang_command(&text) {
            bang::spawn(cmd.to_string(), self.sink.clone());
            self.live.notices.push(format!("! {cmd}"));
            cx.notify();
            return;
        }
        // A closed topic refuses new turns. The notice says why, so the send is
        // never dropped without a word (#2305). It sits after the parse above
        // because a command is not a turn: `/new` and `/sessions` still work
        // from a closed topic.
        if self
            .transcript
            .session
            .is_some_and(|id| self.sessions.is_closed(id))
        {
            self.live
                .notices
                .push("This topic is closed. Reopen it to continue.".to_string());
            cx.notify();
            return;
        }
        if self.agent_error.is_some() {
            self.live.notices.push(format!(
                "agent offline: {}",
                self.agent_error.as_deref().unwrap_or("bootstrap failed")
            ));
            cx.notify();
            return;
        }
        if self.busy {
            // The message joins the session's queue, which the agent drains
            // between tool calls, the terminal's own path (#2441). With no
            // session open it waits in the window's own list. Busy is read
            // for the conversation on screen, so work running in another one
            // never queues this text behind it (#2616).
            // Up already took the recalled message out of the queue (#2637),
            // so this text is simply queued behind the turn. The messages the
            // walk took before it go back to wait when the recall is released
            // below, after this one, which is the order they were queued in.
            match self.transcript.session {
                Some(sid) => {
                    agent_bridge::queue_push(sid, Self::pending_message(text, display.clone()))
                }
                None => self
                    .unsent
                    .push(Self::pending_message(text, display.clone())),
            }
            self.finish_recall(&sent_from, cx);
            // The composer now says a message is waiting on the turn, the
            // moment it is queued rather than on the next turn event (#2630).
            self.sync_composer_hint(cx);
            cx.notify();
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: keep the message, it fires on Ready. The queue
            // strip is where the user sees it.
            self.unsent
                .push(Self::pending_message(text, display.clone()));
            self.sync_composer_hint(cx);
            cx.notify();
            return;
        };
        // The turn goes to the conversation on screen. With none open there
        // is nothing to write into, so one is created and the text waits in
        // the queue rather than vanishing into a session nobody is looking at.
        let Some(sid) = self.transcript.session else {
            self.unsent
                .push(Self::pending_message(text, display.clone()));
            self.new_session(cx);
            return;
        };
        self.live
            .begin(display.clone().unwrap_or_else(|| text.clone()));
        // A send is the reader asking for the end of the conversation (#2698).
        self.rearm_transcript_follow();
        self.transcript.live_expanded = false;
        self.settle_retries = 0;
        self.settle_deadline = None;
        self.settle_rechecks = 0;
        // The finished turn must add one row to the session before the live
        // copy may be dropped (see `reload_transcript`). Measured on the
        // session's own row total, which never saturates the way the windowed
        // turn count does (#2705).
        self.settle_expect = Some(self.transcript.row_total() + 1);
        // A recalled message is the turn's own text now, so it leaves the
        // queue with it; the messages the walk took before it go back to wait
        // (#2637).
        self.finish_recall(&sent_from, cx);
        self.busy = true;
        self.sync_composer_hint(cx);
        let cancel = tokio_util::sync::CancellationToken::new();
        self.cancel = Some(cancel.clone());
        // Attachments are tracked against the session the same way the TUI
        // does it (`FileService::get_or_create_file`), so the file lands in
        // the session's artifacts, not just in the message text.
        let attached = editor::marker_paths(&text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            sid,
            text,
            display,
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), sid, attached);
        }
        cx.notify();
    }

    /// The two taps a plan waiting on approval offers (#2755).
    ///
    /// They send the literal commands the terminal and Telegram send, through
    /// the composer path that parses them, so a tap and a typed `/execute` are
    /// one code path and never two implementations of an approval.
    pub(crate) fn plan_actions(&mut self, session: Uuid, cx: &mut Context<Self>) -> plan_strip::PlanActions {
        let approve = cx.listener(move |this, _ev: &gpui::ClickEvent, _window, cx| {
            cx.stop_propagation();
            this.send_turn_to(session, "/execute".to_string(), None, cx);
        });
        let discard = cx.listener(move |this, _ev: &gpui::ClickEvent, _window, cx| {
            cx.stop_propagation();
            this.send_turn_to(session, "/discard".to_string(), None, cx);
        });
        plan_strip::PlanActions {
            on_approve: std::rc::Rc::new(approve),
            on_discard: std::rc::Rc::new(discard),
        }
    }

    /// The queue entry for a send: a hidden prompt keeps its display line, so
    /// the queue strip never reads `[SYSTEM: ...]` (#2756).
    pub(crate) fn pending_message(text: String, display: Option<String>) -> agent_bridge::PendingMessage {
        match display {
            Some(shown) => agent_bridge::PendingMessage::system(text, shown),
            None => agent_bridge::PendingMessage::plain(text),
        }
    }

    /// Drain the bridge channel: fold progress into the live view, finalize
    /// the turn on Done/Failed, reload the transcript so the DB becomes the
    /// rendering.
    pub(crate) fn poll(&mut self, cx: &mut Context<Self>) {
        // Retire a drag whose moves stopped before the drop arrived: the
        // window must not stay tinted after the pointer leaves it.
        if self.drag_seen.is_some() && !self.file_drag_active() {
            self.drag_seen = None;
            cx.notify();
        }
        let models_changed = self.poll_model_fetches();
        if models_changed {
            cx.notify();
        }
        // Pruned even on the busy path: a flash window that closed while a
        // turn was streaming must not keep the map alive.
        let flash_active = self.plan_flash_active(std::time::Instant::now());
        let events = agent_bridge::drain(&self.agent_rx);
        if events.is_empty() {
            // A live turn needs a repaint even when no event arrived: that is
            // what advances the spinner frame and the `(5s ...)` clock. The
            // old early return froze the line between chunks, which read as
            // "nothing is happening" on a slow provider.
            // A pane runs its own turn too (#2614), and its spinner, clock
            // and token counter advance on the same repaint. So does a rail row
            // with work in flight, and a plan waiting for approval keeps
            // pulsing its ring until the owner opens it (#2636).
            if self.busy
                || self.pane_states.values().any(|pane| pane.read(cx).busy)
                || self
                    .rail_activity_map(cx)
                    .values()
                    .any(|row| row.badge.is_some() || row.ring)
                // A rail flash is an animation of its own: the plan bar has to
                // keep alternating orange and its resting colour while the
                // window is open, and stop the moment it closes.
                || flash_active
            {
                // Only the spinner, the clock and the flash move in this
                // branch, so they repaint on a slower cadence than the poll
                // itself. At 20 Hz this walked every pane's transcript five
                // times as often as the animation needed (#2688).
                self.anim_tick = self.anim_tick.wrapping_add(1);
                if busy_repaint_due(self.anim_tick) {
                    cx.notify();
                }
            }
            return;
        }
        let mut reload: Option<Uuid> = None;
        for ev in events {
            // The tool's follow-up offer is stored here, before the pane
            // filter below, because it belongs to a session and not to a card
            // (#2708). Handling it in the match further down would have sent a
            // split leaf's offer into `fold_pane_progress` with the rest of
            // that leaf's progress, and the pane would never draw it.
            if let agent_bridge::TurnEvent::ProgressFor(
                sid,
                ProgressEvent::SuggestedOptions(items),
                _,
            ) = &ev
            {
                agent_bridge::set_options(*sid, items.clone());
                cx.notify();
            }
            // A split leaf runs its own turn (#2614), so its progress paints
            // into that leaf's card. A sub-agent's session has no leaf and is
            // dropped here rather than painted into somebody else's pane.
            // This filter is also what keeps one conversation's tool rows off
            // another's card (#2616): only the session on screen reaches the
            // window's live turn below.
            if let agent_bridge::TurnEvent::ProgressFor(sid, progress, at) = &ev
                && self.transcript.session != Some(*sid)
            {
                self.fold_pane_progress(*sid, progress.clone(), *at, cx);
                continue;
            }
            match ev {
                agent_bridge::TurnEvent::Ready => {
                    self.agent_ready = true;
                    logging::info("agent ready");
                    // The footer's denominator comes from the service, the
                    // same number compaction enforces for this session.
                    if let Some(b) = agent_bridge::bridge() {
                        self.ctx_max = match self.transcript.session {
                            Some(sid) => b.service.context_limit_for_session(sid),
                            None => b
                                .service
                                .context_window_for_model(&b.service.provider_model()),
                        };
                    }
                    // A window opened on an empty ledger has no conversation
                    // to send into: make one before the first message can be
                    // typed into nothing.
                    if self.transcript.session.is_none() {
                        self.new_session(cx);
                    }
                    self.sync_composer_hint(cx);
                    self.drain_pending(cx);
                }
                agent_bridge::TurnEvent::SessionCreated(id) => {
                    logging::info(&format!("session created {id}"));
                    self.refresh_rail(cx);
                    let for_split = std::mem::take(&mut self.split_creating);
                    if for_split && self.split_pick.is_some() {
                        self.choose_split(id, cx);
                    } else {
                        self.open_session(id, cx);
                    }
                }
                agent_bridge::TurnEvent::SessionCreateFailed(e) => {
                    self.live.notices.push(format!("new session: {e}"));
                }
                agent_bridge::TurnEvent::UpdateAvailable(version) => {
                    self.update_available = Some(version);
                    cx.notify();
                }
                agent_bridge::TurnEvent::MessagesCleared(id) => {
                    if self.transcript.session == Some(id) {
                        self.transcript.turns = Vec::new();
                        self.transcript.hidden = 0;
                        self.transcript.error = None;
                    }
                    self.live.notices.push("Session cleared.".to_string());
                    // The card must drop the line of a chat that no longer has
                    // messages. Only the previews are re-read: a rail refresh
                    // here would reorder rows under the cursor.
                    self.refresh_previews(cx);
                    cx.notify();
                }
                agent_bridge::TurnEvent::ClearFailed(text) => {
                    self.live.notices.push(text);
                    cx.notify();
                }
                agent_bridge::TurnEvent::BangOutput(text) => {
                    self.live.notices.push(text);
                    cx.notify();
                }
                agent_bridge::TurnEvent::EvolveDone(text) => {
                    self.live.notices.push(text);
                    cx.notify();
                }
                // A text command answered on the bridge: one line, the same
                // wording the terminal prints (#2469). The line belongs to the
                // session that ran the command, so a pane answers on its own
                // strip; a window-level notice (surface, boot summary) carries
                // no session and stays where it has always been (#2672).
                agent_bridge::TurnEvent::CommandReply { session, text } => {
                    match session {
                        Some(id) => self.push_session_notice(id, text, cx),
                        None => self.live.notices.push(text),
                    }
                    cx.notify();
                }
                // A text command that needs one visible turn: `/plan <query>`,
                // `/execute` seeding the checklist, or `/goal`.
                agent_bridge::TurnEvent::CommandTurn {
                    session,
                    text,
                    display,
                } => {
                    self.send_turn_to(session, text, display, cx);
                }
                agent_bridge::TurnEvent::SessionDeleted(id) => {
                    logging::info(&format!("session deleted {id}"));
                    if self.transcript.session == Some(id) {
                        // The open pane pointed at a row that no longer is:
                        // clear it rather than leave a transcript of a
                        // deleted conversation on screen.
                        self.transcript.session = None;
                        self.transcript.turns.clear();
                        self.live.clear();
                        // The deleted conversation's turn, if one ran there,
                        // goes with it: the row it belonged to is gone.
                        self.stashed.remove(&id);
                        self.busy = false;
                        // The files screen of a deleted session has nothing
                        // left to list: back to the chat, never a ghost list.
                        if self.screen == Screen::Files {
                            self.screen = Screen::Chat;
                        }
                    }
                    self.refresh_rail(cx);
                }
                agent_bridge::TurnEvent::SessionDeleteFailed(e) => {
                    self.live.notices.push(format!("delete session: {e}"));
                }
                agent_bridge::TurnEvent::PlanPath { session, path } => {
                    // Stale lookups (user switched conversations while the
                    // DB read ran) are dropped, not painted. A pane's own
                    // lookup lands on that pane's strip (#2530).
                    if self.plan_for == Some(session) {
                        self.plan.set_path(path);
                        cx.notify();
                    } else if let Some(pane) = self.pane_states.get(&session).cloned() {
                        // The path belongs to the session the pane was opened
                        // for, so a leaf for another conversation ignores it.
                        if pane.read(cx).plan_for == Some(session) {
                            pane.update(cx, |pane, cx| {
                                pane.plan.set_path(path);
                                cx.notify();
                            });
                        }
                    }
                }
                agent_bridge::TurnEvent::BootstrapFailed(e) => {
                    logging::error(&format!("agent offline: {e}"));
                    self.agent_error = Some(e.clone());
                    self.live.notices.push(format!("agent offline: {e}"));
                }
                agent_bridge::TurnEvent::ModelSwitched { session, line } => {
                    if self.models_switching.take() != Some(session) {
                        logging::debug("stale model switch completion ignored");
                        continue;
                    }
                    logging::info(&format!("model switched: {line}"));
                    self.push_session_notice(session, line, cx);
                    // The rail row carries the pair the service answers with,
                    // so a switch is only real once the row is re-read.
                    self.refresh_rail(cx);
                    // The gauge is denominated in the pair that serves the
                    // session now. `ctx_max` used to be recomputed only on
                    // `Ready` and on open, so a switch left the old provider's
                    // window on screen: the number was the one thing that
                    // would have proved the pick landed, and it never moved.
                    // The last prompt size came from the old provider's
                    // tokenizer, so it goes with it (#2644).
                    if self.transcript.session == Some(session)
                        && let Some(b) = agent_bridge::bridge()
                    {
                        self.ctx_max = b.service.context_limit_for_session(session);
                        self.ctx_tokens = None;
                    }
                    // A pick is a commitment: apply it, say so once, and put
                    // the user back in the conversation that asked. A stale
                    // completion never closes a different screen.
                    if self.screen == Screen::Models && self.models_session == Some(session) {
                        self.screen = Screen::Chat;
                        if self.transcript.session == Some(session) {
                            self.pane_focus = None;
                        } else if self
                            .panes
                            .as_ref()
                            .is_some_and(|tree| tree.contains(session))
                        {
                            self.pane_focus = Some(session);
                        }
                        self.sync_composer_session();
                        self.refocus_screen();
                    }
                    cx.notify();
                }
                agent_bridge::TurnEvent::ModelSwitchFailed { session, error } => {
                    if self.models_switching.take() != Some(session) {
                        logging::debug("stale model switch failure ignored");
                        continue;
                    }
                    logging::error(&format!("model switch: {error}"));
                    self.push_session_notice(session, format!("model switch: {error}"), cx);
                    cx.notify();
                }
                agent_bridge::TurnEvent::BrainReady(line) => {
                    logging::info(&format!("brain: {line}"));
                    self.live.notices.push(format!("brain: {line}"));
                    // A brain written from the wizard's own step ends that
                    // step; the setup itself is saved by the same finish path
                    // the Enter key uses, so the two routes cannot diverge.
                    let mut step_done = false;
                    if let Some(ob) = self.onboard.as_mut()
                        && ob.step() == onboard::OnboardingStep::BrainSetup
                    {
                        ob.mark_brain_generated();
                        step_done = matches!(ob.advance(), onboard::Advance::Done);
                    }
                    if step_done {
                        self.finish_onboard(cx);
                    } else {
                        // The brain files are the system prompt, so the stack
                        // that just answered does not carry them yet: reload it
                        // the same way a saved config does, and the next answer
                        // does.
                        self.reload_agent(cx, None);
                    }
                }
                agent_bridge::TurnEvent::BrainFailed(e) => {
                    logging::error(&format!("brain: {e}"));
                    self.live
                        .notices
                        .push(format!("brain generation failed: {e}"));
                    // The step keeps the reason and stays up: Enter still
                    // finishes the setup, so a failed writer never traps the
                    // user, and the prose is retried on save.
                    if let Some(ob) = self.onboard.as_mut()
                        && ob.step() == onboard::OnboardingStep::BrainSetup
                    {
                        ob.mark_brain_failed(e);
                    }
                    cx.notify();
                }
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::Thinking, _) => {
                    self.live.thinking = true;
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::StreamingChunk { text },
                    at,
                ) => {
                    // Counted, not just appended: the live line's token number
                    // comes from the chunk itself, so it moves as the answer
                    // streams instead of appearing with the final answer.
                    //
                    // Stamped with its ARRIVAL (#2703): `drain` hands this loop
                    // a whole batch, so reading the clock here would stamp every
                    // chunk in the batch with one instant and collapse the
                    // window the rate is measured over.
                    self.live.thinking = false;
                    self.live.note_chunk_at(&text, at);
                    // The rail's preview follows the answer as it streams, at
                    // most one batched read a second.
                    self.refresh_previews_if_live(cx);
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::IntermediateText { text, reasoning },
                    at,
                ) => {
                    // Narration between tool calls is part of the answer the
                    // user is reading, so it joins the stream instead of
                    // opening a second copy somewhere else on screen. The
                    // reasoning that came with it is shown live, above the
                    // answer, the way the TUI shows it.
                    // The provider reports the same narration twice: once as
                    // streamed chunks and again as intermediate text. Appending
                    // both is what drew the paragraph twice, so both paths
                    // dedupe against what is already on screen.
                    self.live.thinking = false;
                    if let Some(reasoning) = reasoning {
                        self.live.push_reasoning_at(&reasoning, at);
                    }
                    self.live.push_narration(&text);
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::ToolStarted {
                        tool_name,
                        tool_input,
                        ..
                    },
                    _,
                ) => {
                    self.live.thinking = false;
                    // The ledger's own wording (`tui::app::messaging`), so a
                    // live row reads exactly like the row that persists.
                    let description =
                        opencrabs::tui::app::App::format_tool_description(&tool_name, &tool_input);
                    self.live
                        .push_tool(transcript::ToolCall::running(tool_name, description));
                    // A silent tool run still moves the preview's tool line, so
                    // the same throttled read the chunk path uses is asked for
                    // here too. Without it a long build leaves the rail frozen
                    // for exactly the sessions that are working longest (#762's
                    // lesson, applied to the preview).
                    self.refresh_previews_if_live(cx);
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::ToolCompleted {
                        tool_name, success, ..
                    },
                    _,
                ) => {
                    // Close the newest open row this call opened; with no open
                    // row (an event lost across a mode switch) draw it closed
                    // rather than dropping the call from the turn.
                    //
                    // Rows carrying a `task_id` are detached commands: their own
                    // finish settles them, so a synchronous call must never close
                    // one that happens to share its tool name.
                    let open = self
                        .live
                        .tools
                        .iter_mut()
                        .rev()
                        .find(|c| !c.done && c.name == tool_name && c.task_id.is_none());
                    match open {
                        Some(call) => {
                            call.done = true;
                            call.success = success;
                        }
                        None => self.live.push_tool(transcript::ToolCall {
                            name: tool_name.clone(),
                            description: tool_name,
                            success,
                            done: true,
                            task_id: None,
                        }),
                    }
                }
                // Every API response and tool execution reports the context
                // size it just ran with, so the footer tracks the turn while
                // it is still in flight.
                // The agent took a queued message between tool calls: it shows
                // in the live turn right away, where the terminal puts it (#2441).
                agent_bridge::TurnEvent::ProgressFor(
                    sid,
                    ProgressEvent::QueuedUserMessage { text },
                    _,
                ) if self.transcript.session == Some(sid) => {
                    self.live.injected.push(text);
                    cx.notify();
                }
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::TokenCount(count), _) => {
                    self.ctx_tokens = Some(count.min(u32::MAX as usize) as u32);
                }
                // A detached command's row lands synchronously, while the
                // turn that spawned it is still live, and its finish finds it
                // again by `task_id` (#2146). Keyed by id, never by position:
                // two parallel calls would otherwise settle each other's rows.
                agent_bridge::TurnEvent::ProgressFor(
                    sid,
                    ProgressEvent::DetachedTaskStarted { task_id, label },
                    _,
                ) => {
                    if Some(sid) == self.transcript.session {
                        self.live.thinking = false;
                        self.live
                            .push_tool(transcript::ToolCall::detached(task_id, label));
                    }
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::DetachedTaskFinished {
                        task_id,
                        label,
                        success,
                        elapsed_secs,
                    },
                    _,
                ) => {
                    // A finish with no row is not an error: the turn may have
                    // been settled, or the task outlived a restart. The receipt
                    // still arrives as `BackgroundTaskDone`.
                    if !self.settle_detached_row(&task_id, success, elapsed_secs) {
                        logging::debug(&format!(
                            "detached task {task_id} ({label}) finished with no row to settle"
                        ));
                    }
                }
                // A plan just closed (#2255, #2691). The settle emits this
                // before the plan file is archived, so it is the one signal
                // that cannot be missed by a file read. The chime is opt-in
                // and feature-gated, and no toast or desktop notification is
                // raised, exactly as the terminal's arm.
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::PlanCompleted { .. }, _) => {
                    Self::play_task_chime();
                }
                // Compaction notices and queue echoes: the phase-2 transcript
                // mirror. They persist to the DB and show on the reload;
                // nothing to draw live yet. The follow-up offer is already
                // stored further up this loop (#2708), so it reaches this
                // catch-all spent.
                agent_bridge::TurnEvent::ProgressFor(_, _, _) => {}
                agent_bridge::TurnEvent::Done {
                    session,
                    content,
                    context_tokens,
                    tokens_per_second,
                } => {
                    // The card's live line belongs to this turn (#2718), so
                    // it retires with it: the preview under it is the ledger's
                    // and stops representing the turn the moment it settles.
                    self.rail_live.remove(&session);
                    // A settled turn is what moves a row up the rail (#2666).
                    // The ledger bumps `updated_at` beside the message insert
                    // and the rail paints that order, so without a refetch
                    // here a conversation used a minute ago kept the slot and
                    // the date it had when the rail was last read.
                    self.refresh_rail(cx);
                    // A split pane runs its own turn (#2614): the answer
                    // belongs to that leaf, so the window's footer and its
                    // conversation are left exactly where the user put them.
                    if self.transcript.session != Some(session) {
                        // The turn finished in a conversation that is not on
                        // screen: its parked card retires here, and the rows
                        // in the ledger are what drawing that session reads
                        // when the user opens it again (#2616). A split pane's
                        // own turn settles in its leaf.
                        self.stashed.remove(&session);
                        self.settle_pane(session, tokens_per_second, cx);
                        // A sub-agent's turn ends the same way and has no pane
                        // to settle (#2636). Clearing is idempotent, so a leaf
                        // that was never recorded costs nothing.
                        if self.rail.clear(session) {
                            cx.notify();
                        }
                        continue;
                    }
                    // The provider's own prompt size for the call that just
                    // answered: the honest number for the ctx footer, not an
                    // estimate of what we think we sent.
                    self.ctx_tokens = Some(context_tokens);
                    // The same numbers, frozen for THIS turn, so the settled
                    // card can print what the turn cost instead of whatever
                    // the session's budget reads later (#2324).
                    if let Some(sid) = self.transcript.session {
                        self.pending_metrics.insert(
                            sid,
                            transcript::TurnMetrics {
                                ctx_used: context_tokens,
                                ctx_max: self.ctx_max,
                                tps: tokens_per_second,
                                tool_calls: self.live.tools.len(),
                            },
                        );
                    }
                    // Freeze the turn's rate on the same measure every other
                    // surface uses: the service's own tokens_per_second when
                    // it reported one, else the local active-streaming
                    // estimate. The footer and the settled card then agree.
                    // Captured before finalize, which resets the accumulator:
                    // the window is what turns a token count into a rate, and
                    // a log that prints the count alone cannot tell a short
                    // window from an inflated numerator (#2703).
                    let active_secs = self.live.streaming_active_secs();
                    self.live.finalize(tokens_per_second);
                    // The answer stays on screen: the stream when chunks
                    // arrived, otherwise the response the turn returned. Live
                    // state clears only once the reload proves the DB has it.
                    if self.live.stream.trim().is_empty() && !content.trim().is_empty() {
                        self.live.settled = Some(content.clone());
                    }
                    self.busy = false;
                    self.live.thinking = false;
                    // One line per turn, with the stream's own numbers: a
                    // turn that rendered as one silent dump is diagnosable
                    // from the log (chunks=0 means the provider never
                    // streamed, not that the window failed to draw).
                    logging::info(&format!(
                        "turn done ({} chars, {} chunks, {} bytes, first {:?}, {}s elapsed, {:.2}s active, {} tok, rate {:.1} tok/s, service {:?}), reloading transcript",
                        content.len(),
                        self.live.chunks,
                        self.live.chunk_bytes,
                        self.live.first_chunk,
                        self.live.elapsed_secs(),
                        active_secs,
                        self.live.tokens,
                        self.live.tokens_per_sec(),
                        tokens_per_second
                    ));
                    reload = self.transcript.session;
                }
                agent_bridge::TurnEvent::Failed { session, text } => {
                    // Same as Done: the live line was this turn's, so it goes
                    // with it (#2718).
                    self.rail_live.remove(&session);
                    // A failure is still activity, so the row moves like a
                    // settled turn does (#2666).
                    self.refresh_rail(cx);
                    if self.transcript.session != Some(session) {
                        // Same as Done: a failure for a conversation that is
                        // not on screen retires its parked card and leaves the
                        // visible turn untouched (#2616).
                        self.stashed.remove(&session);
                        self.fail_pane(session, text, cx);
                        // A failed sub-agent turn is still a finished turn, so
                        // its rail badge has to go with it (#2636).
                        if self.rail.clear(session) {
                            cx.notify();
                        }
                        continue;
                    }
                    // A card a stop already took back has no line to write on:
                    // the cancellation landing afterwards is the echo of the
                    // stop, not news (#2635).
                    if !self.live.is_empty() {
                        self.live.notices.push(text);
                    }
                    // A failed turn still ran, so it keeps the numbers it
                    // actually measured instead of showing a bare clock while
                    // its neighbours carry ctx and rate (#2705). The record is
                    // only written when the provider streamed something: a
                    // failure before the first call has no reading of its own,
                    // and borrowing the previous turn's budget would be a lie.
                    self.pending_metrics.remove(&session);
                    self.live.finalize(None);
                    if self.live.tokens > 0
                        && let Some(used) = self.ctx_tokens
                    {
                        self.pending_metrics.insert(
                            session,
                            transcript::TurnMetrics {
                                ctx_used: used,
                                ctx_max: self.ctx_max,
                                tps: Some(f64::from(self.live.tokens_per_sec()))
                                    .filter(|t| *t > 0.0),
                                tool_calls: self.live.tools.len(),
                            },
                        );
                    }
                    self.busy = false;
                    self.live.thinking = false;
                    reload = self.transcript.session;
                }
                agent_bridge::TurnEvent::Cancelled { session } => {
                    // A stop ends the turn, so the card's live line does too
                    // (#2718).
                    self.rail_live.remove(&session);
                    // A stop dropped the turn where it stood (#2657), so no
                    // Done and no Failed follows. This is the settle, and it
                    // carries no text: whoever asked for the stop already said
                    // "Operation cancelled", so a second line would be noise.
                    self.refresh_rail(cx);
                    self.pending_metrics.remove(&session);
                    // The verdict needs a row (#2811). A cancelled turn writes
                    // no error and no answer, so without this the turn settles
                    // as Finished and the stop vanishes on the next reload.
                    // Written for the cancelled session, whichever one is on
                    // screen, because the turn was real where it ran.
                    if let Some(bridge) = agent_bridge::bridge() {
                        agent_bridge::spawn_record_turn_stopped(bridge.service.clone(), session);
                    }
                    if self.transcript.session == Some(session) {
                        // No reload on purpose. The dropped future cannot
                        // promise the ledger has the text it streamed, and a
                        // re-read now would wipe what the user can still see.
                        // The card keeps its stream, with the clock and rate
                        // frozen where the stop landed.
                        self.live.finalize(None);
                        self.busy = false;
                        self.live.thinking = false;
                        // The stop is the settle (#2811): the card keeps its
                        // stream and now says it was stopped, instead of
                        // waiting on a spinner that will never turn again.
                        self.live.stopped = true;
                        self.cancel = None;
                        logging::info("turn settled after the stop dropped it");
                    } else {
                        // A split leaf: same settle inside its own card, and
                        // the parked copy of a switched-away turn retires.
                        self.stashed.remove(&session);
                        if let Some(pane) = self.pane_states.get(&session) {
                            pane.update(cx, |pane, cx| {
                                pane.live.finalize(None);
                                pane.live.thinking = false;
                                pane.live.stopped = true;
                                pane.busy = false;
                                pane.cancel = None;
                                cx.notify();
                            });
                        }
                        // The stop is acknowledged: the leaf's queue becomes
                        // the next turn now, never while the cancel unwinds
                        // (#2695, the #2631 ordering the window keeps).
                        self.drain_pane_pending(session, cx);
                    }
                    // The turn is gone, so its rail spinner goes with it.
                    self.rail.clear(session);
                    // Unconditional (#2811): a card that just stopped has to
                    // paint its stop glyph once. It is static from here, so
                    // this is the only repaint it needs, and the animation
                    // gate stays off instead of repainting an idle window five
                    // times a second for a turn nobody is running.
                    cx.notify();
                }
                agent_bridge::TurnEvent::TurnReverted { session } => {
                    // The stopped turn's rows are deleted and its question is
                    // back in the composer, so the transcript must stop drawing
                    // the bubble the user just took back (#2635).
                    if self.transcript.session == Some(session) {
                        reload = Some(session);
                    }
                }
                agent_bridge::TurnEvent::TurnResumed {
                    session,
                    user_message,
                    cancel,
                } => {
                    // The last process died inside this turn and it runs again
                    // now (#2627). The card has to say so from the first frame:
                    // a recovered turn that painted into a window believing it
                    // was idle would look exactly like the work being lost.
                    if self.transcript.session == Some(session) {
                        self.live = transcript::LiveTurn::default();
                        // The ledger already holds this question: the process
                        // that asked it died after writing the row. Resuming is
                        // not a fresh send, and starting it as one stamped the
                        // resume time, missed the row in the identity window,
                        // and painted the question twice (#2740).
                        self.live.resume(user_message);
                        self.busy = true;
                        self.cancel = Some(cancel);
                        // No reload here on purpose: the turn's rows are
                        // already in the ledger and a reload landing now would
                        // fight the card it is about to fill.
                    } else {
                        // Recovered in a conversation nobody is looking at: it
                        // parks under its own session, which is where a switch
                        // brings it back whole (#2616).
                        let mut live = transcript::LiveTurn::default();
                        // Same rule as the on-screen branch: the parked copy
                        // draws a row the ledger already has, so it starts as a
                        // resumed turn, not a fresh one (#2740).
                        live.resume(user_message);
                        self.stashed.insert(
                            session,
                            StashedTurn {
                                live,
                                cancel: Some(cancel),
                                settle_expect: None,
                                settle_retries: 0,
                                ctx_tokens: None,
                                // Recovered mid-flight, so the turn is running.
                                busy: true,
                            },
                        );
                    }
                    self.refresh_rail(cx);
                    cx.notify();
                }
                agent_bridge::TurnEvent::BackgroundTaskDone {
                    session_id,
                    context_text,
                    display_text,
                } => {
                    // The whole point of backgrounding: the command ends and
                    // the agent is told, so the work is reported instead of the
                    // window going quiet with the turn already finished.
                    //
                    // The completion belongs to the session that started the
                    // task, never to whatever conversation happens to be on
                    // screen. The terminal resumes that session wherever the
                    // user is looking (`tui/app/state.rs`), and the window now
                    // does the same (#2704): a session already working takes
                    // the receipt in its queue, and an idle one starts its turn
                    // and parks it in the stash when it is not on screen.
                    if self.running_here(session_id, cx) {
                        // Queue it against the session the task belongs to, in
                        // the store the agent drains, so it goes out between
                        // tool calls instead of waiting for the turn to end
                        // (#2629). `QueuedUserMessage::join` keeps the system
                        // half out of what the user "said" (#765).
                        agent_bridge::queue_push(
                            session_id,
                            agent_bridge::PendingMessage::system(context_text, display_text),
                        );
                    } else if self.transcript.session == Some(session_id) {
                        self.resume_finished_task(session_id, context_text, display_text, cx);
                    } else {
                        self.resume_off_screen_task(session_id, context_text, display_text, cx);
                    }
                }
            }
        }
        if let Some(sid) = reload {
            self.reload_transcript(sid, cx);
        }
        // A finished turn is the moment the queue drains: before this it only
        // drained on `Ready`, so a message typed while busy never went out.
        // The hint follows the turn and the queue, including a report that
        // landed mid-turn, so it is repainted on every event batch (#2630).
        self.sync_composer_hint(cx);
        if !self.busy {
            self.cancel = None;
            self.drain_pending(cx);
        }
        self.follow_chat();
        cx.notify();
    }

    /// The running detached command, formatted exactly as the TUI formats it.
    ///
    /// `None` when nothing runs, so the strip is omitted rather than drawn
    /// empty. Elapsed time is read from each task's own `Instant` at paint
    /// time, which is why the label ticks off the existing 50 ms poll with no
    /// extra timer.
    pub(crate) fn running_tasks_label(&self) -> Option<String> {
        let session = self.transcript.session?;
        self.running_tasks_label_for(session)
    }

    /// The same roster for ANY session, not just the open conversation, so every
    /// pane prints its own detached commands above its composer (#2593).
    pub(crate) fn running_tasks_label_for(&self, session: Uuid) -> Option<String> {
        let bridge = agent_bridge::bridge()?;
        let mgr = bridge.service.background_manager()?;
        let tasks: Vec<(String, u64)> = mgr
            .running_tasks(session)
            .into_iter()
            .map(|t| (t.label, t.started.elapsed().as_secs()))
            .collect();
        opencrabs::tui::render::format_background_tasks(&tasks, opencrabs::tui::render::LABEL_CHARS)
    }
}
