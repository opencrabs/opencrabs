//! Arriving on a conversation, and moving between what the window shows:
//! the follow that keeps a transcript on its newest row, ctrl+c, the rail
//! hand-back, the overlay stack and the focus each screen leaves behind.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// Re-read the channel chip now (#2452): after a Start click succeeds,
    /// the daemon appears within a second or two, so poll instead of
    /// waiting for the next 5-second tick. All IO runs off the UI thread.
    pub(crate) fn refresh_channel_chip(&mut self, cx: &mut Context<Self>) {
        let names = self.chip_channel_names.clone();
        cx.spawn(async move |this, cx| {
            let line = cx
                .background_executor()
                .spawn(async move { channel_chip_line_from(&names) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.channel_chip != line {
                    logging::info(&format!(
                        "channel chip -> {}",
                        line.as_ref().map(|s| s.as_ref()).unwrap_or("hidden")
                    ));
                    this.channel_chip = line;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The chip's action (#2452): install and start the daemon service via
    /// the onboarding path (launchd on macOS, systemd user unit on Linux).
    /// This is the only way the GUI ever starts one, and only on a click:
    /// nothing here spawns a ChannelManager or claims a channel lock.
    pub(crate) fn start_daemon(&mut self, cx: &mut Context<Self>) {
        if self.daemon_starting {
            return;
        }
        self.daemon_starting = true;
        self.daemon_error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { opencrabs::tui::onboarding::config::install_daemon_service() })
                .await;
            this.update(cx, |this, cx| {
                this.daemon_starting = false;
                match result {
                    Ok(()) => {
                        // The probe loop and the refresh below confirm the
                        // daemon is live; the chip drops on its own word.
                        this.refresh_channel_chip(cx);
                        logging::info("daemon service installed");
                    }
                    Err(e) => {
                        this.daemon_error = Some(SharedString::from(e));
                        cx.notify();
                    }
                }
            })
        })
        .detach();
    }

    /// Title of the conversation the chat pane is on, if the rail knows it.
    /// The name of the conversation on screen, as every surface prints it
    /// (#2766). One parse lives behind this: the platform is the card's mark
    /// and the channel key is bookkeeping, so neither belongs in a header, a
    /// pane border or the footer identity.
    pub(crate) fn session_title(&self) -> Option<String> {
        self.chat_session.and_then(|id| {
            self.sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.as_deref())
                .map(|stored| rail::chip_name(Some(stored), None))
        })
    }

    /// The one place the open conversation is written.
    ///
    /// `chat_state` owns the conversation on screen and `chat_session` mirrors
    /// it for the window, which decides the rail's mark, the split fill, the
    /// plan strip and the model picker with it on every frame. A second write
    /// site would let the mirror drift from its owner, and a stale mirror
    /// points every one of those surfaces at the wrong conversation, so both
    /// halves move here and nowhere else: a switch, a new topic and a session
    /// being deleted all arrive through this door.
    pub(crate) fn set_chat_session(&mut self, session: Option<Uuid>) {
        // A conversation opens at its newest window: the history another one
        // loaded with ctrl+o does not follow it here (#2805).
        if self.chat_state.transcript.session != session {
            self.chat_state.transcript.older_loaded = 0;
        }
        self.chat_state.transcript.session = session;
        self.chat_session = session;
    }

    /// One door for arriving on a conversation (#2481): every surface that
    /// belongs to the conversation on screen resets here, so nothing carries
    /// across. Budget, metrics, queue, composer draft and attachments are
    /// per-conversation state; a fresh topic must open as a completely
    /// isolated conversation, and a plain switch must stop carrying a draft.
    /// The reset keeps the same-session guard the old open_session had:
    /// re-clicking the chip of the conversation already on screen is not a
    /// switch, so its queue and draft survive. The create-topic path always
    /// passes a brand-new id, so the full reset fires there every time.
    pub(crate) fn begin_conversation(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let switched = self.chat_state.session() != Some(id);
        self.sessions.selected = Some(id);
        // An in-place rewrite names a row in one conversation (#2826). Leaving
        // that conversation ends it, or the row would still be open for a
        // rewrite in a chat the user is no longer looking at.
        self.edit_row = None;
        if switched {
            // The turn on screen belongs to the conversation being left. A
            // turn still running there is parked under it, and the
            // destination's own turn (or an empty slot) comes on screen, so
            // neither the spinner nor the ctx counter follows the switch
            // (#2616). Runs before `set_chat_session` moves it: the previous
            // id is the key the parked turn is stored under.
            swap_visible_turn(
                &mut self.stashed,
                self.chat_state.session(),
                id,
                &mut self.turn,
            );
            // Every switch says who it left, who it arrived on and whether a
            // card was parked (#2811). The report that started this had only
            // screenshots to work from: a switch left no trace at all, so "the
            // state vanished" could not be told apart from "it was never
            // parked". One line makes the next repro greppable.
            logging::info(&format!(
                "session switch {:?} -> {id}: {}",
                self.chat_state.session(),
                if self
                    .chat_state
                    .session()
                    .is_some_and(|from| self.stashed.contains_key(&from))
                {
                    "card parked"
                } else {
                    "nothing parked"
                }
            ));
            // Metrics belong to the turn they settled, never to the session
            // the user just switched to. Only the window's own numbers are
            // dropped here: a pane's settled card is claimed by its own
            // session, and clearing the whole map wiped it before the claim
            // ever ran (#2854).
            if let Some(from) = self.chat_state.session() {
                self.metrics.pending.remove(&from);
            }
            if let Some(b) = agent_bridge::bridge() {
                self.ctx_max = b.service.context_limit_for_session(id);
            }
            // A recall belongs to the conversation it started in too, so its
            // messages go back to wait instead of vanishing with the old draft
            // (#2637, #2696).
            let returned = self
                .composer
                .editor
                .update(cx, |ed, _| ed.release_recall(false));
            if let Some((session, msgs)) = returned {
                for msg in msgs {
                    agent_bridge::queue_recall_return(session, msg);
                }
            }
            // The draft and its attachments belong to the conversation they
            // were typed in (#2481). The text is filed under the conversation
            // being left and the destination's own draft comes back, so a
            // switch and a switch back return the same half-typed message.
            let previous = self.chat_state.session();
            let draft = self.composer.editor.update(cx, |ed, cx| ed.take_draft(cx));
            if let Some(previous) = previous {
                self.composer.drafts.store(previous, draft);
            }
            let returning = self.composer.drafts.take(id);
            self.composer.editor.update(cx, move |ed, cx| {
                ed.begin_conversation(cx);
                if let Some(draft) = returning {
                    ed.restore_state(draft, cx);
                }
            });
        }
        let previous = self.chat_state.session();
        self.set_chat_session(Some(id));
        // The chat is a leaf of the pane tree, so switching conversations
        // moves that leaf instead of leaving a stale pane behind (#2472).
        self.adopt_chat_leaf(previous, id, cx);
        // The next launch opens this conversation. Both surfaces read the same
        // file, so switching here means the terminal comes back to it too, and
        // a restart no longer lands on whatever the rail listed first (#2444).
        remember_open_session(id);
        agent_bridge::set_composer_session(Some(id));
        self.chat_state.transcript.error = None;
        self.chat_state.transcript.turns = Vec::new();
        self.chat_state.transcript.rows = Vec::new();
        self.chat_state.transcript.hidden = 0;
        // A menu or a pick belongs to the rows that were on screen: keeping
        // either across a conversation switch would act on another chat's
        // messages, so both are dropped here with the rows (#2613, #2608).
        self.msg_menu = None;
        self.msg_menu_row = None;
        self.msg_menu_leaf = None;
        self.msg_select.cancel();
        self.forward_pick = None;
        self.forward_leaf = None;
        self.forward_query.clear();
        self.forward_cursor = None;
    }

    /// Keep the chat transcript on its newest row (#2685).
    ///
    /// The snap is judged from the reader's own place: a transcript pinned at
    /// the bottom keeps following a turn as it streams, and one the reader
    /// scrolled up stays where they put it. Content that grows moves the max
    /// offset and leaves the offset alone, so growth can never be mistaken for
    /// the reader leaving.
    pub(crate) fn follow_chat(&mut self) {
        let offset = scrolled_from_top(f32::from(self.chat_state.scroll.offset().y));
        let max = f32::from(self.chat_state.scroll.max_offset().height);
        if self.chat_state.follow.update(offset, max) {
            self.chat_state.scroll.scroll_to_bottom();
        }
    }

    /// Give the chat transcript back its follow and land on the newest row.
    ///
    /// Sending, opening a conversation and coming back from a panel all say
    /// the same thing: the reader wants the end of it (#2698).
    pub(crate) fn rearm_transcript_follow(&mut self) {
        self.chat_state.follow.rearm();
        self.chat_state.scroll.scroll_to_bottom();
    }

    /// Stop following because the reader moved by hand: a wheel tick up or a
    /// PageUp (#2698). The TUI drops `auto_scroll` on the same two gestures.
    pub(crate) fn release_transcript_follow(&mut self) {
        self.chat_state.follow.release();
    }

    /// One Ctrl+C press (#2308), the terminal's three steps ported whole.
    ///
    /// Scrolled up, the press only gives the bottom back. Otherwise it throws
    /// the draft away and arms a three second window, and a second press
    /// inside that window stops the turn in flight and closes the window. The
    /// keystroke interceptor and `on_key` both land here, so the key means one
    /// thing wherever it is caught.
    pub(crate) fn ctrl_c_press(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        // One press means the field and the transcript the caret is on
        // (#2885). The window's editor and follow belong to the chat leaf, so
        // reading them from inside a split judged another pane's field and
        // then threw that pane's draft away, and scrolled a transcript the
        // reader was not looking at.
        let composer = self.composer_entity(cx);
        let leaf = self.split_focus();
        let following = match leaf {
            Some(id) if Some(id) != self.chat_session => self
                .pane_states
                .get(&id)
                .map(|pane| pane.read(cx).follow.is_following())
                .unwrap_or(false),
            _ => self.chat_state.follow.is_following(),
        };
        let input_empty = composer.read(cx).is_empty();
        let action = ctrl_c_action(following, input_empty, self.ctrl_c_pending, now);
        match action {
            CtrlCAction::SnapBottom => {
                self.ctrl_c_pending = None;
                // The pane the keyboard is on snaps its own transcript; the
                // chat leaf's follow is the window's.
                match leaf {
                    Some(id) if Some(id) != self.chat_session => {
                        self.rearm_pane_follow(id, cx);
                    }
                    _ => self.rearm_transcript_follow(),
                }
                composer.update(cx, |editor, cx| editor.discard_draft(cx));
                logging::info("ctrl+c snapped the transcript to the bottom");
            }
            CtrlCAction::ClearAndArm => {
                self.ctrl_c_pending = Some(now);
                composer.update(cx, |editor, cx| editor.discard_draft(cx));
                self.turn
                    .live
                    .notices
                    .push("Press Ctrl+C again to quit".to_string());
            }
            CtrlCAction::Quit => {
                self.ctrl_c_pending = None;
                // The stop reaches the pane the keyboard is on first, then the
                // window's own turn, the same order Esc uses. That turn is the
                // chat leaf's, so a caret on a pane leaves it alone (#2885).
                self.cancel_focused_pane_turn("Ctrl+C", cx);
                if self.split_focus().is_none() {
                    if let Some(cancel) = self.turn.cancel.take() {
                        cancel.cancel();
                    }
                    self.turn.busy = false;
                    self.turn.live.thinking = false;
                    self.turn.cancel = None;
                }
                logging::info("ctrl+c quit the window");
                cx.quit();
                return;
            }
        }
        // The draft is gone, so the hint that describes an empty field is the
        // one that belongs under it now (#2308).
        let (hint, panes) = (self.composer_hint(), self.pane_editors(cx));
        self.composer.sync_composer_hint(hint, &panes, cx);
        cx.notify();
    }

    /// The same pair for one pane's transcript (#2685, #2698). The pane is its
    /// own entity, so the follow moves inside it (#2805).
    pub(crate) fn release_pane_follow(&mut self, session: Uuid, cx: &mut Context<Self>) {
        if let Some(pane) = self.pane_states.get(&session) {
            pane.update(cx, |pane, _| pane.follow.release());
        }
    }

    pub(crate) fn rearm_pane_follow(&mut self, session: Uuid, cx: &mut Context<Self>) {
        if let Some(pane) = self.pane_states.get(&session) {
            pane.update(cx, |pane, _| {
                pane.follow.rearm();
                pane.scroll.scroll_to_bottom();
            });
        }
    }

    /// The same for one pane's transcript (#2685).
    pub(crate) fn follow_pane(&mut self, session: Uuid, cx: &mut Context<Self>) {
        if let Some(pane) = self.pane_states.get(&session) {
            pane.update(cx, |pane, _| pane.snap_if_following());
        }
    }

    /// Switch one leaf to another conversation (#2686).
    ///
    /// A topic chip drawn inside a pane has to move that pane. The window door
    /// (`open_session`) moves the chat leaf instead, so a click in a second
    /// pane used to move a conversation the user was not reading and leave the
    /// pane they were looking at untouched. The chat leaf and a session that
    /// already sits on screen keep going through the window door, because that
    /// path already closes a duplicate pane and moves the chat's own leaf.
    pub(crate) fn open_topic_in_pane(&mut self, pane: Uuid, id: Uuid, cx: &mut Context<Self>) {
        if Some(pane) == self.chat_session || Some(id) == self.chat_session {
            self.open_session(id, cx);
            return;
        }
        if self.panes.as_ref().is_some_and(|tree| tree.contains(id)) {
            // Already on screen: hand it the keyboard instead of showing one
            // conversation in two leaves.
            self.pane_focus = Some(id);
            self.rail_focused = false;
            self.sync_composer_session();
            self.request_composer_caret(cx);
            cx.notify();
            return;
        }
        let renamed = self
            .panes
            .as_mut()
            .is_some_and(|tree| tree.rename_leaf(pane, id));
        if !renamed {
            return;
        }
        // The pane's transcript, live turn, plan strip and editor all belong
        // to the conversation that just left the leaf; its draft is filed
        // under that conversation first.
        // Parked, not dropped (#2854): the view keeps its live turn and its
        // cancel token, and comes back if this conversation returns to a leaf.
        self.park_pane(pane, cx);
        self.open_pane_state(id, cx);
        self.pane_focus = Some(id);
        self.rail_focused = false;
        self.sync_composer_session();
        self.request_composer_caret(cx);
        self.persist_layout();
        cx.notify();
    }

    /// A pick that lands in the pane holding the caret (#2819).
    ///
    /// Every commit door used to call `open_session`, which is the window door
    /// and knows only the chat leaf. With a second pane holding the caret,
    /// picking a conversation renamed the chat leaf and left the pane the
    /// reader was looking at untouched, so the wrong pane changed. This asks
    /// the caret first, the way `open_topic_in_pane` asks it for a topic chip.
    ///
    /// A conversation already open in a leaf is never renamed into another
    /// one: that drags the chat leaf onto a conversation already on screen or
    /// destroys a pane the user built. The window says so and leaves the tree
    /// alone.
    ///
    /// Returns whether the pick landed, meaning the conversation is the one the
    /// pane holding the caret now shows. The topic prompt resolves its parent
    /// from that pane, so a caller that opens a conversation and then acts on
    /// it must not carry on when the window warned instead of moving a leaf.
    pub(crate) fn open_picked_in_focused_pane(
        &mut self,
        id: Uuid,
        hand_back: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let chat = self.chat_session;
        // `pane_focus` is a cache, so it only counts while the tree the render
        // draws still holds the leaf it names (#2620).
        let caret = self
            .pane_focus
            .filter(|focus| Some(*focus) != chat)
            .filter(|focus| {
                self.panes
                    .as_ref()
                    .is_some_and(|tree| tree.contains(*focus))
            });
        if caret == Some(id) {
            // Already the conversation in the pane holding the caret, so the
            // pick asks for nothing but the keyboard back.
            if hand_back {
                self.rail_focused = false;
                self.sync_composer_session();
                self.request_composer_caret(cx);
                self.refocus_screen();
                cx.notify();
            }
            return true;
        }
        let duplicate = (Some(id) != chat
            && self.panes.as_ref().is_some_and(|tree| tree.contains(id)))
            || (caret.is_some() && Some(id) == chat);
        if duplicate {
            // A walk over the list must not stop on a card: the preview just
            // leaves the pane alone and the next arrow carries on.
            if hand_back {
                self.warn_pane_duplicate(id, cx);
            }
            return false;
        }
        let Some(caret) = caret else {
            // The chat leaf holds the caret, which is the leaf this door has
            // always moved.
            self.open_session_handing_back(id, hand_back, cx);
            return true;
        };
        let renamed = self
            .panes
            .as_mut()
            .is_some_and(|tree| tree.rename_leaf(caret, id));
        if !renamed {
            return false;
        }
        // The leaf's transcript, live turn, plan strip and editor all belong to
        // the conversation that just left it, so its draft is filed first.
        // Parked, not dropped (#2854): the same round trip the topic chip
        // door makes, so a pick never destroys a turn that is still running.
        self.park_pane(caret, cx);
        self.open_pane_state(id, cx);
        self.mark_rail_seen(id);
        self.pane_focus = Some(id);
        self.sync_composer_session();
        self.persist_layout();
        if hand_back {
            self.rail_focused = false;
            self.request_composer_caret(cx);
            self.refocus_screen();
            cx.notify();
        }
        true
    }

    /// Opening a conversation is reading its plan: the rail's ring stops here,
    /// not at paint, so a repaint can never be mistaken for a read and a ring
    /// never clears itself just because the row scrolled past. The window door
    /// and a pane pick both land here.
    pub(crate) fn mark_rail_seen(&mut self, id: Uuid) {
        if let Some(evidence) = self
            .rail_index
            .evidence(id, false, std::time::Instant::now())
        {
            self.rail_seen.mark(id, evidence.stamp);
        }
    }

    /// Say a picked conversation already sits in a pane (#2819).
    ///
    /// The plain open path used to pull it into the chat leaf and close the
    /// pane it came from, so a pick could take a split apart for nothing. The
    /// card names the conversation and the tree is left exactly as it was.
    pub(crate) fn warn_pane_duplicate(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let title = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| "That session".to_string());
        self.pane_warning = Some(title);
        self.rail_focused = false;
        self.refocus_screen();
        cx.notify();
    }

    /// Open one conversation: select it in the rail, switch the right pane to
    /// chat, and load its newest window off the UI thread. Every door (rail
    /// click, launch, ctrl+u return, create topic) goes through here, and the
    /// per-conversation reset lives one layer down in `begin_conversation`,
    /// so the screen and the session can never disagree.
    pub(crate) fn open_session(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.open_session_handing_back(id, true, cx);
    }

    /// The same open, with the keyboard decision made explicit (#2665).
    ///
    /// `hand_back` is true for every commit (rail click, Enter, the palette,
    /// the find field, a topic tab, a created topic): the row is chosen, so
    /// the next keystroke is a message and the composer takes the caret. It is
    /// false while the arrows walk the rail, because that walk opens the row
    /// it stands on as a preview, and moving focus to the composer would stop
    /// the next arrow from arriving.
    pub(crate) fn open_session_handing_back(
        &mut self,
        id: Uuid,
        hand_back: bool,
        cx: &mut Context<Self>,
    ) {
        self.begin_conversation(id, cx);
        self.mark_rail_seen(id);
        self.screen = Screen::Chat;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, id).await;
            this.update(cx, |this, cx| {
                // Apply only if the user still sits on this session; a slow
                // fetch for an abandoned click paints nothing.
                if this.chat_session == Some(id) {
                    match result {
                        Ok(loaded) => {
                            let (hidden, turns) = transcript::turns_with_origins(
                                &loaded.rows,
                                loaded.total,
                                &this.metrics.by_anchor,
                                &loaded.origins,
                            );
                            this.chat_state.transcript.hidden = hidden;
                            this.chat_state.transcript.turns = turns;
                            this.chat_state.transcript.rows = loaded.rows;
                            this.chat_state.transcript.reactions = loaded.reactions;
                        }
                        Err(e) => this.chat_state.transcript.error = Some(e.into()),
                    }
                    // Opening a conversation is a fresh start: follow its end
                    // even if the reader was parked in the last one's history
                    // (#2698).
                    this.rearm_transcript_follow();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        // A committed row takes the keyboard off the rail: the rail holds it
        // only while the list is being walked (#2665). The chat leaf is the
        // pane that adopted the conversation, so the caret goes there; a stored
        // split pane would otherwise keep it in a conversation the click did
        // not open (#2741).
        if hand_back {
            self.rail_focused = false;
            self.pane_focus = None;
            self.request_composer_caret(cx);
        }
        self.refocus_screen();
        cx.notify();
    }

    /// Fold or unfold one turn. The state transition lives on TranscriptState
    /// so the main chat and every split pane share the same behavior (#2855).
    pub(crate) fn toggle_turn(&mut self, anchor: &str, cx: &mut Context<Self>) {
        self.chat_state.transcript.toggle_turn(anchor);
        cx.notify();
    }

    /// ctrl+o: toggle the newest turn, the same chord the TUI answers.
    /// The TUI's three presses: folded, then expanded, then details in full,
    /// then folded again (#2440). The first press opens the turn; the second
    /// shows its reasoning whole.
    pub(crate) fn toggle_newest_turn(&mut self, cx: &mut Context<Self>) {
        // The chord's first job is the history the window is not drawing
        // (#2805): the transcript draws a screen of turns and leaves the rest
        // behind a marker, and ctrl+o is what brings the next window back, the
        // same way the terminal's history marker does. Once nothing is left,
        // the chord keeps its old meaning.
        if self.chat_state.transcript.load_older() {
            cx.notify();
            return;
        }
        // A turn in flight owns the chord: the card on screen is the live one,
        // so ctrl+o opens or folds IT, the way it opens a settled turn (#2442).
        if !self.turn.live.is_empty() {
            self.chat_state.transcript.live_expanded = !self.chat_state.transcript.live_expanded;
            cx.notify();
            return;
        }
        let Some(anchor) = self
            .chat_state
            .transcript
            .newest_anchor()
            .map(str::to_string)
        else {
            return;
        };
        if !self.chat_state.transcript.expanded.contains(&anchor) {
            self.chat_state.transcript.expanded.insert(anchor);
        } else if self.chat_state.transcript.details.remove(&anchor) {
            self.chat_state.transcript.expanded.remove(&anchor);
        } else {
            self.chat_state.transcript.details.insert(anchor);
        }
        cx.notify();
    }

    /// Every overlay open right now, bottom to top. The keyboard owner is
    /// derived from this one list, so an opener cannot forget to move focus
    /// and a closer cannot forget to hand it back (#2454, #2455).
    pub(crate) fn open_overlays(&self) -> Vec<OverlayId> {
        // The settings modal owns the keyboard on every screen, the same law
        // as the theme modal: it is the topmost surface while it is open, so
        // nothing behind it may answer a key (#2878).
        if self.settings_open {
            return vec![OverlayId::Settings];
        }
        // The theme modal owns the keyboard on every screen, including the
        // non-chat ones: it is the topmost surface while it is open, so
        // nothing behind it may answer a key (#2619).
        if self.themes_open {
            return vec![OverlayId::Themes];
        }
        // A full screen is its own surface: the rail and the chat overlays are
        // not drawn over it, so nothing below can own a key there.
        if self.screen != Screen::Chat {
            return Vec::new();
        }
        let mut open = Vec::new();
        if self.rail_open && self.rail_focused {
            open.push(OverlayId::Rail);
        }
        if self.split_pick.is_some() {
            open.push(OverlayId::SplitPick);
        }
        if self.topic_info.is_some() {
            open.push(OverlayId::TopicInfo);
        }
        if self.topic_draft.is_some() {
            open.push(OverlayId::TopicPrompt);
        }
        if self.help_open {
            open.push(OverlayId::Help);
        }
        if self.preview.is_some() {
            open.push(OverlayId::Preview);
        }
        if self.sessions.rail_menu.is_some() {
            open.push(OverlayId::RailMenu);
        }
        if self.confirm_delete.is_some() {
            open.push(OverlayId::ConfirmDelete);
        }
        if self.group_target.is_some() {
            open.push(OverlayId::GroupPick);
        }
        if self.topic_warning.is_some() || self.pane_warning.is_some() {
            open.push(OverlayId::Warning);
        }
        if self.sessions.filter_active {
            open.push(OverlayId::RailFind);
        }
        if self.finder.is_some() {
            open.push(OverlayId::CommandPalette);
        }
        open
    }

    /// Ask the next frame to put focus where the current screen expects it.
    ///
    /// A transition calls this instead of touching focus itself: only the
    /// render holds a window to focus through, and this is the one place that
    /// decides which element owns the keyboard (#2350, #2356, #2454).
    pub(crate) fn refocus_screen(&mut self) {
        self.refocus = Some(focus_target(keyboard_owner(
            &self.open_overlays(),
            self.screen,
        )));
    }

    /// An overlay just opened or closed. Same derived decision, named so a
    /// call site reads as taking the keyboard rather than re-labelling the
    /// screen.
    pub(crate) fn refocus_overlay(&mut self) {
        self.refocus_screen();
    }

    /// A pointer gesture on the sidebar hands the keys back to the composer.
    ///
    /// The rail used to take the keyboard from the press's own x position
    /// (`x <= rail_width`), so every press in the column armed it and only
    /// Esc, Tab or a committed row gave the keys back. Every affordance the
    /// pointer touched without committing a pick cost an Esc before the
    /// composer would take a character: the empty space under the rows, the
    /// scrollbar, the brand header, a filter chip. A press is not
    /// a walk (#2798): the rail keeps the keys only while a key put it there,
    /// and a split still waiting for its pane keeps them because its dialog
    /// answers N and Esc.
    pub(crate) fn rail_hands_back(&mut self) {
        if !self.rail_focused || self.split_pick.is_some() {
            return;
        }
        self.rail_focused = false;
        self.refocus_screen();
    }

    /// Ask the composer that is drawn to take the caret, and let the field
    /// re-assert it from its own render (#2741).
    ///
    /// The press that led here is dispatched before the next frame is drawn,
    /// and it can still hand the window root the keys: any element without
    /// `track_focus` lets it, and the owner may not even change, so the
    /// reconciler in `render` never fires. The deferred request is the one
    /// that sticks, because the editor claims the caret while the tree is
    /// being built, after that press is done. `refocus_screen` stays the one
    /// decision point, so the two can never disagree about who owns the keys.
    pub(crate) fn request_composer_caret(&mut self, cx: &mut Context<Self>) {
        let composer = self.composer_entity(cx);
        composer.update(cx, |editor, cx| {
            editor.focus_pending = true;
            cx.notify();
        });
        self.refocus_screen();
    }
}
