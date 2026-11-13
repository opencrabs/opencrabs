//! Splits: the leaf lifecycle, the divider drag, the footer each pane
//! wears and the tree the window draws. The drawing itself lives in
//! `pane_layout.rs` and `pane_view.rs`; this is the state they walk.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// The sessions rail: `/sessions` and ctrl+s share one toggle, so the
    /// command and the chord can never disagree about what showing it means.
    /// A hidden rail has no field to keep listening, so the filter exits.
    /// Start a split: open the sidebar, focus it, and ask for the session
    /// that shares the space with the pane holding the caret. The pane being
    /// split is the one the caret is on, so a nested split grows from the
    /// pane being read rather than from the window's first leaf (#2472).
    pub(crate) fn begin_split(&mut self, layout: Layout, cx: &mut Context<Self>) {
        // Every door splits the pane holding the caret (#2625), and the caret
        // is resolved the way the render resolves it rather than read
        // literally: a cached `pane_focus` naming a leaf the tree no longer
        // holds made all three doors refuse with "Open a session before
        // splitting it." while the window was plainly drawing a live focused
        // pane (#2757).
        let Some(target) = self.split_origin() else {
            self.turn
                .live
                .notices
                .push("Open a session before splitting it.".to_string());
            cx.notify();
            return;
        };
        if !self.rail_open {
            self.toggle_rail(cx);
        }
        self.split_pick = Some(layout);
        self.split_target = Some(target);
        // A fresh split starts with the pane it grows from in front, whatever
        // the last one left behind (#2474).
        self.pane_focus = if Some(target) == self.chat_session {
            None
        } else {
            Some(target)
        };
        self.rail_focused = true;
        // The picker's cursor starts on the pane being split, not on the
        // chat's row: with the caret in a right-hand pane, the chat's row is
        // a different session and the list opened pointing somewhere else
        // (#2625).
        self.sessions.cursor = Some(target);
        // Focus goes to the window, not the composer, so the sidebar keys and the
        // dialog's N and Esc reach it while the split waits.
        self.refocus = Some(FocusTarget::Root);
        cx.notify();
    }

    /// Split the pane holding the caret and put `id` in the new slot (#2625).
    ///
    /// The rail's row menu used to open the row and then start a pick, which
    /// had two faults: `open_session` moves the chat leaf to the clicked row,
    /// so the split grew from the window's first pane instead of the pane
    /// being read, and for a session the tree did not hold yet the leaf reset
    /// in `adopt_chat_leaf` threw the split away entirely. Choosing the menu
    /// entry already named the session, so the pick is answered here and the
    /// second step is skipped.
    pub(crate) fn split_row(&mut self, id: Uuid, layout: Layout, cx: &mut Context<Self>) {
        let chat = self.chat_session;
        // The pane the split grows from, seeded from the chat's own leaf when
        // the window holds no tree yet: the same rule the chord door walks, so
        // the two doors cannot disagree about the originating pane (#2757).
        let Some(target) = self.split_origin() else {
            self.turn
                .live
                .notices
                .push("Open a session before splitting it.".to_string());
            cx.notify();
            return;
        };
        if Some(id) == chat || self.panes.as_ref().is_some_and(|tree| tree.contains(id)) {
            self.turn
                .live
                .notices
                .push("That session is already open in a pane.".to_string());
            cx.notify();
            return;
        }
        self.split_pick = Some(layout);
        self.split_target = Some(target);
        self.choose_split(id, cx);
    }

    /// Esc during the pick: the split is dropped and the chat keeps focus.
    pub(crate) fn cancel_split_pick(&mut self, cx: &mut Context<Self>) {
        self.split_pick = None;
        self.split_target = None;
        self.rail_focused = false;
        self.refocus_screen();
        cx.notify();
    }

    /// The session holding the caret: the chat's own session when no pane
    /// took it (#2474).
    pub(crate) fn focused_pane(&self) -> Option<Uuid> {
        self.pane_focus.or(self.chat_session)
    }

    /// The leaf a split door grows from (#2757).
    ///
    /// `focused_pane` reads `pane_focus` literally, and that id is a cache: a
    /// closed pane, a layout pruned at boot, or a session moved into the chat
    /// leaf all leave it naming a leaf the tree no longer holds. Filtered
    /// literally, that stale id made every door push "Open a session before
    /// splitting it." and return before the picker was ever set, while the
    /// window went on drawing a live focused pane, because the render already
    /// resolves the same id through `resolve_split_focus` (#2620). The door
    /// now asks the same question the render asks: the caret leaf, then the
    /// chat's own leaf, then the first surviving one. A window with no tree
    /// starts from the chat's leaf, the way the row menu always has.
    pub(crate) fn split_origin(&mut self) -> Option<Uuid> {
        let chat = self.chat_session;
        if self.panes.is_none()
            && let Some(chat) = chat
        {
            self.panes = Some(PaneTree::leaf(chat));
        }
        let leaves = self.panes.as_ref()?.leaves();
        // `resolve_split_focus` answers `None` for the chat leaf, which is the
        // caret unless another pane took it.
        resolve_split_focus(&leaves, self.pane_focus, chat)
            .or(chat)
            .filter(|id| leaves.contains(id))
            .or_else(|| leaves.last().copied())
    }

    /// Point every composer at the conversation it draws (#2629, #2637, #2909).
    ///
    /// A recall reads the queue of the session its own field belongs to, so the
    /// binding follows the leaf and nothing else. It used to be one
    /// process-wide "composer session" that this method rewrote from the
    /// focused leaf, which is what let a recall in one pane take another
    /// conversation's queued messages: the caret, a closed pane, or a session
    /// switch could leave that global naming a different chat from the field
    /// the user was typing in.
    ///
    /// The chat composer follows the open conversation; each split pane's
    /// composer keeps the session it was built for, and a pane whose leaf moved
    /// gets its new id here because the same entity outlives the move.
    pub(crate) fn sync_composer_session(&mut self, cx: &mut Context<Self>) {
        let chat = self.chat_session;
        self.composer
            .editor
            .update(cx, |editor, _| editor.set_session(chat));
        for (session, pane) in self.pane_states.iter() {
            let session = *session;
            pane.update(cx, |pane, cx| {
                pane.editor
                    .update(cx, |editor, _| editor.set_session(Some(session)));
            });
        }
    }

    /// Write the split the window is showing, so the next launch puts the same
    /// panes back in the same slots at the same sizes (#2675). Called wherever
    /// the layout can change under the user's hand: a split, a close, a
    /// divider drag, a caret move, a conversation switch. The chat's own leaf
    /// goes with it, so a launch knows which pane was the window's transcript
    /// (#2689).
    pub(crate) fn persist_layout(&self) {
        layout_store::save(
            &layout_store::path(),
            self.panes.as_ref(),
            self.pane_focus,
            self.chat_session,
        );
    }

    /// Put back the split the last run left (#2675). Runs once, after the boot
    /// open, because the chat's own leaf has to exist before the tree can
    /// agree with it. A saved layout that does not hold the conversation on
    /// screen, a pane whose session the ledger no longer has, and a file that
    /// cannot be read all fall back to the plain chat: a wrong window is worse
    /// than a plain one.
    pub(crate) fn restore_layout(
        &mut self,
        saved: Option<layout_store::SavedLayout>,
        cx: &mut Context<Self>,
    ) {
        let Some(saved) = saved else {
            return;
        };
        let Some(chat) = self.chat_session else {
            return;
        };
        let Some(tree) = saved.panes else {
            return;
        };
        if !tree.contains(chat) {
            logging::info("gui layout: the chat is not one of its panes, opening plain");
            return;
        }
        let known: std::collections::HashSet<Uuid> =
            self.sessions.items.iter().map(|item| item.id).collect();
        let Some(tree) = layout_store::prune(tree, &known) else {
            logging::info("gui layout: fewer than two panes survive, opening plain");
            return;
        };
        let leaves = tree.leaves();
        self.panes = Some(tree);
        // Every pane but the chat needs its own state: its composer, its
        // transcript, its plan strip. The chat's is the window's own.
        for leaf in leaves.iter().copied().filter(|id| *id != chat) {
            self.open_pane_state(leaf, cx);
        }
        // The caret goes back where it was, unless that pane is gone or it is
        // the chat itself, which the window spells as `None`.
        self.pane_focus = saved.focus.filter(|id| *id != chat && leaves.contains(id));
        self.sync_composer_session(cx);
        // The pruned tree is what this run holds now, so the file matches it.
        self.persist_layout();
        logging::info(&format!(
            "gui layout: restored {} panes, caret {:?}",
            leaves.len(),
            self.pane_focus
        ));
        cx.notify();
    }

    /// The composer the keyboard owns (#2610): the focused pane's own field,
    /// or the window's when the chat leaf holds the caret.
    ///
    /// A split draws one real field per leaf, so anything that aims the
    /// keyboard or hands the composer a file resolves the leaf first. Aiming
    /// at the window's editor while a pane owns the caret puts the text and
    /// the attachment chips into a field the split never draws.
    pub(crate) fn composer_entity(&self, cx: &App) -> Entity<editor::Editor> {
        match self.split_focus() {
            Some(id) if Some(id) == self.chat_session => self.composer.editor.clone(),
            Some(id) => self
                .pane_states
                .get(&id)
                .map(|pane| pane.read(cx).editor.clone())
                .unwrap_or_else(|| self.composer.editor.clone()),
            None => self.composer.editor.clone(),
        }
    }

    /// Name the element the window gives the keys to, for the caret receipt
    /// (#2741). Three answers: the composer that is drawn, the window root
    /// that owns overlays and non-chat screens, or a field the dashboard does
    /// not own (the rail's find box, a rename prompt).
    pub(crate) fn caret_owner_name(&self, window: &Window, cx: &App) -> &'static str {
        let Some(handle) = window.focused(cx) else {
            return "none";
        };
        if handle == self.composer_entity(cx).read(cx).focus_handle {
            "composer"
        } else if handle == self.focus {
            "root"
        } else {
            "field"
        }
    }

    /// The leaf that owns the keyboard in the split, `None` meaning the chat
    /// leaf. Resolved through [`resolve_split_focus`] so the rules can be
    /// tested without a window.
    pub(crate) fn split_focus(&self) -> Option<Uuid> {
        let leaves = self
            .panes
            .as_ref()
            .map(|tree| tree.leaves())
            .unwrap_or_default();
        resolve_split_focus(&leaves, self.pane_focus, self.chat_session)
    }

    /// Everything one split pane's draw reads from the window, folded into one
    /// number (#2805).
    ///
    /// A pane is cached, so a pane the window did not notify replays its last
    /// frame instead of drawing again. Any value its draw takes from outside
    /// itself has to be in here, or a change it cannot see would sit on screen
    /// until something else moved it: the keyboard owner, the status row, the
    /// drop band, the plan and queue toggles, the queue and the offers waiting
    /// for it, the running task line, and the session and topic data the strip
    /// is built from. The plan file is not here; the pane re-reads it on its
    /// mtime in `sync_frame`, which is the same pass that settles this.
    fn pane_stamp(&self, session: Uuid, width: f32, count: usize) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        width.to_bits().hash(&mut hasher);
        count.hash(&mut hasher);
        (self.split_focus() == Some(session)).hash(&mut hasher);
        // The frame's own answers (#2899): both are resolved once per frame for
        // every leaf, so the stamp only reads what the walk already built.
        self.frame_footers.get(&session).hash(&mut hasher);
        self.pane_drop
            .filter(|(target, _)| *target == session)
            .map(|(_, half)| (half.layout.is_stacked(), half.after))
            .hash(&mut hasher);
        self.plan_expanded.contains(&session).hash(&mut hasher);
        self.composer.queue_expanded.hash(&mut hasher);
        self.topic_draft.is_none().hash(&mut hasher);
        self.frame_tasks.get(&session).hash(&mut hasher);
        // The forward picker replaces this leaf's composer (#2827). Its query
        // and cursor are window state, so both belong in the cache key while
        // this leaf owns the picker.
        let forwarding = self.forward_pick.is_some() && self.forward_leaf == Some(session);
        forwarding.hash(&mut hasher);
        if forwarding {
            self.forward_query.hash(&mut hasher);
            self.forward_cursor.hash(&mut hasher);
            self.forward_pick.as_ref().map(Vec::len).hash(&mut hasher);
        }
        // A queued message and an offer both draw in the pane's column, and
        // both arrive from the bridge rather than through the window's own
        // state, so the text is what the pane can be told apart by.
        for queued in agent_bridge::queue_snapshot(session) {
            queued.display_text.hash(&mut hasher);
        }
        for offer in agent_bridge::options_snapshot(session) {
            offer.label.hash(&mut hasher);
        }
        // The topic strip is built from the session store, so a topic opening,
        // closing, being renamed or picking up an icon has to reach the pane
        // that draws the family. The counts catch the structural changes; the
        // two reload passes also notify every pane directly.
        self.sessions.items.len().hash(&mut hasher);
        self.sessions.topics.len().hash(&mut hasher);
        self.sessions.parents.len().hash(&mut hasher);
        self.sessions.topic_icons.len().hash(&mut hasher);
        self.sessions.topic_titles.len().hash(&mut hasher);
        self.topic_edge.as_str().hash(&mut hasher);
        hasher.finish()
    }

    /// Redraw every split pane (#2805).
    ///
    /// A pane is cached, so the two passes that reload the session store, the
    /// rail's refetch and the topic refresh, have to tell the panes that draw
    /// from it. A rename or an icon change that leaves every count the same
    /// would otherwise sit behind a stale strip until something else moved it.
    pub(crate) fn notify_panes(&mut self, cx: &mut Context<Self>) {
        for pane in self.pane_states.values() {
            pane.update(cx, |_, cx| cx.notify());
        }
    }

    /// Expand or collapse one pane's plan strip (#2624). The choice is per
    /// leaf: a pane in a wall of eight opens on its bar, and the one the user
    /// is reading trades room for the whole checklist without touching the
    /// others. The chat leaf keys on its own session, the same id every other
    /// pane uses, so the two cannot collide.
    pub(crate) fn toggle_pane_plan(&mut self, session: Uuid, cx: &mut Context<Self>) {
        if !self.plan_expanded.remove(&session) {
            self.plan_expanded.insert(session);
        }
        // An expanded strip takes room from the transcript under it, and a
        // viewport that shrinks must not leave the newest row clipped while
        // the reader believes they are at the bottom (#2685, #2624).
        if Some(session) == self.chat_session {
            self.chat_state.follow.rearm();
        } else if let Some(pane) = self.pane_states.get(&session) {
            pane.update(cx, |pane, _| pane.follow.rearm());
        }
        cx.notify();
    }

    /// Close one pane and hand its space to its neighbour (#2438, #2472).
    /// Closing the chat itself moves the window to the first survivor, whose
    /// leaf takes the chat's place, so the panes that remain keep the slots
    /// they were dragged to and the window never shows a session the user
    /// did not pick.
    pub(crate) fn close_pane(&mut self, session: Uuid, cx: &mut Context<Self>) {
        let Some(tree) = self.panes.as_mut() else {
            return;
        };
        if tree.leaves().len() <= 1 {
            return;
        }
        let Some(absorbed) = tree.close(session) else {
            return;
        };
        // The text typed in this pane is filed under its conversation before
        // the editor goes, so reopening it returns the draft.
        self.stash_pane_draft(session, cx);
        self.pane_states.remove(&session);
        // The pane is gone for good, so a parked view of the same conversation
        // has nothing left to return to (#2854).
        self.parked_panes.remove(&session);
        self.split_drag = None;
        self.pane_focus = None;
        self.sync_composer_session(cx);
        if self.chat_session == Some(session) {
            self.open_session(absorbed, cx);
        } else {
            self.refocus_screen();
            cx.notify();
        }
        // The pane is gone: the file must say so before the next launch (#2675).
        self.persist_layout();
    }

    /// `ctrl+x` and the × on the focused pane's header: close the pane holding
    /// the caret (#2474).
    pub(crate) fn close_focused_pane(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.focused_pane() {
            self.close_pane(session, cx);
        }
    }

    /// Alt+Tab walks the caret to the next pane and Alt+Shift+Tab to the
    /// previous one (#2580, #2579). Tab alone keeps every job it already had:
    /// accepting the emoji picker and the follow-up suggestion, and walking to
    /// the rail. Binding rotation to the bare key made those two fight for one
    /// keystroke, which is why the terminal moved to Alt and the window
    /// followed here.
    ///
    /// The leaves are visited in the order they are drawn, so the accent
    /// follows the layout the eye reads (#2475, #2472). With one pane there is
    /// nothing to switch to, so the key keeps whatever it did. The pane
    /// holding the caret wears the accent in its header and takes the page
    /// keys.
    pub(crate) fn cycle_pane_focus(&mut self, clockwise: bool, cx: &mut Context<Self>) {
        let Some(tree) = self.panes.as_ref() else {
            return;
        };
        let leaves = tree.leaves();
        let at = self
            .focused_pane()
            .and_then(|id| leaves.iter().position(|leaf| *leaf == id))
            .unwrap_or(0);
        let rotation = if clockwise {
            split::PaneRotation::Clockwise
        } else {
            split::PaneRotation::Anticlockwise
        };
        // The wraparound lives in the core, where it is tested without a
        // window (#2580).
        let Some(index) = split::rotated_index(at, leaves.len(), rotation) else {
            return;
        };
        let next = leaves[index];
        // The chat's own leaf is the pane the caret reaches as `None`, which
        // is how every other surface already names it.
        self.pane_focus = if Some(next) == self.chat_session {
            None
        } else {
            Some(next)
        };
        // A receipt in the daily log, so "Tab did nothing" can be told apart
        // from "Tab moved the caret and the header did not show it".
        logging::debug(&format!("pane focus -> {:?}", self.pane_focus));
        self.sync_composer_session(cx);
        // The keyboard walks with the caret (#2610). Without this the split
        // highlighted the new pane while every keystroke still landed in the
        // chat leaf's field, so the chord appeared to rotate exactly once.
        self.refocus_screen();
        // The caret is the layout too: the next launch puts it back (#2675).
        self.persist_layout();
        cx.notify();
    }

    /// Everything a fresh pane needs, keyed by the session it shows: its own
    /// composer wired to the send path, its own drag-to-copy, its plan path,
    /// and its newest messages. Split out of `choose_split` so a restored
    /// layout builds a pane exactly the way a split does (#2675).
    pub(crate) fn open_pane_state(&mut self, id: Uuid, cx: &mut Context<Self>) {
        // A pane parked by a switch comes back whole (#2854). Its live turn,
        // its tool blocks and its cancel token are still in the entity, so
        // rebuilding here would throw away exactly what the switch was
        // supposed to preserve.
        if let Some(view) = self.parked_panes.remove(&id) {
            // Parking files the half-typed text under the conversation, so it
            // goes back into the editor that came with the view.
            if let Some(draft) = self.composer.drafts.take(id) {
                let editor = view.read(cx).editor.clone();
                editor.update(cx, |ed, cx| ed.restore_state(draft, cx));
            }
            self.pane_states.insert(id, view);
            // The rows it drew are stale: the conversation kept working while
            // the pane was away, so they are re-read before the pane is
            // trusted again.
            self.reload_pane(id, cx);
            cx.notify();
            return;
        }
        let title = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| "(untitled)".to_string());
        // The pane's composer is its own entity (#2530), so a draft belongs to
        // the pane it was typed in and the send carries that pane's session.
        // It walks the window's shared history, not a private one, so history
        // is the same whichever pane has the caret.
        let pane_editor = cx.new({
            let history = self.history.clone();
            let history_path = composer::default_history_path();
            move |cx| editor::Editor::new_with_history(history, history_path, cx)
        });
        // The pane's composer writes into this pane's conversation and no
        // other, so its queue recall can never reach a neighbour (#2909).
        pane_editor.update(cx, |editor, _| editor.set_session(Some(id)));
        // A draft filed before this pane was closed, moved, or switched comes
        // back with it.
        if let Some(draft) = self.composer.drafts.take(id) {
            pane_editor.update(cx, |ed, cx| ed.restore_state(draft, cx));
        }
        cx.subscribe(&pane_editor, move |this, _editor, event, cx| match event {
            editor::EditorEvent::Sent(text) => {
                this.send_from_pane(id, text.clone(), None, SendOrigin::Composer, cx);
            }
            editor::EditorEvent::Preview(path) => this.open_preview(path.clone(), cx),
        })
        .detach();
        // The pane's own drag-to-copy (#2623). The transcript cannot reach a
        // composer, so the pane's editor is the one that says "Copied to
        // clipboard" once a dragged range lands (#2587).
        let pane_copy = Rc::new(CopyDrag::default());
        {
            let editor = pane_editor.downgrade();
            pane_copy.set_copied_listener(Rc::new(move |cx: &mut App| {
                let _ = editor.update(cx, |editor, cx| editor.show_copied(cx));
            }));
        }
        // The pane's plan lives wherever its session is bound, which is a DB
        // read; the same lookup the window's strip uses, resolved once here.
        agent_bridge::spawn_plan_path(self.db_path.clone(), id, self.sink.clone());
        // The leaf's state is its own type now (#2805). It opens at the
        // width the window last painted the wall at, and the walk corrects it
        // the first frame the pane has a box of its own.
        let pane_width = self
            .split_areas
            .borrow()
            .get(&Vec::new())
            .copied()
            .map(|[x0, _, x1, _]| x1 - x0)
            .unwrap_or(0.0);
        let window = cx.entity().downgrade();
        // The leaf is an entity of its own, so it can be drawn, notified and
        // read without the window holding every field (#2805).
        // The leaf's own context window, read for ITS session (#2854): a pane
        // can run a different provider from the window, so borrowing the
        // window's number would print another conversation's budget.
        let pane_ctx_max = agent_bridge::bridge()
            .map(|b| b.service.context_limit_for_session(id))
            .unwrap_or(0);
        let pane = cx.new(|_| {
            PaneView::new(
                id,
                title,
                pane_editor,
                pane_copy,
                pane_width,
                pane_ctx_max,
                window,
            )
        });
        self.pane_states.insert(id, pane);
        // Paint the pane now, with its header, rather than waiting for the
        // transcript load to come back: an empty pane that appears at once
        // reads as a split, where a silent wait reads as nothing (#2438).
        cx.notify();
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, id).await;
            this.update(cx, |this, cx| {
                // The rows are measured against the window's metrics, read
                // before the leaf is borrowed: a pane's own context cannot
                // reach the window's map (#2805).
                let loaded = result.map(|loaded| {
                    let (hidden, turns) = transcript::turns_with_origins(
                        &loaded.rows,
                        loaded.total,
                        &this.metrics.by_anchor,
                        &loaded.origins,
                    );
                    (hidden, turns, loaded.rows, loaded.reactions)
                });
                let Some(pane) = this.pane_states.get(&id).cloned() else {
                    return;
                };
                pane.update(cx, |pane, cx| {
                    match loaded {
                        Ok((hidden, turns, rows, reactions)) => {
                            pane.transcript.hidden = hidden;
                            pane.transcript.turns = turns;
                            pane.transcript.rows = rows;
                            pane.transcript.reactions = reactions;
                        }
                        Err(e) => pane.transcript.error = Some(e.into()),
                    }
                    // A new pane opens on its newest row (#2698).
                    pane.follow.rearm();
                    pane.scroll.scroll_to_bottom();
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    /// A pick from any list: during a split pick it fills the waiting pane,
    /// otherwise it opens the conversation (#2760).
    ///
    /// The rail's full rows and its Enter key asked the picker first, but the
    /// topic chips, the filter's Enter and the Seeker's session rows opened
    /// directly. A topic chip also stops propagation, so the parent row never
    /// got the chance: the topic landed in the chat leaf while the picker
    /// stayed armed on the pane it started from, and the next click split
    /// something nobody pointed at.
    pub(crate) fn pick_or_open(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.split_pick.is_some() {
            self.choose_split(id, cx);
        } else {
            // The pane holding the caret takes the pick, not the chat leaf
            // (#2819).
            self.open_picked_in_focused_pane(id, true, cx);
        }
    }

    /// Fill a new pane with `id`, splitting the pane the pick started from,
    /// and load its newest messages (#2438, #2472).
    pub(crate) fn choose_split(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(layout) = self.split_pick else {
            return;
        };
        let Some(target) = self.split_target else {
            self.split_pick = None;
            return;
        };
        if self.chat_session == Some(id)
            || self.panes.as_ref().is_some_and(|tree| tree.contains(id))
        {
            self.turn
                .live
                .notices
                .push("That session is already open in a pane. Pick another.".to_string());
            cx.notify();
            return;
        }
        self.split_pick = None;
        self.split_target = None;
        self.rail_focused = false;
        // The pane just picked is the one the caret and ctrl+x land on, so a
        // split opens ready to act on what was clicked (#2474).
        self.pane_focus = Some(id);
        self.sync_composer_session(cx);
        self.refocus_screen();
        let split = self
            .panes
            .as_mut()
            .is_some_and(|tree| tree.split(target, layout, id));
        if !split {
            self.pane_focus = None;
            self.turn
                .live
                .notices
                .push("That pane is no longer open.".to_string());
            cx.notify();
            return;
        }
        self.open_pane_state(id, cx);
        // The layout is the user's now, so it is written down for the next
        // launch (#2675).
        self.persist_layout();
    }

    /// Park one pane's whole view under the conversation it was showing (#2854).
    ///
    /// A switch used to drop the entity outright, and the live turn, its tool
    /// blocks, the thinking flag and the cancel token went with it. The
    /// conversation kept working while the pane came back empty. The view is
    /// kept under the session it draws, so `open_pane_state` restores it when
    /// that session returns to a leaf: the same round trip the window already
    /// makes with `swap_visible_turn` for its own card (#2616).
    pub(crate) fn park_pane(&mut self, session: Uuid, cx: &mut Context<Self>) {
        // The text typed here is filed under the conversation before anything
        // is moved, so a draft survives even if the parked view is later
        // pruned.
        self.stash_pane_draft(session, cx);
        if let Some(view) = self.pane_states.remove(&session) {
            self.parked_panes.insert(session, view);
        }
        // A parked view is memory, so how many are kept is bounded. Idle ones
        // go first, which means a conversation still working is never evicted
        // to make room for one that already finished.
        const MAX_PARKED_PANES: usize = 12;
        if self.parked_panes.len() > MAX_PARKED_PANES {
            let excess = self.parked_panes.len() - MAX_PARKED_PANES;
            let idle: Vec<Uuid> = self
                .parked_panes
                .iter()
                .filter(|(_, view)| !view.read(cx).busy)
                .map(|(id, _)| *id)
                .take(excess)
                .collect();
            for id in idle {
                self.parked_panes.remove(&id);
            }
        }
    }

    /// The view drawing one conversation, whether it is on screen or parked
    /// (#2854).
    ///
    /// A turn does not stop being that session's turn because its pane was
    /// switched to another conversation. Settling, folding progress into the
    /// card, reloading rows and stopping the turn all resolve through here, so
    /// an off-screen leaf keeps receiving what it owns.
    pub(crate) fn pane_view(&self, session: Uuid) -> Option<Entity<PaneView>> {
        self.pane_states
            .get(&session)
            .or_else(|| self.parked_panes.get(&session))
            .cloned()
    }

    /// File one pane's half-typed text under the conversation it belongs to,
    /// before the pane's editor is dropped (#2494).
    pub(crate) fn stash_pane_draft(&mut self, session: Uuid, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_states.get(&session) else {
            return;
        };
        let editor = pane.read(cx).editor.clone();
        let draft = editor.update(cx, |ed, cx| ed.take_draft(cx));
        self.composer.drafts.store(session, draft);
    }

    /// The same for every pane, for a tree replaced wholesale.
    pub(crate) fn stash_all_pane_drafts(&mut self, cx: &mut Context<Self>) {
        let sessions: Vec<Uuid> = self.pane_states.keys().copied().collect();
        for session in sessions {
            self.stash_pane_draft(session, cx);
        }
    }

    /// Keep the chat on a leaf of the tree (#2472). A first open starts the
    /// tree; a plain switch moves the chat's own leaf to the new session;
    /// opening a session another pane already shows moves it into the chat and
    /// closes that pane, so one session never sits in two leaves.
    pub(crate) fn adopt_chat_leaf(
        &mut self,
        previous: Option<Uuid>,
        id: Uuid,
        cx: &mut Context<Self>,
    ) {
        self.place_chat_leaf(previous, id, cx);
        // The chat's leaf is part of the saved layout, so every path that
        // moves it writes the tree, the one-pane case included (#2675).
        self.persist_layout();
    }

    /// The body of `adopt_chat_leaf`. It writes nothing, so the caller can
    /// save once, whichever path the switch took.
    pub(crate) fn place_chat_leaf(
        &mut self,
        previous: Option<Uuid>,
        id: Uuid,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.panes.as_mut() else {
            self.panes = Some(PaneTree::leaf(id));
            return;
        };
        if tree.contains(id) {
            // The tree work first, so the borrow of `self.panes` ends before
            // the pane's draft is filed.
            if let Some(was) = previous
                && was != id
                && tree.leaves().len() > 1
            {
                tree.close(was);
            }
            self.stash_pane_draft(id, cx);
            self.pane_states.remove(&id);
            // The chat leaf draws this conversation now, so a parked pane for
            // it would be a second copy waiting to come back (#2854).
            self.parked_panes.remove(&id);
            return;
        }
        if let Some(was) = previous
            && tree.rename_leaf(was, id)
        {
            return;
        }
        self.panes = Some(PaneTree::leaf(id));
        self.stash_all_pane_drafts(cx);
        self.pane_states.clear();
        // The tree was replaced wholesale, so no parked view has a leaf left to
        // return to (#2854).
        self.parked_panes.clear();
    }

    /// Grab the divider between two children of one split (#2472, #2476).
    /// A double click puts that pair back to even, the quick way out of a
    /// share dragged somewhere awkward.
    pub(crate) fn begin_divider_drag(
        &mut self,
        path: &[usize],
        divider: usize,
        ev: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        // A double click is a layout change on its own: the pair goes back to
        // even and that size is what the next launch must show (#2675). It is
        // answered before the borrow below so the write can take the window.
        if ev.click_count == 2 {
            let evened = self
                .panes
                .as_mut()
                .is_some_and(|tree| tree.even_divider(path, divider));
            if evened {
                self.persist_layout();
                cx.notify();
            }
            return;
        }
        let stacked = {
            let Some(tree) = self.panes.as_mut() else {
                return;
            };
            let Some(PaneTree::Split {
                layout, children, ..
            }) = tree.at(path)
            else {
                return;
            };
            if divider + 1 >= children.len() {
                return;
            }
            layout.is_stacked()
        };
        let axis = if stacked {
            ev.position.y
        } else {
            ev.position.x
        };
        self.split_drag = Some(DividerDrag {
            path: path.to_vec(),
            divider,
            stacked,
            last: f32::from(axis),
        });
        cx.notify();
    }

    /// N in the pick: a new session that fills the second pane (#2438).
    pub(crate) fn split_new(&mut self, cx: &mut Context<Self>) {
        self.split_creating = true;
        self.new_session(cx);
    }

    /// One pane's footer text (#2593): pane index, whether that session is
    /// mid-turn, who is answering it, and its plan progress. The detached
    /// commands ride the strip above the composer instead, so the one-line
    /// footer never grows past the pane. Resolved here, once per frame,
    /// because it reads the bridge and the plan state, neither of which
    /// belongs in a draw pass per pane.
    pub(crate) fn pane_footer_text(&self, session: Uuid, index: usize, count: usize) -> String {
        let mut parts: Vec<String> = Vec::new();
        if count > 1 {
            parts.push(format!("[{index}/{count}]"));
        }
        if self.turn.busy && self.chat_session == Some(session) {
            // The GUI runs one conversation at a time, so the only pane that
            // can be mid-turn is the open one.
            parts.push("⠙ working".to_string());
        }
        if let Some(bridge) = agent_bridge::bridge() {
            let (provider, model) = bridge.service.provider_pair_for_session(session);
            if !model.is_empty() {
                let label = model.rsplit('/').next().unwrap_or(&model).to_string();
                parts.push(format!("{provider} / {label}"));
            }
        }
        // The detached commands are NOT repeated here: each pane prints its
        // own strip above the composer, right-aligned in the palette's cyan
        // (#2617). Printing the same label a second time in a one-line footer
        // made it run past the pane's edge and over its neighbour.
        if self.plan_for == Some(session)
            && let Some(progress) = self.plan.progress_label()
        {
            parts.push(progress);
        }
        parts.join("  ·  ")
    }

    /// The pane body for the chat screen and the themes preview: the chat
    /// element alone while the tree is one leaf, or the whole tree drawn
    /// around it (#2472). The chat element is moved into its own leaf, so it
    /// is built once and never duplicated.
    pub(crate) fn panes_body(
        &mut self,
        chat: AnyElement,
        content: f32,
        interactive: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Shares are clamped to the pixel floor against the area the last
        // frame painted into, so a window resize never squeezes a pane below
        // a usable width (#2472). The clamp is idempotent, so it settles on
        // the first frame that has a measurement.
        let root_area = self.split_areas.borrow().get(&Vec::new()).copied();
        if let Some([x0, y0, x1, y1]) = root_area
            && let Some(tree) = self.panes.as_mut()
        {
            let span = match tree {
                PaneTree::Split { layout, .. } => {
                    if layout.is_stacked() {
                        y1 - y0
                    } else {
                        x1 - x0
                    }
                }
                PaneTree::Leaf { .. } => 0.0,
            };
            tree.normalize(span);
        }
        let Some(tree) = self.panes.as_ref() else {
            return chat;
        };
        if tree.leaves().len() < 2 {
            return chat;
        }
        let title = match self.session_title() {
            Some(title) => SharedString::from(title),
            None => SharedString::from("Chat"),
        };
        // Every pane carries its own status row (#2593), resolved once per
        // frame for EVERY leaf here (#2899): the walk stamps each pane with
        // this text and the pane draws its own row from it, and the two used to
        // resolve it separately, each taking the bridge lock and allocating the
        // string. The detached-command line is resolved here too, for the same
        // reason: it takes the background manager's lock.
        let leaves = tree.leaves();
        let count = leaves.len();
        // One owner for the keyboard, resolved the same way the drop handler
        // and the caret resolve it (#2620).
        let focus = self.split_focus();
        let mut footers: HashMap<Uuid, SharedString> = HashMap::new();
        let mut tasks: HashMap<Uuid, SharedString> = HashMap::new();
        for (ix, session) in leaves.iter().enumerate() {
            footers.insert(
                *session,
                SharedString::from(self.pane_footer_text(*session, ix + 1, count)),
            );
            if let Some(label) = self.running_tasks_label_for(*session) {
                tasks.insert(*session, SharedString::from(label));
            }
        }
        self.frame_footers = footers.clone();
        self.frame_tasks = tasks;
        // The tree is borrowed again here: the label pass above wrote the
        // window's own fields, which the first borrow could not outlive.
        let Some(tree) = self.panes.as_ref() else {
            return chat;
        };
        // Each leaf records the frame the walk is about to give it, so its own
        // transcript wraps to its width instead of the window's (#2624,
        // #2805). The width comes from the tree's shares, the same one the
        // plan strip reads for its density below.
        //
        // A pane is inserted through gpui's cached view path (#2805), so it
        // replays its last frame until it is notified. The stamp is everything
        // its draw reads from the window: a different one redraws the pane,
        // and a pane nobody touched costs one hash.
        for session in leaves.iter() {
            if Some(*session) == self.chat_session {
                continue;
            }
            let share = tree.leaf_width_share(*session).unwrap_or(1.0);
            let width = if share < 1.0 {
                split_pane_width(content, false, share)
            } else {
                content
            };
            let stamp = self.pane_stamp(*session, width, count);
            if let Some(pane) = self.pane_states.get(session) {
                pane.update(cx, |pane, cx| pane.sync_frame(width, stamp, cx));
            }
        }
        // Per-pane chrome (#2530): every pane carries its own plan strip,
        // queue and composer, exactly as the terminal's panes do. A pane
        // builds that column in its own render (#2805); what is built here is
        // the chat leaf's, because its state is the window's own.
        let mut chrome: HashMap<Uuid, AnyElement> = HashMap::new();
        // One topic strip per pane, in that pane's own chrome (#2686): the
        // strip used to resolve `self.chat_session`, so every pane in a
        // wall showed the chat leaf's topics and clicking one moved a
        // conversation the user was not reading.
        let mut tops: HashMap<Uuid, AnyElement> = HashMap::new();
        if interactive {
            for session in &leaves {
                let is_chat = Some(*session) == self.chat_session;
                // The pane builds its own topics, plan strip, queue, offers,
                // running task line and composer (#2805). The window's chrome
                // below is for the chat leaf alone, whose state is the
                // window's own.
                if !is_chat {
                    continue;
                }
                // The chat leaf's topics, resolved from its own session, the
                // same way a pane resolves its own.
                if let Some(strip) = self.pane_topics(*session, cx) {
                    tops.insert(*session, strip);
                }
                // The chat leaf owns the keyboard exactly when no pane does.
                let focused = focus.is_none();
                // Each pane's plan and queue, read from that pane's session.
                // The plan is the terminal's three-line pane strip (#2622):
                // a split pane is a fraction of the window, and the full
                // checklist clipped mid-word and pushed its header past the
                // pane's edge.
                // How much plan this pane can read (#2624): a wall of panes
                // keeps a bar, a pair keeps the running task and the next one
                // or two. The width comes from the tree, so a narrow column
                // compacts even when there are only two panes.
                let share = self
                    .panes
                    .as_ref()
                    .and_then(|tree| tree.leaf_width_share(*session))
                    .unwrap_or(1.0);
                let pane_width = if share < 1.0 {
                    split_pane_width(content, false, share)
                } else {
                    content
                };
                let density = plan_strip::pane_density(count, pane_width);
                let expanded = self.plan_expanded.contains(session);
                // A plan waiting on approval is the one card a pane must not
                // lose in a split (#2755): it is the only place the tap lives.
                let actions = self.plan_actions(*session, cx);
                let (plan_view, queued, editor) = {
                    self.plan.refresh();
                    (
                        self.plan
                            .editing_view(Some(&actions))
                            .or_else(|| self.plan.pane_view(density, expanded)),
                        self.queued_for_strip(),
                        self.composer.editor.clone(),
                    )
                };
                let mut col = div().flex().flex_col().flex_none().gap_2();
                if let Some(view) = plan_view {
                    // A click trades the conversation's room for the whole
                    // checklist, per pane (#2624). The handler stops the event
                    // so the press does not also hand the leaf the keyboard.
                    let plan_session = *session;
                    col = col.child(
                        view.id(SharedString::from(format!("plan-strip-{plan_session}")))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                                    cx.stop_propagation();
                                    this.toggle_pane_plan(plan_session, cx);
                                }),
                            ),
                    );
                }
                if !queued.is_empty() {
                    let strip_session = *session;
                    // Only a folded queue is worth a click, so a two-message
                    // list does not react to a stray tap (#2629).
                    let folds = queued.len() > 2;
                    col = col.child(
                        queue_strip(&queued, self.composer.queue_expanded)
                            .id(SharedString::from(format!("queue-strip-{strip_session}")))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this: &mut Self, _, _, cx| {
                                    cx.stop_propagation();
                                    if folds {
                                        this.composer.queue_expanded =
                                            !this.composer.queue_expanded;
                                        cx.notify();
                                    }
                                }),
                            ),
                    );
                }
                // The follow-up offer sits with the queue, above the composer
                // it will send into (#2708).
                let offers = agent_bridge::options_snapshot(*session);
                if !offers.is_empty() {
                    let rows = self.option_rows(*session, offers, cx);
                    col = col.child(options_strip(&rows));
                }
                if let Some(label) = self.running_tasks_label_for(*session) {
                    col = col.child(task_strip(label));
                }
                // The pane holding the keyboard draws the real composer; the
                // others show the draft they hold, dimmed and inert, which is
                // the same "disabled while unfocused" language the rest of the
                // split uses.
                // Select mode and the forward picker both stand where the
                // composer stands (#2613, #2608): they are answers to the
                // conversation, not a second panel, and the composer has
                // nothing to say while either is up. Select mode belongs to
                // the chat; a forward opened in another leaf draws there.
                // The picker outranks the bar: it is opened from select mode
                // and the picks are still live under it, so the bar would
                // otherwise keep the slot the destination is chosen in.
                let composer: AnyElement = if focused {
                    if is_chat && self.forward_pick.is_some() && self.forward_leaf.is_none() {
                        self.forward_picker(cx)
                    } else if is_chat && self.msg_select.is_active() {
                        self.select_bar(cx)
                    } else {
                        // The popups that hang over the input travel with it:
                        // a split must not lose the slash list or the emoji
                        // panel, and they read the field of the pane that owns
                        // the keyboard, not the chat underneath it (#2530,
                        // #2610).
                        let mut cell = div().flex().flex_col().flex_none().gap_2();
                        let slash_popup = editor.read(cx).command_popup();
                        let emoji_popup = editor.read(cx).emoji_popup();
                        if self.topic_draft.is_none()
                            && let Some((names, selected)) = slash_popup
                        {
                            cell = cell.child(command_popup_view(&names, selected));
                        } else if self.topic_draft.is_none()
                            && let Some((items, selected)) = emoji_popup
                        {
                            cell = cell.child(emoji_popup_view(&items, selected));
                        }
                        cell.child(editor).into_any_element()
                    }
                } else {
                    let draft = editor.read(cx).content.clone();
                    pane_composer_dim(draft, *session, is_chat, cx.entity().downgrade())
                };
                col = col.child(composer);
                chrome.insert(*session, col.into_any_element());
            }
        }
        let mut ctx = PanesCtx {
            chat: Some(chat),
            chat_session: self.chat_session,
            chat_title: title,
            states: &self.pane_states,
            // The resolved owner, not the raw id: a stale `pane_focus` named a
            // leaf the tree no longer holds, so every leaf read as unfocused,
            // drew dim, and painted no ring (#2620).
            focus,
            areas: &self.split_areas,
            entity: cx.entity().downgrade(),
            interactive,
            footers: &footers,
            chrome: &mut chrome,
            tops: &mut tops,
            drop: self.pane_drop,
        };
        let mut path = Vec::new();
        let tree = self.panes.as_ref().expect("checked a moment ago");
        pane_element(tree, &mut ctx, &mut path)
    }
}
