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
        // A voice message is its transcription plus the clip it came from
        // (#3044): `transcript <<AUD:/…/voice-123.ogg>>` goes in, `transcript`
        // goes to the agent and the whole line to the row. The agent reads the
        // words, because a recording is not something it can answer; the row
        // keeps the marker, because the transcript draws the clip and plays it
        // back. `original` keeps the marker for the file tracker, which has to
        // see the path that was just stripped off the words.
        let original = text.clone();
        // Only a clip the window recorded is a voice message whose words stand
        // in for the audio (#3044). A clip picked with the paperclip is a file
        // the agent may need to open, so its marker stays in the prompt.
        let voice = editor::split_audio_marker(&original).filter(|(_, marker)| {
            editor::marker_paths(marker)
                .first()
                .is_some_and(|path| crate::voice_input::is_recording(path))
        });
        let text = match &voice {
            // The agent cannot play the clip, so the one thing it has to learn
            // from the marker is that the words in front of it were spoken: a
            // transcript read as a typed question gets a typed answer, and the
            // point of a voice note is the spoken reply (#3044). Added where
            // the marker comes off, so the note never reaches the screen.
            Some((spoken, _)) => format!("{} {spoken}", editor::VOICE_NOTE_PREFIX),
            None => text,
        };
        let display = match &voice {
            Some((spoken, marker)) => Some(display.unwrap_or_else(|| format!("{spoken} {marker}"))),
            None => display,
        };
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
        // And the ring that was standing on it: leaving the index behind would
        // let a later offer steal the keyboard without anyone pressing up
        // (#3101).
        if self.options_focus.map(|(sid, _)| sid) == Some(session) {
            self.options_focus = None;
        }
        // A slash line is a screen command: `/help`, `/models` and `/sessions`
        // read and write the window's own chrome, so they run there and the
        // leaf is left where it is. A prompt is the pane's own turn (#2614).
        if let Some(outcome) = commands::parse_command(&text) {
            // Commands act on the window's chrome, but the reply belongs to
            // the field it was typed in: after the command runs, the notices
            // it wrote move to that pane, so a split never answers in the
            // wrong leaf (#2614).
            let before = self.turn.live.notices.len();
            self.run_command(outcome, Some(session), cx);
            if Some(session) != self.chat_state.session()
                && let Some(pane) = self.pane_states.get(&session).cloned()
            {
                let split_at = before.min(self.turn.live.notices.len());
                let moved: Vec<String> = self.turn.live.notices.split_off(split_at);
                pane.update(cx, |pane, cx| {
                    pane.live.notices.extend(moved);
                    cx.notify();
                });
            }
            cx.notify();
            return;
        }
        if let Some(cmd) = opencrabs::ui_core::bang::bang_command(&text) {
            // The echo and the output belong to the conversation that ran the
            // command (#3160). The echo goes through the same door as the
            // output, so the two lines stay together: a pane's `!cmd` reads
            // its own line and its own output, and never a neighbour's.
            self.push_session_notice(session, format!("! {cmd}"), cx);
            bang::spawn(cmd.to_string(), Some(session), self.sink.clone());
            cx.notify();
            return;
        }
        if let Some(err) = self.composer.agent_error.clone() {
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
        // A command can send into a conversation whose pane was switched away,
        // and that turn belongs to the same card (#2854).
        if let Some(pane) = self.pane_view(session) {
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
        let attached = editor::marker_paths(&original);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session,
            text,
            display,
            // A turn that began as a recording is answered with speech when it
            // settles (#3044), and the turn carries that fact itself (#3096).
            voice.is_some(),
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
        // A parked leaf keeps its line too (#2854): its card is what the user
        // sees when the rail scrolls past a conversation that is still working.
        if let Some(pane) = self.pane_view(id) {
            let pane = pane.read(cx);
            if pane.busy {
                return line(&pane.live);
            }
        }
        if let Some(turn) = self.stashed.get(&id) {
            return line(&turn.live);
        }
        if self.turn.busy && self.chat_state.session() == Some(id) {
            return line(&self.turn.live);
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
        // Drawn or parked: a conversation does not stop owning its turn
        // because its pane was switched away (#2854).
        if let Some(pane) = self.pane_view(session) {
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
        // A turn settles where it ran, not where the user is looking (#2854).
        // This used to return early when the pane was off screen, so a turn
        // that finished after a switch recorded no metrics, reloaded no rows
        // and never drained its queue, and no reload could recover any of it.
        let Some(pane) = self.pane_view(session) else {
            return;
        };
        let (calls, ctx_used, ctx_max) = pane.update(cx, |pane, cx| {
            pane.live.finalize(tokens_per_second);
            pane.live.thinking = false;
            // The leaf's clock ends with its turn too (#2954), on the same
            // rule the window keeps: freeze the span, never the verdict.
            pane.live.freeze();
            pane.busy = false;
            pane.cancel = None;
            // The turn ends where the reader is, not where it ended (#2698).
            pane.snap_if_following();
            cx.notify();
            (pane.live.tools.len(), pane.ctx_used, pane.ctx_max)
        });
        // The leaf's card keeps the numbers it showed in flight, the same ones
        // the window's card keeps (#2632). The budget is the PANE's (#2854):
        // this read the window's counter and window, so a leaf could print
        // another conversation's ctx and another provider's limit.
        self.metrics.pending.insert(
            session,
            transcript::TurnMetrics {
                ctx_used: ctx_used.unwrap_or(0),
                ctx_max,
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
        // A failure reaches the card it belongs to, drawn or parked (#2854).
        let Some(pane) = self.pane_view(session) else {
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
        let Some(pane) = self.pane_view(session) else {
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
        if let Some(pane) = self.pane_view(session) {
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
            // A flushed queue is typed text: the take behind it was sent long
            // before, and a queue that speaks would answer words nobody spoke
            // (#3096).
            false,
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
                        &this.metrics.by_anchor,
                        &loaded.origins,
                    );
                    // The turn this leaf's card was watching has landed, so it
                    // takes the numbers it showed in flight before the rows are
                    // drawn (#2632).
                    let turns = this.claim_pane_metrics(session, turns);
                    if let Some(pane) = this.pane_view(session) {
                        pane.update(cx, |pane, cx| {
                            // A read that worked ends the last failure (#2981):
                            // a stale error line would otherwise stand in place
                            // of this pane's conversation for good.
                            pane.transcript.error = None;
                            pane.transcript.hidden = hidden;
                            pane.transcript.turns = turns;
                            pane.transcript.reactions = loaded.reactions;
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
                    if let Some(pane) = this.pane_view(session) {
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
        if Some(session) == self.chat_state.session() {
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
        // A voice message is its transcription plus the clip it came from
        // (#3044), the same split `send_from_pane` makes: the agent reads the
        // words and the row keeps the marker that draws the clip.
        let original = text.clone();
        // Only a clip the window recorded is a voice message whose words stand
        // in for the audio (#3044). A clip picked with the paperclip is a file
        // the agent may need to open, so its marker stays in the prompt.
        let voice = editor::split_audio_marker(&original).filter(|(_, marker)| {
            editor::marker_paths(marker)
                .first()
                .is_some_and(|path| crate::voice_input::is_recording(path))
        });
        let text = match &voice {
            // Same note as the pane path (#3044): the words were spoken, and a
            // transcript handed over as ordinary prose gets an ordinary answer.
            Some((spoken, _)) => format!("{} {spoken}", editor::VOICE_NOTE_PREFIX),
            None => text,
        };
        let display = match &voice {
            Some((spoken, marker)) => Some(display.unwrap_or_else(|| format!("{spoken} {marker}"))),
            None => display,
        };
        // A send ends the offer (#2708): the buttons belonged to the turn that
        // made them, and leaving them up over a new question would send a
        // stale follow-up into it.
        if let Some(sid) = self.chat_state.session() {
            agent_bridge::options_take(sid);
            // The keyboard's ring goes with it, or the next offer would arrive
            // already holding keys nobody handed it (#3101).
            if self.options_focus.map(|(focused, _)| focused) == Some(sid) {
                self.options_focus = None;
            }
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
            self.run_command(outcome, self.chat_state.session(), cx);
            return;
        }
        // `!cmd` runs a shell command here, not a turn, as the terminal does (#2437).
        if let Some(cmd) = opencrabs::ui_core::bang::bang_command(&text) {
            // The conversation the command belongs to is taken now, because
            // the command answers later and the reader may well have switched
            // by then (#3160). The id travels with the output, so the line
            // lands in the conversation that asked for it.
            let session = self.chat_state.session();
            match session {
                Some(id) => self.push_session_notice(id, format!("! {cmd}"), cx),
                // With no conversation on screen there is nothing to own the
                // line, so it stays on the window's own strip.
                None => self.turn.live.notices.push(format!("! {cmd}")),
            }
            bang::spawn(cmd.to_string(), session, self.sink.clone());
            cx.notify();
            return;
        }
        // A closed topic refuses new turns. The notice says why, so the send is
        // never dropped without a word (#2305). It sits after the parse above
        // because a command is not a turn: `/new` and `/sessions` still work
        // from a closed topic.
        if self
            .chat_state
            .session()
            .is_some_and(|id| self.sessions.is_closed(id))
        {
            self.turn
                .live
                .notices
                .push("This topic is closed. Reopen it to continue.".to_string());
            cx.notify();
            return;
        }
        if self.composer.agent_error.is_some() {
            self.turn.live.notices.push(format!(
                "agent offline: {}",
                self.composer
                    .agent_error
                    .as_deref()
                    .unwrap_or("bootstrap failed")
            ));
            cx.notify();
            return;
        }
        if self.turn.busy {
            // The message joins the session's queue, which the agent drains
            // between tool calls, the terminal's own path (#2441). With no
            // session open it waits in the window's own list. Busy is read
            // for the conversation on screen, so work running in another one
            // never queues this text behind it (#2616).
            // Up already took the recalled message out of the queue (#2637),
            // so this text is simply queued behind the turn. The messages the
            // walk took before it go back to wait when the recall is released
            // below, after this one, which is the order they were queued in.
            match self.chat_state.session() {
                Some(sid) => {
                    agent_bridge::queue_push(sid, Self::pending_message(text, display.clone()))
                }
                None => self
                    .composer
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
            self.composer
                .unsent
                .push(Self::pending_message(text, display.clone()));
            self.sync_composer_hint(cx);
            cx.notify();
            return;
        };
        // The turn goes to the conversation on screen. With none open there
        // is nothing to write into, so one is created and the text waits in
        // the queue rather than vanishing into a session nobody is looking at.
        let Some(sid) = self.chat_state.session() else {
            self.composer
                .unsent
                .push(Self::pending_message(text, display.clone()));
            self.new_session(cx);
            return;
        };
        self.turn
            .live
            .begin(display.clone().unwrap_or_else(|| text.clone()));
        // A send is the reader asking for the end of the conversation (#2698).
        self.rearm_transcript_follow();
        self.chat_state.transcript.live_expanded = false;
        // The finished turn must add one row to the session before the live
        // copy may be dropped (see `reload_transcript`). Measured on the
        // session's own row total, which never saturates the way the windowed
        // turn count does (#2705).
        self.turn.settle.begin(self.chat_state.row_total());
        // A recalled message is the turn's own text now, so it leaves the
        // queue with it; the messages the walk took before it go back to wait
        // (#2637).
        self.finish_recall(&sent_from, cx);
        self.turn.busy = true;
        self.sync_composer_hint(cx);
        let cancel = tokio_util::sync::CancellationToken::new();
        self.turn.cancel = Some(cancel.clone());
        // Attachments are tracked against the session the same way the TUI
        // does it (`FileService::get_or_create_file`), so the file lands in
        // the session's artifacts, not just in the message text.
        let attached = editor::marker_paths(&original);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            sid,
            text,
            display,
            // Same rule as the pane path: the turn says whether it was spoken,
            // and nothing outside it remembers (#3096).
            voice.is_some(),
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), sid, attached);
        }
        cx.notify();
    }

    /// Send an edited message (#2826): stop the turn that is running, drop
    /// every row after the one being replaced, then send the new text as the
    /// next turn.
    ///
    /// The order is the whole point. Truncating while a turn is streaming lets
    /// the assistant row it is still appending land on a branch that no longer
    /// exists, so the stop comes first, the truncate second, and the send last.
    /// The window is re-read between the last two, so the rows the edit dropped
    /// leave the screen at the same moment they leave the store.
    pub(crate) fn send_edited(
        &mut self,
        target: chat_leaf::EditTarget,
        text: String,
        cx: &mut Context<Self>,
    ) {
        self.stop_turn_for_edit(target.session, cx);
        let db_path = self.db_path.clone();
        let session = target.session;
        let sequence = target.sequence;
        cx.spawn(async move |this, cx| {
            // A live row with no stored sequence has nothing behind it to cut
            // (#2925): the stop above already ended the turn, so the rewrite
            // simply speaks for the exchange from here.
            let Some(sequence) = sequence else {
                this.update(cx, |this, cx| {
                    logging::info(&format!(
                        "edit: live row rewritten, turn stopped, in {session}"
                    ));
                    this.send_message_showing(text, None, cx);
                })
                .ok();
                return;
            };
            let truncated = transcript::truncate_after(db_path.clone(), session, sequence).await;
            let loaded = match &truncated {
                Ok(_) => Some(transcript::load_recent(db_path, session).await),
                Err(_) => None,
            };
            this.update(cx, |this, cx| match truncated {
                Ok(deleted) => {
                    logging::info(&format!(
                        "edit: dropped {deleted} row(s) after sequence {sequence} in {session}"
                    ));
                    if let Some(Ok(loaded)) = loaded {
                        this.install_loaded(loaded, cx);
                    }
                    this.send_message_showing(text, None, cx);
                }
                Err(e) => {
                    this.turn.live.notices.push(format!("edit failed: {e}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The same edit, confirmed from a field inside a split leaf's row (#2826).
    ///
    /// The difference is only where the answer lands: the truncation and the
    /// new turn are the same, but the re-read goes into the leaf that owns the
    /// conversation and the text goes back out through that leaf's own send.
    /// Writing the window's transcript here would repaint a conversation the
    /// pane is not showing.
    pub(crate) fn send_edited_in_pane(
        &mut self,
        pane_id: Uuid,
        target: chat_leaf::EditTarget,
        text: String,
        cx: &mut Context<Self>,
    ) {
        self.stop_turn_for_edit(target.session, cx);
        let db_path = self.db_path.clone();
        let session = target.session;
        let sequence = target.sequence;
        cx.spawn(async move |this, cx| {
            // Same as the window's own path (#2925): a live row the store has
            // not taken yet has nothing behind it to cut, so the leaf only
            // stops the turn and sends the rewrite.
            let Some(sequence) = sequence else {
                this.update(cx, |this, cx| {
                    logging::info(&format!(
                        "pane edit: live row rewritten, turn stopped, in {session}"
                    ));
                    this.send_from_pane(session, text, None, SendOrigin::Composer, cx);
                })
                .ok();
                return;
            };
            let truncated = transcript::truncate_after(db_path.clone(), session, sequence).await;
            let loaded = match &truncated {
                Ok(_) => Some(transcript::load_recent(db_path, session).await),
                Err(_) => None,
            };
            this.update(cx, |this, cx| match truncated {
                Ok(deleted) => {
                    logging::info(&format!(
                        "pane edit: dropped {deleted} row(s) after sequence {sequence} in {session}"
                    ));
                    if let Some(Ok(loaded)) = loaded {
                        this.install_loaded_in_pane(pane_id, loaded, cx);
                    }
                    this.send_from_pane(session, text, None, SendOrigin::Composer, cx);
                }
                Err(e) => {
                    if let Some(pane) = this.pane_states.get(&pane_id).cloned() {
                        pane.update(cx, |pane, cx| {
                            pane.live.notices.push(format!("edit failed: {e}"));
                            cx.notify();
                        });
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Stop whatever is streaming into this conversation before its history is
    /// cut (#2826).
    ///
    /// The window's turn is only this edit's business when the window is
    /// showing the same conversation; a pane running it is stopped either way,
    /// because a row it appends would land under the truncation.
    fn stop_turn_for_edit(&mut self, session: Uuid, cx: &mut Context<Self>) {
        if self.chat_state.session() == Some(session) && self.turn.busy {
            if let Some(cancel) = self.turn.cancel.take() {
                cancel.cancel();
            }
            self.turn.busy = false;
            self.turn.live.thinking = false;
            self.turn.live.stop();
            logging::info(&format!(
                "turn stopped for an edit of a row in session {session}"
            ));
        }
        if let Some(pane) = self.pane_states.get(&session).cloned() {
            let stopped = pane.update(cx, |pane, cx| {
                let Some(cancel) = pane.cancel.take() else {
                    return false;
                };
                cancel.cancel();
                pane.busy = false;
                pane.live.thinking = false;
                pane.live.stop();
                cx.notify();
                true
            });
            if stopped {
                logging::info(&format!(
                    "pane turn stopped for an edit in session {session}"
                ));
            }
        }
    }

    /// Put a freshly read window on screen (#2826). The same fields the rail's
    /// open installs, so a re-read after a truncation leaves the window and the
    /// store saying the same thing.
    pub(crate) fn install_loaded(&mut self, loaded: transcript::Loaded, cx: &mut Context<Self>) {
        // Before the rows move (#3064): the folded prompts among them are
        // written to the conversation's project files, so the card on screen
        // names a file that exists.
        let documents = prompt_files::documents(&loaded.rows);
        let (hidden, turns) = transcript::turns_with_origins(
            &loaded.rows,
            loaded.total,
            &self.metrics.by_anchor,
            &loaded.origins,
        );
        self.chat_state.transcript.hidden = hidden;
        self.chat_state.transcript.turns = turns;
        self.chat_state.transcript.rows = loaded.rows;
        self.chat_state.transcript.reactions = loaded.reactions;
        self.persist_prompt_documents(documents, cx);
        cx.notify();
    }

    /// The same install, into the leaf that owns the conversation (#2826).
    pub(crate) fn install_loaded_in_pane(
        &mut self,
        pane_id: Uuid,
        loaded: transcript::Loaded,
        cx: &mut Context<Self>,
    ) {
        let documents = prompt_files::documents(&loaded.rows);
        let (hidden, turns) = transcript::turns_with_origins(
            &loaded.rows,
            loaded.total,
            &self.metrics.by_anchor,
            &loaded.origins,
        );
        let Some(pane) = self.pane_states.get(&pane_id).cloned() else {
            return;
        };
        pane.update(cx, |pane, cx| {
            pane.transcript.hidden = hidden;
            pane.transcript.turns = turns;
            pane.transcript.rows = loaded.rows;
            pane.transcript.reactions = loaded.reactions;
            cx.notify();
        });
        self.persist_prompt_documents(documents, cx);
    }

    /// Write a window's folded prompts to the conversation's project files
    /// (#3064), off the UI thread, and keep where each one landed so the card
    /// can open that file instead of a copy held in memory.
    ///
    /// The write is best-effort by design: a conversation with no project, a
    /// read-only home or a full disk leaves the row exactly as it was, and the
    /// reader still opens the text the row already holds. Nothing here is
    /// allowed to fail a load.
    fn persist_prompt_documents(
        &mut self,
        documents: Vec<prompt_files::PromptDocument>,
        cx: &mut Context<Self>,
    ) {
        if documents.is_empty() {
            return;
        }
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let paths = prompt_files::persist_off_thread(db_path, documents).await;
            if paths.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.prompt_docs.extend(paths);
                cx.notify();
            });
        })
        .detach();
    }

    /// The two taps a plan waiting on approval offers (#2755).
    ///
    /// They send the literal commands the terminal and Telegram send, through
    /// the composer path that parses them, so a tap and a typed `/execute` are
    /// one code path and never two implementations of an approval.
    pub(crate) fn plan_actions(
        &mut self,
        session: Uuid,
        cx: &mut Context<Self>,
    ) -> plan_strip::PlanActions {
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
    pub(crate) fn pending_message(
        text: String,
        display: Option<String>,
    ) -> agent_bridge::PendingMessage {
        match display {
            Some(shown) => agent_bridge::PendingMessage::system(text, shown),
            None => agent_bridge::PendingMessage::plain(text),
        }
    }

    /// Drain the bridge channel: fold progress into the live view, finalize
    /// the turn on Done/Failed, reload the transcript so the DB becomes the
    /// rendering.
    pub(crate) fn poll(&mut self, cx: &mut Context<Self>) {
        // Config reloads first (#3040): the watcher thread has already loaded
        // the file and published it to `Config::current()`, so this tick only
        // re-derives the values the window cached at boot.
        self.drain_config_reloads(cx);
        // Report window focus to the notification transport (#3005). Nothing
        // in the GUI ever called `set_surface_focused`, so the transport read
        // "unknown", and `should_raise` treats unknown as "the user is not
        // looking": every event raised a banner, including the ones for work
        // that settled while the window sat frontmost. `active_window` is the
        // platform's main window, which is nil whenever another application
        // holds focus, so its presence is the same signal the TUI reports from
        // its focus events. Sampled every tick instead of wired to a callback
        // because gpui hands the dashboard no activation event, and a relaxed
        // atomic store twenty times a second costs nothing.
        opencrabs::utils::desktop_notify::set_surface_focused(cx.active_window().is_some());
        // One clock for the whole tick (#2899): the checks below that cost a
        // file read or a bridge lock answer a question a repaint asks, so they
        // run on the repaint cadence and their answer is reused in between.
        self.poll_tick = self.poll_tick.wrapping_add(1);
        // A frame that stopped moving gets written once (#3082). Sits above the
        // early returns because an idle window is exactly when a drag ends.
        self.settle_window_frame();
        // Playback and background waveform decoding leave no event behind
        // (#3054). One snapshot covers position, pause, completion and the
        // pending visual count, so the dashboard paints exactly when either
        // kind of visible voice state changes.
        let voice_paint = crate::voice_play::paint_state();
        if voice_paint != self.voice_paint {
            self.voice_paint = voice_paint;
            cx.notify();
        }
        // A click on a desktop banner posts the conversation it named (#2751).
        // Drained before the early return below, because an idle window is
        // exactly when a banner gets clicked. The app comes forward first: a
        // click on a notification is a request to look at the window, and
        // opening the conversation behind a hidden window would answer half of
        // it. A session missing from the ledger is dropped with one line
        // rather than opening a window onto a conversation that is gone.
        while let Some(session) = opencrabs::utils::desktop_notify::take_activation() {
            cx.activate(true);
            if !self.sessions.items.iter().any(|row| row.id == session) {
                logging::debug(&format!(
                    "banner click for {session} dropped: the session is gone"
                ));
                continue;
            }
            self.open_session(session, cx);
        }
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
            // A rail animation is worth a repaint only while the rail is on
            // screen (#2899): a closed rail draws no badge, no ring and no
            // flash bar, so those would repaint the whole window, every pane's
            // transcript with it, to change nothing the reader can see.
            let rail_visible = self.rail_open || self.screen == Screen::Onboard;
            // Resolved into locals first (#2899): the rail check writes the
            // window's own fields, so the borrow the pane scan takes has to end
            // before it runs.
            let pane_busy = self.pane_states.values().any(|pane| pane.read(cx).busy);
            let rail_live = self.rail_animating(cx);
            // The rail moves on its own clock now (#2933): it asks for its own
            // repaint, on its own cadence, and the window is not notified, so a
            // spinner frame replays the transcripts beside it instead of
            // rebuilding them.
            if rail_live {
                self.rail_tick(cx);
            }
            if self.turn.busy
                || pane_busy
                // A clip being made is work in flight with no event to carry
                // it (#3092): the card's speaking segment has to keep blinking
                // for however long synthesis takes.
                || !self.voice_pending.is_empty()
                // A rail flash is an animation of its own: the plan bar has to
                // keep alternating orange and its resting colour while the
                // window is open, and stop the moment it closes. The window
                // keeps this one because the flash is resolved on its own clock
                // and handed to the rail in its frame.
                || (rail_visible && flash_active)
            {
                // Only the spinner, the clock and the flash move in this
                // branch, so they repaint on a slower cadence than the poll
                // itself. At 20 Hz this walked every pane's transcript five
                // times as often as the animation needed (#2688).
                //
                // A clip being made is the exception (#3092): its highlight
                // travels on the clock alone, and at a fifth of the poll it
                // moved further than its own width between frames, which reads
                // as a stutter instead of a sweep. The burst lasts exactly as
                // long as synthesis does.
                let waiting_clip = !self.voice_pending.is_empty();
                self.composer.anim_tick = self.composer.anim_tick.wrapping_add(1);
                if waiting_clip || busy_repaint_due(self.composer.anim_tick) {
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
                && self.chat_state.session() != Some(*sid)
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
                        self.ctx_max = match self.chat_state.session() {
                            Some(sid) => b.service.context_limit_for_session(sid),
                            None => b
                                .service
                                .context_window_for_model(&b.service.provider_model()),
                        };
                    }
                    // A window opened on an empty ledger has no conversation
                    // to send into: make one before the first message can be
                    // typed into nothing.
                    if self.chat_state.session().is_none() {
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
                    self.turn.live.notices.push(format!("new session: {e}"));
                }
                agent_bridge::TurnEvent::UpdateAvailable(version) => {
                    self.update_available = Some(version);
                    cx.notify();
                }
                agent_bridge::TurnEvent::MessagesCleared(id) => {
                    if self.chat_state.session() == Some(id) {
                        self.chat_state.transcript.turns = Vec::new();
                        self.chat_state.transcript.hidden = 0;
                        self.chat_state.transcript.error = None;
                    }
                    self.turn.live.notices.push("Session cleared.".to_string());
                    // The card must drop the line of a chat that no longer has
                    // messages. Only the previews are re-read: a rail refresh
                    // here would reorder rows under the cursor.
                    self.refresh_previews(cx);
                    cx.notify();
                }
                agent_bridge::TurnEvent::ClearFailed(text) => {
                    self.turn.live.notices.push(text);
                    cx.notify();
                }
                // The output of a `!cmd`, with the conversation that ran it
                // (#3160). The event used to carry the text alone, so the
                // window painted it on whatever happened to be on screen
                // when the command came back: another pane's chat in a
                // split, and a conversation the reader had already switched
                // away from otherwise. The id it carries is the one that
                // asked, and the line goes to that owner, the same door a
                // command reply takes (#3036).
                agent_bridge::TurnEvent::BangOutput { session, text } => {
                    match session {
                        Some(id) => {
                            // The receipt is logged before it is routed, so a
                            // reader who saw nothing can be told the output
                            // arrived and which conversation took it.
                            logging::debug(&format!("bang output for {id}: {text}"));
                            self.push_session_notice(id, text, cx)
                        }
                        // With no conversation to own it, the line stays on
                        // the window's own strip.
                        None => self.turn.live.notices.push(text),
                    }
                    cx.notify();
                }
                // An editor the user asked for with `!vi`, with the
                // conversation that asked (#3163). The id decides which pane
                // answers and which working directory a relative path is read
                // from, so a split never shows one pane's buffer in another.
                agent_bridge::TurnEvent::EditorRequest { session, command } => {
                    self.open_editor(session, command, cx);
                }
                agent_bridge::TurnEvent::EvolveDone(text) => {
                    self.turn.live.notices.push(text);
                    cx.notify();
                }
                // A text command answered on the bridge: one line, the same
                // wording the terminal prints (#2469). The line belongs to the
                // session that ran the command, so a pane answers on its own
                // strip; a window-level notice (surface, boot summary) carries
                // no session and stays where it has always been (#2672).
                agent_bridge::TurnEvent::CommandReply { session, text } => {
                    match session {
                        Some(id) => {
                            // The receipt is logged before it is routed, so a
                            // reader who saw nothing can be told whether the
                            // answer arrived and which conversation took it
                            // (#3036: a `/cd` applied, and the window showed
                            // neither a line nor a moved footer).
                            logging::debug(&format!("command reply for {id}: {text}"));
                            self.push_session_notice(id, text, cx)
                        }
                        None => self.turn.live.notices.push(text),
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
                    if self.chat_session == Some(id) {
                        // The open pane pointed at a row that no longer is:
                        // clear it rather than leave a transcript of a
                        // deleted conversation on screen.
                        self.set_chat_session(None);
                        self.chat_state.transcript.turns.clear();
                        self.turn.live.clear();
                        // The deleted conversation's turn, if one ran there,
                        // goes with it: the row it belonged to is gone.
                        self.stashed.remove(&id);
                        self.turn.busy = false;
                        // The files screen of a deleted session has nothing
                        // left to list: back to the chat, never a ghost list.
                        if self.screen == Screen::Files {
                            self.screen = Screen::Chat;
                        }
                    }
                    self.refresh_rail(cx);
                }
                agent_bridge::TurnEvent::SessionDeleteFailed(e) => {
                    self.turn.live.notices.push(format!("delete session: {e}"));
                }
                agent_bridge::TurnEvent::PlanPath { session, path } => {
                    // Stale lookups (user switched conversations while the
                    // DB read ran) are dropped, not painted. A pane's own
                    // lookup lands on that pane's strip (#2530).
                    if self.plan_for == Some(session) {
                        self.plan.set_path(path);
                        cx.notify();
                    } else if let Some(pane) = self.pane_view(session) {
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
                    self.composer.agent_error = Some(e.clone());
                    self.turn.live.notices.push(format!("agent offline: {e}"));
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
                    if self.chat_state.session() == Some(session)
                        && let Some(b) = agent_bridge::bridge()
                    {
                        self.ctx_max = b.service.context_limit_for_session(session);
                        self.turn.ctx_tokens = None;
                    }
                    // A pick is a commitment: apply it, say so once, and put
                    // the user back in the conversation that asked. A stale
                    // completion never closes a different screen.
                    if self.screen == Screen::Models && self.models_session == Some(session) {
                        self.screen = Screen::Chat;
                        if self.chat_state.session() == Some(session) {
                            self.pane_focus = None;
                        } else if self
                            .panes
                            .as_ref()
                            .is_some_and(|tree| tree.contains(session))
                        {
                            self.pane_focus = Some(session);
                        }
                        self.sync_composer_session(cx);
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
                agent_bridge::TurnEvent::ModelScopeCounted {
                    session,
                    pair,
                    count,
                } => {
                    if self.models_switching.take() != Some(session) {
                        logging::debug("stale model scope count ignored");
                        continue;
                    }
                    // Nothing to confirm when no other session would change:
                    // the pick is already this-session-only in effect, and a
                    // prompt that says "0 other sessions" only adds a tap.
                    if count == 0 {
                        self.models_apply_confirm = None;
                        self.switch_model(pair, cx);
                        continue;
                    }
                    // The ask line is built before `pair` moves into the
                    // pending confirm: the same borrow escape #3155 closed.
                    let ask_line = {
                        let (provider, model) = pair.split_once('/').unwrap_or((pair.as_str(), ""));
                        opencrabs::channels::commands::apply_all_prompt_text(provider, model, count)
                    };
                    self.models_apply_confirm = Some((pair, count));
                    // A typed `/models <pair> all` never opened the picker
                    // (#3168), so the ask cannot live in a footer nobody can
                    // see: it lands in the conversation, built from the same
                    // text the channels use, with the two keys that answer
                    // it. The picker keeps its footer and adds nothing.
                    if self.screen != Screen::Models {
                        self.push_session_notice(session, ask_line, cx);
                        self.push_session_notice(
                            session,
                            "enter apply to all  esc cancel".to_string(),
                            cx,
                        );
                    }
                    cx.notify();
                }
                agent_bridge::TurnEvent::BrainReady(line) => {
                    logging::info(&format!("brain: {line}"));
                    self.turn.live.notices.push(format!("brain: {line}"));
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
                    self.turn
                        .live
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
                    self.turn.live.thinking = true;
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
                    self.turn.live.thinking = false;
                    self.turn.live.note_chunk_at(&text, at);
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
                    self.turn.live.thinking = false;
                    if let Some(reasoning) = reasoning {
                        self.turn.live.push_reasoning_at(&reasoning, at);
                    }
                    self.turn.live.push_narration(&text);
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
                    self.turn.live.thinking = false;
                    // The ledger's own wording (`tui::app::messaging`), so a
                    // live row reads exactly like the row that persists.
                    let description =
                        opencrabs::tui::app::App::format_tool_description(&tool_name, &tool_input);
                    self.turn
                        .live
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
                        .turn
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
                        None => self.turn.live.push_tool(transcript::ToolCall {
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
                ) if self.chat_state.session() == Some(sid) => {
                    self.turn.live.injected.push(text);
                    cx.notify();
                }
                agent_bridge::TurnEvent::ProgressFor(sid, ProgressEvent::TokenCount(count), _) => {
                    let used = count.min(u32::MAX as usize) as u32;
                    // The count belongs to the conversation that produced it
                    // (#2854). This wrote the window's counter for every
                    // session, so a pane's prompt size landed on the window's
                    // card and the pane's own stayed empty.
                    if self.chat_state.session() == Some(sid) {
                        self.turn.ctx_tokens = Some(used);
                    } else if let Some(pane) = self.pane_view(sid) {
                        pane.update(cx, |pane, cx| {
                            pane.ctx_used = Some(used);
                            cx.notify();
                        });
                    }
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
                    if Some(sid) == self.chat_state.session() {
                        self.turn.live.thinking = false;
                        self.turn
                            .live
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
                // that cannot be missed by a file read. This is the ONLY
                // completion sound: one chime per plan, never one per task
                // (#3118), and it is gated on `notify_on_plan_completed`.
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::PlanCompleted { .. }, _) => {
                    Self::play_plan_chime();
                }
                // The service discarded text it had already streamed, so the
                // card drops it now instead of waiting for the reload (#3021).
                // Without this arm a phantom iteration's working-out stayed on
                // screen as the answer for every retry the guard took.
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::StripStreamedContent { bytes, .. },
                    _,
                ) => {
                    self.turn.live.strip_streamed(bytes);
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
                    voice,
                    completed_plan,
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
                    if self.chat_state.session() != Some(session) {
                        // The turn finished in a conversation that is not on
                        // screen: its parked card retires here, and the rows
                        // in the ledger are what drawing that session reads
                        // when the user opens it again (#2616). A split pane's
                        // own turn settles in its leaf.
                        self.stashed.remove(&session);
                        // The provider's own prompt size for the turn that just
                        // answered, recorded on the leaf before it settles so
                        // the settled card prints what the turn cost (#2854).
                        if let Some(pane) = self.pane_view(session) {
                            pane.update(cx, |pane, cx| {
                                pane.ctx_used = Some(context_tokens);
                                cx.notify();
                            });
                        }
                        self.settle_pane(session, tokens_per_second, cx);
                        // A leaf's voice turn is answered aloud too (#3044),
                        // on the same rule the window's is: `spawn_speak_reply`
                        // runs `channels::voice::synthesize` on the answer the
                        // leaf just drew. Read before the settle finalizes the
                        // card's stream. The flag is the turn's own (#3096), so
                        // a settle that never arrived cannot leave the next
                        // typed turn of this leaf speaking.
                        if voice {
                            let spoken = if content.trim().is_empty() {
                                self.pane_view(session)
                                    .and_then(|pane| {
                                        pane.read(cx).live.answer().map(str::to_string)
                                    })
                                    .unwrap_or_default()
                            } else {
                                content.clone()
                            };
                            agent_bridge::spawn_speak_reply(
                                self.db_path.clone(),
                                self.sink.clone(),
                                session,
                                spoken,
                            );
                        }
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
                    self.turn.ctx_tokens = Some(context_tokens);
                    // The same numbers, frozen for THIS turn, so the settled
                    // card can print what the turn cost instead of whatever
                    // the session's budget reads later (#2324).
                    if let Some(sid) = self.chat_state.session() {
                        self.metrics.pending.insert(
                            sid,
                            transcript::TurnMetrics {
                                ctx_used: context_tokens,
                                ctx_max: self.ctx_max,
                                tps: tokens_per_second,
                                tool_calls: self.turn.live.tools.len(),
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
                    let active_secs = self.turn.live.streaming_active_secs();
                    self.turn.live.finalize(tokens_per_second);
                    // The answer stays on screen: the stream when chunks
                    // arrived, otherwise the response the turn returned. Live
                    // state clears only once the reload proves the DB has it.
                    if self.turn.live.stream.trim().is_empty() && !content.trim().is_empty() {
                        self.turn.live.settled = Some(content.clone());
                    }
                    self.turn.busy = false;
                    self.turn.live.thinking = false;
                    // The turn ended here, so the clock ends here (#2954). It
                    // used to run until the reload proved the row had landed,
                    // which counted the settle wait as part of the turn and
                    // logged a span longer than the one the card settles with.
                    // Not `stop`: that is the verdict, and this turn finished.
                    self.turn.live.freeze();
                    // One line per turn, with the stream's own numbers: a
                    // turn that rendered as one silent dump is diagnosable
                    // from the log (chunks=0 means the provider never
                    // streamed, not that the window failed to draw).
                    logging::info(&format!(
                        "turn done ({} chars, {} chunks, {} bytes, first {:?}, {}s elapsed, {:.2}s active, {} tok, rate {:.1} tok/s, service {:?}), appending to the transcript",
                        content.len(),
                        self.turn.live.chunks,
                        self.turn.live.chunk_bytes,
                        self.turn.live.first_chunk,
                        self.turn.live.elapsed_secs(),
                        active_secs,
                        self.turn.live.tokens,
                        self.turn.live.tokens_per_sec(),
                        tokens_per_second
                    ));
                    // A turn that began as a recording is answered the way a
                    // channel answers a voice note (#3044): aloud, when speech
                    // output is configured. `spawn_speak_reply` runs
                    // `channels::voice::synthesize` on the answer that just
                    // settled, and the clip is written into the assistant row
                    // so the transcript draws it under the words it speaks and
                    // keeps it across a restart. Off-thread on purpose: the
                    // answer is already on screen and synthesis is a network
                    // call per chunk. The flag travels on this event (#3096),
                    // so a turn that never settled cannot hand its voice to
                    // the typed question that follows it.
                    if voice {
                        let spoken = if content.trim().is_empty() {
                            self.turn
                                .live
                                .answer()
                                .map(str::to_string)
                                .unwrap_or_default()
                        } else {
                            content.clone()
                        };
                        agent_bridge::spawn_speak_reply(
                            self.db_path.clone(),
                            self.sink.clone(),
                            session,
                            spoken,
                        );
                    }
                    // The finished turn joins the list as it stands (#3032).
                    // The card has been painting this exchange all along, so
                    // refetching the window and rebuilding every turn only
                    // proved what this state already holds: a thread, a fresh
                    // runtime, its own DB connection and a 200-row read, once
                    // per turn, 1:1 with the turns themselves. The reload is
                    // kept for the reads that need the rows, which is a
                    // resumed task with no question of its own and the settle
                    // path that proves a slow write landed.
                    //
                    // `rows` stays behind by one on purpose: the turn's row id
                    // only exists once a read returns it, and inventing one
                    // would put an id in the copy order that no row carries.
                    // That is the state the live card was already in, and the
                    // next reload reconciles it.
                    if let Some(sid) = self.chat_state.session() {
                        match transcript::turn_from_live(&self.turn.live, None) {
                            Some(mut turn) => {
                                // The checklist this turn finished is a row in
                                // the ledger, not something the card painted,
                                // so the append has to carry it: without it the
                                // card only came back on the next reload, and a
                                // conversation that finishes a plan and keeps
                                // talking never had one (#3142).
                                if let Some(block) = completed_plan
                                    .as_deref()
                                    .and_then(transcript::completed_plan_block)
                                {
                                    turn.blocks.push(block);
                                }
                                logging::info(&format!(
                                    "turn appended: {} blocks, anchor {}",
                                    turn.blocks.len(),
                                    turn.anchor
                                ));
                                self.chat_state.transcript.turns.push(turn);
                                // The numbers recorded at `Done` belong to the
                                // turn that just went on the list (#2324).
                                self.attach_pending_metrics(sid);
                                // No read compares a total against these, so
                                // they must not outlive the turn: a stale
                                // expectation would turn the next reload's
                                // first short read into a phantom retry
                                // (#2705).
                                self.turn.settle.expect = None;
                                self.turn.settle.retries = 0;
                                self.turn.settle.deadline = None;
                                self.turn.settle.rechecks = 0;
                                self.turn.live.clear();
                            }
                            // Nothing to append from, so the rows are the only
                            // record of this turn.
                            None => reload = Some(sid),
                        }
                    }
                }
                agent_bridge::TurnEvent::Failed { session, text } => {
                    // Same as Done: the live line was this turn's, so it goes
                    // with it (#2718).
                    self.rail_live.remove(&session);
                    // A failed turn is not spoken (#3044): there is no answer
                    // to read aloud. Nothing has to be cleared here either, the
                    // flag lived on the turn that just failed (#3096).
                    // A failure is still activity, so the row moves like a
                    // settled turn does (#2666).
                    self.refresh_rail(cx);
                    if self.chat_state.session() != Some(session) {
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
                    if !self.turn.live.is_empty() {
                        self.turn.live.notices.push(text);
                    }
                    // A failed turn still ran, so it keeps the numbers it
                    // actually measured instead of showing a bare clock while
                    // its neighbours carry ctx and rate (#2705). The record is
                    // only written when the provider streamed something: a
                    // failure before the first call has no reading of its own,
                    // and borrowing the previous turn's budget would be a lie.
                    self.metrics.pending.remove(&session);
                    self.turn.live.finalize(None);
                    if self.turn.live.tokens > 0
                        && let Some(used) = self.turn.ctx_tokens
                    {
                        self.metrics.pending.insert(
                            session,
                            transcript::TurnMetrics {
                                ctx_used: used,
                                ctx_max: self.ctx_max,
                                tps: Some(f64::from(self.turn.live.tokens_per_sec()))
                                    .filter(|t| *t > 0.0),
                                tool_calls: self.turn.live.tools.len(),
                            },
                        );
                    }
                    self.turn.busy = false;
                    self.turn.live.thinking = false;
                    reload = self.chat_state.session();
                }
                agent_bridge::TurnEvent::Cancelled { session } => {
                    // What the card had streamed when the stop landed (#2851).
                    // The cancel drops the turn's future, so nothing else will
                    // ever write this text, and every read of the session after
                    // the stop (a switch, a reload, a restart) can only draw
                    // what the rows hold. Capture it before the branches below
                    // settle the card, in the order the rail resolves which
                    // card owns the turn: a split leaf first, then the parked
                    // copy of a switched-away turn, then the window's own.
                    let stopped_answer = {
                        let pane_answer = self
                            .pane_states
                            .get(&session)
                            .and_then(|pane| pane.read(cx).live.answer().map(str::to_string));
                        let parked_answer = self
                            .stashed
                            .get(&session)
                            .and_then(|parked| parked.live.answer().map(str::to_string));
                        let live_answer = if self.chat_state.session() == Some(session) {
                            self.turn.live.answer().map(str::to_string)
                        } else {
                            None
                        };
                        pane_answer.or(parked_answer).or(live_answer)
                    };
                    // A stop ends the turn, so the card's live line does too
                    // (#2718).
                    self.rail_live.remove(&session);
                    // A stop dropped the turn where it stood (#2657), so no
                    // Done and no Failed follows. This is the settle, and it
                    // carries no text: whoever asked for the stop already said
                    // "Operation cancelled", so a second line would be noise.
                    self.refresh_rail(cx);
                    self.metrics.pending.remove(&session);
                    // The verdict needs a row (#2811). A cancelled turn writes
                    // no error and no answer of its own, so without this the
                    // turn settles as Finished and the stop vanishes on the
                    // next reload. Written for the cancelled session, whichever
                    // one is on screen, because the turn was real where it ran.
                    // The text the card had streamed goes with it (#2851): the
                    // future that would have written it is gone, so this call is
                    // the only thing that can put it in the ledger, and the
                    // reload after the stop reads nothing else.
                    if let Some(bridge) = agent_bridge::bridge() {
                        agent_bridge::spawn_record_turn_stopped(
                            bridge.service.clone(),
                            session,
                            stopped_answer,
                        );
                    }
                    if self.chat_state.session() == Some(session) {
                        // No reload on purpose. The write above runs on its own
                        // thread, so a read taken from this frame can land
                        // before the answer row exists and draw the turn
                        // without it. The card keeps its stream, with the clock
                        // and rate frozen where the stop landed, and the next
                        // read of this session finds the answer in the ledger.
                        self.turn.live.finalize(None);
                        self.turn.busy = false;
                        self.turn.live.thinking = false;
                        // The stop is the settle (#2811): the card keeps its
                        // stream and now says it was stopped, instead of
                        // waiting on a spinner that will never turn again.
                        self.turn.live.stop();
                        // A stop is still a turn that ran, so it keeps the
                        // numbers it measured (#2953). The record was removed
                        // above and never written back, so the card showed ctx
                        // and rate on screen and lost both on the next read.
                        // Same guard as a failure: nothing streamed means no
                        // reading of its own, and borrowing the previous
                        // turn's budget would be a lie.
                        if let Some(metrics) = transcript::TurnMetrics::measured(
                            self.turn.live.tokens,
                            self.turn.ctx_tokens,
                            self.ctx_max,
                            self.turn.live.tokens_per_sec(),
                            self.turn.live.tools.len(),
                        ) {
                            self.metrics.pending.insert(session, metrics);
                        }
                        self.turn.cancel = None;
                        logging::info("turn settled after the stop dropped it");
                    } else {
                        // A split leaf: same settle inside its own card, and
                        // the parked copy of a switched-away turn retires.
                        self.stashed.remove(&session);
                        if let Some(pane) = self.pane_view(session) {
                            let measured = pane.update(cx, |pane, cx| {
                                pane.live.finalize(None);
                                pane.live.thinking = false;
                                pane.live.stop();
                                pane.busy = false;
                                pane.cancel = None;
                                cx.notify();
                                (
                                    pane.live.tokens,
                                    pane.ctx_used,
                                    pane.ctx_max,
                                    pane.live.tokens_per_sec(),
                                    pane.live.tools.len(),
                                )
                            });
                            // The leaf keeps its own reading, never the
                            // window's (#2953, the #2854 split).
                            let (tokens, ctx_used, ctx_max, tps, tool_calls) = measured;
                            if let Some(metrics) = transcript::TurnMetrics::measured(
                                tokens, ctx_used, ctx_max, tps, tool_calls,
                            ) {
                                self.metrics.pending.insert(session, metrics);
                            }
                        }
                        // The stop is acknowledged: the leaf's queue becomes
                        // the next turn now, never while the cancel unwinds
                        // (#2695, the #2631 ordering the window keeps).
                        self.drain_pane_pending(session, cx);
                    }
                    // The turn is gone, so its rail spinner goes with it.
                    self.rail.clear(session);
                    // A stopped turn says nothing, so it is not spoken either
                    // (#3044). Nothing has to be cleared for that: the flag was
                    // the stopped turn's own and it is over with it (#3096).
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
                    if self.chat_state.session() == Some(session) {
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
                    if self.chat_state.session() == Some(session) {
                        self.turn.live = transcript::LiveTurn::default();
                        // The ledger already holds this question: the process
                        // that asked it died after writing the row. Resuming is
                        // not a fresh send, and starting it as one stamped the
                        // resume time, missed the row in the identity window,
                        // and painted the question twice (#2740).
                        self.turn.live.resume(user_message);
                        self.turn.busy = true;
                        self.turn.cancel = Some(cancel);
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
                            // Recovered mid-flight, so the turn is running.
                            chat_leaf::Turn::running(live, cancel),
                        );
                    }
                    self.refresh_rail(cx);
                    cx.notify();
                }
                // The clip for a voice turn is being made (#3092):
                // `VoiceReplyPending` says synthesis started, which is a
                // network round trip per chunk. The answer settled a moment
                // ago, so the card is told to say the clip is coming rather
                // than sitting finished and then growing a clip out of nowhere.
                agent_bridge::TurnEvent::VoiceReplyPending { session } => {
                    if self.voice_pending.insert(session) {
                        cx.notify();
                    }
                }
                // Spoken or not, the attempt is over (#3092). The success path
                // sends `VoiceReplySaved` first, which is what reloads the row;
                // this only takes the segment off the card.
                agent_bridge::TurnEvent::VoiceReplyDone { session } => {
                    if self.voice_pending.remove(&session) {
                        cx.notify();
                    }
                }
                // The spoken reply for a voice turn is on disk and its marker
                // is in the assistant row (#3044). The `Done` that settled the
                // turn has already reloaded without it, so this read is what
                // puts the clip under the words it speaks.
                //
                // It plays itself (#3092), which is the whole point of waiting
                // for it, but only in the conversation the reader is looking at
                // and only when nothing else is already sounding: a reply that
                // lands while he is listening to another clip waits, and a
                // conversation off screen starts silent.
                agent_bridge::TurnEvent::VoiceReplySaved { session, path } => {
                    if self.chat_state.session() == Some(session) {
                        // Every outcome leaves a line (#3092). The player
                        // reports a clip that would not open and nothing else,
                        // so a run that autoplayed and a run that skipped look
                        // identical in the log, which is no way to answer "did
                        // it play" after the sound is already gone.
                        if let Some(other) = crate::voice_play::sounding() {
                            logging::info(&format!(
                                "voice reply autoplay: held back, {} is still sounding",
                                other.display()
                            ));
                        } else if let Err(reason) = crate::voice_play::autoplay(&path) {
                            logging::error(&format!("voice reply autoplay: {reason}"));
                        } else {
                            logging::info(&format!("voice reply autoplay: {}", path.display()));
                        }
                        reload = Some(session);
                    } else if self.pane_view(session).is_some() {
                        self.reload_pane(session, cx);
                    }
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
                    } else if self.chat_state.session() == Some(session_id) {
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
        // The row above the composer reads the turn and the queue from this
        // same state, so the repaint below is what shows the drain (#3102).
        if !self.turn.busy {
            self.turn.cancel = None;
            self.drain_pending(cx);
        }
        // The turn and the queue both just moved, and the field's hint is
        // decided from them (#2630, #3102).
        self.sync_composer_hint(cx);
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
        let session = self.chat_state.session()?;
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
        opencrabs::tui::render::format_background_tasks(&tasks, task_list::STRIP_LABEL_CHARS)
    }

    /// One session's running tasks as the card prints them, oldest first.
    pub(crate) fn task_rows(&self, session: Uuid) -> Vec<task_list::TaskRow> {
        let Some(mgr) = agent_bridge::bridge().and_then(|b| b.service.background_manager()) else {
            return Vec::new();
        };
        mgr.running_tasks(session)
            .into_iter()
            .map(|t| task_list::TaskRow {
                id: t.task_id,
                tag: t.source.tag(),
                secs: t.started.elapsed().as_secs(),
                ours: t.cancel.is_some(),
                label: t.label,
            })
            .collect()
    }

    /// What sits above the composer while `session` has work running: the one
    /// line, or the list it opens into. `label` is the line the caller already
    /// formatted for this frame.
    ///
    /// At rest it is always the one line: the list shows only after a click
    /// or a Down from the composer. `None` when nothing runs, which also
    /// forgets that the list was open, so the card closes on its own when the
    /// last task ends and the next task starts collapsed.
    pub(crate) fn task_area(
        &mut self,
        session: Uuid,
        label: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let rows = self.task_rows(session);
        if rows.is_empty() {
            self.forget_tasks(session);
            return None;
        }
        if !self.tasks_open.contains(&session) {
            let weak = cx.entity().downgrade();
            let tap: StripTap = Rc::new(move |_window, app| {
                let _ = weak.update(app, |this, cx| this.toggle_tasks(session, cx));
            });
            return Some(task_strip(label.to_string(), Some(tap)).into_any_element());
        }
        let focused = self.task_focus == Some(session);
        // A row that finished takes its output with it: the card must not go
        // on pointing at a task that is no longer there.
        let open = self
            .task_open
            .get(&session)
            .copied()
            .filter(|id| rows.iter().any(|row| row.id == *id));
        if open.is_none()
            && let Some(gone) = self.task_open.remove(&session)
        {
            self.task_tails.remove(&gone);
        }
        // With the keyboard on the list a row is always selected: when the
        // one it stood on finishes or is cancelled, the cursor lands on the
        // oldest task left, so the next Esc has a visible target.
        let open = match open {
            None if focused => {
                let first = rows[0].id;
                self.task_open.insert(session, first);
                Some(first)
            }
            other => other,
        };
        let tail = open.and_then(|task| self.task_tail(session, task));
        let on_header: transcript::ClickHandler = Rc::new(cx.listener(
            move |this: &mut Self, _ev: &gpui::ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.toggle_tasks(session, cx);
            },
        ));
        let on_rows: Vec<transcript::ClickHandler> = rows
            .iter()
            .map(|row| {
                let task = row.id;
                let pick: transcript::ClickHandler = Rc::new(cx.listener(
                    move |this: &mut Self, _ev: &gpui::ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.toggle_task_output(session, task, cx);
                    },
                ));
                pick
            })
            .collect();
        let on_open: Vec<transcript::ClickHandler> = rows
            .iter()
            .map(|row| {
                let task = row.id;
                let door: transcript::ClickHandler = Rc::new(cx.listener(
                    move |this: &mut Self, _ev: &gpui::ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.open_task_detail(session, task, cx);
                    },
                ));
                door
            })
            .collect();
        Some(
            task_list::task_card(
                session,
                &rows,
                open,
                focused,
                tail.as_ref().map(|t| t.as_ref()),
                task_list::CardClicks {
                    header: on_header,
                    rows: on_rows,
                    open: on_open,
                },
            )
            .into_any_element(),
        )
    }

    /// The open task's output, re-read at most once a tick. The file is
    /// looked up until it is found and then remembered, because a claude-cli
    /// task's file appears a moment after the task does.
    fn task_tail(&mut self, session: Uuid, task: Uuid) -> Option<SharedString> {
        let fresh = self
            .task_tails
            .get(&task)
            .is_some_and(|tail| tail.read_at.elapsed() < task_list::TAIL_TICK);
        if !fresh {
            let mgr = agent_bridge::bridge().and_then(|b| b.service.background_manager())?;
            let path = self
                .task_tails
                .get(&task)
                .and_then(|tail| tail.path.clone())
                .or_else(|| mgr.output_path_for(session, task));
            let text = path
                .as_deref()
                .map(|p| SharedString::from(mgr.output_tail(p, task_list::TAIL_LINES)));
            self.task_tails.insert(
                task,
                TaskTail {
                    read_at: std::time::Instant::now(),
                    path,
                    text,
                },
            );
        }
        self.task_tails
            .get(&task)
            .and_then(|tail| tail.text.clone())
    }

    /// Open or close a session's task list with the pointer. Closing a list
    /// that held the keyboard gives the keyboard back.
    pub(crate) fn toggle_tasks(&mut self, session: Uuid, cx: &mut Context<Self>) {
        if self.tasks_open.remove(&session) {
            if self.task_focus == Some(session) {
                self.task_focus = None;
            }
        } else {
            self.tasks_open.insert(session);
        }
        self.task_area_resized(session, cx);
    }

    /// Down from the composer: open the session's task list and hand it the
    /// keyboard, the cursor on the row already showing or on the oldest task.
    /// Nothing running leaves the keyboard where it was.
    pub(crate) fn focus_tasks(&mut self, session: Uuid, cx: &mut Context<Self>) {
        let rows = self.task_rows(session);
        let Some(first) = rows.first() else {
            return;
        };
        let standing = self
            .task_open
            .get(&session)
            .is_some_and(|id| rows.iter().any(|row| row.id == *id));
        if !standing && let Some(was) = self.task_open.insert(session, first.id) {
            self.task_tails.remove(&was);
        }
        self.tasks_open.insert(session);
        self.task_focus = Some(session);
        self.task_area_resized(session, cx);
    }

    /// One key while the task list holds the keyboard (see
    /// [`task_list::key_step`]).
    pub(crate) fn task_key(&mut self, session: Uuid, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        let rows = self.task_rows(session);
        let at = self
            .task_open
            .get(&session)
            .and_then(|id| rows.iter().position(|row| row.id == *id));
        match task_list::key_step(ks.key.as_str(), at, rows.len()) {
            task_list::KeyStep::Select(ix) => {
                let task = rows[ix].id;
                if let Some(was) = self.task_open.insert(session, task)
                    && was != task
                {
                    self.task_tails.remove(&was);
                }
                self.task_area_resized(session, cx);
            }
            task_list::KeyStep::Cancel(ix) => {
                let outcome = agent_bridge::bridge()
                    .and_then(|b| b.service.background_manager())
                    .map(|mgr| mgr.cancel(session, rows[ix].id));
                logging::debug(&format!(
                    "task list: esc on {:?} -> {outcome:?}",
                    rows[ix].label
                ));
                // A stopped task leaves the roster through its own completion
                // path, and the cursor then moves to the next row. A task that
                // is not ours to stop cannot be cancelled from here, so Esc is
                // the way out instead of a key that does nothing.
                if !outcome.is_some_and(|outcome| outcome.signalled()) {
                    self.leave_tasks(session, cx);
                } else {
                    cx.notify();
                }
            }
            task_list::KeyStep::Open(ix) => self.open_task_detail(session, rows[ix].id, cx),
            task_list::KeyStep::Leave => self.leave_tasks(session, cx),
        }
    }

    /// Lift one running task into its full view (see [`task_detail`]).
    pub(crate) fn open_task_detail(&mut self, session: Uuid, task: Uuid, cx: &mut Context<Self>) {
        let Some(row) = self.task_rows(session).into_iter().find(|r| r.id == task) else {
            return;
        };
        // The id, not the label: a label is a command or a description and
        // can carry whatever the user typed into it.
        logging::info(&format!("task detail: opened {task}"));
        self.task_detail = Some(task_detail::TaskDetail::new(
            session, task, row.label, row.tag, row.ours, row.secs,
        ));
        // A fresh handle: the last view's offset belongs to another output.
        self.task_detail_scroll = ScrollHandle::new();
        self.task_detail_generation = self.task_detail_generation.wrapping_add(1);
        let generation = self.task_detail_generation;
        self.refresh_task_detail();
        // The view repaints on its own beat while it is open: a new line and
        // the pin to the end must land even when nothing else in the window
        // changes. The loop ends with the view it was started for.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(task_detail::REPAINT_TICK)
                    .await;
                let open = this
                    .update(cx, |view, cx| {
                        if view.task_detail.is_none() || view.task_detail_generation != generation {
                            return false;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !open {
                    break;
                }
            }
        })
        .detach();
        self.refocus_overlay();
        cx.notify();
    }

    /// Close the view. The keyboard goes back to the task list when it is
    /// still open, and to the composer otherwise.
    pub(crate) fn close_task_detail(&mut self, cx: &mut Context<Self>) {
        if self.task_detail.take().is_some() {
            logging::info("task detail: closed");
        }
        self.refocus_screen();
        cx.notify();
    }

    /// Look at the open task again, at most once a [`task_detail::READ_TICK`]:
    /// whether it still runs, where its output is, and what it wrote since.
    fn refresh_task_detail(&mut self) {
        let Some(detail) = self.task_detail.as_mut() else {
            return;
        };
        if detail
            .read_at
            .is_some_and(|at| at.elapsed() < task_detail::READ_TICK)
        {
            return;
        }
        detail.read_at = Some(std::time::Instant::now());
        let Some(mgr) = agent_bridge::bridge().and_then(|b| b.service.background_manager()) else {
            return;
        };
        let running = mgr
            .running_tasks(detail.session)
            .iter()
            .any(|t| t.task_id == detail.task);
        if !running && detail.ended.is_none() {
            detail.ended = Some(detail.started.elapsed().as_secs());
            detail.confirm_stop = false;
        }
        if detail.path.is_none() && running {
            detail.path = mgr.output_path_for(detail.session, detail.task);
        }
        let Some(path) = detail.path.clone() else {
            return;
        };
        // A file that has not changed is not read again: the read scrubs up
        // to half a megabyte, which is not worth doing once a second for a
        // task that is thinking.
        let stamp = std::fs::metadata(&path)
            .ok()
            .map(|m| (m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
        if stamp.is_some() && stamp == detail.stamp {
            return;
        }
        detail.stamp = stamp;
        detail.absorb(mgr.output_history(&path));
    }

    /// One key while the full view holds the keyboard (see
    /// [`task_detail::detail_key`]).
    pub(crate) fn task_detail_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        let Some(detail) = self.task_detail.as_mut() else {
            return;
        };
        let step =
            task_detail::detail_key(ks.key.as_str(), detail.confirm_stop, detail.stoppable());
        let scroll = self.task_detail_scroll.clone();
        match step {
            task_detail::DetailStep::Close => self.close_task_detail(cx),
            task_detail::DetailStep::Scroll(dy) => {
                let offset = scroll.offset();
                let max = scroll.max_offset().height;
                let y = (offset.y - px(dy)).min(px(0.0)).max(-max);
                scroll.set_offset(gpui::point(offset.x, y));
                // Up leaves the newest line; the render decides whether a
                // Down that reached the end is following again.
                if dy < 0.0 {
                    detail.following = false;
                }
                cx.notify();
            }
            task_detail::DetailStep::Top => {
                scroll.set_offset(gpui::point(scroll.offset().x, px(0.0)));
                detail.following = false;
                cx.notify();
            }
            task_detail::DetailStep::Follow => self.follow_task_detail(cx),
            task_detail::DetailStep::AskStop => {
                detail.confirm_stop = true;
                cx.notify();
            }
            task_detail::DetailStep::ConfirmStop => self.stop_task_detail(cx),
            task_detail::DetailStep::DropConfirm => {
                detail.confirm_stop = false;
                cx.notify();
            }
            task_detail::DetailStep::Ignore => {}
        }
    }

    /// Jump to the newest line and stay on it.
    fn follow_task_detail(&mut self, cx: &mut Context<Self>) {
        if let Some(detail) = self.task_detail.as_mut() {
            detail.following = true;
            detail.unseen = 0;
            let scroll = &self.task_detail_scroll;
            let max = scroll.max_offset().height;
            scroll.set_offset(gpui::point(scroll.offset().x, -max));
            detail.pinned_max = f32::from(max);
        }
        cx.notify();
    }

    /// The stop door: the first press asks, the second stops. The task then
    /// ends through its own exit path, and the view says so when it does.
    fn stop_task_detail(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.task_detail.as_mut() else {
            return;
        };
        if !detail.stoppable() {
            return;
        }
        if !detail.confirm_stop {
            detail.confirm_stop = true;
            cx.notify();
            return;
        }
        detail.confirm_stop = false;
        let outcome = agent_bridge::bridge()
            .and_then(|b| b.service.background_manager())
            .map(|mgr| mgr.cancel(detail.session, detail.task));
        logging::info(&format!(
            "task detail: stop on {} -> {outcome:?}",
            detail.task
        ));
        detail.stop_sent = outcome.is_some_and(|outcome| outcome.signalled());
        cx.notify();
    }

    /// The modal for this frame: a fresh look at the task, the follow
    /// reconciled with where the reader is, and the doors minted.
    pub(crate) fn task_detail_overlay(
        &mut self,
        viewport: (f32, f32),
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.refresh_task_detail();
        let scroll = self.task_detail_scroll.clone();
        let detail = self.task_detail.as_mut()?;
        let offset = scroll.offset();
        let max = f32::from(scroll.max_offset().height);
        let (following, pin) = task_detail::follow_step(
            detail.following,
            -f32::from(offset.y),
            max,
            detail.pinned_max,
        );
        detail.following = following;
        if let Some(pin) = pin {
            scroll.set_offset(gpui::point(offset.x, px(-pin)));
            detail.pinned_max = pin;
            detail.unseen = 0;
        }
        let close: transcript::ClickHandler = Rc::new(cx.listener(
            |this: &mut Self, _ev: &gpui::ClickEvent, _window, cx| this.close_task_detail(cx),
        ));
        let stop: transcript::ClickHandler = Rc::new(cx.listener(
            |this: &mut Self, _ev: &gpui::ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.stop_task_detail(cx);
            },
        ));
        let follow: transcript::ClickHandler = Rc::new(cx.listener(
            |this: &mut Self, _ev: &gpui::ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.follow_task_detail(cx);
            },
        ));
        let detail = self.task_detail.as_ref()?;
        Some(
            task_detail::detail_modal(
                detail,
                &scroll,
                viewport,
                task_detail::DetailDoors {
                    close,
                    stop,
                    follow,
                },
            )
            .into_any_element(),
        )
    }

    /// Give the keyboard back to the composer and fold the list to its one
    /// line, which is how it rests.
    fn leave_tasks(&mut self, session: Uuid, cx: &mut Context<Self>) {
        self.forget_tasks(session);
        self.task_area_resized(session, cx);
    }

    /// Drop the open and focused state of every session that has nothing
    /// running any more. The render asks before it decides who owns the
    /// keyboard, because a session with no task draws no strip and so never
    /// reaches [`Self::task_area`] to be forgotten there.
    pub(crate) fn prune_task_state(&mut self) {
        if self.tasks_open.is_empty() && self.task_focus.is_none() {
            return;
        }
        let stale: Vec<Uuid> = self
            .tasks_open
            .iter()
            .copied()
            .chain(self.task_focus)
            .filter(|session| self.task_rows(*session).is_empty())
            .collect();
        for session in stale {
            self.forget_tasks(session);
        }
    }

    /// Show one task's output, or hide it when it is the one already showing.
    fn toggle_task_output(&mut self, session: Uuid, task: Uuid, cx: &mut Context<Self>) {
        if let Some(was) = self.task_open.remove(&session) {
            self.task_tails.remove(&was);
            if was != task {
                self.task_open.insert(session, task);
            }
        } else {
            self.task_open.insert(session, task);
        }
        self.task_area_resized(session, cx);
    }

    fn forget_tasks(&mut self, session: Uuid) {
        self.tasks_open.remove(&session);
        if self.task_focus == Some(session) {
            self.task_focus = None;
        }
        if let Some(task) = self.task_open.remove(&session) {
            self.task_tails.remove(&task);
        }
    }

    /// The card takes its room from the transcript above it, so a reader at
    /// the bottom must stay at the bottom, the same care the plan strip takes
    /// (#2685, #2624).
    fn task_area_resized(&mut self, session: Uuid, cx: &mut Context<Self>) {
        if Some(session) == self.chat_session {
            self.chat_state.follow.rearm();
        } else if let Some(pane) = self.pane_states.get(&session) {
            pane.update(cx, |pane, _| pane.follow.rearm());
        }
        cx.notify();
    }

    /// Whether any rail row is animating, resolved at the repaint cadence
    /// rather than the poll's (#2899).
    ///
    /// The scan walks every ordered session and each row costs a plan `stat`
    /// and a detached-task lock, while the badge it looks for only changes
    /// what the rail draws. The repaint it gates already runs at 5 Hz, so
    /// asking twenty times a second bought nothing but four identical answers
    /// and four times the file reads. The answer is cached for the ticks in
    /// between, and a closed rail is not asked at all: it draws no badge.
    ///
    /// Only a live turn's spinner and a waiting plan's ring move (#2842). A
    /// plan holding a task draws a still mark, so it is not a reason to tick
    /// the rail's clock at all.
    fn rail_animating(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.rail_open && self.screen != Screen::Onboard {
            self.rail_anim_live = false;
            return false;
        }
        if !busy_repaint_due(self.poll_tick) {
            return self.rail_anim_live;
        }
        let live = self
            .rail_activity_map(cx)
            .values()
            .any(|row| row.badge == Some(crate::rail_activity::Badge::Live) || row.ring);
        self.rail_anim_live = live;
        live
    }
}
